//! The live session behind a window or a headless server: the project and its undo history,
//! the engine kept in step with it, media loading, recording and input monitoring, the
//! metronome, automation writing, meters and saving. Nothing here draws, so the same session
//! runs under the egui app, without a screen, and behind the control API.

mod automation;
mod files;
mod health;
pub mod learn;
mod live;
mod media;
mod midi;
pub mod render;
mod transport;

pub mod access;
pub mod actions;
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
pub use automation::AutoTarget;
use automation::AutoWrite;
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
    pub history: History,
    pub engine: Engine,
    pub recording: Option<ActiveRecording>,
    /// The setlist being played and where in it.
    pub live: LiveState,
    /// Record mode "time selection auto-punch": keep only what falls inside the time selection.
    pub auto_punch: bool,
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
    autosaved_at: Instant,
    /// The highest DSP load of the last window, and when the window started.
    load_window: (f32, Instant),
    toast: Option<(String, Instant)>,
    gesture: Option<(String, Project)>,
    meters_at: Instant,
    /// Controls being written as automation during this pass.
    auto_writes: Vec<AutoWrite>,
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
    /// MIDI and OSC learn: listening, and the action shortcuts.
    pub learn: learn::LearnState,
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
            auto_punch: false,
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
            autosaved_at: Instant::now(),
            load_window: (0.0, Instant::now()),
            toast: None,
            gesture: None,
            meters_at: Instant::now(),
            auto_writes: Vec::new(),
            waker: Arc::new(|| {}),
            revision: 0,
            loads: 0,
            select: None,
            notices: Vec::new(),
            merge: None,
            surface_status: Vec::new(),
            selected_track: None,
            learn: Default::default(),
        };
        if let Some(reason) = session.engine.info().fallback_reason.clone() {
            session.notify(format!("Audio device unavailable, running silent: {reason}"));
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

    /// Asks the views to select `track`, as a control surface's select button does.
    pub fn select_track(&mut self, track: TrackId) {
        self.selected_track = Some(track);
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

    /// Ends the gesture; item moves and trims end with automatic crossfades on `tracks`, the
    /// tracks they touched.
    pub fn end_gesture_crossfading(&mut self, tracks: impl IntoIterator<Item = TrackId>) {
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

    /// Hands the project to the engine and opens or closes the live inputs and outputs it
    /// needs.
    pub fn sync(&mut self) {
        self.revision += 1;
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
        let monitor =
            !self.no_input && (ltc || self.project.tracks.iter().any(|t| t.armed && t.monitor && !is_midi(t)));
        if let Err(e) = self.engine.set_monitoring(monitor) {
            self.notify(format!("Input monitoring unavailable: {e}"));
        }
        let outputs = self.midi_outputs_needed();
        if let Err(e) = self.engine.set_midi_output_devices(&outputs) {
            self.notify(format!("MIDI output unavailable: {e}"));
        }
        self.engine.set_sync(self.sync_config());
        let inputs = if self.no_input { Vec::new() } else { self.midi_inputs_needed() };
        match self.engine.set_midi_input_devices(&inputs) {
            Err(e) if self.midi_error.as_deref() != Some(e.as_str()) => {
                self.notify(format!("MIDI input unavailable: {e}"));
                self.midi_error = Some(e);
            }
            Err(_) => {}
            Ok(()) => self.midi_error = None,
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
