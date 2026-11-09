//! What happened, for clients that poll instead of holding a WebSocket open (AI agents above
//! all): the session keeps its recent events, numbered, and `events.since` hands out those
//! after a number; `events.meters` is a snapshot of the meters and the audio device's health.

use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json as value};

use super::{ApiError, Event, json, snapshot, transport};
use crate::Session;

/// Events kept for polling.
const KEPT: usize = 256;

/// The recent events worth telling a client that comes back later: project changes, notices,
/// setlist and transport changes, the device's health when it changes. Meters and the moving
/// play position are left out.
#[derive(Debug, Default)]
pub struct EventLog {
    events: VecDeque<(u64, u64, Event)>,
    next: u64,
}

impl EventLog {
    pub fn push(&mut self, event: Event) {
        if matches!(event, Event::Meters(_)) {
            return;
        }
        // One project change stands for the ones before it that nothing else came between.
        if let (Event::ProjectChanged { .. }, Some((_, _, Event::ProjectChanged { .. }))) = (&event, self.events.back())
        {
            self.events.pop_back();
        }
        self.next += 1;
        let ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        self.events.push_back((self.next, ms, event));
        while self.events.len() > KEPT {
            self.events.pop_front();
        }
    }

    /// The number of the newest event; 0 before any.
    pub fn latest(&self) -> u64 {
        self.next
    }

    fn since(&self, after: u64) -> (Vec<Value>, bool) {
        let missed = self.events.front().is_some_and(|(seq, _, _)| *seq > after + 1);
        let events = self
            .events
            .iter()
            .filter(|(seq, _, _)| *seq > after)
            .map(|(seq, ms, e)| {
                let mut v = json(e);
                v["seq"] = (*seq).into();
                v["time_ms"] = (*ms).into();
                v
            })
            .collect();
        (events, missed)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum EventsCommand {
    /// The events after number `since` (all kept when left out), oldest first, with `latest`
    /// to pass as `since` next time; `missed` says older ones were dropped in between. Events:
    /// `project_changed` (with `revision`), `notice`, `setlist`, `transport` (play, stop,
    /// record, loop, metronome changes), `health` (xruns, device lost). Also answers the
    /// project `revision` and the transport now.
    #[serde(rename = "events.since")]
    Since {
        #[serde(default)]
        since: Option<u64>,
    },
    /// Meter levels now (peak dBFS per channel, decaying like the UI's) of the master, every
    /// track and every monitor mix, with the audio device's health (xruns, load, status).
    #[serde(rename = "events.meters")]
    Meters,
}

fn peak_db(peak: (f32, f32)) -> [f64; 2] {
    let db = |g: f32| if g > 1e-6 { ((20.0 * (g as f64).log10()) * 10.0).round() / 10.0 } else { -150.0 };
    [db(peak.0), db(peak.1)]
}

pub fn execute(session: &mut Session, command: EventsCommand) -> Result<Value, ApiError> {
    match command {
        EventsCommand::Since { since } => {
            let (events, missed) = session.event_log.since(since.unwrap_or(0));
            Ok(value!({
                "latest": session.event_log.latest(),
                "missed": missed,
                "revision": session.revision(),
                "transport": snapshot::transport(session),
                "events": events,
            }))
        }
        EventsCommand::Meters => {
            let level = |id: scythe_core::TrackId| peak_db(session.levels.get(&id).copied().unwrap_or_default());
            let tracks: Vec<Value> = session
                .project
                .tracks
                .iter()
                .map(|t| value!({"id": t.id.0, "name": t.name, "peak_db": level(t.id)}))
                .collect();
            let mixes: Vec<Value> = session
                .project
                .monitor_mixes()
                .into_iter()
                .map(|m| value!({"id": m.track.0, "musician": m.musician, "peak_db": level(m.track)}))
                .collect();
            Ok(value!({
                "master": {"peak_db": peak_db(session.master_level)},
                "tracks": tracks,
                "mixes": mixes,
                "health": transport::health(session),
                "transport": snapshot::transport(session),
            }))
        }
    }
}
