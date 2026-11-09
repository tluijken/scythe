use std::collections::{HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use scythe_core::midi::{MidiSource, TimedNote};
use scythe_core::smf::write_smf;
use scythe_core::{Item, ItemId, Project, Source, TempoMap, TimeRange, TrackId};
use scythe_engine::bounce::{BitDepth, Encoding, Finish, bounce_finished, bounce_to_wav};
use scythe_engine::{Engine, MediaPool, MediaState};
use scythe_session::{LoaderMsg, RenderedItems, Session};

pub use scythe_session::{AutoTarget, Selection};

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
    /// Range to zoom the arrange view to on the next frame, once its width is known.
    pub fit: Option<TimeRange>,
    /// The monitor mixes page replaces the arrange view and mixer.
    pub monitor_page: bool,
    pub monitor_wizard: Option<crate::ui::monitor::Wizard>,
    pub cue_dialog: Option<crate::ui::monitor::Wizard>,
    /// The setlist page replaces the arrange view.
    pub setlist: bool,
    /// The track grouping matrix window is open, on this group (0-based).
    pub groups: Option<usize>,
    /// The control surfaces window, while open.
    pub surfaces: Option<crate::ui::surfaces::SurfacesWindow>,
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

/// The window around a [`Session`]: selection, view, clipboard, editors and scripts. The
/// session's fields and methods are reachable through `Deref`, so `app.project` and
/// `app.edit(…)` read as before the split.
pub struct ScytheApp {
    pub session: Session,
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
    /// Hardware MIDI outputs, listed once.
    midi_out_names: Option<Vec<String>>,
    pub last_script: Option<PathBuf>,
    /// What the last script printed (`ShowConsoleMsg`) or its error, until closed.
    pub script_console: Option<String>,
    /// A script that deferred work, run once per frame, and its file name.
    running_script: Option<(crate::script::RunningScript, String)>,
    /// What the app does for scripts, made when a script starts.
    script_services: Option<std::rc::Rc<crate::script_host::AppServices>>,
    /// The running script's graphics window.
    pub script_gfx: crate::ui::gfx_view::ScriptGfxView,
    midi_cache: HashMap<ItemId, (MidiKey, Arc<Vec<TimedNote>>)>,
    pub ctx: egui::Context,
    /// Plugin editors open in their own windows.
    editors: Vec<OpenEditor>,
    /// JSFX views (`@gfx`) open in windows inside the main one.
    pub gfx_views: Vec<GfxView>,
    /// The session's load count the view was last reset for.
    seen_loads: u64,
    /// The NAM model library, TONE3000 and the NAM panel.
    pub nam: crate::nam::NamHub,
}

impl Deref for ScytheApp {
    type Target = Session;

    fn deref(&self) -> &Session {
        &self.session
    }
}

impl DerefMut for ScytheApp {
    fn deref_mut(&mut self) -> &mut Session {
        &mut self.session
    }
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
    /// The effect's `@gfx` thread, started when the view first shows.
    pub runner: Option<scythe_engine::eel::gfx::View>,
    /// A menu the script waits on.
    pub menu: Option<crate::ui::gfx_view::GfxMenu>,
    /// Screen pixels per point of the last frame.
    pub scale: f32,
    pub ran: Option<Instant>,
    pub pointer: crate::ui::gfx_view::GfxPointer,
}

impl ScytheApp {
    /// A window on `project`, playing on the default device, or on a silent clock with
    /// `null_audio`.
    pub fn new(ctx: egui::Context, project: Project, null_audio: bool) -> Self {
        let pool = Arc::new(MediaPool::with_resample(scythe_core::render_settings::ResampleMode::PLAYBACK));
        let engine = if null_audio { Engine::start_offline(pool, 48_000) } else { Engine::start(pool) };
        let mut session = Session::new(engine);
        session.load(project);
        Self::with_session(ctx, session)
    }

    pub fn with_session(ctx: egui::Context, mut session: Session) -> Self {
        let wake = ctx.clone();
        session.set_waker(Arc::new(move || wake.request_repaint()));
        let mut app = Self {
            session,
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
                fit: None,
                monitor_page: false,
                monitor_wizard: None,
                cue_dialog: None,
                setlist: false,
                groups: None,
                surfaces: None,
            },
            palette: None,
            clipboard: Vec::new(),
            multi: HashSet::new(),
            razor: Vec::new(),
            clipboard_origin: 0.0,
            midi_out_names: None,
            last_script: None,
            script_console: None,
            running_script: None,
            script_services: None,
            script_gfx: Default::default(),
            midi_cache: HashMap::new(),
            ctx,
            editors: Vec::new(),
            gfx_views: Vec::new(),
            seen_loads: 0,
            nam: crate::nam::NamHub::new(),
        };
        app.follow_session();
        app
    }

    /// Catches the view up with the session: a newly loaded project resets selection and
    /// zoom, placed items become the selection, and selected things that are gone are let go.
    pub fn follow_session(&mut self) {
        if self.seen_loads != self.session.loads() {
            self.seen_loads = self.session.loads();
            self.sel = Selection { track: self.project.tracks.first().map(|t| t.id), item: None };
            self.view.track_scroll = 0.0;
            let end = self.project.length().max(self.project.tempo.bar_seconds() * 16.0);
            self.view.fit = Some(TimeRange::new(0.0, end));
        }
        if let Some(sel) = self.session.take_selection() {
            self.multi.clear();
            self.sel = sel;
        }
        if self.sel.track.is_some_and(|t| self.project.track(t).is_none()) {
            self.sel.track = None;
        }
        if self.sel.item.is_some_and(|i| self.project.find_item(i).is_none()) {
            self.sel.item = None;
        }
        self.session.selected_track = self.sel.track;
    }

    pub fn poll(&mut self) {
        self.session.poll();
        self.run_marker_commands();
        self.poll_nam();
        self.follow_session();
    }

    /// Ends the gesture, crossfading on the tracks of the selected items after a move or trim.
    pub fn end_gesture(&mut self) {
        if !self.session.in_gesture() {
            return;
        }
        let tracks: Vec<TrackId> =
            self.selected_items().into_iter().filter_map(|id| self.project.find_item(id).map(|(t, _)| t)).collect();
        self.session.end_gesture_crossfading(tracks);
    }

    pub fn undo(&mut self) {
        self.end_gesture();
        self.session.undo();
        self.follow_session();
    }

    pub fn redo(&mut self) {
        self.session.redo();
        self.follow_session();
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

    pub fn go_to_start(&mut self) {
        self.set_cursor(0.0);
        self.view.start = 0.0;
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
        self.session.new_project();
        self.follow_session();
    }

    pub fn load_demo(&mut self) {
        match crate::demo::build(self.engine.sample_rate()) {
            Ok(project) => {
                self.session.load(project);
                self.follow_session();
                self.sel.track = self.project.tracks.get(2).map(|t| t.id);
                let tempo = self.project.tempo.clone();
                self.view.fit = Some(TimeRange::new(tempo.bar_start(33), tempo.bar_start(49)));
            }
            Err(e) => self.notify(format!("Could not create demo project: {e}")),
        }
    }

    pub fn open_path(&mut self, path: &Path) {
        let _ = self.session.open_path(path);
        self.follow_session();
    }

    pub fn open_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new().add_filter("REAPER project", &["rpp", "RPP"]).pick_file() {
            self.open_path(&path);
        }
    }

    pub fn save(&mut self) {
        match self.project.path.clone() {
            Some(path) => {
                let _ = self.save_to(&path);
            }
            None => self.save_as(),
        }
    }

    pub fn save_as(&mut self) {
        let dialog = rfd::FileDialog::new()
            .add_filter("REAPER project", &["rpp"])
            .set_file_name(format!("{}.rpp", self.project.name()));
        if let Some(path) = dialog.save_file() {
            let _ = self.save_to(&path);
        }
    }

    pub fn import_dialog(&mut self) {
        let files = rfd::FileDialog::new()
            .add_filter("Media", &["wav", "flac", "mp3", "ogg", "aif", "aiff", "m4a", "caf", "mid", "midi"])
            .pick_files();
        if let Some(files) = files {
            let at = self.project.cursor;
            self.session.import_files(files, self.sel.track, at);
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
        let mut encoding = Encoding::wav(BitDepth::Int24);
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
                        (t.solo, t.solo_mode, t.solo_defeat) = (i == j, scythe_core::SoloMode::IgnoreRouting, false);
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
                last = bounce_finished(&project, &pool, range, rate, channels, encoding.clone(), &path, finish, |_| {})
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
                runner: None,
                menu: None,
                scale: 1.0,
                ran: None,
                pointer: Default::default(),
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
            let index = |t: TrackId| self.session.project.track_index(t).unwrap_or(0);
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
        self.script_services = Some(std::rc::Rc::new(self.make_script_services()));
        self.script_gfx = Default::default();
        let ctx = self.script_context();
        match crate::script::run(&source, &name, &self.project, ctx) {
            Ok((out, running)) => {
                self.apply_script(&name, out);
                match running {
                    Some(mut script) => {
                        self.show_script_gfx(&mut script);
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
                self.show_script_gfx(&mut script);
                if script.is_running() {
                    self.running_script = Some((script, name));
                } else {
                    self.finish_script(script, name);
                }
            }
            Err(e) => {
                self.script_gfx.open = false;
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
        self.script_gfx.open = false;
        let ctx = self.script_context();
        match script.stop(&self.project, ctx) {
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

    fn show_script_gfx(&mut self, script: &mut crate::script::RunningScript) {
        if let Some(w) = script.gfx_window() {
            self.script_gfx.update(&self.ctx, w);
        }
    }

    /// Slots for every effect a script can add by name, and the media pool for lengths.
    fn make_script_services(&self) -> crate::script_host::AppServices {
        let fx = self.engine.fx();
        let mut plugins: Vec<scythe_core::FxSlot> =
            scythe_engine::builtin::Builtin::ALL.iter().map(|b| b.slot()).collect();
        plugins.extend(fx.clap_plugins().iter().map(scythe_engine::fx::clap_slot));
        plugins.extend(fx.lv2_plugins().iter().map(scythe_engine::fx::lv2_slot));
        plugins.extend(fx.vst3_plugins().iter().map(scythe_engine::fx::vst3_slot));
        crate::script_host::AppServices { plugins, pool: self.engine.pool().clone() }
    }

    /// The running effects of each track, by slot.
    fn live_fx(&self) -> HashMap<TrackId, Vec<Option<std::rc::Rc<dyn crate::script::LiveFx>>>> {
        let master = std::iter::once((None, &self.project.master));
        let tracks = self.project.tracks.iter().map(|t| (Some(t.id), t));
        master
            .chain(tracks)
            .map(|(key, t)| {
                let live =
                    t.fx.iter()
                        .enumerate()
                        .map(|(i, slot)| {
                            self.engine.fx().instance(key, slot, i).map(|inst| {
                                std::rc::Rc::new(crate::script_host::LiveInstance(inst))
                                    as std::rc::Rc<dyn crate::script::LiveFx>
                            })
                        })
                        .collect();
                (t.id, live)
            })
            .collect()
    }

    fn script_context(&mut self) -> crate::script::ScriptContext {
        let resource_dir = reaper_resource_dir().unwrap_or_else(crate::demo::data_dir);
        let closed = std::mem::take(&mut self.script_gfx.closed);
        crate::script::ScriptContext {
            selected_tracks: self.sel.track.into_iter().collect(),
            selected_items: self.selected_items(),
            cursor: self.project.cursor,
            playing: self.is_playing(),
            play_position: self.position(),
            fx: self.live_fx(),
            services: self.script_services.clone().map(|s| s as std::rc::Rc<dyn crate::script::Services>),
            resource_dir: Some(resource_dir),
            ext_state_file: Some(crate::demo::data_dir().join("reaper-extstate.ini")),
            undo: self.history.undo_label().map(str::to_owned),
            redo: self.history.redo_label().map(str::to_owned),
            dirty: self.dirty,
            gfx: self.script_gfx.pointer.take_input(),
            gfx_closed: closed,
        }
    }

    fn apply_script(&mut self, name: &str, out: crate::script::ScriptOutcome) {
        if let Some(project) = out.project {
            let label = out.undo.unwrap_or_else(|| format!("Run {name}"));
            self.edit(&label, |p| *p = project);
        }
        if let Some((tracks, items)) = out.selection {
            self.sel.track = tracks.iter().copied().find(|t| self.project.track(*t).is_some());
            self.multi = items.iter().copied().collect();
            self.sel.item = items.first().copied();
        }
        if let Some(t) = out.cursor {
            self.set_cursor(t);
        }
        match out.play {
            Some(true) if !self.is_playing() => self.toggle_play(),
            Some(false) => self.stop(),
            _ => {}
        }
        if out.fx_touched {
            self.engine.wake_effects();
            self.dirty = true;
        }
        self.dirty |= out.dirty;
        for action in out.commands.into_iter().filter_map(crate::palette::Action::from_reaper_command) {
            action.run(self);
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
pub(crate) fn resource_dialog(sub: &str) -> rfd::FileDialog {
    let dir = reaper_resource_dir().map(|d| d.join(sub)).filter(|d| d.is_dir());
    match dir {
        Some(d) => rfd::FileDialog::new().set_directory(d),
        None => rfd::FileDialog::new(),
    }
}
