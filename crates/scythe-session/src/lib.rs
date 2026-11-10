//! The live session behind a window or a headless server: the project and its undo history,
//! the engine kept in step with it, media loading, recording and input monitoring, the
//! metronome, automation writing, meters and saving. Nothing here draws, so the same session
//! runs under the egui app, without a screen, and behind the control API.

pub mod audio;
mod automation;
mod bay;
mod customize;
pub mod explorer;
mod files;
mod health;
pub mod learn;
mod live;
mod media;
mod midi;
pub mod perf;
pub mod render;
pub mod rig;
pub mod tabs;
mod theme;
mod transport;

pub mod access;
pub mod actions;
pub mod agents;
pub mod api;
pub mod control;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use scythe_core::history::History;
use scythe_core::prefs::Preferences;
use scythe_core::{Project, TrackId};
use scythe_engine::Engine;
use scythe_engine::midi_io::midi_input_channel;

pub use api::surface::SurfaceStatus;
use automation::AutoWrite;
pub use automation::{AutoTarget, PreviewSpan, Remembered};
pub use live::LiveState;
use media::{LoadQueue, StretchKey};
pub use media::{LoaderMsg, RenderedItems};
pub use scythe_core::ItemId;
pub use transport::ActiveRecording;

/// Called from background threads when they have something for [`Session::poll`], so a UI
/// can wake up for it.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub track: Option<TrackId>,
    pub item: Option<ItemId>,
}

/// Messages kept for clients that missed them, at most.
const NOTICE_BACKLOG: usize = 64;

pub struct Session {
    pub project: Project,
    /// The media explorer's settings and the file metadata it has read.
    pub explorer: explorer::Explorer,
    /// The colour theme, kept in `theme.ScytheTheme` beside the preferences.
    pub theme: scythe_core::theme::Theme,
    pub history: History,
    pub engine: Engine,
    pub recording: Option<ActiveRecording>,
    /// The setlist being played and where in it.
    pub live: LiveState,
    /// The items "selected item auto-punch" records into: the views' selection.
    pub record_items: Vec<ItemId>,
    /// Live input (monitoring) is off for the whole session (`--no-input`).
    pub no_input: bool,
    /// What REAPER keeps in reaper.ini: pre-roll, count-in at the bar line, the default
    /// metronome output.
    pub prefs: Preferences,
    /// Where [`Session::change_preferences`] saves them; tests leave it unset.
    prefs_file: Option<PathBuf>,
    /// The MIDI devices REAPER-style: stable numbers, names, aliases, which are plugged in.
    pub midi_devices: scythe_engine::midi_io::MidiDevices,
    pub dirty: bool,
    pub saved_at: Option<Instant>,
    pub busy: Option<String>,
    /// Glue and apply-FX renders still running.
    pub items_rendering: usize,
    /// Displayed meter levels with peak decay, per track and for the master.
    pub levels: HashMap<TrackId, (f32, f32)>,
    pub master_level: (f32, f32),
    /// Background work reports here; [`Session::poll`] picks it up.
    pub loader_tx: Sender<LoaderMsg>,
    /// Messages also go to standard error, for sessions without a window.
    pub log_notices: bool,
    loader_rx: Receiver<LoaderMsg>,
    pending: HashSet<PathBuf>,
    pending_stretch: HashSet<StretchKey>,
    load_queue: Arc<std::sync::Mutex<LoadQueue>>,
    /// The last MIDI input error shown, so it is not repeated on every edit.
    midi_error: Option<String>,
    monitor_error: Option<String>,
    midi_out_error: Option<String>,
    autosaved_at: Instant,
    /// The highest DSP load of the last window, and when the window started.
    load_window: (f32, Instant),
    perf: perf::PerfSampler,
    toast: Option<(String, Instant)>,
    gesture: Option<(String, Project)>,
    /// The gesture changes a running effect, whose state reaches the project when it ends.
    fx_gesture: bool,
    /// A model or impulse response being tried in a slot over the control API.
    pub preview: Option<rig::Preview>,
    meters_at: Instant,
    /// Controls being written as automation during this pass.
    auto_writes: Vec<AutoWrite>,
    latch_preview: Vec<automation::Remembered>,
    waker: Waker,
    revision: u64,
    loads: u64,
    select: Option<Selection>,
    notices: Vec<String>,
    /// Label and time of the last edit that later edits of the same control fold into.
    merge: Option<(String, Instant)>,
    /// How the control surfaces in the preferences are doing, as the surface host last saw it.
    pub surface_status: Vec<SurfaceStatus>,
    /// The selected track, as the window shows it; control surfaces light it and select it.
    pub selected_track: Option<TrackId>,
    /// Every selected track in track order, the selected track among them: what "selected
    /// tracks" actions work on.
    pub selected_tracks: Vec<TrackId>,
    /// A track selection the session made (select all, a new folder) for the window to take.
    track_select: Option<Vec<TrackId>>,
    /// MIDI and OSC learn: listening, and the action shortcuts.
    pub learn: learn::LearnState,
    /// `reaper-kb.ini` but for the MIDI and OSC shortcuts `learn` keeps: keyboard shortcuts,
    /// custom actions, scripts registered as actions.
    pub keymap: scythe_core::keymap::Keymap,
    /// Customised toolbars (`reaper-menu.ini`).
    pub menus: scythe_core::menus::MenuFile,
    /// Mouse modifiers (`reaper-mouse.ini`).
    pub mouse: scythe_core::mousemap::MouseMap,
    /// A window runs the actions the session cannot (set by the desktop app).
    pub host_actions: bool,
    host_commands: Vec<String>,
    /// Toggle states only the window knows (snap, mixer shown, open toolbars), by command.
    pub host_toggles: HashMap<String, bool>,
    /// Every project tab; the active one's slot is empty, its project being `project`.
    parked_tabs: Vec<Option<tabs::ParkedTab>>,
    active_tab: usize,
    /// Recent events for clients that poll (`events.since`).
    pub event_log: api::feed::EventLog,
    /// The AI agents driving the session over MCP, what they did and whether they are paused.
    pub agents: agents::Agents,
    /// Runs ReaScript for `script.run`; the desktop app sets it.
    pub script_runner: Option<api::script::ScriptRunner>,
    /// The channel maps and aliases of the preferences (`[alias_in_*]`, `[alias_out_*]`), by
    /// section.
    aliases: HashMap<String, scythe_core::prefs::ChannelAliases>,
    /// `mlockall` was called (it lasts for the process).
    memory_locked: bool,
    /// `/dev/cpu_dma_latency` held at 0 while the device runs.
    cpu_awake: Option<std::fs::File>,
    /// The device was closed while stopped ("Close audio device when stopped").
    audio_closed: bool,
    /// A device switch waiting for its busy device: the devices to go back to when the wait
    /// ends without it, and whether to save the new choice once it opens.
    switching: Option<audio::Switch>,
    /// Why a device could not be opened, for the error window; taken by the window.
    device_error: Option<String>,
    /// Opens the devices a request names beside the current engine (a seam for tests).
    opener: audio::Opener,
}

impl Session {
    pub fn new(engine: Engine) -> Self {
        let (loader_tx, loader_rx) = channel();
        let mut session = Self {
            project: Project::default(),
            history: History::default(),
            engine,
            recording: None,
            live: LiveState::default(),
            record_items: Vec::new(),
            no_input: false,
            prefs: Preferences::default(),
            prefs_file: None,
            midi_devices: Default::default(),
            dirty: false,
            saved_at: None,
            busy: None,
            items_rendering: 0,
            levels: HashMap::new(),
            master_level: (0.0, 0.0),
            loader_tx,
            log_notices: false,
            loader_rx,
            pending: HashSet::new(),
            pending_stretch: HashSet::new(),
            load_queue: Arc::default(),
            midi_error: None,
            monitor_error: None,
            midi_out_error: None,
            autosaved_at: Instant::now(),
            load_window: (0.0, Instant::now()),
            perf: Default::default(),
            toast: None,
            gesture: None,
            fx_gesture: false,
            preview: None,
            meters_at: Instant::now(),
            auto_writes: Vec::new(),
            latch_preview: Vec::new(),
            waker: Arc::new(|| {}),
            revision: 0,
            loads: 0,
            select: None,
            notices: Vec::new(),
            merge: None,
            surface_status: Vec::new(),
            selected_track: None,
            selected_tracks: Vec::new(),
            track_select: None,
            learn: Default::default(),
            keymap: Default::default(),
            explorer: Default::default(),
            theme: Default::default(),
            menus: Default::default(),
            mouse: Default::default(),
            host_actions: false,
            host_commands: Vec::new(),
            host_toggles: HashMap::new(),
            parked_tabs: vec![None],
            active_tab: 0,
            event_log: Default::default(),
            agents: Default::default(),
            script_runner: None,
            aliases: HashMap::new(),
            memory_locked: false,
            cpu_awake: None,
            audio_closed: false,
            switching: None,
            device_error: None,
            opener: audio::open_engine,
        };
        match session.engine.info().fallback_reason.clone() {
            Some(reason) if scythe_engine::devices::is_busy(&reason) => {
                let request = session.engine.devices().clone();
                session.wait_for_device(request, None, false);
            }
            Some(reason) => session.notify(format!("Audio device unavailable, running silent: {reason}")),
            None => {}
        }
        session.after_load();
        session
    }

    pub fn set_waker(&mut self, waker: Waker) {
        self.waker = waker;
    }

    pub fn waker(&self) -> Waker {
        self.waker.clone()
    }

    /// Counts every change to the project, so clients can tell their copy is stale.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Counts projects loaded (opened, new, replaced), so a view can reset itself.
    pub fn loads(&self) -> u64 {
        self.loads
    }

    /// What the session placed and a view should select: imported, recorded or glued items.
    pub fn take_selection(&mut self) -> Option<Selection> {
        self.select.take()
    }

    /// The selected tracks, or the selected track when the window keeps no list.
    pub fn selection(&self) -> Vec<TrackId> {
        let all: Vec<TrackId> =
            self.project.tracks.iter().map(|t| t.id).filter(|id| self.selected_tracks.contains(id)).collect();
        if all.is_empty() { self.selected_track.into_iter().collect() } else { all }
    }

    /// Selects `tracks` (the first becomes the selected track) and asks the window to show it.
    pub fn select_tracks(&mut self, tracks: Vec<TrackId>) {
        self.selected_track = tracks.first().copied();
        self.selected_tracks = tracks.clone();
        self.track_select = Some(tracks);
    }

    /// A track selection the session made since the last call.
    pub fn take_track_selection(&mut self) -> Option<Vec<TrackId>> {
        self.track_select.take()
    }

    /// Asks the views to select `track`, as a control surface's select button does.
    pub fn select_track(&mut self, track: TrackId) {
        self.selected_track = Some(track);
        self.selected_tracks = vec![track];
        self.select = Some(Selection { track: Some(track), item: None });
    }

    /// Messages since the last call, oldest first.
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    fn after_load(&mut self) {
        self.live.list = 0;
        self.live.player = Default::default();
        self.live.driving = false;
        self.engine.stop();
        self.engine.seek(self.project.cursor);
        self.sync();
        self.loads += 1;
        self.follow_project_rate();
        let saved = self.project.saved_selection();
        if !saved.is_empty() {
            self.select_tracks(saved);
        }
    }

    // ---- editing -------------------------------------------------------------------------

    /// Applies an undoable edit.
    pub fn edit(&mut self, label: &str, f: impl FnOnce(&mut Project)) {
        let before = self.project.clone();
        f(&mut self.project);
        if self.project != before {
            self.history.record(label, before);
            self.merge = None;
            self.changed();
        }
    }

    /// An edit that folds into the previous one when it changes the same control (`key`)
    /// within a second, so a remote fader moved in many small steps is one undo step.
    pub fn edit_merging(&mut self, label: &str, key: &str, f: impl FnOnce(&mut Project)) {
        let recent = self.merge.as_ref().is_some_and(|(k, at)| k == key && at.elapsed() < Duration::from_secs(1));
        if recent && self.history.undo_label() == Some(label) {
            let before = self.project.clone();
            f(&mut self.project);
            if self.project != before {
                self.changed();
            }
        } else {
            self.edit(label, f);
        }
        self.merge = Some((key.to_owned(), Instant::now()));
    }

    /// An edit that, when `fold` is set and the last undo step is `label`, becomes part of
    /// that step, so trying many settings in a row undoes in one go.
    pub fn edit_folding(&mut self, label: &str, fold: bool, f: impl FnOnce(&mut Project)) {
        if fold && self.history.undo_label() == Some(label) {
            let before = self.project.clone();
            f(&mut self.project);
            if self.project != before {
                self.merge = None;
                self.changed();
            }
        } else {
            self.edit(label, f);
        }
    }

    /// A change saved with the project that is not an undo step (how a folder shows).
    pub fn change_view(&mut self, f: impl FnOnce(&mut Project)) {
        let before = self.project.clone();
        f(&mut self.project);
        if self.project != before {
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
        self.end_gesture_crossfading([]);
    }

    /// Changes a running effect (its parameters or its own state) as one undo step per gesture:
    /// the effect's state is stored in the project when the gesture starts and again when it
    /// ends, so undo brings the earlier state back. Ends with [`Session::end_gesture`].
    pub fn fx_gesture<R>(
        &mut self,
        label: &str,
        inst: &scythe_engine::fx::FxInstance,
        f: impl FnOnce(&mut dyn scythe_engine::fx::Effect) -> R,
    ) -> Option<R> {
        if self.gesture.is_none() {
            self.engine.store_plugin_states(&mut self.project);
            self.gesture = Some((label.to_owned(), self.project.clone()));
            self.fx_gesture = true;
        }
        let out = inst.with(f);
        self.engine.wake_effects();
        self.dirty = true;
        out
    }

    /// A single change to a running effect as its own undo step.
    pub fn fx_edit<R>(
        &mut self,
        label: &str,
        inst: &scythe_engine::fx::FxInstance,
        f: impl FnOnce(&mut dyn scythe_engine::fx::Effect) -> R,
    ) -> Option<R> {
        self.end_gesture();
        let out = self.fx_gesture(label, inst, f);
        self.end_gesture();
        out
    }

    /// Ends the gesture; item moves and trims end with automatic crossfades on `tracks`, the
    /// tracks they touched.
    pub fn end_gesture_crossfading(&mut self, tracks: impl IntoIterator<Item = TrackId>) {
        if std::mem::take(&mut self.fx_gesture) {
            self.engine.store_plugin_states(&mut self.project);
            self.sync();
        }
        if self
            .gesture
            .as_ref()
            .is_some_and(|(label, _)| matches!(label.as_str(), "Move item" | "Move items" | "Trim item"))
        {
            let tracks: HashSet<TrackId> = tracks.into_iter().collect();
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

    pub fn in_gesture(&self) -> bool {
        self.gesture.is_some()
    }

    pub fn undo(&mut self) {
        self.end_gesture();
        let by = self.history.undo_by().map(|by| format!(" ({by})")).unwrap_or_default();
        match self.history.undo(&mut self.project) {
            Some(label) => {
                self.notify(format!("Undo: {label}{by}"));
                self.changed();
            }
            None => self.notify("Nothing to undo"),
        }
    }

    pub fn redo(&mut self) {
        let by = self.history.redo_by().map(|by| format!(" ({by})")).unwrap_or_default();
        match self.history.redo(&mut self.project) {
            Some(label) => {
                self.notify(format!("Redo: {label}{by}"));
                self.changed();
            }
            None => self.notify("Nothing to redo"),
        }
    }

    fn changed(&mut self) {
        self.dirty = true;
        self.sync();
    }

    /// Whether the metronome is on (it clicks while playing unless the project says otherwise).
    pub fn metronome(&self) -> bool {
        self.project.metronome.enabled()
    }

    pub fn toggle_metronome(&mut self) {
        self.set_metronome(!self.metronome());
    }

    /// Turns the metronome on or off. Like REAPER's action this is saved with the project but
    /// is not an undo step.
    pub fn set_metronome(&mut self, on: bool) {
        self.change_metronome(|m| m.set(scythe_core::metronome::ENABLED, on));
    }

    /// Changes the project's metronome settings (count-in, volume, outputs), not as an undo step.
    pub fn change_metronome(&mut self, f: impl FnOnce(&mut scythe_core::metronome::Metronome)) {
        let before = self.project.metronome.clone();
        f(&mut self.project.metronome);
        if self.project.metronome != before {
            self.changed();
        }
    }

    /// Reads the preferences from `path` (defaults when it does not exist yet) and saves later
    /// changes there.
    pub fn load_preferences(&mut self, path: PathBuf) {
        match std::fs::read_to_string(&path) {
            Ok(text) => self.prefs = Preferences::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => self.notify(format!("Cannot read {}: {e}", path.display())),
        }
        self.prefs_file = Some(path);
        self.load_action_bindings();
        self.load_customizations();
        self.load_theme();
        self.load_channel_aliases();
        self.apply_audio_options();
        self.engine.set_default_click_output(self.prefs.metronome_default_output);
        if let Some(file) = self.monitor_fx_file()
            && let Ok(text) = std::fs::read_to_string(&file)
        {
            match scythe_core::rpp::parse(&format!("<FXCHAIN\n{text}\n>\n")) {
                Ok(chunk) => self.engine.set_monitor_fx(scythe_core::fx_chain::split_chain(&chunk.children).0),
                Err(e) => self.notify(format!("Cannot read {}: {e}", file.display())),
            }
        }
        self.rescan_midi_devices();
    }

    /// Freezes `track`: renders its items through its effects (before the fader, with a
    /// second of tail) to `reaper_freeze_<name>.wav` beside the project, then stores the
    /// effects and items as a freeze level and plays the render in their place, as REAPER's
    /// "Freeze to stereo" does. Freezing a frozen track stacks another level.
    pub fn freeze_track(&mut self, track: TrackId) -> Result<PathBuf, String> {
        const TAIL: f64 = 1.0;
        let t = self.project.track(track).ok_or("the track is gone")?;
        let ids: Vec<_> = t.items.iter().filter(|i| !i.muted).map(|i| i.id).collect();
        let (mut isolated, range) =
            scythe_core::glue::isolate(&self.project, track, &ids, true).ok_or("the track has no items to freeze")?;
        if let (Some(iso), Some(src)) = (isolated.tracks.first_mut(), self.project.track(track)) {
            iso.channels = src.channels;
        }
        let dir = self.project.path.as_deref().and_then(std::path::Path::parent).ok_or("save the project first")?;
        let base: String =
            t.name.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' }).collect();
        let base = format!("reaper_freeze_{}", if base.trim().is_empty() { "track" } else { base.trim() });
        let path = (0..1000)
            .map(|n| dir.join(if n == 0 { format!("{base}.wav") } else { format!("{base}-{n:03}.wav") }))
            .find(|p| !p.exists())
            .ok_or("no free file name for the freeze render")?;
        let range = scythe_core::TimeRange::new(range.start, range.end + TAIL);
        let rate = self.engine.sample_rate();
        let pool = self.engine.pool().clone();
        for take in isolated.tracks.iter().flat_map(|t| &t.items).flat_map(|i| &i.takes) {
            if let Some(f) = take.source.file() {
                let _ = pool.load(&scythe_core::rpp::resolve_media_path(&self.project, f), rate);
            }
        }
        pool.prepare_project(&isolated);
        scythe_engine::bounce::bounce_to_wav(
            &isolated,
            &pool,
            range,
            rate,
            scythe_engine::bounce::BitDepth::Float32,
            &path,
            |_| {},
        )
        .map_err(|e| e.to_string())?;
        let file = path.to_string_lossy().into_owned();
        self.edit("Freeze track", |p| {
            if let Some(t) = p.track_mut(track) {
                scythe_core::freeze::freeze(t, &file, range);
            }
        });
        Ok(path)
    }

    /// Restores the effects and items of `track`'s last freeze level; false when it is not
    /// frozen.
    pub fn unfreeze_track(&mut self, track: TrackId) -> bool {
        let frozen = self.project.track(track).is_some_and(|t| scythe_core::freeze::levels(t) > 0);
        if frozen {
            self.edit("Unfreeze track", |p| {
                if let Some(t) = p.track_mut(track) {
                    scythe_core::freeze::unfreeze(t);
                }
            });
        }
        frozen
    }

    /// Where the monitoring effects are kept: `reaper-hwoutfx.ini` beside the preferences, as
    /// in REAPER's resource folder.
    fn monitor_fx_file(&self) -> Option<PathBuf> {
        Some(self.prefs_file.as_deref()?.with_file_name("reaper-hwoutfx.ini"))
    }

    /// The monitoring effects: after the master, on what the device plays only.
    pub fn monitor_fx(&self) -> Vec<scythe_core::FxSlot> {
        self.engine.fx().monitor_fx.clone()
    }

    /// Replaces the monitoring effects and saves them with the preferences (not the project).
    pub fn set_monitor_fx(&mut self, slots: Vec<scythe_core::FxSlot>) {
        let body = scythe_core::rpp::write_body(&scythe_core::fx_chain::join_chain(&slots));
        self.engine.set_monitor_fx(slots);
        self.sync();
        let Some(file) = self.monitor_fx_file() else { return };
        let written = file.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(&file, body));
        if let Err(e) = written {
            self.notify(format!("Cannot save the monitoring FX to {}: {e}", file.display()));
        }
    }

    /// Changes the preferences and saves them, keeping whatever else their file holds.
    pub fn change_preferences(&mut self, f: impl FnOnce(&mut Preferences)) {
        let before = self.prefs.clone();
        f(&mut self.prefs);
        if self.prefs == before {
            return;
        }
        if self.prefs.audio != before.audio {
            self.apply_audio_options();
        }
        if self.prefs.metronome_default_output != before.metronome_default_output {
            self.engine.set_default_click_output(self.prefs.metronome_default_output);
            self.sync();
        }
        let Some(path) = self.prefs_file.clone() else { return };
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::write(&path, self.prefs.write_into(&text)));
        if let Err(e) = written {
            self.notify(format!("Cannot save preferences to {}: {e}", path.display()));
        }
    }

    /// Forgets the device errors already shown, so a new device reports its own, and the
    /// media loads begun for the old device.
    fn reset_device_errors(&mut self) {
        self.midi_error = None;
        self.monitor_error = None;
        self.midi_out_error = None;
        self.pending.clear();
        self.pending_stretch.clear();
    }

    /// Hands the project to the engine and opens or closes the live inputs and outputs it
    /// needs.
    pub fn sync(&mut self) {
        self.revision += 1;
        self.project.resolve_automation_items();
        self.engine.sync(&self.project);
        let is_midi = |t: &scythe_core::Track| midi_input_channel(t.rec_input).is_some();
        let wants_midi = self.project.tracks.iter().any(|t| t.midi_out.is_some() || (t.armed && is_midi(t)))
            || self.project.timecode_sync.enabled()
            || self.control_inputs_wanted();
        if wants_midi && self.midi_devices.inputs.is_empty() && self.midi_devices.outputs.is_empty() {
            self.midi_devices
                .merge_present(&scythe_engine::midi_io::input_names(), &scythe_engine::midi_io::output_names());
            scythe_engine::midi_io::share_devices(&self.midi_devices);
        }
        let sync = self.sync_config();
        let ltc = matches!(sync.chase.map(|c| c.source), Some(scythe_engine::midi_sync::ChaseSource::Ltc { .. }));
        let monitor = !self.no_input
            && (ltc
                || self.project.tracks.iter().any(|t| t.armed && t.monitor && !is_midi(t))
                || scythe_engine::stock::reainsert::in_project(&self.project));
        if let Some(e) = new_error(&mut self.monitor_error, self.engine.set_monitoring(monitor)) {
            self.notify(format!("Input monitoring unavailable: {e}"));
        }
        let outputs = self.midi_outputs_needed();
        if let Some(e) = new_error(&mut self.midi_out_error, self.engine.set_midi_output_devices(&outputs)) {
            self.notify(format!("MIDI output unavailable: {e}"));
        }
        self.engine.set_sync(self.sync_config());
        let inputs = if self.no_input { Vec::new() } else { self.midi_inputs_needed() };
        if let Some(e) = new_error(&mut self.midi_error, self.engine.set_midi_input_devices(&inputs)) {
            self.notify(format!("MIDI input unavailable: {e}"));
        }
        self.request_media();
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

    // ---- feedback ------------------------------------------------------------------------

    pub fn notify(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        if self.log_notices {
            eprintln!("{msg}");
        }
        if self.notices.len() >= NOTICE_BACKLOG {
            self.notices.remove(0);
        }
        self.notices.push(msg.clone());
        self.toast = Some((msg, Instant::now()));
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
}

/// `$XDG_CONFIG_HOME/scythe/preferences.ini`: the preferences REAPER keeps in reaper.ini.
pub fn preferences_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(std::env::temp_dir)
        .join("scythe")
        .join("preferences.ini")
}

/// Where Scythe keeps its own files: the demo song, recordings of unsaved projects, presets.
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("scythe")
}

/// The error of `result` when it differs from the last one kept in `last`, so a failure that
/// every sync repeats is reported once.
fn new_error(last: &mut Option<String>, result: Result<(), String>) -> Option<String> {
    match result {
        Err(e) if last.as_deref() != Some(e.as_str()) => {
            *last = Some(e.clone());
            Some(e)
        }
        Err(_) => None,
        Ok(()) => {
            *last = None;
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::new_error;

    #[test]
    fn a_repeated_failure_is_reported_once() {
        let mut last = None;
        assert_eq!(new_error(&mut last, Err("off".into())).as_deref(), Some("off"));
        assert_eq!(new_error(&mut last, Err("off".into())), None);
        assert_eq!(new_error(&mut last, Err("busy".into())).as_deref(), Some("busy"));
        assert_eq!(new_error(&mut last, Ok(())), None);
        assert_eq!(new_error(&mut last, Err("busy".into())).as_deref(), Some("busy"));
    }
    /// A drag on an effect's parameter (from its editor or its parameter rows) is one undo
    /// step, and undo brings the running effect back to where it was.
    #[test]
    fn an_effect_gesture_is_one_undo_step() {
        use scythe_engine::builtin::Builtin;
        let pool = std::sync::Arc::new(scythe_engine::MediaPool::with_resample(
            scythe_core::render_settings::ResampleMode::PLAYBACK,
        ));
        let mut s = super::Session::new(scythe_engine::Engine::start_offline(pool, 48_000));
        let t = s.project.add_track("a");
        s.edit("Add plugin", |p| p.track_mut(t).unwrap().fx.push(Builtin::Compressor.slot()));
        let inst = || {
            let s: &super::Session = &s;
            let slot = s.project.track(t).unwrap().fx[0].clone();
            s.engine.fx().instance(Some(t), &slot, 0).unwrap()
        };
        let threshold = |s: &super::Session| {
            let slot = s.project.track(t).unwrap().fx[0].clone();
            s.engine.fx().instance(Some(t), &slot, 0).unwrap().with(|e| e.param(0).unwrap().0).unwrap()
        };
        let first = inst();
        let before = threshold(&s);
        for v in [-3.0, -9.0, -18.0] {
            s.fx_gesture("Adjust Threshold", &first, |e| e.set_param(0, v));
        }
        s.end_gesture();
        assert!((threshold(&s) + 18.0).abs() < 0.01);
        assert_eq!(s.history.undo_label(), Some("Adjust Threshold"));
        s.undo();
        assert!((threshold(&s) - before).abs() < 0.01);
        assert_eq!(s.history.undo_label(), Some("Add plugin"));
        s.redo();
        assert!((threshold(&s) + 18.0).abs() < 0.01);
    }
}
