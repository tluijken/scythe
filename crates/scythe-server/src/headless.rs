//! A session without a window: the audio device, the project, input monitoring and FX, the
//! control API and auto-save, until SIGINT or SIGTERM. Needs no display server.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use scythe_core::render_settings::ResampleMode;
use scythe_engine::{Engine, MediaPool};
use scythe_session::Session;

use crate::config::{Options, USAGE};
use crate::server::{ControlServer, ServerConfig};

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    // A second signal means the clean shutdown is stuck.
    if STOP.swap(true, Ordering::SeqCst) {
        unsafe { libc::_exit(130) };
    }
}

fn install_signal_handlers() {
    let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
}

/// Parses `args` (without the program name) and runs; the exit code to end with.
pub fn main_with_args(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return 0;
    }
    let options = match crate::config::options(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}\n\n{USAGE}");
            return 2;
        }
    };
    if options.install_service {
        return report(crate::service::install(args, &options));
    }
    if options.status {
        return report(crate::service::status(&options));
    }
    report(run(options))
}

fn report(result: Result<(), String>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

pub fn run(options: Options) -> Result<(), String> {
    if options.list_devices {
        crate::config::print_devices(options.list_all);
        return Ok(());
    }
    install_signal_handlers();
    scythe_engine::realtime::enable();
    scythe_engine::plugin_scan::Catalog::global().snapshot();
    let pool = Arc::new(MediaPool::with_resample(ResampleMode::PLAYBACK));
    let mut notice = None;
    let engine = if options.null_audio {
        Engine::start_offline(pool, options.devices.sample_rate.unwrap_or(48_000))
    } else {
        let unset = scythe_engine::DeviceRequest { no_input: options.devices.no_input, ..Default::default() };
        let devices = if options.devices == unset {
            let saved = std::fs::read_to_string(scythe_session::preferences_path()).unwrap_or_default();
            let prefs = scythe_core::prefs::Preferences::parse(&saved);
            scythe_engine::DeviceRequest {
                no_input: unset.no_input,
                ..scythe_session::audio::request_from_prefs(&prefs.audio)
            }
        } else {
            options.devices.clone()
        };
        let (engine, fallback) = scythe_session::audio::start_engine(pool, &devices);
        notice = fallback;
        engine
    };
    let info = engine.info().clone();
    eprintln!(
        "audio: {} @ {} Hz{}",
        info.name,
        info.sample_rate,
        info.fallback_reason.as_ref().map(|r| format!(" (fallback: {r})")).unwrap_or_default()
    );
    let mut session = Session::new(engine);
    session.log_notices = true;
    session.no_input = options.devices.no_input;
    session.load_preferences(scythe_session::preferences_path());
    if let Some(n) = notice {
        session.notify(n);
    }
    if let Some(path) = &options.project {
        session.open_path(path)?;
    }

    let mut control = None;
    if let Some(addr) = options.control {
        let config = ServerConfig {
            addr,
            token: options.token.clone(),
            access_file: options.access_file.clone(),
            folders: options.folders.clone(),
        };
        if config.is_public() && config.token.is_none() {
            eprintln!("warning: the control API at {addr} takes commands from anyone on the network; set a token");
        }
        let (server, pump) = ControlServer::start(config, Arc::new(|| {}))
            .map_err(|e| format!("cannot serve the control API on {addr}: {e}"))?;
        eprintln!("control API: http://{}/api", server.addr());
        control = Some((server, pump));
    }
    eprintln!("running; stop with Ctrl+C or SIGTERM");

    let mut surfaces = scythe_surface::SurfaceHost::new(Arc::new(|| {}));
    let mut xruns = XrunLog::default();
    while !STOP.load(Ordering::SeqCst) {
        session.poll();
        xruns.check(&session);
        session.update_meters();
        session.tick_automation();
        surfaces.pump(&mut session);
        match &mut control {
            Some((_, pump)) => pump.wait(&mut session, Duration::from_millis(10)),
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }

    eprintln!("stopping");
    drop(control);
    if session.is_recording() {
        session.stop();
    }
    session.engine.stop();
    if session.dirty {
        session.write_backup();
        if session.project.path.is_some() {
            eprintln!("unsaved changes were written to the project's Backups folder");
        }
    }
    Ok(())
}

/// Reports new xruns on standard error (the service's journal), at most every few seconds.
struct XrunLog {
    seen: (u64, u64),
    at: Instant,
}

impl Default for XrunLog {
    fn default() -> Self {
        Self { seen: (0, 0), at: Instant::now() }
    }
}

impl XrunLog {
    const INTERVAL: Duration = Duration::from_secs(10);

    fn check(&mut self, session: &Session) {
        if self.at.elapsed() < Self::INTERVAL {
            return;
        }
        let h = session.health();
        let now = (h.xruns, h.overloads);
        if now != self.seen {
            eprintln!(
                "audio: {} new xrun(s), {} new overload(s); cpu peak {:.0}%",
                now.0 - self.seen.0,
                now.1 - self.seen.1,
                h.load_peak * 100.0
            );
            self.seen = now;
        }
        self.at = Instant::now();
    }
}
