//! Carries control API commands from other threads (a web server, an MCP bridge) to the
//! thread that owns the [`Session`], and events back out. The session and its engine stay on
//! one thread; everything else talks to it through a [`ControlHandle`].

use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel as mpsc_channel};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::api::events::{Meters, TrackMeter};
use crate::api::snapshot::{self, TransportState};
use crate::api::{self, ApiError, Command, Event};
use crate::{Session, Waker};

/// How often transport and meter events go out at most: about 20 a second.
pub const EVENT_INTERVAL: Duration = Duration::from_millis(50);

pub type Reply = Box<dyn FnOnce(Result<Value, ApiError>) + Send>;

/// Receives every event the session publishes, on the session's thread; it should only hand
/// the event on.
pub type EventSink = Arc<dyn Fn(&Event) + Send + Sync>;

struct Request {
    command: Command,
    reply: Reply,
}

/// The sending side, cheap to clone and usable from any thread.
#[derive(Clone)]
pub struct ControlHandle {
    tx: Sender<Request>,
    wake: Waker,
}

impl ControlHandle {
    /// Queues `command`; `reply` is called with its answer on the session's thread. Returns
    /// false when the session is gone.
    pub fn send(&self, command: Command, reply: impl FnOnce(Result<Value, ApiError>) + Send + 'static) -> bool {
        let sent = self.tx.send(Request { command, reply: Box::new(reply) }).is_ok();
        if sent {
            (self.wake)();
        }
        sent
    }
}

/// The session's side: runs queued commands and publishes events.
pub struct ControlPump {
    rx: Receiver<Request>,
    sink: EventSink,
    transport: Option<TransportState>,
    transport_at: Instant,
    meters_at: Instant,
    revision: u64,
}

/// A connected pair. `wake` is called when a command is queued, so a UI that sleeps between
/// frames can run it soon.
pub fn channel(wake: Waker, sink: EventSink) -> (ControlHandle, ControlPump) {
    let (tx, rx) = mpsc_channel();
    let due = Instant::now().checked_sub(EVENT_INTERVAL).unwrap_or_else(Instant::now);
    let pump = ControlPump { rx, sink, transport: None, transport_at: due, meters_at: due, revision: 0 };
    (ControlHandle { tx, wake }, pump)
}

impl ControlPump {
    /// Runs the queued commands, then publishes what changed. Call it every frame.
    pub fn pump(&mut self, session: &mut Session) {
        while let Ok(request) = self.rx.try_recv() {
            Self::run(session, request);
        }
        self.publish(session);
    }

    /// Waits up to `timeout` for a command, then pumps. For a loop without a UI to pace it.
    pub fn wait(&mut self, session: &mut Session, timeout: Duration) {
        match self.rx.recv_timeout(timeout) {
            Ok(request) => Self::run(session, request),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {}
        }
        self.pump(session);
    }

    fn run(session: &mut Session, request: Request) {
        let answer = api::execute(session, request.command);
        (request.reply)(answer);
    }

    fn publish(&mut self, session: &mut Session) {
        for message in session.take_notices() {
            (self.sink)(&Event::Notice { message });
        }
        if self.revision != session.revision() {
            self.revision = session.revision();
            (self.sink)(&Event::ProjectChanged { revision: self.revision });
        }
        let now = Instant::now();
        let state = snapshot::transport(session);
        let running = state.playing || state.recording;
        let changed = self.transport.as_ref().is_none_or(|last| !same_state(last, &state));
        if changed || (running && now.duration_since(self.transport_at) >= EVENT_INTERVAL) {
            self.transport_at = now;
            (self.sink)(&Event::Transport(state.clone()));
            self.transport = Some(state);
        }
        if now.duration_since(self.meters_at) >= EVENT_INTERVAL {
            self.meters_at = now;
            let meter = |t: &scythe_core::Track| TrackMeter {
                id: t.id.0,
                peak: session.levels.get(&t.id).copied().unwrap_or_default(),
            };
            let tracks: Vec<TrackMeter> = session.project.tracks.iter().map(meter).collect();
            let mixes =
                session.project.tracks.iter().filter(|t| session.project.is_monitor_mix(t.id)).map(meter).collect();
            (self.sink)(&Event::Meters(Meters { master: session.master_level, tracks, mixes }));
        }
    }
}

/// Equal apart from the play position moving on.
fn same_state(a: &TransportState, b: &TransportState) -> bool {
    let position = a.playing || a.position == b.position;
    position
        && a.playing == b.playing
        && a.recording == b.recording
        && a.cursor == b.cursor
        && a.loop_enabled == b.loop_enabled
        && a.loop_range == b.loop_range
        && a.metronome == b.metronome
}
