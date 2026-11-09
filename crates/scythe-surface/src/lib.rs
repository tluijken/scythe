//! Control surfaces: OSC devices driven by REAPER's `.ReaperOSC` pattern files, Mackie
//! Control (with extenders) and HUI over MIDI, configured as REAPER keeps them in reaper.ini
//! (`csurf_<i>` lines in the preferences). A [`SurfaceHost`] runs them on the session's
//! thread: each update it applies what the devices sent through the control API and sends
//! the devices what changed.

pub use scythe_session::actions;
pub mod hui;
pub mod mcu;
pub mod midi;
pub mod osc;
pub mod view;

use std::sync::Arc;
use std::time::{Duration, Instant};

use scythe_core::TrackId;
use scythe_core::surface::{MidiKind, Surface};
use scythe_session::api::{self, ApiError};
use scythe_session::{AutoTarget, Session, SurfaceStatus};
use serde_json::{Value, json};

/// Called from a device's thread when it sent something, so the session's thread wakes up.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// How often feedback goes out to MIDI surfaces at most.
const FEEDBACK_INTERVAL: Duration = Duration::from_millis(30);
/// How long a surface that failed to open waits before trying again.
const RETRY_INTERVAL: Duration = Duration::from_secs(5);

/// Runs a control API command given as JSON.
pub(crate) fn exec(session: &mut Session, command: Value) -> Result<Value, ApiError> {
    api::execute_json(session, command)
}

/// Runs an action a surface asked for; when it cannot run, says why as the UI would.
pub(crate) fn run_action(session: &mut Session, action: &str) {
    if let Err(e) = actions::run(session, action) {
        session.notify(format!("Control surface: {e}"));
    }
}

/// Moves a fader: writes automation when the track writes it while playing, else sets the
/// volume. `None` is the master.
pub(crate) fn set_volume(session: &mut Session, track: Option<TrackId>, gain: f64) {
    let gain = gain.clamp(0.0, scythe_core::units::db_to_gain(24.0));
    if let Some(id) = track
        && session.automate(id, AutoTarget::Volume, gain)
    {
        return;
    }
    let target = track.map_or(json!("master"), |id| json!(id.0));
    let _ = exec(session, json!({ "cmd": "track.volume", "track": target, "volume": gain }));
}

pub(crate) fn set_pan(session: &mut Session, track: Option<TrackId>, pan: f64) {
    let pan = pan.clamp(-1.0, 1.0);
    if let Some(id) = track
        && session.automate(id, AutoTarget::Pan, pan)
    {
        return;
    }
    let target = track.map_or(json!("master"), |id| json!(id.0));
    let _ = exec(session, json!({ "cmd": "track.pan", "track": target, "pan": pan }));
}

enum Running {
    Osc(Box<osc::OscSurface>),
    Mcu(Box<mcu::McuSurface>),
    Hui(Box<hui::HuiSurface>),
}

struct Slot {
    config: Surface,
    running: Option<Running>,
    error: Option<String>,
    tried: Option<Instant>,
}

/// The surfaces in the preferences, opened and kept in step with them.
pub struct SurfaceHost {
    slots: Vec<Slot>,
    wake: Waker,
    /// The bank Mackie and HUI surfaces share, as REAPER's global bank offset: the track the
    /// first strip shows, from 0.
    bank: usize,
    fed_at: Instant,
}

impl SurfaceHost {
    pub fn new(wake: Waker) -> Self {
        Self { slots: Vec::new(), wake, bank: 0, fed_at: Instant::now() }
    }

    /// Opens, closes and runs the surfaces; call it every frame on the session's thread.
    pub fn pump(&mut self, session: &mut Session) {
        session.poll_control();
        self.sync(session);
        let feed = self.fed_at.elapsed() >= FEEDBACK_INTERVAL;
        if feed {
            self.fed_at = Instant::now();
        }
        let mut view = None;
        for slot in &mut self.slots {
            match &mut slot.running {
                Some(Running::Osc(s)) => s.pump(session),
                Some(Running::Mcu(s)) => {
                    s.input(session, &mut self.bank);
                    if feed {
                        s.feedback(view.get_or_insert_with(|| view::View::read(session)), self.bank);
                    }
                }
                Some(Running::Hui(s)) => {
                    s.input(session, &mut self.bank);
                    if feed {
                        s.feedback(view.get_or_insert_with(|| view::View::read(session)), self.bank);
                    }
                }
                None => {}
            }
        }
        self.bank = self.bank.min(session.project.tracks.len().saturating_sub(1));
    }

    fn sync(&mut self, session: &mut Session) {
        let wanted = &session.prefs.surfaces;
        let same = wanted.len() == self.slots.len() && wanted.iter().zip(&self.slots).all(|(w, s)| *w == s.config);
        let mut changed = !same;
        if !same {
            let mut old: Vec<Slot> = std::mem::take(&mut self.slots);
            for config in wanted {
                let slot = match old.iter().position(|s| s.config == *config) {
                    Some(i) => old.remove(i),
                    None => Slot { config: config.clone(), running: None, error: None, tried: None },
                };
                self.slots.push(slot);
            }
        }
        for slot in &mut self.slots {
            if slot.running.is_some() || slot.tried.is_some_and(|t| t.elapsed() < RETRY_INTERVAL) {
                continue;
            }
            let first = slot.tried.is_none();
            slot.tried = Some(Instant::now());
            let (running, error) = match open(&slot.config, self.wake.clone()) {
                Ok(r) => (r, None),
                Err(e) => (None, Some(e)),
            };
            if error != slot.error || first {
                if let Some(e) = error.as_ref().filter(|_| error != slot.error) {
                    session.notify(format!("Control surface {}: {e}", slot.config.describe()));
                }
                changed = true;
            }
            if let Some(Running::Osc(o)) = &running {
                for w in &o.warnings {
                    session.notify(format!("OSC: {w}"));
                }
            }
            changed |= running.is_some();
            slot.running = running;
            slot.error = error;
        }
        if changed {
            session.surface_status = self.status();
        }
    }

    /// How each configured surface is doing, in the order of the preferences.
    pub fn status(&self) -> Vec<SurfaceStatus> {
        self.slots
            .iter()
            .map(|s| SurfaceStatus {
                line: s.config.line(),
                running: s.running.is_some(),
                error: s.error.clone().or_else(|| match &s.config {
                    Surface::Midi(m) if matches!(m.kind, MidiKind::FaderPort | MidiKind::FaderPort2) => {
                        Some("PreSonus FaderPort surfaces are kept in the settings but not driven yet".into())
                    }
                    Surface::Other { kind, .. } => Some(format!("{kind} surfaces are not supported")),
                    _ => None,
                }),
                port: match &s.running {
                    Some(Running::Osc(o)) => Some(o.local_port()),
                    _ => None,
                },
            })
            .collect()
    }
}

fn open(config: &Surface, wake: Waker) -> Result<Option<Running>, String> {
    Ok(match config {
        Surface::Osc(o) => Some(Running::Osc(Box::new(osc::OscSurface::open(o, wake)?))),
        Surface::Midi(m) => match m.kind {
            MidiKind::Mcu | MidiKind::McuExtender => Some(Running::Mcu(Box::new(mcu::McuSurface::open(m, wake)?))),
            MidiKind::Hui => Some(Running::Hui(Box::new(hui::HuiSurface::open(m, wake)?))),
            MidiKind::FaderPort | MidiKind::FaderPort2 => None,
        },
        Surface::Other { .. } => None,
    })
}
