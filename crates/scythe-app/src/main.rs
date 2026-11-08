mod app;
mod demo;
mod fx_preset;
mod palette;
mod plugin_window;
mod script;
mod theme;
mod ui;

use std::path::PathBuf;

use scythe_core::Project;

struct Shell {
    app: app::ScytheApp,
}

impl eframe::App for Shell {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.app.poll();
        self.app.update_meters();
        self.app.tick_editors();
        self.app.tick_automation();
        self.app.tick_script();
        palette::handle_shortcuts(&mut self.app, &ctx);
        ui::root(&mut self.app, ui);
        // Meters and the clock need a steady frame rate even when nothing is playing.
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }
}

/// Renders with the project's own render settings, like REAPER's `-renderproject`.
fn render_with_project_settings(path: &std::path::Path) -> Result<(), String> {
    use scythe_core::render_settings::{Format, RenderSettings};
    let project = scythe_core::rpp::read_project(path).map_err(|e| e.to_string())?;
    let settings = RenderSettings::from_project(&project);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let jobs = settings.jobs(&project, now)?;
    if let Format::Other(ref id) = settings.format {
        eprintln!("note: the project renders to the {id} format; writing 24-bit WAV instead");
    }
    let encoding = scythe_engine::bounce::Encoding::from_format(&settings.format);
    let rate = encoding.sample_rate().or(settings.sample_rate).or(project.sample_rate).unwrap_or(48_000);
    let pool = load_media(&project, rate, settings.resample);
    for (range, out) in jobs {
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let finish = scythe_engine::bounce::Finish::from_settings(&settings);
        let channels = settings.channels;
        scythe_engine::bounce::bounce_finished(&project, &pool, range, rate, channels, encoding, &out, finish, |_| {})
            .map_err(|e| e.to_string())?;
        eprintln!("rendered {:.1}-{:.1} s to {}", range.start, range.end, out.display());
    }
    Ok(())
}

pub(crate) fn load_media(
    project: &scythe_core::Project,
    rate: u32,
    resample: scythe_core::render_settings::ResampleMode,
) -> scythe_engine::MediaPool {
    use rayon::prelude::*;
    let pool = scythe_engine::MediaPool::with_resample(resample);
    let mut files: Vec<std::path::PathBuf> = project
        .tracks
        .iter()
        .flat_map(|t| &t.items)
        .flat_map(|i| &i.takes)
        .filter_map(|t| t.source.file().map(|f| scythe_core::rpp::resolve_media_path(project, f)))
        .collect();
    files.sort();
    files.dedup();
    files.par_iter().for_each(|path| {
        if let Err(e) = pool.load(path, rate) {
            eprintln!("warning: {e}");
        }
    });
    pool.prepare_project(project);
    pool
}

/// Renders a whole project to a 24-bit WAV without opening a window.
fn render_headless(
    project: &std::path::Path,
    out: &std::path::Path,
    range: Option<scythe_core::TimeRange>,
) -> Result<(), String> {
    let project = scythe_core::rpp::read_project(project).map_err(|e| e.to_string())?;
    let rate = project.sample_rate.unwrap_or(48_000);
    let pool = load_media(&project, rate, Default::default());
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

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `--view START-END` (seconds) opens zoomed to that range.
    let view = args.iter().find_map(|a| a.strip_prefix("--view=")).and_then(|v| {
        let (a, b) = v.split_once('-')?;
        Some(scythe_core::TimeRange::new(a.parse().ok()?, b.parse().ok()?))
    });
    let open: Option<PathBuf> = args.iter().find(|a| !a.starts_with("--")).map(PathBuf::from);
    let demo = args.iter().any(|a| a == "--demo") || open.is_none();
    let play = args.iter().any(|a| a == "--play");
    let no_input = args.iter().any(|a| a == "--no-input");

    if args.iter().any(|a| a == "--render-project") {
        let Some(path) = open.as_deref() else {
            eprintln!("--render-project needs a project file");
            std::process::exit(2);
        };
        match render_with_project_settings(path) {
            Ok(()) => std::process::exit(0),
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

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Scythe")
            .with_inner_size([1440.0, 940.0])
            .with_min_inner_size([900.0, 600.0])
            .with_drag_and_drop(true),
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
            let mut app = app::ScytheApp::new(cc.egui_ctx.clone(), Project::default());
            app.no_input = no_input;
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
            Ok(Box::new(Shell { app }))
        }),
    )
}
