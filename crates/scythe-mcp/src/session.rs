//! A session in this process, for `scythe-mcp --headless`: the engine, the project and a
//! control channel, on a thread of their own.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use scythe_core::render_settings::ResampleMode;
use scythe_engine::{DeviceRequest, Engine, MediaPool};
use scythe_session::Session;
use scythe_session::control::{self, ControlHandle};

use crate::backend::Local;

/// A running session; dropping it stops the session, keeping unsaved changes in the project's
/// backups as the headless server does.
pub struct InProcess {
    handle: ControlHandle,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl InProcess {
    /// Starts the engine (with no audio device when `null_audio`) and opens `project`.
    pub fn start(project: Option<PathBuf>, null_audio: bool) -> Result<Self, String> {
        let (handle, mut pump) = control::channel(Arc::new(|| {}), Arc::new(|_| {}));
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let (started, ready) = mpsc::channel::<Result<(), String>>();
        let thread = std::thread::Builder::new()
            .name("scythe-session".into())
            .spawn(move || {
                let pool = Arc::new(MediaPool::with_resample(ResampleMode::PLAYBACK));
                let engine = if null_audio {
                    Engine::start_offline(pool, 48_000)
                } else {
                    Engine::start_with(pool, &DeviceRequest::default())
                };
                let mut session = Session::new(engine);
                if let Some(path) = &project
                    && let Err(e) = session.open_path(path)
                {
                    let _ = started.send(Err(e));
                    return;
                }
                let _ = started.send(Ok(()));
                while !stopping.load(Ordering::SeqCst) {
                    session.poll();
                    session.update_meters();
                    session.tick_automation();
                    pump.wait(&mut session, Duration::from_millis(10));
                }
                if session.is_recording() {
                    session.stop();
                }
                session.engine.stop();
                if session.dirty {
                    session.write_backup();
                }
            })
            .map_err(|e| e.to_string())?;
        let running = Self { handle, stop, thread: Some(thread) };
        match ready.recv() {
            Ok(Ok(())) => Ok(running),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("the session stopped while starting".into()),
        }
    }

    pub fn backend(&self) -> Local {
        Local(self.handle.clone())
    }
}

impl Drop for InProcess {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
