mod app;
mod commands;
mod demo;
mod dialog;
mod fx_preset;
mod keys;
mod live;
mod nam;
mod palette;
mod plugin_window;
mod pump;
mod script;
mod script_host;
mod theme;
mod ui;

#[cfg(all(test, feature = "itest"))]
mod itest;

use std::path::PathBuf;

struct Shell {
    /// Runs the control API's commands and events, the control surfaces (OSC devices, Mackie
    /// Control, HUI), marker actions and automation between frames.
    _pump: pump::Pump,
    /// The control API, when `--control` serves it; dropped (and stopped) with the window.
    _control: Option<scythe_server::ControlServer>,
    app: app::ScytheApp,
    /// ReaScripts from the command line, run once the window has drawn: a dialog they open
    /// blocks the window until answered.
    scripts: Vec<PathBuf>,
}

impl eframe::App for Shell {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.app.session.acquire();
        self.app.poll();
        if ctx.cumulative_frame_nr() > 0 {
            for script in std::mem::take(&mut self.scripts) {
                self.app.run_script(&script);
            }
        }
        ui::frame(&mut self.app, ui);
        self.app.session.release();
        // Meters and the clock need a steady frame rate even when nothing is playing.
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }
}

/// Renders a whole project to a 24-bit WAV without opening a window.
fn render_headless(
    project: &std::path::Path,
    out: &std::path::Path,
    range: Option<scythe_core::TimeRange>,
) -> Result<(), String> {
    let project = scythe_core::rpp::read_project(project).map_err(|e| e.to_string())?;
    let rate = project.sample_rate.unwrap_or(48_000);
    let pool = scythe_session::render::load_media(&project, rate, Default::default());
    let range = range.unwrap_or_else(|| scythe_core::TimeRange::new(0.0, project.length()));
    let started = std::time::Instant::now();
    scythe_engine::bounce::bounce_to_wav(
        &project,
        &pool,
        range,
        rate,
        scythe_engine::bounce::BitDepth::Int24,
        out,
        |_| {},
    )
    .map_err(|e| e.to_string())?;
    eprintln!("rendered {:.1} s in {:.1} s to {}", range.length(), started.elapsed().as_secs_f64(), out.display());
    Ok(())
}

/// The control API to serve beside the window: `--control ADDR` (or `SCYTHE_CONTROL`), with
/// `--control-token` (or `SCYTHE_CONTROL_TOKEN`) for clients to present.
fn control_config(args: &[String]) -> Result<Option<scythe_server::ServerConfig>, String> {
    let value = |name: &str| {
        let prefix = format!("{name}=");
        args.iter().enumerate().find_map(|(i, a)| match a.strip_prefix(&prefix) {
            Some(v) => Some(v.to_owned()),
            None if a == name => args.get(i + 1).cloned(),
            None => None,
        })
    };
    let Some(addr) = value("--control").or_else(|| std::env::var("SCYTHE_CONTROL").ok()).filter(|a| a != "off") else {
        return Ok(None);
    };
    let addr = addr.parse().map_err(|_| format!("--control takes an address like 0.0.0.0:8770, not {addr}"))?;
    let token =
        value("--control-token").or_else(|| std::env::var("SCYTHE_CONTROL_TOKEN").ok()).filter(|t| !t.is_empty());
    let config = scythe_server::ServerConfig {
        access_file: scythe_server::access::default_file(),
        ..scythe_server::ServerConfig::new(addr, token)
    };
    if config.is_public() && config.token.is_none() {
        eprintln!("warning: the control API at {addr} takes commands from anyone on the network; set a token");
    }
    Ok(Some(config))
}

/// The devices to open: the ones saved in the preferences, with the command line's audio
/// flags over them. Naming another audio system drops the saved devices, which belong to the
/// saved one.
fn devices_for(flags: &scythe_server::config::AudioConfig, no_input: bool) -> scythe_engine::DeviceRequest {
    let saved = std::fs::read_to_string(scythe_session::preferences_path()).unwrap_or_default();
    let saved = scythe_session::audio::request_from_prefs(&scythe_core::prefs::Preferences::parse(&saved).audio);
    overlay(saved, flags, no_input)
}

fn overlay(
    mut r: scythe_engine::DeviceRequest,
    flags: &scythe_server::config::AudioConfig,
    no_input: bool,
) -> scythe_engine::DeviceRequest {
    if let Some(host) = &flags.host {
        if !r.host.as_deref().is_some_and(|h| h.eq_ignore_ascii_case(host)) {
            (r.output, r.input) = (None, None);
        }
        r.host = Some(host.clone());
    }
    r.output = flags.output.clone().or(r.output);
    r.input = flags.input.clone().or(r.input);
    r.sample_rate = flags.sample_rate.or(r.sample_rate);
    r.buffer_size = flags.buffer_size.or(r.buffer_size);
    r.output_channels = flags.output_channels.or(r.output_channels);
    r.input_channels = flags.input_channels.or(r.input_channels);
    r.no_input = no_input;
    r
}

fn is_script(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["lua", "eel", "py"].iter().any(|s| e.eq_ignore_ascii_case(s)))
}

fn app_icon() -> egui::IconData {
    let img = image::load_from_memory(include_bytes!("../assets/icon.png")).expect("bundled icon").to_rgba8();
    egui::IconData { width: img.width(), height: img.height(), rgba: img.into_raw() }
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "--script-dialog") {
        dialog::run_child();
        return Ok(());
    }
    // `--view START-END` (seconds) opens zoomed to that range.
    let view = args.iter().find_map(|a| a.strip_prefix("--view=")).and_then(|v| {
        let (a, b) = v.split_once('-')?;
        Some(scythe_core::TimeRange::new(a.parse().ok()?, b.parse().ok()?))
    });
    if args.iter().any(|a| a == "--headless") {
        std::process::exit(scythe_server::headless::main_with_args(&args));
    }
    let control = match control_config(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let valued = |i: usize| {
        i > 0 && {
            let flag = args[i - 1].as_str();
            matches!(flag, "--control" | "--control-token") || scythe_server::config::VALUED.contains(&flag)
        }
    };
    if args.iter().any(|a| a == "--list-devices") {
        scythe_server::config::print_devices();
        return Ok(());
    }
    let audio = match scythe_server::config::audio_flags(&args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    // Like REAPER, a ReaScript on the command line runs once the project is open.
    let (scripts, positional): (Vec<PathBuf>, Vec<PathBuf>) = args
        .iter()
        .enumerate()
        .filter(|(i, a)| !a.starts_with("--") && !valued(*i))
        .map(|(_, a)| PathBuf::from(a))
        .partition(|p| is_script(p));
    let open = positional.into_iter().next();
    let demo = args.iter().any(|a| a == "--demo") || open.is_none();
    let play = args.iter().any(|a| a == "--play");
    let no_input = args.iter().any(|a| a == "--no-input");
    let null_audio = args.iter().any(|a| a == "--null-audio");
    let setlist = args.iter().any(|a| a == "--setlist");

    if let Some(at) = args.iter().position(|a| a == "--render-queue") {
        let dir = args.get(at + 1).filter(|a| !a.starts_with("--")).map(std::path::PathBuf::from);
        let Some(dir) = dir.or_else(scythe_core::render_queue::default_dir) else {
            eprintln!("--render-queue needs a folder");
            std::process::exit(2);
        };
        let run = scythe_session::render::run_queue(&dir);
        for (queued, e) in &run.failed {
            eprintln!("{}: {e}", queued.display());
        }
        std::process::exit(if run.failed.is_empty() { 0 } else { 1 });
    }
    if args.iter().any(|a| a == "--render-project") {
        let Some(path) = open.as_deref() else {
            eprintln!("--render-project needs a project file");
            std::process::exit(2);
        };
        match scythe_session::render::render_project_file(path) {
            Ok(_) => std::process::exit(0),
            Err(e) => {
                eprintln!("render failed: {e}");
                std::process::exit(1);
            }
        }
    }
    if let Some(out) = args.iter().find_map(|a| a.strip_prefix("--render=")) {
        let Some(path) = open.as_deref() else {
            eprintln!("--render needs a project file");
            std::process::exit(2);
        };
        let range = args.iter().find_map(|a| a.strip_prefix("--range=")).and_then(|v| {
            let (a, b) = v.split_once('-')?;
            Some(scythe_core::TimeRange::new(a.parse().ok()?, b.parse().ok()?))
        });
        match render_headless(path, std::path::Path::new(out), range) {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("render failed: {e}");
                std::process::exit(1);
            }
        }
    }

    scythe_engine::realtime::enable();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Scythe")
            .with_inner_size([1440.0, 940.0])
            .with_min_inner_size([900.0, 600.0])
            .with_drag_and_drop(true)
            .with_app_id("scythe")
            .with_icon(app_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "Scythe",
        options,
        Box::new(move |cc| {
            theme::install(&cc.egui_ctx);
            if let Some(scale) = std::env::var("SCYTHE_UI_SCALE").ok().and_then(|v| v.parse::<f32>().ok()) {
                cc.egui_ctx.set_zoom_factor(scale.clamp(0.5, 3.0));
            }
            let devices = devices_for(&audio, no_input);
            let mut app = app::ScytheApp::with_devices(cc.egui_ctx.clone(), &devices, null_audio);
            app.load_preferences(scythe_session::preferences_path());
            ui::window::load(&cc.egui_ctx, ui::window::placements_file());
            app.view.setlist = setlist;
            match open {
                Some(path) => app.open_path(&path),
                None if demo => app.load_demo(),
                None => {}
            }
            let info = app.engine.info();
            eprintln!(
                "audio: {} @ {} Hz{}",
                info.name,
                info.sample_rate,
                info.fallback_reason.as_ref().map(|r| format!(" (fallback: {r})")).unwrap_or_default()
            );
            if view.is_some() {
                app.view.fit = view;
            }
            if play {
                app.toggle_play();
            }
            app.session.script_runner = Some(crate::script_host::control_runner());
            let doorbell = pump::doorbell(&app.session);
            let (control, commands) = match control.map(|c| scythe_server::ControlServer::start(c, doorbell.clone())) {
                Some(Ok((server, commands))) => {
                    eprintln!("control API: http://{}/api", server.addr());
                    (Some(server), Some(commands))
                }
                Some(Err(e)) => {
                    app.notify(format!("Control API unavailable: {e}"));
                    (None, None)
                }
                None => (None, None),
            };
            let surfaces = scythe_surface::SurfaceHost::new(doorbell);
            let wake = cc.egui_ctx.clone();
            let pump = pump::Pump::start(
                &app.session,
                surfaces,
                commands,
                std::sync::Arc::new(move || wake.request_repaint()),
            );
            app.session.release();
            Ok(Box::new(Shell { _pump: pump, _control: control, app, scripts }))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn audio_flags_override_the_saved_device() {
        let saved = scythe_engine::DeviceRequest {
            host: Some("ALSA".into()),
            output: Some("hw:CARD=USB,DEV=0".into()),
            sample_rate: Some(44_100),
            output_channels: Some(20),
            ..Default::default()
        };
        let flags =
            scythe_server::config::audio_flags(&args(&["--sample-rate", "48000", "--input-channels=18"])).unwrap();
        let r = overlay(saved.clone(), &flags, false);
        assert_eq!(r.output.as_deref(), Some("hw:CARD=USB,DEV=0"));
        assert_eq!((r.sample_rate, r.output_channels, r.input_channels), (Some(48_000), Some(20), Some(18)));
        let flags = scythe_server::config::audio_flags(&args(&["--audio-host", "JACK"])).unwrap();
        let r = overlay(saved, &flags, true);
        assert_eq!((r.host.as_deref(), r.output.as_deref(), r.no_input), (Some("JACK"), None, true));
        assert!(scythe_server::config::audio_flags(&args(&["--buffer-size", "lots"])).is_err());
    }
}
