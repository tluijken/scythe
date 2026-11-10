//! Runs the session's side of the control API, the control surfaces, marker actions and
//! automation on a thread of their own, so they keep their pace when the window gets no
//! frames (hidden, minimised, or behind another window on Wayland). The window and the thread
//! take turns holding the [`Session`]: the window for the length of a frame, the thread for
//! one tick.

use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use scythe_session::control::ControlPump;
use scythe_session::{Session, Waker};
use scythe_surface::SurfaceHost;

/// How long the thread sleeps between ticks when nothing rings: the headless server's pace.
const TICK: Duration = Duration::from_millis(10);

struct Shared {
    /// The session while the window does not hold it.
    session: Mutex<Option<Box<Session>>>,
    returned: Condvar,
    bell: Mutex<bool>,
    rung: Condvar,
    stop: AtomicBool,
}

impl Shared {
    fn ring(&self) {
        *self.bell.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.rung.notify_one();
    }

    fn slot(&self) -> MutexGuard<'_, Option<Box<Session>>> {
        self.session.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The window's hold on the session. It starts out held, so code that never starts a
/// [`Pump`] (tests, tools) uses it as a plain session.
pub struct SessionCell {
    shared: Arc<Shared>,
    held: Option<Box<Session>>,
}

impl SessionCell {
    pub fn new(session: Session) -> Self {
        let shared = Shared {
            session: Mutex::new(None),
            returned: Condvar::new(),
            bell: Mutex::new(false),
            rung: Condvar::new(),
            stop: AtomicBool::new(false),
        };
        Self { shared: Arc::new(shared), held: Some(Box::new(session)) }
    }

    /// Takes the session back, waiting for the pump's tick to end.
    pub fn acquire(&mut self) {
        if self.held.is_some() {
            return;
        }
        let mut slot = self.shared.slot();
        loop {
            if let Some(session) = slot.take() {
                self.held = Some(session);
                return;
            }
            slot = self.shared.returned.wait(slot).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Lends the session to the pump until the next [`Self::acquire`].
    pub fn release(&mut self) {
        if let Some(session) = self.held.take() {
            *self.shared.slot() = Some(session);
            self.shared.returned.notify_all();
        }
    }
}

impl Deref for SessionCell {
    type Target = Session;

    fn deref(&self) -> &Session {
        self.held.as_deref().expect("the window holds the session during a frame")
    }
}

impl DerefMut for SessionCell {
    fn deref_mut(&mut self) -> &mut Session {
        self.held.as_deref_mut().expect("the window holds the session during a frame")
    }
}

/// Wakes the pump thread early: a control API command or a surface's message arrived.
pub fn doorbell(cell: &SessionCell) -> Waker {
    let shared = cell.shared.clone();
    Arc::new(move || shared.ring())
}

/// The pump thread; stopped and joined when dropped.
pub struct Pump {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Pump {
    /// Starts pumping the session of `cell` with `surfaces` and `control`; `changed` is called
    /// after a tick that ran commands or changed the project, so the window can redraw.
    pub fn start(
        cell: &SessionCell,
        mut surfaces: SurfaceHost,
        mut control: Option<ControlPump>,
        changed: Waker,
    ) -> Self {
        let shared = cell.shared.clone();
        let thread = std::thread::Builder::new()
            .name("session pump".into())
            .spawn({
                let shared = shared.clone();
                move || {
                    while !shared.stop.load(Ordering::SeqCst) {
                        {
                            let bell = shared.bell.lock().unwrap_or_else(|e| e.into_inner());
                            let (mut bell, _) = shared
                                .rung
                                .wait_timeout_while(bell, TICK, |rung| !*rung)
                                .unwrap_or_else(|e| e.into_inner());
                            *bell = false;
                        }
                        let mut slot = shared.slot();
                        while slot.is_none() {
                            if shared.stop.load(Ordering::SeqCst) {
                                return;
                            }
                            slot = shared.returned.wait(slot).unwrap_or_else(|e| e.into_inner());
                        }
                        let session = slot.as_deref_mut().expect("the window lent the session");
                        let revision = session.revision();
                        session.poll();
                        session.update_meters();
                        session.tick_automation();
                        surfaces.pump(session);
                        let ran = control.as_mut().map_or(0, |c| c.pump(session));
                        if ran > 0 || revision != session.revision() {
                            changed();
                        }
                    }
                }
            })
            .expect("spawn the session pump");
        Self { shared, thread: Some(thread) }
    }
}

impl Drop for Pump {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.ring();
        drop(self.shared.slot());
        self.shared.returned.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Instant;

    use super::*;

    fn session() -> Session {
        let pool = Arc::new(scythe_engine::MediaPool::default());
        Session::new(scythe_engine::Engine::start_offline(pool, 48_000))
    }

    #[test]
    fn commands_are_answered_while_no_frame_runs() {
        let mut cell = SessionCell::new(session());
        let (events, _) = mpsc::channel::<String>();
        let events = Mutex::new(events);
        let (handle, control) = scythe_session::control::channel(
            doorbell(&cell),
            Arc::new(move |e| {
                let _ = events.lock().unwrap().send(format!("{e:?}"));
            }),
        );
        let surfaces = SurfaceHost::new(doorbell(&cell));
        let pump = Pump::start(&cell, surfaces, Some(control), Arc::new(|| {}));
        cell.release();

        let (tx, rx) = mpsc::channel();
        let command = serde_json::from_value(serde_json::json!({ "cmd": "track.add", "name": "Bass" })).unwrap();
        let sent = Instant::now();
        assert!(handle.send(command, move |answer| tx.send(answer.is_ok()).unwrap()));
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(true));
        assert!(sent.elapsed() < Duration::from_secs(1));

        cell.acquire();
        assert_eq!(cell.project.tracks.last().map(|t| t.name.as_str()), Some("Bass"));
        drop(pump);
    }

    #[test]
    fn the_window_gets_the_session_back_between_ticks() {
        let mut cell = SessionCell::new(session());
        let pump = Pump::start(&cell, SurfaceHost::new(Arc::new(|| {})), None, Arc::new(|| {}));
        for _ in 0..20 {
            cell.release();
            std::thread::sleep(Duration::from_millis(3));
            cell.acquire();
            let _ = cell.project.tracks.len();
        }
        drop(pump);
        cell.acquire();
    }
}
