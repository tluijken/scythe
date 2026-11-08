use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use scythe_core::history::History;
use scythe_core::midi::{MidiSource, TimedNote};
use scythe_core::rpp::{autosave_path, read_project, resolve_media_path, save_project, write_project};
use scythe_core::{Item, ItemId, Project, Take, TempoMap, TimeRange, TrackId};
use scythe_engine::bounce::{BitDepth, bounce_to_wav};
use scythe_engine::record::{RecordTarget, Recording};
use scythe_engine::{Engine, Media, MediaPool, MediaState};

use crate::palette::PaletteState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectorTab {
    Fx,
    Sends,
    Envelopes,
}

#[derive(Debug)]
pub struct ViewState {
    /// Project time at the left edge of the lanes, in seconds.
    pub start: f64,
    pub px_per_sec: f64,
    pub track_scroll: f32,
    pub show_mixer: bool,
    pub inspector_tab: InspectorTab,
    pub plugin_browser: bool,
    /// Track whose colour palette is open, where, and whether it opened this frame.
    pub color_picker: Option<(TrackId, egui::Pos2, bool)>,
    /// Marker being renamed on the ruler, with the text typed so far.
    pub renaming_marker: Option<(usize, String)>,
    /// Track row height in points.
    pub row_h: f32,
    /// FX slot whose parameters are shown in the inspector.
    pub expanded_fx: Option<(TrackId, usize)>,
    /// Index into `GRID_STEPS`.
    pub grid: usize,
    pub midi_editor: Option<ItemId>,
    pub midi_state: crate::ui::midi_editor::EditorState,
    pub snap: bool,
    pub metronome: bool,
    /// Range to zoom the arrange view to on the next frame, once its width is known.
    pub fit: Option<TimeRange>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Selection {
    pub track: Option<TrackId>,
    pub item: Option<ItemId>,
}

enum LoaderMsg {
    Loaded { path: PathBuf, result: Result<Arc<Media>, String>, place: Option<(TrackId, f64)> },
    Bounced(Result<PathBuf, String>),
}

/// What a cached MIDI note layout depends on.
type MidiKey = (u64, u64, u64, u64, bool, usize, TempoMap);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderKind {
    Project,
    Selection,
    Stems,
    /// The project's own render settings (bounds, file pattern).
    ProjectSettings,
}

/// Snap grid choices: label and step in quarter notes.
pub const GRID_STEPS: [(&str, f64); 5] = [("1/4", 1.0), ("1/8", 0.5), ("1/16", 0.25), ("1/32", 0.125), ("bar", 4.0)];

pub struct ActiveRecording {
    pub session: Recording,
    pub start: f64,
    pub tracks: Vec<TrackId>,
}

pub struct ScytheApp {
    pub project: Project,
    pub history: History,
    pub engine: Engine,
    pub sel: Selection,
    pub view: ViewState,
    pub palette: Option<PaletteState>,
    /// Copied items with their track offset from the topmost copied track.
    pub clipboard: Vec<(usize, Item)>,
    /// Items selected besides `sel.item`.
    pub multi: HashSet<ItemId>,
    pub recording: Option<ActiveRecording>,
    /// Live input (monitoring) is off for the whole session (`--no-input`).
    pub no_input: bool,
    autosaved_at: Instant,
    midi_cache: HashMap<ItemId, (MidiKey, Arc<Vec<TimedNote>>)>,
    pub dirty: bool,
    pub saved_at: Option<Instant>,
    toast: Option<(String, Instant)>,
    gesture: Option<(String, Project)>,
    loader_tx: Sender<LoaderMsg>,
    loader_rx: Receiver<LoaderMsg>,
    pending: HashSet<PathBuf>,
    pub busy: Option<String>,
    pub ctx: egui::Context,
    /// Displayed meter levels with peak decay, per track and for the master.
    pub levels: HashMap<TrackId, (f32, f32)>,
    pub master_level: (f32, f32),
    meters_at: Instant,
}

impl ScytheApp {
    pub fn new(ctx: egui::Context, project: Project) -> Self {
        let pool = Arc::new(MediaPool::default());
        let engine = Engine::start(pool);
        let (loader_tx, loader_rx) = channel();
        let mut app = Self {
            project,
            history: History::default(),
            engine,
            sel: Selection::default(),
            view: ViewState {
                start: 0.0,
                px_per_sec: 40.0,
                track_scroll: 0.0,
                show_mixer: true,
                inspector_tab: InspectorTab::Fx,
                plugin_browser: false,
                color_picker: None,
                renaming_marker: None,
                row_h: 72.0,
                expanded_fx: None,
                grid: 0,
                midi_editor: None,
                midi_state: Default::default(),
                snap: true,
                metronome: false,
                fit: None,
            },
            palette: None,
            clipboard: Vec::new(),
            multi: HashSet::new(),
            recording: None,
            no_input: false,
            autosaved_at: Instant::now(),
            midi_cache: HashMap::new(),
            dirty: false,
            saved_at: None,
            toast: None,
            gesture: None,
            loader_tx,
            loader_rx,
            pending: HashSet::new(),
            busy: None,
            ctx,
            levels: HashMap::new(),
            master_level: (0.0, 0.0),
            meters_at: Instant::now(),
        };
        if let Some(reason) = app.engine.info().fallback_reason.clone() {
            app.notify(format!("Audio device unavailable, running silent: {reason}"));
        }
        app.after_load();
        app
    }

    fn after_load(&mut self) {
        self.sel = Selection { track: self.project.tracks.first().map(|t| t.id), item: None };
        self.engine.stop();
        self.engine.seek(self.project.cursor);
        self.sync();
        self.view.track_scroll = 0.0;
        let end = self.project.length().max(self.project.tempo.bar_seconds() * 16.0);
        self.view.fit = Some(TimeRange::new(0.0, end));
    }

    /// Drains the engine meters into display levels that fall back at ~20 dB/s.
    pub fn update_meters(&mut self) {
        let dt = self.meters_at.elapsed().as_secs_f32();
        self.meters_at = Instant::now();
        let decay = 10f32.powf(-dt);
        let blend = |old: (f32, f32), new: (f32, f32)| (new.0.max(old.0 * decay), new.1.max(old.1 * decay));
        for t in &self.project.tracks {
            let peak = self.engine.meters().get(t.id).map(|m| m.take()).unwrap_or_default();
            let old = self.levels.get(&t.id).copied().unwrap_or_default();
            self.levels.insert(t.id, blend(old, peak));
        }
        if let Some(rec) = &self.recording {
            for (track, peak) in rec.tracks.iter().zip(rec.session.take_levels()) {
                let old = self.levels.get(track).copied().unwrap_or_default();
                self.levels.insert(*track, blend(old, (peak, peak)));
            }
        }
        self.levels.retain(|id, _| self.project.track(*id).is_some());
        self.master_level = blend(self.master_level, self.engine.meters().master.take());
    }

    // ---- editing -------------------------------------------------------------------------

    /// Applies an undoable edit.
    pub fn edit(&mut self, label: &str, f: impl FnOnce(&mut Project)) {
        let before = self.project.clone();
        f(&mut self.project);
        if self.project != before {
            self.history.record(label, before);
            self.changed();
        }
    }

    /// Continuous edits (fader drags, item moves) record one undo step for the whole gesture.
    pub fn gesture(&mut self, label: &str, f: impl FnOnce(&mut Project)) {
        if self.gesture.is_none() {
            self.gesture = Some((label.to_owned(), self.project.clone()));
        }
        f(&mut self.project);
        self.changed();
    }

    pub fn end_gesture(&mut self) {
        // Item moves and trims end with automatic crossfades on the tracks they touched.
        if self
            .gesture
            .as_ref()
            .is_some_and(|(label, _)| matches!(label.as_str(), "Move item" | "Move items" | "Trim item"))
        {
            let tracks: HashSet<TrackId> =
                self.selected_items().into_iter().filter_map(|id| self.project.find_item(id).map(|(t, _)| t)).collect();
            for t in tracks {
                self.project.auto_crossfade(t);
            }
            self.sync();
        }
        if let Some((label, before)) = self.gesture.take()
            && before != self.project
        {
            self.history.record(label, before);
        }
    }

    pub fn undo(&mut self) {
        self.end_gesture();
        match self.history.undo(&mut self.project) {
            Some(label) => {
                self.notify(format!("Undo: {label}"));
                self.changed();
            }
            None => self.notify("Nothing to undo"),
        }
    }

    pub fn redo(&mut self) {
        match self.history.redo(&mut self.project) {
            Some(label) => {
                self.notify(format!("Redo: {label}"));
                self.changed();
            }
            None => self.notify("Nothing to redo"),
        }
    }

    fn changed(&mut self) {
        self.dirty = true;
        self.sync();
    }

    pub fn toggle_metronome(&mut self) {
        self.view.metronome = !self.view.metronome;
        self.engine.set_metronome(if self.view.metronome { 0.5 } else { 0.0 });
        self.engine.sync(&self.project);
    }

    pub fn sync(&mut self) {
        if self.sel.track.is_some_and(|t| self.project.track(t).is_none()) {
            self.sel.track = None;
        }
        if self.sel.item.is_some_and(|i| self.project.find_item(i).is_none()) {
            self.sel.item = None;
        }
        self.engine.sync(&self.project);
        let monitor = !self.no_input && self.project.tracks.iter().any(|t| t.armed && t.monitor);
        if let Err(e) = self.engine.set_monitoring(monitor) {
            self.notify(format!("Input monitoring unavailable: {e}"));
        }
        self.request_media();
    }

    // ---- media ---------------------------------------------------------------------------

    fn request_media(&mut self) {
        let paths: Vec<PathBuf> = self
            .project
            .tracks
            .iter()
            .flat_map(|t| &t.items)
            .flat_map(|i| &i.takes)
            .filter_map(|t| t.source.file().map(|p| resolve_media_path(&self.project, p)))
            .collect();
        for path in paths {
            if self.engine.pool().get(&path).is_none() && !self.pending.contains(&path) {
                self.spawn_load(path, None);
            }
        }
    }

    fn spawn_load(&mut self, path: PathBuf, place: Option<(TrackId, f64)>) {
        self.pending.insert(path.clone());
        let pool = self.engine.pool().clone();
        let rate = self.engine.sample_rate();
        let tx = self.loader_tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let result = pool.load(&path, rate).map_err(|e| e.to_string());
            let _ = tx.send(LoaderMsg::Loaded { path, result, place });
            ctx.request_repaint();
        });
    }

    /// Notes of a MIDI item placed on the timeline, cached until the item or tempo changes.
    pub fn midi_notes(&mut self, item: &Item) -> Option<Arc<Vec<TimedNote>>> {
        let take = item.take()?;
        let scythe_core::Source::Raw(chunk) = &take.source else { return None };
        let key: MidiKey = (
            item.position.to_bits(),
            item.length.to_bits(),
            take.source_offset.to_bits(),
            take.playrate.to_bits(),
            item.looped,
            chunk.children.len(),
            self.project.tempo.clone(),
        );
        if let Some((k, notes)) = self.midi_cache.get(&item.id)
            && *k == key
        {
            return Some(notes.clone());
        }
        let source = MidiSource::parse(chunk)?;
        let notes = Arc::new(source.place(item, take, &self.project.tempo.timeline()));
        self.midi_cache.insert(item.id, (key, notes.clone()));
        Some(notes)
    }

    pub fn media(&self, take: &Take) -> Option<MediaState> {
        take.source.file().and_then(|p| self.engine.pool().get(&resolve_media_path(&self.project, p)))
    }

    pub fn media_loading(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Imports audio files onto `track` (or a new track) starting at `position`.
    pub fn import_files(&mut self, files: Vec<PathBuf>, track: Option<TrackId>, position: f64) {
        for (i, path) in files.into_iter().enumerate() {
            let track = match (track, i) {
                (Some(t), 0) => t,
                _ => {
                    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    let mut id = TrackId(0);
                    self.edit("Add track", |p| id = p.add_track(name));
                    id
                }
            };
            self.spawn_load(path, Some((track, position)));
        }
    }

    pub fn poll(&mut self) {
        self.autosave();
        while let Ok(msg) = self.loader_rx.try_recv() {
            match msg {
                LoaderMsg::Loaded { path, result, place } => {
                    self.pending.remove(&path);
                    match result {
                        Ok(media) => {
                            if let Some((track, position)) = place {
                                let length = media.duration();
                                let item = Item::new(position, length, Take::from_file(path.clone()));
                                let id = item.id;
                                self.edit("Import media", |p| {
                                    if let Some(t) = p.track_mut(track) {
                                        t.items.push(item);
                                    }
                                });
                                self.sel = Selection { track: Some(track), item: Some(id) };
                            }
                            self.engine.sync(&self.project);
                        }
                        Err(e) => self.notify(format!("Could not load {}: {e}", path.display())),
                    }
                }
                LoaderMsg::Bounced(result) => {
                    self.busy = None;
                    match result {
                        Ok(path) => self.notify(format!("Rendered {}", path.display())),
                        Err(e) => self.notify(format!("Render failed: {e}")),
                    }
                }
            }
        }
    }

    // ---- transport -----------------------------------------------------------------------

    pub fn position(&self) -> f64 {
        if self.engine.transport().is_playing() {
            self.engine.transport().position_seconds()
        } else {
            self.project.cursor
        }
    }

    pub fn is_playing(&self) -> bool {
        self.engine.transport().is_playing()
    }

    /// Play/pause: pausing leaves the edit cursor where playback stopped.
    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    /// Starts recording every armed track from the edit cursor, or stops and adds the takes.
    pub fn toggle_record(&mut self) {
        if let Some(rec) = self.recording.take() {
            self.engine.stop();
            let overruns = rec.session.overruns();
            match rec.session.finish() {
                Ok(takes) => {
                    let start = rec.start;
                    let mut first = None;
                    self.edit("Record", |p| {
                        for take in &takes {
                            if take.frames == 0 {
                                continue;
                            }
                            let len = take.frames as f64 / take.sample_rate as f64;
                            let item = Item::new(start, len, Take::from_file(take.path.clone()));
                            first.get_or_insert(item.id);
                            if let Some(t) = p.track_mut(take.track) {
                                t.items.push(item);
                            }
                        }
                    });
                    self.sel.item = first;
                    let msg = if overruns > 0 { format!(" ({overruns} samples dropped)") } else { String::new() };
                    self.notify(format!("Recorded {} take(s){msg}", takes.len()));
                    self.request_media();
                }
                Err(e) => self.notify(format!("Recording failed: {e}")),
            }
            self.engine.seek(self.project.cursor);
            return;
        }

        let armed: Vec<_> = self.project.tracks.iter().filter(|t| t.armed).cloned().collect();
        if armed.is_empty() {
            self.notify("Arm a track to record");
            return;
        }
        let dir = match self.project.path.as_deref().and_then(Path::parent) {
            Some(d) => d.to_owned(),
            None => crate::demo::data_dir().join("recordings"),
        };
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let targets: Vec<RecordTarget> = armed
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let base: String = if t.name.is_empty() { format!("Track {}", i + 1) } else { t.name.clone() };
                let safe: String =
                    base.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' }).collect();
                RecordTarget {
                    track: t.id,
                    channels: RecordTarget::channels_for_input(t.rec_input),
                    path: dir.join(format!("{}-{stamp}.wav", safe.trim())),
                }
            })
            .collect();
        match Recording::start(targets, self.engine.sample_rate()) {
            Ok(session) => {
                let start = self.project.cursor;
                self.engine.seek(start);
                self.engine.play();
                self.notify(format!("Recording from {}", session.device));
                self.recording = Some(ActiveRecording { session, start, tracks: armed.iter().map(|t| t.id).collect() });
            }
            Err(e) => self.notify(format!("Cannot record: {e}")),
        }
    }

    pub fn toggle_play(&mut self) {
        if self.is_playing() {
            self.project.cursor = self.engine.transport().position_seconds();
            self.engine.stop();
        } else {
            self.engine.seek(self.project.cursor);
            self.engine.play();
        }
    }

    /// Stop returns the play cursor to the edit cursor, as in REAPER.
    pub fn stop(&mut self) {
        if self.is_recording() {
            self.toggle_record();
            return;
        }
        self.engine.stop();
        self.engine.seek(self.project.cursor);
    }

    pub fn set_cursor(&mut self, seconds: f64) {
        self.project.cursor = seconds.max(0.0);
        self.engine.seek(self.project.cursor);
    }

    pub fn go_to_start(&mut self) {
        self.set_cursor(0.0);
        self.view.start = 0.0;
    }

    pub fn toggle_loop(&mut self) {
        let enabled = !self.project.loop_enabled;
        self.edit(if enabled { "Enable repeat" } else { "Disable repeat" }, |p| p.loop_enabled = enabled);
    }

    pub fn set_loop_range(&mut self, range: Option<TimeRange>) {
        self.gesture("Set loop points", |p| p.time_selection = range);
    }

    pub fn snap(&self, seconds: f64, bypass: bool) -> f64 {
        if self.view.snap && !bypass {
            self.project.tempo.timeline().snap_to(seconds, GRID_STEPS[self.view.grid].1)
        } else {
            seconds
        }
    }

    // ---- project files -------------------------------------------------------------------

    pub fn new_project(&mut self) {
        self.engine.pool().clear();
        self.project = Project::default();
        self.history.clear();
        self.dirty = false;
        self.saved_at = None;
        self.after_load();
    }

    pub fn load_demo(&mut self) {
        match crate::demo::build(self.engine.sample_rate()) {
            Ok(project) => {
                self.project = project;
                self.history.clear();
                self.history.set_limit(200);
                self.dirty = false;
                self.saved_at = None;
                self.after_load();
                self.sel.track = self.project.tracks.get(2).map(|t| t.id);
                let tempo = self.project.tempo.clone();
                self.view.fit = Some(TimeRange::new(tempo.bar_start(33), tempo.bar_start(49)));
            }
            Err(e) => self.notify(format!("Could not create demo project: {e}")),
        }
    }

    pub fn open_path(&mut self, path: &Path) {
        match read_project(path) {
            Ok(project) => {
                self.project = project;
                self.history.clear();
                // Undo keeps whole snapshots: bound them to roughly 256 MB for big projects.
                let bytes = std::fs::metadata(path).map_or(0, |m| m.len() as usize).max(1);
                self.history.set_limit((256_000_000 / (bytes * 3)).clamp(30, 200));
                self.dirty = false;
                self.saved_at = None;
                self.after_load();
                self.notify(format!("Opened {}", path.display()));
            }
            Err(e) => self.notify(format!("Could not open {}: {e}", path.display())),
        }
    }

    pub fn open_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new().add_filter("REAPER project", &["rpp", "RPP"]).pick_file() {
            self.open_path(&path);
        }
    }

    /// Every few minutes, writes unsaved changes to a timestamped file in the project's
    /// `Backups` folder. The project file itself is only written by an explicit save.
    fn autosave(&mut self) {
        const INTERVAL: Duration = Duration::from_secs(300);
        if !self.dirty || self.autosaved_at.elapsed() < INTERVAL || self.gesture.is_some() {
            return;
        }
        self.autosaved_at = Instant::now();
        let Some(path) = self.project.path.clone() else { return };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let target = autosave_path(&path, now);
        let result = target
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .map_err(Into::into)
            .and_then(|()| write_project(&self.project, &target));
        if let Err(e) = result {
            self.notify(format!("Auto-save failed: {e}"));
        }
    }

    pub fn save(&mut self) {
        match self.project.path.clone() {
            Some(path) => self.save_to(&path),
            None => self.save_as(),
        }
    }

    pub fn save_as(&mut self) {
        let dialog = rfd::FileDialog::new()
            .add_filter("REAPER project", &["rpp"])
            .set_file_name(format!("{}.rpp", self.project.name()));
        if let Some(path) = dialog.save_file() {
            self.save_to(&path);
        }
    }

    fn save_to(&mut self, path: &Path) {
        self.project.path = Some(path.to_owned());
        self.engine.store_plugin_states(&mut self.project);
        match save_project(&self.project, path) {
            Ok(()) => {
                self.dirty = false;
                self.autosaved_at = Instant::now();
                self.saved_at = Some(Instant::now());
                self.notify(format!("Saved {}", path.display()));
            }
            Err(e) => self.notify(format!("Could not save: {e}")),
        }
    }

    pub fn import_dialog(&mut self) {
        let files = rfd::FileDialog::new()
            .add_filter("Audio", &["wav", "flac", "mp3", "ogg", "aif", "aiff", "m4a", "caf"])
            .pick_files();
        if let Some(files) = files {
            let at = self.project.cursor;
            self.import_files(files, self.sel.track, at);
        }
    }

    pub fn render_dialog(&mut self) {
        self.render(RenderKind::Project);
    }

    /// Renders to 24-bit WAV in the background: the whole project, the time selection, or one
    /// file per track (each soloed, so folder and master processing stay in the stem).
    pub fn render(&mut self, kind: RenderKind) {
        let range = match kind {
            RenderKind::Selection => match self.project.time_selection.filter(|r| !r.is_empty()) {
                Some(r) => r,
                None => {
                    self.notify("Make a time selection first");
                    return;
                }
            },
            _ => TimeRange::new(0.0, self.project.length()),
        };
        if range.is_empty() {
            self.notify("Nothing to render yet");
            return;
        }
        let name = self.project.name();
        let mut ranges: Vec<TimeRange> = Vec::new();
        let mut mp3 = None;
        let jobs: Vec<(Project, PathBuf)> = if kind == RenderKind::ProjectSettings {
            let settings = scythe_core::render_settings::RenderSettings::from_project(&self.project);
            if let scythe_core::render_settings::Format::Mp3 { kbps } = settings.format {
                mp3 = Some(kbps);
            }
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            match settings.jobs(&self.project, now) {
                Ok(jobs) => jobs
                    .into_iter()
                    .map(|(r, path)| {
                        ranges.push(r);
                        (self.project.clone(), path)
                    })
                    .collect(),
                Err(e) => {
                    self.notify(format!("Cannot render: {e}"));
                    return;
                }
            }
        } else if kind == RenderKind::Stems {
            let Some(dir) = rfd::FileDialog::new().set_title("Folder for the stems").pick_folder() else { return };
            (0..self.project.tracks.len())
                .filter(|&i| !self.project.tracks[i].items.is_empty())
                .map(|i| {
                    let mut p = self.project.clone();
                    for (j, t) in p.tracks.iter_mut().enumerate() {
                        t.solo = i == j;
                    }
                    let track = &p.tracks[i];
                    let label = if track.name.is_empty() { format!("Track {}", i + 1) } else { track.name.clone() };
                    let safe: String =
                        label.chars().map(|c| if c.is_alphanumeric() || " -_".contains(c) { c } else { '_' }).collect();
                    (p, dir.join(format!("{name} - {:02} {}.wav", i + 1, safe.trim())))
                })
                .collect()
        } else {
            let dialog = rfd::FileDialog::new().add_filter("WAV", &["wav"]).set_file_name(format!("{name}.wav"));
            let Some(path) = dialog.save_file() else { return };
            vec![(self.project.clone(), path)]
        };
        let pool = self.engine.pool().clone();
        let rate = self.engine.sample_rate();
        let tx = self.loader_tx.clone();
        let ctx = self.ctx.clone();
        self.busy = Some("Rendering…".into());
        std::thread::spawn(move || {
            let mut last = Ok(PathBuf::new());
            for (i, (project, path)) in jobs.into_iter().enumerate() {
                let range = ranges.get(i).copied().unwrap_or(range);
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                last = match mp3 {
                    Some(kbps) => {
                        scythe_engine::bounce::bounce_to_mp3(&project, &pool, range, rate, kbps, &path, |_| {})
                            .map(|()| path)
                    }
                    None => bounce_to_wav(&project, &pool, range, rate, BitDepth::Int24, &path, |_| {})
                        .map(|()| path)
                        .map_err(|e| e.to_string()),
                };
                if last.is_err() {
                    break;
                }
            }
            let _ = tx.send(LoaderMsg::Bounced(last));
            ctx.request_repaint();
        });
    }

    // ---- feedback ------------------------------------------------------------------------

    pub fn notify(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    pub fn toast(&mut self) -> Option<&str> {
        if self.toast.as_ref().is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(4)) {
            self.toast = None;
        }
        self.toast.as_ref().map(|(m, _)| m.as_str())
    }

    pub fn saved_label(&self) -> String {
        if self.dirty {
            return "Unsaved changes".into();
        }
        match self.saved_at {
            None if self.project.path.is_none() => "Not saved yet".into(),
            None => "No changes".into(),
            Some(t) => {
                let s = t.elapsed().as_secs();
                match s {
                    0..60 => "Saved just now".into(),
                    60..3600 => format!("Saved {} min ago", s / 60),
                    _ => format!("Saved {} h ago", s / 3600),
                }
            }
        }
    }

    pub fn selected_track_index(&self) -> Option<usize> {
        self.sel.track.and_then(|t| self.project.track_index(t))
    }

    pub fn delete_selection(&mut self) {
        let items = self.selected_items();
        if !items.is_empty() {
            self.edit("Delete items", |p| {
                for t in &mut p.tracks {
                    t.items.retain(|i| !items.contains(&i.id));
                }
            });
            self.sel.item = None;
            self.multi.clear();
        } else if let Some(track) = self.sel.track {
            let idx = self.project.track_index(track).unwrap_or(0);
            self.edit("Delete track", |p| {
                p.remove_track(track);
            });
            self.sel.track =
                self.project.tracks.get(idx.min(self.project.tracks.len().saturating_sub(1))).map(|t| t.id);
        }
    }

    /// Every selected item: the primary one and those added with Ctrl+click or a marquee.
    pub fn selected_items(&self) -> Vec<ItemId> {
        let mut ids: Vec<ItemId> = self.sel.item.into_iter().chain(self.multi.iter().copied()).collect();
        ids.sort();
        ids.dedup();
        ids.retain(|id| self.project.find_item(*id).is_some());
        ids
    }

    pub fn is_selected(&self, id: ItemId) -> bool {
        self.sel.item == Some(id) || self.multi.contains(&id)
    }

    /// Selected items with their track index, earliest first.
    fn selected_with_tracks(&self) -> Vec<(usize, Item)> {
        let mut out: Vec<(usize, Item)> = self
            .selected_items()
            .into_iter()
            .filter_map(|id| {
                let (track, item) = self.project.find_item(id)?;
                Some((self.project.track_index(track)?, item.clone()))
            })
            .collect();
        out.sort_by(|a, b| a.1.position.total_cmp(&b.1.position));
        out
    }

    pub fn copy_item(&mut self, cut: bool) {
        let items = self.selected_with_tracks();
        let Some(first_track) = items.iter().map(|(t, _)| *t).min() else { return };
        self.clipboard = items.into_iter().map(|(t, it)| (t - first_track, it)).collect();
        if cut {
            self.delete_selection();
        }
    }

    /// Pastes the clipboard at the edit cursor: the topmost copied track lands on the selected
    /// track, the rest keep their spacing in time and in tracks.
    pub fn paste_item(&mut self) {
        if self.clipboard.is_empty() {
            return;
        }
        let base = self.selected_track_index().unwrap_or(0);
        let start = self.clipboard.iter().map(|(_, it)| it.position).fold(f64::INFINITY, f64::min);
        let cursor = self.project.cursor;
        let mut placed = Vec::new();
        let mut end = cursor;
        let clip = self.clipboard.clone();
        self.edit("Paste items", |p| {
            for (offset, item) in &clip {
                let Some(track) = p.tracks.get_mut(base + offset) else { continue };
                let mut new = item.duplicate();
                new.position = cursor + (item.position - start);
                end = end.max(new.end());
                placed.push((track.id, new.id));
                track.items.push(new);
            }
        });
        self.multi = placed.iter().map(|(_, id)| *id).collect();
        if let Some(&(track, item)) = placed.first() {
            self.sel = Selection { track: Some(track), item: Some(item) };
        }
        self.set_cursor(end);
    }

    /// Duplicates the selection right after itself.
    pub fn duplicate_item(&mut self) {
        let items = self.selected_with_tracks();
        let (Some(start), Some(end)) = (
            items.iter().map(|(_, it)| it.position).reduce(f64::min),
            items.iter().map(|(_, it)| it.end()).reduce(f64::max),
        ) else {
            return;
        };
        let mut placed = Vec::new();
        self.edit("Duplicate items", |p| {
            for (track, item) in &items {
                let mut new = item.duplicate();
                new.position += end - start;
                placed.push(new.id);
                p.tracks[*track].items.push(new);
            }
        });
        self.sel.item = placed.first().copied();
        self.multi = placed.into_iter().collect();
    }

    /// Reverses the active take by wrapping its source in a reversed section, or unwrapping it.
    pub fn reverse_item(&mut self) {
        let Some(id) = self.sel.item else { return };
        let media_len = self.project.find_item(id).and_then(|(_, it)| it.take()).and_then(|t| match self.media(t) {
            Some(MediaState::Ready(m)) => Some(m.audio.frames() as f64 / m.audio.sample_rate as f64),
            _ => None,
        });
        let Some(media_len) = media_len else {
            self.notify("Reverse needs the item's media to be loaded");
            return;
        };
        self.edit("Reverse item", |p| {
            let Some(it) = p.tracks.iter_mut().find_map(|t| t.item_mut(id)) else { return };
            let (len, active) = (it.length, it.active_take);
            let take = &mut it.takes[active];
            // The reversed take must still play the same stretch of source, mirrored.
            let (start, sec_len) = match &take.source {
                scythe_core::Source::Section(s) => {
                    (s.start, if s.length > 0.0 { s.length } else { media_len - s.start })
                }
                _ => (0.0, media_len),
            };
            let used = len * take.playrate;
            take.source_offset = (sec_len - take.source_offset - used).max(0.0);
            take.source = match std::mem::replace(&mut take.source, scythe_core::Source::Empty) {
                scythe_core::Source::Section(mut s) => {
                    s.reverse = !s.reverse;
                    if !s.reverse && s.start == 0.0 && s.length <= 0.0 {
                        *s.inner
                    } else {
                        scythe_core::Source::Section(s)
                    }
                }
                other => scythe_core::Source::Section(scythe_core::Section {
                    inner: Box::new(other),
                    start,
                    length: sec_len,
                    reverse: true,
                    raw: scythe_core::rpp::Chunk::new("SOURCE", &["SECTION"]),
                }),
            };
        });
    }

    pub fn add_track(&mut self) {
        let n = self.project.tracks.len() + 1;
        let mut id = TrackId(0);
        let insert_at = self.selected_track_index().map(|i| i + 1);
        self.edit("Add track", |p| {
            id = p.add_track(format!("Track {n}"));
            if let Some(at) = insert_at {
                let t = p.tracks.pop().unwrap();
                p.tracks.insert(at, t);
            }
        });
        self.sel = Selection { track: Some(id), item: None };
    }

    pub fn split_selected_item(&mut self) {
        let Some(item_id) = self.sel.item else {
            self.notify("Select an item to split");
            return;
        };
        let at = self.position();
        let mut new_id = None;
        self.edit("Split item", |p| {
            for t in &mut p.tracks {
                let Some(idx) = t.items.iter().position(|i| i.id == item_id) else { continue };
                if let Some(right) = t.items[idx].split_at(at) {
                    new_id = Some(right.id);
                    t.items.insert(idx + 1, right);
                }
                return;
            }
        });
        if new_id.is_none() {
            self.notify("Put the cursor inside the selected item to split it");
        }
        self.sel.item = new_id.or(self.sel.item);
    }
}
