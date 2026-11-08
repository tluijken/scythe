use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use scythe_core::history::History;
use scythe_core::midi::{MidiSource, TimedNote};
use scythe_core::model::MIN_ITEM_LENGTH;
use scythe_core::recording::{RecordPass, place_midi_recording, place_recording};
use scythe_core::render_settings::ResampleMode;
use scythe_core::rpp::{autosave_path, read_project, resolve_media_path, save_project, write_project};
use scythe_core::smf::{read_smf, write_smf};
use scythe_core::{AutoMode, EnvelopeKind, Item, ItemId, Project, Source, Take, TempoMap, TimeRange, TrackId};
use scythe_engine::bounce::{BitDepth, Encoding, Finish, bounce_finished, bounce_to_wav};
use scythe_engine::midi_io::{LiveMidi, midi_input_channel};
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
    Loaded {
        path: PathBuf,
        result: Result<Arc<Media>, String>,
        place: Option<(TrackId, f64)>,
    },
    Bounced(Result<PathBuf, String>),
    /// Isolated items were rendered to `path` for gluing or applying track FX.
    ItemsRendered {
        path: PathBuf,
        result: Result<(), String>,
        then: RenderedItems,
    },
    /// A stretched, pitch-shifted or rate-changed copy is ready.
    Stretched(StretchKey),
}

/// What becomes of a render of isolated items.
enum RenderedItems {
    /// The items are replaced with one item playing the render.
    Glue { track: TrackId, items: Vec<ItemId>, range: TimeRange },
    /// The render becomes the item's new, active take.
    NewTake { item: ItemId },
}

/// A file and the `Processing::key` of a processed copy.
type StretchKey = (PathBuf, (u64, u64, u64, i32));

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
    /// Audio input, when an armed track records audio.
    pub session: Option<Recording>,
    pub pass: RecordPass,
    /// Every armed track.
    pub tracks: Vec<TrackId>,
    /// Tracks recording audio, in the order of the session's levels.
    audio_tracks: Vec<TrackId>,
    /// Tracks recording MIDI, with the channel they listen to (`None` for all).
    midi_tracks: Vec<(TrackId, Option<u8>)>,
    /// MIDI received so far, with the project frame it arrived at.
    midi: Vec<(i64, LiveMidi)>,
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
    /// Razor edit areas: time ranges on single tracks, made with Alt+drag.
    pub razor: Vec<(TrackId, TimeRange)>,
    /// Where the copied material starts, so a paste keeps space before the first item.
    clipboard_origin: f64,
    pub recording: Option<ActiveRecording>,
    /// Record mode "time selection auto-punch": keep only what falls inside the time selection.
    pub auto_punch: bool,
    /// The last MIDI input error shown, so it is not repeated on every edit.
    midi_error: Option<String>,
    /// Hardware MIDI outputs, listed once.
    midi_out_names: Option<Vec<String>>,
    pub last_script: Option<PathBuf>,
    /// What the last script printed (`ShowConsoleMsg`) or its error, until closed.
    pub script_console: Option<String>,
    /// A script that deferred work, run once per frame, and its file name.
    running_script: Option<(crate::script::RunningScript, String)>,
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
    pending_stretch: HashSet<StretchKey>,
    pub busy: Option<String>,
    /// Glue and apply-FX renders still running.
    items_rendering: usize,
    pub ctx: egui::Context,
    /// Displayed meter levels with peak decay, per track and for the master.
    pub levels: HashMap<TrackId, (f32, f32)>,
    pub master_level: (f32, f32),
    meters_at: Instant,
    /// Plugin editors open in their own windows.
    editors: Vec<OpenEditor>,
    /// JSFX views (`@gfx`) open in windows inside the main one.
    pub gfx_views: Vec<GfxView>,
    /// Controls being written as automation during this pass.
    auto_writes: Vec<AutoWrite>,
}

/// A control that automation writing follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoTarget {
    Volume,
    Pan,
}

#[derive(Debug, Clone)]
struct AutoWrite {
    track: TrackId,
    target: AutoTarget,
    /// The control's value (volume as gain, pan -1..=1).
    value: f64,
    touching: bool,
    /// Time of the last written point; points after it are replaced as the pass moves on.
    last: Option<f64>,
}

/// A plugin editor in its window. The instance is held so the plugin outlives its editor.
pub struct OpenEditor {
    pub track: Option<TrackId>,
    pub index: usize,
    instance: Arc<scythe_engine::fx::FxInstance>,
    editor: Box<dyn scythe_engine::editor::Editor>,
    window: crate::plugin_window::PluginWindow,
}

/// A JSFX's own view: what its `@gfx` drew last and the mouse it has not seen yet.
pub struct GfxView {
    pub track: Option<TrackId>,
    pub index: usize,
    pub title: String,
    pub instance: Arc<scythe_engine::fx::FxInstance>,
    /// The size `@gfx` asks for, 0 where it does not care.
    pub size: (u32, u32),
    /// The screen `@gfx` drew last.
    pub texture: Option<egui::TextureHandle>,
    /// A menu the script waits on.
    pub menu: Option<crate::ui::gfx_view::GfxMenu>,
    pub ran: Option<Instant>,
    /// Buttons pressed since the last run, so a quick click is not missed.
    pub pressed: u32,
    pub wheel: (f64, f64),
    pub chars: Vec<u32>,
    pub mouse: (f64, f64),
}

impl ScytheApp {
    pub fn new(ctx: egui::Context, project: Project) -> Self {
        let pool = Arc::new(MediaPool::with_resample(ResampleMode::PLAYBACK));
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
            razor: Vec::new(),
            clipboard_origin: 0.0,
            recording: None,
            auto_punch: false,
            midi_error: None,
            midi_out_names: None,
            last_script: None,
            script_console: None,
            running_script: None,
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
            pending_stretch: HashSet::new(),
            busy: None,
            items_rendering: 0,
            ctx,
            levels: HashMap::new(),
            master_level: (0.0, 0.0),
            editors: Vec::new(),
            gfx_views: Vec::new(),
            auto_writes: Vec::new(),
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
        if let Some(rec) = &mut self.recording {
            if let Some(session) = &rec.session {
                for (track, peak) in rec.audio_tracks.iter().zip(session.take_levels()) {
                    let old = self.levels.get(track).copied().unwrap_or_default();
                    self.levels.insert(*track, blend(old, (peak, peak)));
                }
            }
            if !rec.midi_tracks.is_empty() {
                rec.midi.extend(self.engine.take_recorded_midi());
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
        let is_midi = |t: &scythe_core::Track| midi_input_channel(t.rec_input).is_some();
        let monitor = !self.no_input && self.project.tracks.iter().any(|t| t.armed && t.monitor && !is_midi(t));
        if let Err(e) = self.engine.set_monitoring(monitor) {
            self.notify(format!("Input monitoring unavailable: {e}"));
        }
        let outputs = self.project.tracks.iter().any(|t| t.midi_out.is_some());
        if let Err(e) = self.engine.set_midi_outputs(outputs) {
            self.notify(format!("MIDI output unavailable: {e}"));
        }
        let midi = !self.no_input && self.project.tracks.iter().any(|t| t.armed && is_midi(t));
        match self.engine.set_midi_input(midi) {
            Err(e) if self.midi_error.as_deref() != Some(e.as_str()) => {
                self.notify(format!("MIDI input unavailable: {e}"));
                self.midi_error = Some(e);
            }
            Err(_) => {}
            Ok(()) => self.midi_error = None,
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
        let default_mode = self.project.default_pitch_mode;
        let stretches: Vec<(PathBuf, scythe_core::Processing)> = self
            .project
            .tracks
            .iter()
            .flat_map(|t| &t.items)
            .flat_map(|i| &i.takes)
            .filter_map(|t| Some((resolve_media_path(&self.project, t.source.file()?), t.processing(default_mode)?)))
            .collect();
        for (path, processing) in stretches {
            let key = (path.clone(), processing.key());
            let pool = self.engine.pool().clone();
            if pool.ready(&path).is_none()
                || pool.processed(&path, &processing).is_some()
                || !self.pending_stretch.insert(key.clone())
            {
                continue;
            }
            let tx = self.loader_tx.clone();
            let ctx = self.ctx.clone();
            std::thread::spawn(move || {
                pool.prepare_processed(&path, &processing);
                let _ = tx.send(LoaderMsg::Stretched(key));
                ctx.request_repaint();
            });
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

    /// Imports audio and MIDI files onto `track` (or a new track) starting at `position`.
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
            if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("mid") || e.eq_ignore_ascii_case("midi")) {
                self.import_midi(&path, track, position);
            } else {
                self.spawn_load(path, Some((track, position)));
            }
        }
    }

    /// Places a MIDI file as an in-project MIDI item (REAPER's default for imported MIDI), its
    /// length following the project tempo map.
    fn import_midi(&mut self, path: &Path, track: TrackId, position: f64) {
        let parsed =
            std::fs::read(path).map_err(|e| e.to_string()).and_then(|b| read_smf(&b).map_err(|e| e.to_string()));
        let (source, _) = match parsed {
            Ok(s) => s,
            Err(e) => {
                self.notify(format!("Could not load {}: {e}", path.display()));
                return;
            }
        };
        let timeline = self.project.tempo.timeline();
        let end = timeline.time_at_qn(timeline.qn_at(position) + source.length_qn());
        let mut take = Take::from_file(path.to_owned());
        take.source = Source::Raw(source.to_chunk());
        let item = Item::new(position, (end - position).max(MIN_ITEM_LENGTH), take);
        let id = item.id;
        self.edit("Import MIDI", |p| {
            if let Some(t) = p.track_mut(track) {
                t.items.push(item);
            }
        });
        self.sel = Selection { track: Some(track), item: Some(id) };
    }

    /// Saves the selected MIDI item's source as a standard MIDI file.
    pub fn export_midi(&mut self) {
        let found = self.sel.item.and_then(|id| self.project.find_item(id)).and_then(|(_, it)| {
            let take = it.take()?;
            let Source::Raw(c) = &take.source else { return None };
            Some((MidiSource::parse(c)?, take.name.clone(), it.position))
        });
        let Some((source, name, position)) = found else {
            self.notify("Select a MIDI item first");
            return;
        };
        let stem = Path::new(&name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let Some(path) = rfd::FileDialog::new()
            .add_filter("MIDI", &["mid"])
            .set_file_name(format!("{}.mid", if stem.is_empty() { "item" } else { &stem }))
            .save_file()
        else {
            return;
        };
        let bytes = write_smf(&source, Some(self.project.tempo.bpm_at(position)), Some(&stem));
        match std::fs::write(&path, bytes) {
            Ok(()) => self.notify(format!("Exported {}", path.display())),
            Err(e) => self.notify(format!("Could not export: {e}")),
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
                            self.request_media();
                        }
                        Err(e) => self.notify(format!("Could not load {}: {e}", path.display())),
                    }
                }
                LoaderMsg::Stretched(key) => {
                    self.pending_stretch.remove(&key);
                    self.engine.sync(&self.project);
                }
                LoaderMsg::ItemsRendered { path, result, then } => {
                    self.items_rendering = self.items_rendering.saturating_sub(1);
                    if self.items_rendering == 0 {
                        self.busy = None;
                    }
                    match result {
                        Ok(()) => self.place_rendered_items(path, then),
                        Err(e) => self.notify(format!("Render failed: {e}")),
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

    /// Starts recording every armed track from the edit cursor (or the playhead while playing),
    /// or stops and adds the takes.
    pub fn toggle_record(&mut self) {
        if let Some(rec) = self.recording.take() {
            self.finish_recording(rec);
            return;
        }

        let armed: Vec<_> = self.project.tracks.iter().filter(|t| t.armed).cloned().collect();
        if armed.is_empty() {
            self.notify("Arm a track to record");
            return;
        }
        let (midi_armed, audio_armed): (Vec<_>, Vec<_>) =
            armed.iter().partition(|t| midi_input_channel(t.rec_input).is_some());
        let dir = match self.project.path.as_deref().and_then(Path::parent) {
            Some(d) => d.to_owned(),
            None => crate::demo::data_dir().join("recordings"),
        };
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let targets: Vec<RecordTarget> = audio_armed
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
        let session = if targets.is_empty() {
            None
        } else {
            match Recording::start(targets, self.engine.sample_rate()) {
                Ok(s) => Some(s),
                Err(e) => {
                    self.notify(format!("Cannot record: {e}"));
                    return;
                }
            }
        };
        if !midi_armed.is_empty() {
            if let Err(e) = self.engine.set_midi_input(true) {
                self.notify(format!("Cannot record MIDI: {e}"));
                if session.is_none() {
                    return;
                }
            }
            // Whatever was played before recording started is not part of the take.
            self.engine.take_recorded_midi();
        }
        // Recording during playback punches in at the playhead.
        let playing = self.is_playing();
        let start = if playing { self.engine.transport().position_seconds() } else { self.project.cursor };
        let selection = self.project.time_selection.filter(|r| !r.is_empty());
        let pass = RecordPass {
            start,
            looped: selection.filter(|_| self.project.loop_enabled),
            punch: selection.filter(|_| self.auto_punch),
        };
        if !playing {
            self.engine.seek(start);
            self.engine.play();
        }
        let from = session.as_ref().map(|s| s.device.clone());
        let midi = self.engine.midi_input_names().join(", ");
        self.notify(match (from, midi.is_empty()) {
            (Some(d), true) => format!("Recording from {d}"),
            (Some(d), false) => format!("Recording from {d} and {midi}"),
            (None, _) => format!("Recording MIDI from {midi}"),
        });
        self.recording = Some(ActiveRecording {
            session,
            pass,
            tracks: armed.iter().map(|t| t.id).collect(),
            audio_tracks: audio_armed.iter().map(|t| t.id).collect(),
            midi_tracks: midi_armed.iter().map(|t| (t.id, midi_input_channel(t.rec_input).flatten())).collect(),
            midi: Vec::new(),
        });
    }

    fn finish_recording(&mut self, mut rec: ActiveRecording) {
        let stop = self.engine.transport().position_seconds();
        self.engine.stop();
        rec.midi.extend(self.engine.take_recorded_midi());
        let sr = self.engine.sample_rate() as f64;
        let timeline = self.project.tempo.timeline();
        let mut items: Vec<(TrackId, Item)> = Vec::new();
        for &(track, channel) in &rec.midi_tracks {
            let events: Vec<(f64, [u8; 3], u8)> = rec
                .midi
                .iter()
                .filter(|(_, m)| channel.is_none_or(|c| m.channel() == c))
                .map(|(frame, m)| (*frame as f64 / sr, m.bytes, m.len))
                .collect();
            if let Some(item) = place_midi_recording(&events, rec.pass, stop, &timeline) {
                items.push((track, item));
            }
        }
        let midi_count = items.len();
        let mut note = String::new();
        let mut failed = false;
        if let Some(session) = rec.session {
            let overruns = session.overruns();
            match session.finish() {
                Ok(takes) => {
                    // What was played along to left the speakers `output_latency` late and
                    // reached the file `input_latency` after that.
                    let output_latency = self.engine.transport().output_latency();
                    for take in &takes {
                        let offset = take.input_latency + output_latency;
                        let len = take.frames as f64 / take.sample_rate as f64;
                        if let Some(item) = place_recording(take.path.clone(), len, offset, rec.pass) {
                            items.push((take.track, item));
                        }
                    }
                    if overruns > 0 {
                        note = format!(" ({overruns} samples dropped)");
                    }
                }
                Err(e) => {
                    self.notify(format!("Recording failed: {e}"));
                    failed = true;
                }
            }
        }
        let first = items.first().map(|(_, it)| it.id);
        let count = items.len();
        if count > 0 {
            self.edit("Record", |p| {
                for (track, item) in items {
                    if let Some(t) = p.track_mut(track) {
                        t.items.push(item);
                    }
                }
            });
            self.sel.item = first;
            let midi = if midi_count > 0 { format!(", {midi_count} MIDI") } else { String::new() };
            self.notify(format!("Recorded {count} take(s){midi}{note}"));
            self.request_media();
        } else if !failed {
            self.notify("Nothing was recorded");
        }
        self.engine.seek(self.project.cursor);
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
            .add_filter("Media", &["wav", "flac", "mp3", "ogg", "aif", "aiff", "m4a", "caf", "mid", "midi"])
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
        let mut encoding = Encoding::Wav(BitDepth::Int24);
        let mut channels = 2;
        let mut finish = Finish::default();
        // Every render converts with the project's render mode; playback has its own.
        let resample = scythe_core::render_settings::RenderSettings::from_project(&self.project).resample;
        let jobs: Vec<(Project, PathBuf)> = if kind == RenderKind::ProjectSettings {
            let settings = scythe_core::render_settings::RenderSettings::from_project(&self.project);
            encoding = Encoding::from_format(&settings.format);
            channels = settings.channels;
            finish = Finish::from_settings(&settings);
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
        let mut pool = self.engine.pool().clone();
        let mut rate = self.engine.sample_rate();
        let tx = self.loader_tx.clone();
        let ctx = self.ctx.clone();
        self.busy = Some("Rendering…".into());
        std::thread::spawn(move || {
            // The engine's media is at its own rate and converted for playback; CD images and DDP
            // are written at 44.1 kHz, and another resample mode needs media of its own.
            let fixed = encoding.sample_rate().filter(|r| *r != rate);
            if (fixed.is_some() || resample != pool.resample_mode())
                && let Some((project, _)) = jobs.first()
            {
                rate = fixed.unwrap_or(rate);
                pool = Arc::new(crate::load_media(project, rate, resample));
            }
            let mut last = Ok(PathBuf::new());
            for (i, (project, path)) in jobs.into_iter().enumerate() {
                let range = ranges.get(i).copied().unwrap_or(range);
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                last = bounce_finished(&project, &pool, range, rate, channels, encoding, &path, finish, |_| {})
                    .map(|()| path)
                    .map_err(|e| e.to_string());
                if last.is_err() {
                    break;
                }
            }
            let _ = tx.send(LoaderMsg::Bounced(last));
            ctx.request_repaint();
        });
    }

    /// Glues the selected items of each track into one item playing a render of them.
    pub fn glue_selected(&mut self) {
        self.render_selected_items(false);
    }

    /// Renders each selected item through its track's effects into a new, active take.
    pub fn apply_track_fx(&mut self) {
        self.render_selected_items(true);
    }

    fn render_selected_items(&mut self, with_fx: bool) {
        let selected = self.selected_items();
        if selected.is_empty() {
            self.notify("Select items first");
            return;
        }
        let dir = match self.project.path.as_deref().and_then(Path::parent) {
            Some(d) => d.to_owned(),
            None => crate::demo::data_dir().join("recordings"),
        };
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let mut jobs = Vec::new();
        for (ti, t) in self.project.tracks.iter().enumerate() {
            let ids: Vec<ItemId> = t.items.iter().filter(|i| selected.contains(&i.id)).map(|i| i.id).collect();
            if ids.is_empty() {
                continue;
            }
            let groups: Vec<Vec<ItemId>> = if with_fx { ids.iter().map(|i| vec![*i]).collect() } else { vec![ids] };
            let base = if t.name.is_empty() { format!("Track {}", ti + 1) } else { t.name.clone() };
            let safe: String =
                base.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' }).collect();
            for group in groups {
                let Some((project, range)) = scythe_core::glue::isolate(&self.project, t.id, &group, with_fx) else {
                    continue;
                };
                let kind = if with_fx { "fx" } else { "glued" };
                let path = dir.join(format!("{}-{kind}-{stamp}-{}.wav", safe.trim(), jobs.len() + 1));
                let then = if with_fx {
                    RenderedItems::NewTake { item: group[0] }
                } else {
                    RenderedItems::Glue { track: t.id, items: group, range }
                };
                jobs.push((project, range, path, then));
            }
        }
        if jobs.is_empty() {
            return;
        }
        let pool = self.engine.pool().clone();
        let rate = self.engine.sample_rate();
        let tx = self.loader_tx.clone();
        let ctx = self.ctx.clone();
        self.items_rendering += jobs.len();
        self.busy = Some(if with_fx { "Applying track FX…" } else { "Gluing…" }.into());
        std::thread::spawn(move || {
            for (project, range, path, then) in jobs {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                // Float samples, so material above full scale survives the glue.
                let result = bounce_to_wav(&project, &pool, range, rate, BitDepth::Float32, &path, |_| {})
                    .map_err(|e| e.to_string());
                let _ = tx.send(LoaderMsg::ItemsRendered { path, result, then });
                ctx.request_repaint();
            }
        });
    }

    fn place_rendered_items(&mut self, path: PathBuf, then: RenderedItems) {
        match then {
            RenderedItems::Glue { track, items, range } => {
                let mut glued = Item::new(range.start, range.length(), Take::from_file(path));
                (glued.fade_in.length, glued.fade_out.length) = (0.0, 0.0);
                let id = glued.id;
                self.edit("Glue items", |p| {
                    if let Some(t) = p.track_mut(track) {
                        t.items.retain(|it| !items.contains(&it.id));
                        t.items.push(glued);
                    }
                });
                self.multi.clear();
                self.sel = Selection { track: Some(track), item: Some(id) };
            }
            RenderedItems::NewTake { item } => {
                self.edit("Apply track FX to item as new take", |p| {
                    if let Some(it) = p.tracks.iter_mut().find_map(|t| t.item_mut(item)) {
                        it.takes.push(Take::from_file(path));
                        it.active_take = it.takes.len() - 1;
                    }
                });
            }
        }
        self.request_media();
    }

    // ---- automation writing ------------------------------------------------------------------

    /// Routes a control move on `track` into its envelope when the track writes automation and
    /// the transport runs. Returns false when the move should change the control itself.
    pub fn automate(&mut self, track: TrackId, target: AutoTarget, value: f64) -> bool {
        let writes = self.project.track(track).is_some_and(|t| t.auto_mode.writes());
        if !writes || !self.is_playing() {
            return false;
        }
        match self.auto_writes.iter_mut().find(|w| w.track == track && w.target == target) {
            Some(w) => {
                w.value = value;
                w.touching = true;
            }
            None => self.auto_writes.push(AutoWrite { track, target, value, touching: true, last: None }),
        }
        true
    }

    /// The control was let go: touch mode stops writing it.
    pub fn release_automation(&mut self, track: TrackId, target: AutoTarget) {
        if let Some(w) = self.auto_writes.iter_mut().find(|w| w.track == track && w.target == target) {
            w.touching = false;
        }
    }

    /// The value being written for a control, for the mixer to show instead of the stored one.
    pub fn automation_value(&self, track: TrackId, target: AutoTarget) -> Option<f64> {
        self.auto_writes.iter().find(|w| w.track == track && w.target == target).map(|w| w.value)
    }

    /// Writes the controls of this pass at the play position: touched ones in touch mode,
    /// everything moved since the start in latch mode, and every control in write mode. The
    /// pass becomes one undo step when playback stops.
    pub fn tick_automation(&mut self) {
        if !self.is_playing() {
            if !self.auto_writes.is_empty() {
                self.auto_writes.clear();
                self.end_gesture();
            }
            return;
        }
        let now = self.engine.transport().position_seconds();
        // Write mode records both controls even when nobody touches them.
        for t in self.project.tracks.iter().filter(|t| t.auto_mode == AutoMode::Write) {
            for target in [AutoTarget::Volume, AutoTarget::Pan] {
                if !self.auto_writes.iter().any(|w| w.track == t.id && w.target == target) {
                    let value = match target {
                        AutoTarget::Volume => {
                            t.envelope(EnvelopeKind::Volume).and_then(|e| e.value_at(now)).unwrap_or(1.0)
                        }
                        AutoTarget::Pan => {
                            t.pan - t.envelope(EnvelopeKind::Pan).and_then(|e| e.value_at(now)).unwrap_or(0.0)
                        }
                    };
                    self.auto_writes.push(AutoWrite { track: t.id, target, value, touching: false, last: None });
                }
            }
        }
        let mut writes = std::mem::take(&mut self.auto_writes);
        let modes: HashMap<TrackId, AutoMode> = self.project.tracks.iter().map(|t| (t.id, t.auto_mode)).collect();
        let active: Vec<usize> = writes
            .iter()
            .enumerate()
            .filter(|(_, w)| match modes.get(&w.track) {
                Some(AutoMode::Touch) => w.touching,
                Some(AutoMode::Latch | AutoMode::Write) => true,
                _ => false,
            })
            .map(|(i, _)| i)
            .collect();
        if !active.is_empty() {
            self.gesture("Write automation", |p| {
                for &i in &active {
                    let w = &writes[i];
                    let Some(t) = p.track_mut(w.track) else { continue };
                    let (kind, value) = match w.target {
                        AutoTarget::Volume => (EnvelopeKind::Volume, w.value),
                        // The pan envelope is subtracted from the track pan.
                        AutoTarget::Pan => (EnvelopeKind::Pan, t.pan - w.value),
                    };
                    let env = t.envelope_mut(kind);
                    let from = w.last.unwrap_or(now - 1e-6);
                    env.points.retain(|pt| pt.time <= from || pt.time > now);
                    env.points.push(scythe_core::EnvPoint::new(now, value));
                    env.sort();
                }
            });
            for &i in &active {
                writes[i].last = Some(now);
            }
        }
        self.auto_writes = writes;
    }

    // ---- plugin editors --------------------------------------------------------------------

    /// Opens the editor of effect `index` on `track` (the master when `None`) in its own window,
    /// or closes it when it is already open.
    pub fn toggle_editor(&mut self, track: Option<TrackId>, index: usize) {
        if let Some(at) = self.editors.iter().position(|e| e.track == track && e.index == index) {
            self.close_editor(at);
            return;
        }
        let slots = match track {
            Some(id) => self.project.track(id).map(|t| &t.fx),
            None => Some(&self.project.master.fx),
        };
        let Some(slot) = slots.and_then(|s| s.get(index)).cloned() else { return };
        let Some(instance) = self.engine.fx().instance(track, &slot, index) else {
            self.notify(format!("{} is not running", slot.name));
            return;
        };
        if let Some(at) = self.gfx_views.iter().position(|v| v.track == track && v.index == index) {
            self.gfx_views.remove(at);
            return;
        }
        if let Some(size) = instance.with(|e| e.gfx_size()).flatten() {
            let title = slot.name.split_once(": ").map_or(slot.name.as_str(), |(_, n)| n).to_owned();
            self.gfx_views.push(GfxView {
                track,
                index,
                title,
                instance,
                size,
                texture: None,
                menu: None,
                ran: None,
                pressed: 0,
                wheel: (0.0, 0.0),
                chars: Vec::new(),
                mouse: (-1.0, -1.0),
            });
            return;
        }
        let Some(mut editor) = instance.with(|e| e.editor()).flatten() else {
            self.notify(format!("{} has no editor", slot.name));
            return;
        };
        let window = match crate::plugin_window::PluginWindow::new(&slot.name, 640, 480) {
            Ok(w) => w,
            Err(e) => {
                self.notify(format!("Cannot open a plugin window: {e}"));
                return;
            }
        };
        match editor.open(window.window) {
            Ok((w, h)) => {
                window.resize(w, h);
                self.editors.push(OpenEditor { track, index, instance, editor, window });
            }
            Err(e) => self.notify(format!("{}: {e}", slot.name)),
        }
    }

    pub fn editor_open(&self, track: Option<TrackId>, index: usize) -> bool {
        self.editors.iter().any(|e| e.track == track && e.index == index)
            || self.gfx_views.iter().any(|v| v.track == track && v.index == index)
    }

    /// Whether effect `index` on `track` draws a view of its own (a JSFX with `@gfx`).
    pub fn has_gfx(&self, track: Option<TrackId>, index: usize) -> bool {
        let slots = match track {
            Some(id) => self.project.track(id).map(|t| &t.fx),
            None => Some(&self.project.master.fx),
        };
        slots
            .and_then(|s| s.get(index))
            .and_then(|slot| self.engine.fx().instance(track, slot, index))
            .and_then(|inst| inst.with(|e| e.gfx_size().is_some()))
            .unwrap_or(false)
    }

    fn close_editor(&mut self, at: usize) {
        let mut open = self.editors.remove(at);
        open.editor.close();
    }

    /// Drives open editors: their timers and descriptors, size requests and closing, also
    /// when their effect left the project.
    pub fn tick_editors(&mut self) {
        let mut at = 0;
        while at < self.editors.len() {
            let e = &mut self.editors[at];
            if let Some((w, h)) = e.editor.idle() {
                e.window.resize(w, h);
            }
            let orphaned = std::sync::Arc::strong_count(&e.instance) <= 1;
            if e.window.close_requested() || e.editor.closed() || orphaned {
                self.close_editor(at);
            } else {
                at += 1;
            }
        }
        self.gfx_views.retain(|v| Arc::strong_count(&v.instance) > 1);
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
        if !self.razor.is_empty() {
            let areas = std::mem::take(&mut self.razor);
            self.edit("Remove razor edit contents", |p| p.remove_areas(&areas));
        } else if !items.is_empty() {
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
        if !self.razor.is_empty() {
            let pieces = self.project.pieces_in(&self.razor);
            let index = |t: TrackId| self.project.track_index(t).unwrap_or(0);
            let first_track = self.razor.iter().map(|(t, _)| index(*t)).min().unwrap_or(0);
            self.clipboard_origin = self.razor.iter().map(|(_, r)| r.start).fold(f64::INFINITY, f64::min);
            self.clipboard = pieces.into_iter().map(|(t, it)| (index(t) - first_track, it)).collect();
            if cut {
                self.delete_selection();
            }
            return;
        }
        let items = self.selected_with_tracks();
        let Some(first_track) = items.iter().map(|(t, _)| *t).min() else { return };
        self.clipboard_origin = items.iter().map(|(_, it)| it.position).fold(f64::INFINITY, f64::min);
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
        let start = self.clipboard_origin;
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

    /// REAPER's preset folder, shared with REAPER when its resource folder exists.
    fn preset_root() -> PathBuf {
        reaper_resource_dir().unwrap_or_else(crate::demo::data_dir).join("presets")
    }

    /// Where presets saved by earlier versions of Scythe live: one single-effect `.RfxChain`
    /// per preset.
    fn legacy_preset_dir(slot: &scythe_core::FxSlot) -> PathBuf {
        let safe: String =
            slot.name.chars().map(|c| if c.is_alphanumeric() || " -_().".contains(c) { c } else { '_' }).collect();
        crate::demo::data_dir().join("presets").join(safe.trim())
    }

    /// The REAPER preset file of the plugin in `slot` and what it holds. Like REAPER, this first
    /// takes in the plugin's default library (the `.rpl` beside it) when that changed since the
    /// last import.
    fn preset_file(&self, slot: &scythe_core::FxSlot) -> Option<(PathBuf, crate::fx_preset::PresetFile)> {
        use crate::fx_preset::{Library, PresetFile};
        let path = Self::preset_root().join(crate::fx_preset::file_name(slot)?);
        let mut file = std::fs::read_to_string(&path).map(|t| PresetFile::parse(&t)).unwrap_or_default();
        let library = self.engine.fx().plugin_file(slot).and_then(|plugin| {
            crate::fx_preset::default_libraries(slot, &plugin).into_iter().find_map(|lib| {
                let modified = std::fs::metadata(&lib).ok()?.modified().ok()?;
                let secs = modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
                Some((Library::parse(&std::fs::read_to_string(&lib).ok()?)?, secs))
            })
        });
        if let Some((library, modified)) = library
            && file.import_default(&library, modified)
        {
            let _ = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&path, file.to_text()));
        }
        Some((path, file))
    }

    /// Presets of a track's effect, in REAPER's order: those in its REAPER preset file, then a
    /// VST 3 plugin's `.vstpreset` files or a VST 2 plugin's own programs; then older
    /// `.RfxChain` presets by name.
    pub fn fx_presets(&self, track: TrackId, index: usize) -> Vec<(String, PresetSource)> {
        let Some(slot) = self.project.track(track).and_then(|t| t.fx.get(index)) else { return Vec::new() };
        let mut out = Vec::new();
        if let Some((path, file)) = self.preset_file(slot) {
            out.extend(file.presets.into_iter().map(|p| (p.name.clone(), PresetSource::Reaper(path.clone(), p.name))));
        }
        if let Some((_, vendor, name)) = crate::fx_preset::vst3_identity(slot)
            && let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
        {
            let root = home.join(".vst3/presets").join(&vendor).join(&name);
            out.extend(crate::fx_preset::vst3_preset_files(&home, &vendor, &name).into_iter().map(|p| {
                let shown = p.strip_prefix(&root).unwrap_or(&p).with_extension("");
                (shown.to_string_lossy().into_owned(), PresetSource::Vst3File(p))
            }));
        }
        if let Some(inst) = self.engine.fx().instance(Some(track), slot, index) {
            let programs = inst.with(|e| e.programs()).unwrap_or_default();
            out.extend(programs.into_iter().enumerate().map(|(i, name)| (name, PresetSource::Program(i))));
        }
        if let Ok(entries) = std::fs::read_dir(Self::legacy_preset_dir(slot)) {
            let mut chains: Vec<(String, PresetSource)> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "RfxChain"))
                .filter_map(|p| Some((p.file_stem()?.to_string_lossy().into_owned(), PresetSource::FxChain(p))))
                .collect();
            chains.sort_by(|a, b| a.0.cmp(&b.0));
            out.extend(chains);
        }
        out
    }

    /// A free preset name for a track's effect.
    pub fn new_preset_name(&self, track: TrackId, index: usize) -> String {
        let taken: HashSet<String> = self.fx_presets(track, index).into_iter().map(|(n, _)| n).collect();
        (1..).map(|n| format!("Preset {n}")).find(|n| !taken.contains(n)).unwrap_or_default()
    }

    fn write_presets(&mut self, path: &Path, file: &crate::fx_preset::PresetFile, done: String) {
        let written =
            path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(path, file.to_text()));
        match written {
            Ok(()) => self.notify(done),
            Err(e) => self.notify(format!("Could not save: {e}")),
        }
    }

    /// Saves the current settings of a track's effect as a preset in REAPER's preset file for
    /// the plugin, replacing a preset of the same name.
    pub fn save_fx_preset(&mut self, track: TrackId, index: usize, name: &str) {
        let name = name.trim();
        if name.is_empty() || name.contains(['\n', '\r']) {
            return self.notify("Give the preset a name");
        }
        let mut project = self.project.clone();
        self.engine.store_plugin_states(&mut project);
        let Some(slot) = project.track(track).and_then(|t| t.fx.get(index)) else { return };
        let (Some((path, mut presets)), Some(data)) = (self.preset_file(slot), crate::fx_preset::capture(slot)) else {
            return self.notify("This effect has no presets");
        };
        presets.put(crate::fx_preset::Preset { name: name.to_owned(), data });
        self.write_presets(&path, &presets, format!("Saved preset {name}"));
    }

    /// Renames a preset in its REAPER preset file, or an older `.RfxChain` preset's file.
    /// Factory presets keep their names.
    pub fn rename_fx_preset(&mut self, source: &PresetSource, to: &str) {
        let to = to.trim();
        if to.is_empty() || to.contains(['\n', '\r']) {
            return self.notify("Give the preset a name");
        }
        match source {
            PresetSource::Reaper(path, name) => {
                let mut file =
                    std::fs::read_to_string(path).map(|t| crate::fx_preset::PresetFile::parse(&t)).unwrap_or_default();
                if !file.rename(name, to) {
                    return self.notify(format!("A preset named {to} already exists"));
                }
                self.write_presets(path, &file, format!("Renamed preset {name} to {to}"));
            }
            PresetSource::FxChain(path) => {
                let target = path.with_file_name(format!("{to}.RfxChain"));
                if target.exists() {
                    return self.notify(format!("A preset named {to} already exists"));
                }
                match std::fs::rename(path, &target) {
                    Ok(()) => self.notify(format!("Renamed preset to {to}")),
                    Err(e) => self.notify(format!("Could not rename: {e}")),
                }
            }
            PresetSource::Vst3File(_) | PresetSource::Program(_) => self.notify("Factory presets cannot be renamed"),
        }
    }

    /// Deletes a preset from its REAPER preset file, or an older `.RfxChain` preset.
    pub fn delete_fx_preset(&mut self, source: &PresetSource) {
        match source {
            PresetSource::Reaper(path, name) => {
                let mut file =
                    std::fs::read_to_string(path).map(|t| crate::fx_preset::PresetFile::parse(&t)).unwrap_or_default();
                if file.remove(name) {
                    self.write_presets(path, &file, format!("Deleted preset {name}"));
                }
            }
            PresetSource::FxChain(path) => match std::fs::remove_file(path) {
                Ok(()) => self.notify("Deleted preset"),
                Err(e) => self.notify(format!("Could not delete: {e}")),
            },
            PresetSource::Vst3File(_) | PresetSource::Program(_) => self.notify("Factory presets cannot be deleted"),
        }
    }

    /// Writes the presets in a track's effect's REAPER preset file to a REAPER preset library.
    pub fn export_fx_presets(&mut self, track: TrackId, index: usize) {
        let Some(slot) = self.project.track(track).and_then(|t| t.fx.get(index)) else { return };
        let (Some((_, file)), Some(plugin)) = (self.preset_file(slot), crate::fx_preset::library_name(slot)) else {
            return self.notify("This effect has no presets");
        };
        if file.presets.is_empty() {
            return self.notify("This effect has no presets to export");
        }
        let library = crate::fx_preset::Library { plugin, presets: file.presets };
        let Some(path) = resource_dialog("presets")
            .add_filter("REAPER preset library", &["RPL", "rpl"])
            .set_file_name("presets.RPL")
            .save_file()
        else {
            return;
        };
        match std::fs::write(&path, library.to_text()) {
            Ok(()) => self.notify(format!("Exported {} presets", library.presets.len())),
            Err(e) => self.notify(format!("Could not export: {e}")),
        }
    }

    /// Adds the presets of a REAPER preset library to a track's effect's preset file,
    /// replacing presets of the same name.
    pub fn import_fx_presets(&mut self, track: TrackId, index: usize) {
        let Some(path) = resource_dialog("presets").add_filter("REAPER preset library", &["RPL", "rpl"]).pick_file()
        else {
            return;
        };
        let Some(slot) = self.project.track(track).and_then(|t| t.fx.get(index)) else { return };
        let Some((ini, mut file)) = self.preset_file(slot) else { return self.notify("This effect has no presets") };
        let library = std::fs::read_to_string(&path).ok().and_then(|t| crate::fx_preset::Library::parse(&t));
        let Some(library) = library else { return self.notify(format!("{} is not a preset library", path.display())) };
        let count = library.presets.len();
        for preset in library.presets {
            file.put(preset);
        }
        self.write_presets(&ini, &file, format!("Imported {count} presets"));
    }

    /// Loads a preset into a track's effect, keeping the slot's identity, bypass and envelopes.
    pub fn load_fx_preset(&mut self, track: TrackId, index: usize, source: &PresetSource) {
        let Some(mut slot) = self.project.track(track).and_then(|t| t.fx.get(index)).cloned() else { return };
        match source {
            PresetSource::Reaper(path, name) => {
                let preset = std::fs::read_to_string(path)
                    .ok()
                    .map(|t| crate::fx_preset::PresetFile::parse(&t))
                    .and_then(|f| f.presets.into_iter().find(|p| &p.name == name));
                let Some(preset) = preset else { return self.notify(format!("Preset {name} is gone")) };
                if !crate::fx_preset::apply(&mut slot, &preset.data, name) {
                    return self.notify(format!("Preset {name} does not fit this effect"));
                }
            }
            PresetSource::Vst3File(path) => {
                let preset = std::fs::read(path).ok().and_then(|b| crate::fx_preset::VstPreset::parse(&b));
                let Some(preset) = preset else { return self.notify(format!("Could not read {}", path.display())) };
                let fits = crate::fx_preset::vst3_identity(&slot).is_some_and(|(cid, ..)| cid == preset.class_id);
                let data = preset.data_for(&slot).filter(|_| fits);
                // REAPER names a `.vstpreset` by its full path.
                let name = path.to_string_lossy();
                if !data.is_some_and(|d| crate::fx_preset::apply(&mut slot, &d, &name)) {
                    return self.notify(format!("{} does not fit this effect", path.display()));
                }
            }
            PresetSource::Program(program) => {
                let Some(inst) = self.engine.fx().instance(Some(track), &slot, index) else { return };
                let name = inst.with(|e| {
                    e.set_program(*program);
                    e.programs().get(*program).cloned()
                });
                let mut project = self.project.clone();
                self.engine.store_plugin_states(&mut project);
                let Some(stored) = project.track(track).and_then(|t| t.fx.get(index)).cloned() else { return };
                slot = stored;
                crate::fx_preset::name_program(&mut slot, &name.flatten().unwrap_or_default());
            }
            PresetSource::FxChain(path) => {
                let preset = std::fs::read_to_string(path)
                    .map_err(|e| e.to_string())
                    .and_then(|t| scythe_core::rpp::fx_chain_from_text(&t).map_err(|e| e.to_string()));
                let plugin = match preset {
                    Ok(slots) => slots
                        .into_iter()
                        .next()
                        .and_then(|s| s.raw.into_iter().find(|n| matches!(n, scythe_core::rpp::Node::Chunk(_)))),
                    Err(e) => return self.notify(format!("Could not load {}: {e}", path.display())),
                };
                let Some(plugin) = plugin else { return self.notify("The preset holds no plugin") };
                if let Some(chunk) = slot.raw.iter_mut().find(|n| matches!(n, scythe_core::rpp::Node::Chunk(_))) {
                    *chunk = plugin;
                }
            }
        }
        self.edit("Load FX preset", |p| {
            if let Some(s) = p.track_mut(track).and_then(|t| t.fx.get_mut(index)) {
                *s = slot;
            }
        });
    }

    pub fn run_script_dialog(&mut self) {
        let Some(path) = resource_dialog("Scripts").add_filter("ReaScript", &["lua", "eel", "py"]).pick_file() else {
            return;
        };
        self.run_script(&path);
    }

    /// Runs a ReaScript (Lua, EEL2 or Python) on the project; its changes become one undo step.
    /// A script that defers work keeps running until it stops deferring or is stopped.
    pub fn run_script(&mut self, path: &Path) {
        self.stop_script();
        self.last_script = Some(path.to_owned());
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => return self.notify(format!("Could not read {}: {e}", path.display())),
        };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match crate::script::run(&source, &name, &self.project, self.script_context()) {
            Ok((out, running)) => {
                self.apply_script(&name, out);
                match running {
                    Some(script) => {
                        self.notify(format!("{name} is running"));
                        self.running_script = Some((script, name));
                    }
                    None => self.notify(format!("Ran {name}")),
                }
            }
            Err(e) => {
                self.script_console = Some(e);
                self.notify(format!("{name} failed"));
            }
        }
    }

    /// Runs what the running script deferred to this frame.
    pub fn tick_script(&mut self) {
        let Some((mut script, name)) = self.running_script.take() else { return };
        let ctx = self.script_context();
        match script.tick(&self.project, ctx) {
            Ok(out) => {
                self.apply_script(&name, out);
                if script.is_running() {
                    self.running_script = Some((script, name));
                } else {
                    self.finish_script(script, name);
                }
            }
            Err(e) => {
                self.script_console = Some(e);
                self.notify(format!("{name} failed"));
            }
        }
    }

    /// Ends the running script, if any.
    pub fn stop_script(&mut self) {
        if let Some((script, name)) = self.running_script.take() {
            self.finish_script(script, name);
        }
    }

    pub fn script_running(&self) -> bool {
        self.running_script.is_some()
    }

    fn finish_script(&mut self, script: crate::script::RunningScript, name: String) {
        match script.stop(&self.project, self.script_context()) {
            Ok(out) => {
                self.apply_script(&name, out);
                self.notify(format!("{name} ended"));
            }
            Err(e) => {
                self.script_console = Some(e);
                self.notify(format!("{name} failed"));
            }
        }
    }

    fn script_context(&self) -> crate::script::ScriptContext {
        crate::script::ScriptContext {
            selected_track: self.sel.track,
            selected_items: self.selected_items(),
            cursor: self.project.cursor,
            playing: self.is_playing(),
            play_position: self.position(),
        }
    }

    fn apply_script(&mut self, name: &str, out: crate::script::ScriptOutcome) {
        if let Some(project) = out.project {
            let label = out.undo.unwrap_or_else(|| format!("Run {name}"));
            self.edit(&label, |p| *p = project);
        }
        if let Some(t) = out.cursor {
            self.set_cursor(t);
        }
        match out.play {
            Some(true) if !self.is_playing() => self.toggle_play(),
            Some(false) => self.stop(),
            _ => {}
        }
        if !out.console.is_empty() {
            match &mut self.script_console {
                Some(c) => c.push_str(&out.console),
                None => self.script_console = Some(out.console),
            }
        }
    }

    /// Saves the selected track's effects as a REAPER `.RfxChain` file.
    pub fn save_fx_chain(&mut self) {
        let Some(track) = self.sel.track.and_then(|id| self.project.track(id)) else {
            self.notify("Select a track first");
            return;
        };
        if track.fx.is_empty() {
            self.notify("The track has no effects");
            return;
        }
        let text = scythe_core::rpp::fx_chain_to_text(track);
        let name = if track.name.is_empty() { "chain".to_owned() } else { track.name.clone() };
        let dialog =
            resource_dialog("FXChains").add_filter("FX chain", &["RfxChain"]).set_file_name(format!("{name}.RfxChain"));
        let Some(path) = dialog.save_file() else { return };
        match std::fs::write(&path, text) {
            Ok(()) => self.notify(format!("Saved {}", path.display())),
            Err(e) => self.notify(format!("Could not save: {e}")),
        }
    }

    /// Appends the effects of a `.RfxChain` file to the selected track.
    pub fn load_fx_chain(&mut self) {
        let Some(track) = self.sel.track else {
            self.notify("Select a track first");
            return;
        };
        let Some(path) = resource_dialog("FXChains").add_filter("FX chain", &["RfxChain"]).pick_file() else { return };
        let slots = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| scythe_core::rpp::fx_chain_from_text(&t).map_err(|e| e.to_string()));
        match slots {
            Ok(slots) if !slots.is_empty() => {
                let n = slots.len();
                self.edit("Load FX chain", |p| {
                    if let Some(t) = p.track_mut(track) {
                        t.fx.extend(slots);
                    }
                });
                self.notify(format!("Added {n} effect(s)"));
            }
            Ok(_) => self.notify("The file holds no effects"),
            Err(e) => self.notify(format!("Could not load {}: {e}", path.display())),
        }
    }

    /// Saves the selected track (without its items) as a REAPER `.RTrackTemplate`.
    pub fn save_track_template(&mut self) {
        let Some(track) = self.sel.track.and_then(|id| self.project.track(id)) else {
            self.notify("Select a track first");
            return;
        };
        let text = scythe_core::rpp::track_template(&self.project, &[track.id]);
        let name = if track.name.is_empty() { "track".to_owned() } else { track.name.clone() };
        let dialog = resource_dialog("TrackTemplates")
            .add_filter("Track template", &["RTrackTemplate"])
            .set_file_name(format!("{name}.RTrackTemplate"));
        let Some(path) = dialog.save_file() else { return };
        match std::fs::write(&path, text) {
            Ok(()) => self.notify(format!("Saved {}", path.display())),
            Err(e) => self.notify(format!("Could not save: {e}")),
        }
    }

    /// Inserts the tracks of a `.RTrackTemplate` after the selected track.
    pub fn insert_track_template(&mut self) {
        let Some(path) =
            resource_dialog("TrackTemplates").add_filter("Track template", &["RTrackTemplate"]).pick_file()
        else {
            return;
        };
        let tracks = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| scythe_core::rpp::tracks_from_template(&t).map_err(|e| e.to_string()));
        let tracks = match tracks {
            Ok(t) if !t.is_empty() => t,
            Ok(_) => return self.notify("The file holds no tracks"),
            Err(e) => return self.notify(format!("Could not load {}: {e}", path.display())),
        };
        let first = tracks[0].id;
        let at = self.selected_track_index().map_or(self.project.tracks.len(), |i| i + 1);
        self.edit("Insert track template", |p| {
            for (i, t) in tracks.into_iter().enumerate() {
                p.tracks.insert(at + i, t);
            }
        });
        self.sel = Selection { track: Some(first), item: None };
    }

    pub fn midi_output_names(&mut self) -> &[String] {
        self.midi_out_names.get_or_insert_with(scythe_engine::midi_io::output_names)
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

/// Where a preset listed for an effect comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum PresetSource {
    /// A named preset in one of REAPER's `presets/*.ini` files.
    Reaper(PathBuf, String),
    /// A single-effect `.RfxChain`, as earlier versions of Scythe saved presets.
    FxChain(PathBuf),
    /// A VST 3 plugin's `.vstpreset` file.
    Vst3File(PathBuf),
    /// One of a VST 2 plugin's own programs, by index.
    Program(usize),
}

/// REAPER's resource folder, when REAPER has been run here.
fn reaper_resource_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|c| c.join("REAPER"))
        .filter(|d| d.is_dir())
}

/// A file dialog starting in REAPER's resource folder `sub` (`FXChains`, `TrackTemplates`)
/// when it exists, so files are shared with REAPER.
fn resource_dialog(sub: &str) -> rfd::FileDialog {
    let dir = reaper_resource_dir().map(|d| d.join(sub)).filter(|d| d.is_dir());
    match dir {
        Some(d) => rfd::FileDialog::new().set_directory(d),
        None => rfd::FileDialog::new(),
    }
}
