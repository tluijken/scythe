//! Rendering with a project's own render settings (REAPER's `-renderproject`) and the render
//! queue.

use std::path::{Path, PathBuf};

use scythe_core::Project;
use scythe_core::render_queue;
use scythe_core::render_settings::{Format, RenderSelection, RenderSettings};
use scythe_engine::MediaPool;
use scythe_engine::bounce::{Encoding, Finish, bounce_finished};

use crate::Session;

/// Renders the project file with its own render settings and saved selection; a queued copy
/// writes the files it was queued with. Returns the files written.
pub fn render_project_file(path: &Path) -> Result<Vec<PathBuf>, String> {
    let project = scythe_core::rpp::read_project(path).map_err(|e| e.to_string())?;
    render_project(&project, &RenderSelection::saved(&project))
}

/// Renders `project` with its render settings, `selection` standing for the selected tracks
/// and items.
pub fn render_project(project: &Project, selection: &RenderSelection) -> Result<Vec<PathBuf>, String> {
    let settings = RenderSettings::from_project(project);
    let mut jobs = settings.jobs(project, selection, unix_now())?;
    render_queue::apply_queued_paths(project, &mut jobs);
    if let Format::Other(ref id) = settings.format {
        eprintln!("note: the project renders to the {id} format; writing 24-bit WAV instead");
    }
    let encoding = Encoding::from_format(&settings.format);
    let rate = render_rate(&settings, project);
    let pool = load_media(project, rate, settings.resample);
    let mut written = Vec::new();
    for job in jobs {
        let source = scythe_core::render_source::project_for(project, job.source);
        if let Some(dir) = job.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let finish = Finish::from_settings(&settings);
        bounce_finished(
            &source,
            &pool,
            job.range,
            rate,
            settings.channels,
            encoding.clone(),
            &job.path,
            finish,
            |_| {},
        )
        .map_err(|e| e.to_string())?;
        eprintln!("rendered {:.1}-{:.1} s to {}", job.range.start, job.range.end, job.path.display());
        written.push(job.path);
    }
    Ok(written)
}

/// The rate `settings` render `project` at: the format's fixed rate (CD images), else the render
/// settings' or the project's, never the audio device's.
pub fn render_rate(settings: &RenderSettings, project: &Project) -> u32 {
    Encoding::from_format(&settings.format).sample_rate().unwrap_or_else(|| settings.render_rate(project))
}

/// Loads every file the project's items play, at `rate`.
pub fn load_media(project: &Project, rate: u32, resample: scythe_core::render_settings::ResampleMode) -> MediaPool {
    use rayon::prelude::*;
    let pool = MediaPool::with_resample(resample);
    let mut files: Vec<PathBuf> = project
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

/// What running the queue did.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct QueueRun {
    /// Files written.
    pub rendered: Vec<PathBuf>,
    /// Queued projects that failed, with why; they stay in the queue.
    pub failed: Vec<(PathBuf, String)>,
}

/// Renders every project queued in `dir`, oldest first, deleting each one that renders.
pub fn run_queue(dir: &Path) -> QueueRun {
    let mut run = QueueRun::default();
    for queued in render_queue::list(dir) {
        match render_project_file(&queued) {
            Ok(files) => {
                run.rendered.extend(files);
                let _ = std::fs::remove_file(&queued);
            }
            Err(e) => run.failed.push((queued, e)),
        }
    }
    run
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Session {
    /// The tracks renders take as selected: the selected track. The window passes its own
    /// selection, items included.
    pub fn render_selection(&self) -> RenderSelection {
        RenderSelection { tracks: self.selected_track.into_iter().collect(), items: Vec::new() }
    }

    /// Adds the project, as it is now, to the render queue in `dir` (REAPER's `QueuedRenders`
    /// when left out), with the file names `selection` gives it. Returns the queued copy.
    pub fn queue_render(&mut self, dir: Option<&Path>, selection: &RenderSelection) -> Result<PathBuf, String> {
        self.store_plugin_states();
        let dir = dir.map(Path::to_owned).or_else(render_queue::default_dir).ok_or("no folder for the render queue")?;
        let settings = RenderSettings::from_project(&self.project);
        let jobs = settings.jobs(&self.project, selection, unix_now())?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let (date, time) = scythe_engine::pcmfile::local_date_time();
        let stamp = format!("{}_{}", date.get(2..).unwrap_or_default().replace('-', ""), time.replace('-', ""));
        let path = dir.join(render_queue::file_name(&self.project, &stamp));
        let text = render_queue::queued_text(&self.project, &scythe_core::rpp::project_to_rpp(&self.project), &jobs);
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(path)
    }
}
