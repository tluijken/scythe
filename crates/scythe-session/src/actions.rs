//! The action registry: every action Scythe has, by REAPER command id where REAPER has the same
//! action (named as REAPER's action list names it) or by a Scythe named command
//! (`_SCYTHE_SETLIST_NEXT`), plus the custom actions and scripts of `reaper-kb.ini`. Keyboard
//! shortcuts, the action list, toolbars, mouse modifiers, control surfaces, MIDI/OSC learn,
//! marker actions and ReaScript's `Main_OnCommand` all run actions through it.
//!
//! Actions marked [`Runs::Session`] run here, without a window; the others need the desktop
//! app, which takes them from [`Session::take_host_commands`].

use std::sync::LazyLock;

use crate::Session;
use crate::api::{self, ApiError};
use scythe_core::AutoMode;
use scythe_core::edit::actions as edit_actions;
use scythe_core::keymap::{self, Line, SECTION_MAIN};
use scythe_core::marker_action as ids;
use scythe_core::model::GlobalAutomation;
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Runs {
    /// Without a window: from surfaces, learned controls and the control API alone.
    Session,
    /// Only in the desktop app.
    Window,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Builtin,
    /// A custom action (macro) from `reaper-kb.ini`.
    Custom,
    /// A script registered as an action.
    Script,
}

/// An action as the action list shows it.
#[derive(Debug, Clone, PartialEq, Serialize, schemars::JsonSchema)]
pub struct ActionInfo {
    /// A REAPER command id (`40044`) or a named command (`_SCYTHE_SETLIST_NEXT`).
    pub command: String,
    pub name: String,
    /// The keymap section: 0 for the main window.
    pub section: u32,
    pub runs: Runs,
    pub kind: Kind,
}

const S: Runs = Runs::Session;
const W: Runs = Runs::Window;

/// The built-in actions with a fixed name.
const FIXED: &[(&str, &str, Runs)] = &[
    // Transport.
    ("1007", "Transport: Play", S),
    ("1008", "Transport: Pause", S),
    ("1013", "Transport: Record", S),
    ("1016", "Transport: Stop", S),
    ("1068", "Transport: Toggle repeat", S),
    ("40042", "Transport: Go to start of project", S),
    ("40043", "Transport: Go to end of project", S),
    ("40044", "Transport: Play/stop", S),
    ("40073", "Transport: Play/pause", S),
    ("40328", "Transport: Play/stop (move edit cursor on stop)", S),
    ("40632", "Go to start of loop", S),
    ("40364", "Options: Toggle metronome", S),
    ("40620", "Toggle external timecode synchronization", S),
    ("_SCYTHE_TOGGLE_AUTO_PUNCH", "Record mode: Toggle time selection auto-punch", S),
    ("40252", "Record: Set record mode to normal", S),
    ("40076", "Record: Set record mode to time selection auto-punch", S),
    ("40253", "Record: Set record mode to selected item auto-punch", S),
    // Edit and file.
    ("40029", "Edit: Undo", S),
    ("40030", "Edit: Redo", S),
    ("40026", "File: Save project", S),
    ("40022", "File: Save project as...", W),
    ("40023", "File: New project", W),
    ("40025", "File: Open project", W),
    ("_SCYTHE_LOAD_DEMO", "File: Open demo song", W),
    ("40015", "File: Render project to disk...", W),
    ("_SCYTHE_RENDER_TIME_SELECTION", "File: Render time selection to WAV...", W),
    ("_SCYTHE_RENDER_STEMS", "File: Render stems (one WAV per track)...", W),
    ("41824", "File: Render project, using the most recent render settings", W),
    ("41823", "File: Add project to render queue, using the most recent render settings", W),
    ("41207", "Render all queued renders", W),
    ("40859", "New project tab", W),
    ("41929", "New project tab, ignore default template", W),
    ("_SCYTHE_OPEN_IN_NEW_TAB", "File: Open project in new tab...", W),
    ("40860", "Close current project tab", W),
    ("40861", "Project tabs: Switch to next project tab", W),
    ("40862", "Project tabs: Switch to previous project tab", W),
    ("40697", "Remove items/tracks/envelope points (depending on focus)", W),
    ("40698", "Edit: Copy items", W),
    ("40699", "Edit: Cut items", W),
    ("42398", "Item: Paste items/tracks", W),
    // Items.
    ("40018", "Insert/import media files...", W),
    ("40012", "Item: Split items at edit or play cursor (select right)", W),
    ("40757", "Item: Split items at edit cursor (no change selection)", W),
    ("40061", "Item: Split items at time selection or razor edit", W),
    ("40312", "Item: Remove selected area of items", W),
    ("40201", "Time selection: Remove contents of time selection (moving later items)", W),
    ("40200", "Time selection: Insert empty space at time selection (moving later items)", W),
    ("41295", "Item: Duplicate items", W),
    ("41051", "Item properties: Toggle take reverse", W),
    ("40362", "Item: Glue items, ignoring time selection", W),
    ("41588", "Item: Glue items, expanding to time selection if any", W),
    ("42432", "Item: Glue items within time selection", W),
    ("40209", "Item: Apply track/take FX to items", W),
    ("40125", "Take: Switch items to next take", W),
    ("40153", "Item: Open in built-in MIDI editor (set default behavior in preferences)", W),
    ("_SCYTHE_EXPORT_MIDI_ITEM", "Item: Export MIDI item to file...", W),
    ("40175", "Item properties: Toggle mute", W),
    ("41842", "Item: Add stretch marker at cursor", W),
    ("41844", "Item: Remove all stretch markers", W),
    // Markers and tempo.
    ("40172", "Markers: Go to previous marker/project start", S),
    ("40173", "Markers: Go to next marker/project end", S),
    ("40157", "Markers: Insert marker at current position", S),
    ("40613", "Markers: Delete marker near cursor", S),
    ("42330", "Tempo envelope: Insert tempo marker at edit cursor, without opening tempo edit dialog", W),
    ("_SCYTHE_REMOVE_TEMPO_MARKER", "Tempo envelope: Remove tempo marker nearest the cursor", W),
    ("_SCYTHE_TOGGLE_MARKER_ACTIONS", "Options: Toggle marker actions (run !markers during playback)", S),
    // Tracks.
    ("40001", "Track: Insert new track", W),
    ("6", "Track: Toggle mute for selected tracks", S),
    ("7", "Track: Toggle solo for selected tracks", S),
    ("9", "Track: Toggle record arm for selected tracks", S),
    ("40282", "Track: Invert track polarity/phase", W),
    ("41199", "Track: Toggle track solo defeat", W),
    ("40339", "Track: Unmute all tracks", S),
    ("40340", "Track: Unsolo all tracks", S),
    ("40285", "Track: Go to next track", S),
    ("40286", "Track: Go to previous track", S),
    ("43647", "Track: Move tracks up", W),
    ("43648", "Track: Move tracks down", W),
    ("_SCYTHE_INDENT_TRACK", "Track: Move into the folder above", W),
    ("_SCYTHE_OUTDENT_TRACK", "Track: Move out of its folder", W),
    ("41223", "Track: Freeze to stereo (render pre-fader, save/remove items and online FX)", W),
    ("41644", "Track: Unfreeze tracks (restore previously saved items and FX)", W),
    ("40392", "Track: Save tracks as track template...", W),
    ("_SCYTHE_INSERT_TRACK_TEMPLATE", "Track: Insert track from template...", W),
    ("_SCYTHE_SAVE_FX_CHAIN", "FX: Save track FX chain...", W),
    ("_SCYTHE_LOAD_FX_CHAIN", "FX: Add FX chain to track...", W),
    ("_SCYTHE_MASTER_MORE_CHANNELS", "Master: More channels", W),
    ("_SCYTHE_MASTER_FEWER_CHANNELS", "Master: Fewer channels", W),
    // View.
    ("40078", "View: Toggle mixer visible", W),
    ("_SCYTHE_TOGGLE_ENVELOPE_LANES", "View: Toggle envelope lanes", W),
    // Automation.
    ("40876", "Global automation override: No override (set automation modes per track)", S),
    ("40878", "Global automation override: All automation in trim/read mode", S),
    ("40879", "Global automation override: All automation in read mode", S),
    ("40880", "Global automation override: All automation in touch mode", S),
    ("40881", "Global automation override: All automation in latch mode", S),
    ("42022", "Global automation override: All automation in latch preview mode", S),
    ("40882", "Global automation override: All automation in write mode", S),
    ("40885", "Global automation override: Bypass all automation", S),
    ("40908", "Global automation override: Toggle bypass all automation", S),
    ("40400", "Automation: Set track automation mode to trim/read", S),
    ("40401", "Automation: Set track automation mode to read", S),
    ("40402", "Automation: Set track automation mode to touch", S),
    ("40403", "Automation: Set track automation mode to write", S),
    ("40404", "Automation: Set track automation mode to latch", S),
    ("42023", "Automation: Set track automation mode to latch preview", S),
    ("40088", "Automation: Set all tracks automation mode to trim/read", S),
    ("40086", "Automation: Set all tracks automation mode to read", S),
    ("40087", "Automation: Set all tracks automation mode to touch", S),
    ("40090", "Automation: Set all tracks automation mode to write", S),
    ("40266", "Automation: Set all tracks automation mode to latch", S),
    ("42024", "Automation: Set all tracks automation mode to latch preview", S),
    ("1157", "Options: Toggle snapping", S),
    ("_SCYTHE_NEXT_GRID", "Grid: Next grid size", W),
    ("1012", "View: Zoom in horizontal", W),
    ("1011", "View: Zoom out horizontal", W),
    ("40295", "View: Zoom out project", W),
    ("41325", "View: Increase selected track heights", W),
    ("41326", "View: Decrease selected track heights", W),
    ("40847", "Item: Open item inline editors", W),
    ("41887", "Item: Close item inline editors", W),
    ("40326", "View: Show region/marker manager window", W),
    ("50124", "Media explorer: Show/hide media explorer", W),
    ("41157", "View: Show project bay window", W),
    ("40098", "File: Clean current project directory...", W),
    ("42392", "Options: Show theme color controls", W),
    ("41888", "View: Show region render matrix window", W),
    ("40768", "View: Show track grouping matrix window", W),
    ("40377", "View: Show virtual MIDI keyboard", W),
    ("40619", "Show external timecode synchronization settings", W),
    ("40605", "Show action list", W),
    ("_SCYTHE_SEARCH_ACTIONS", "Actions: Search actions", W),
    ("40905", "Toolbars: Customize...", W),
    ("41356", "Options: Mouse modifier preferences...", W),
    ("41651", "Toolbar: Open/close main toolbar", W),
    // Editing: ripple, locking, grouping, takes, fades, grid and nudges (`scythe_core::edit`).
    ("1155", "Options: Cycle ripple editing mode", S),
    ("40309", "Set ripple editing off", S),
    ("40310", "Set ripple editing per-track", S),
    ("40311", "Set ripple editing all tracks", S),
    ("41990", "Toggle ripple editing per-track", S),
    ("41991", "Toggle ripple editing all tracks", S),
    ("1135", "Options: Toggle locking", S),
    ("40569", "Locking: Enable locking", S),
    ("40570", "Locking: Disable locking", S),
    ("40567", "Locking: Clear all lock modes", S),
    ("40568", "Locking: Set all lock modes", S),
    ("40571", "Locking: Set time selection locking mode", S),
    ("40572", "Locking: Clear time selection locking mode", S),
    ("40573", "Locking: Toggle time selection locking mode", S),
    ("40574", "Locking: Set full item locking mode", S),
    ("40575", "Locking: Clear full item locking mode", S),
    ("40576", "Locking: Toggle full item locking mode", S),
    ("40577", "Locking: Set left/right item locking mode", S),
    ("40578", "Locking: Clear left/right item locking mode", S),
    ("40579", "Locking: Toggle left/right item locking mode", S),
    ("40580", "Locking: Set up/down item locking mode", S),
    ("40581", "Locking: Clear up/down item locking mode", S),
    ("40582", "Locking: Toggle up/down item locking mode", S),
    ("40583", "Locking: Set track envelope locking mode", S),
    ("40584", "Locking: Clear track envelope locking mode", S),
    ("40585", "Locking: Toggle track envelope locking mode", S),
    ("40586", "Locking: Set region locking mode", S),
    ("40587", "Locking: Clear region locking mode", S),
    ("40588", "Locking: Toggle region locking mode", S),
    ("40589", "Locking: Set marker locking mode", S),
    ("40590", "Locking: Clear marker locking mode", S),
    ("40591", "Locking: Toggle marker locking mode", S),
    ("40592", "Locking: Set time signature marker locking mode", S),
    ("40593", "Locking: Clear time signature marker locking mode", S),
    ("40594", "Locking: Toggle time signature marker locking mode", S),
    ("40595", "Locking: Set item edges locking mode", S),
    ("40596", "Locking: Clear item edges locking mode", S),
    ("40597", "Locking: Toggle item edges locking mode", S),
    ("40598", "Locking: Set item fade/volume handles locking mode", S),
    ("40599", "Locking: Clear item fade/volume handles locking mode", S),
    ("40600", "Locking: Toggle item fade/volume handles locking mode", S),
    ("40627", "Locking: Set loop points locking mode", S),
    ("40628", "Locking: Clear loop points locking mode", S),
    ("40629", "Locking: Toggle loop points locking mode", S),
    ("1156", "Options: Toggle item grouping and track media/razor edit grouping", S),
    ("40041", "Options: Auto-crossfade media items when editing", S),
    ("41118", "Options: Enable auto-crossfades", S),
    ("41119", "Options: Disable auto-crossfades", S),
    ("40753", "Snapping: Disable snap", S),
    ("40754", "Snapping: Enable snap", S),
    ("40145", "Options: Toggle grid lines", S),
    ("40923", "Grid: Set measure grid", S),
    ("42304", "Grid: Toggle swing grid", S),
    ("40725", "Grid: Toggle measure grid", S),
    ("40783", "Grid: Adjust by 1/2", S),
    ("40786", "Grid: Adjust by 2", S),
    ("41047", "Grid: Set to 1/128", S),
    ("40774", "Grid: Set to 1/64", S),
    ("41212", "Grid: Set to 1/48 (1/32 triplet)", S),
    ("40775", "Grid: Set to 1/32", S),
    ("41213", "Grid: Set to 1/24 (1/16 triplet)", S),
    ("42001", "Grid: Set to 1/18", S),
    ("40776", "Grid: Set to 1/16", S),
    ("40777", "Grid: Set to 1/12 (1/8 triplet)", S),
    ("42002", "Grid: Set to 1/10 (1/8 quintuplet)", S),
    ("42003", "Grid: Set to 1/9", S),
    ("40778", "Grid: Set to 1/8", S),
    ("42004", "Grid: Set to 1/7 (1/4 septuplet)", S),
    ("41214", "Grid: Set to 1/6 (1/4 triplet)", S),
    ("42005", "Grid: Set to 1/5 (1/4 quintuplet)", S),
    ("40779", "Grid: Set to 1/4", S),
    ("42000", "Grid: Set to 1/3 (1/2 triplet)", S),
    ("40780", "Grid: Set to 1/2", S),
    ("42007", "Grid: Set to 2/3 (whole note triplet)", S),
    ("40781", "Grid: Set to 1", S),
    ("41210", "Grid: Set to 2", S),
    ("42006", "Grid: Set to 3", S),
    ("41211", "Grid: Set to 4", S),
    ("40006", "Item: Remove items", W),
    ("40687", "Item properties: Toggle lock", W),
    ("40688", "Item properties: Lock", W),
    ("40689", "Item properties: Unlock", W),
    ("40032", "Item grouping: Group items", W),
    ("40033", "Item grouping: Remove items from group", W),
    ("40034", "Item grouping: Select all items in groups", W),
    ("40224", "Take: Explode takes of items across tracks", W),
    ("40642", "Take: Explode takes of items in place", W),
    ("40643", "Take: Explode takes of items in order", W),
    ("40438", "Take: Implode items across tracks into takes", W),
    ("40543", "Take: Implode items on same track into takes", W),
    ("40131", "Take: Crop to active take in items", W),
    ("40129", "Take: Delete active take from items", W),
    ("40126", "Take: Switch items to previous take", W),
    ("41059", "Item: Crossfade any overlapping items", W),
    ("40916", "Item: Crossfade items within time selection", W),
    ("41514", "Item: Set fade-in shape to type 1 (linear)", W),
    ("41515", "Item: Set fade-in shape to type 2 (slight convex)", W),
    ("41516", "Item: Set fade-in shape to type 3 (slight concave)", W),
    ("41517", "Item: Set fade-in shape to type 4 (sharp convex)", W),
    ("41518", "Item: Set fade-in shape to type 5 (sharp concave)", W),
    ("41519", "Item: Set fade-in shape to type 6 (slight S-curve)", W),
    ("41836", "Item: Set fade-in shape to type 7 (sharp S-curve)", W),
    ("41520", "Item: Cycle through fade-in shapes", W),
    ("41521", "Item: Set fade-out shape to type 1 (linear)", W),
    ("41522", "Item: Set fade-out shape to type 2 (slight convex)", W),
    ("41523", "Item: Set fade-out shape to type 3 (slight concave)", W),
    ("41524", "Item: Set fade-out shape to type 4 (sharp convex)", W),
    ("41525", "Item: Set fade-out shape to type 5 (sharp concave)", W),
    ("41526", "Item: Set fade-out shape to type 6 (slight S-curve)", W),
    ("41837", "Item: Set fade-out shape to type 7 (sharp S-curve)", W),
    ("41527", "Item: Cycle through fade-out shapes", W),
    ("41528", "Item: Set crossfade shape to type 1 (linear, equal gain)", W),
    ("41529", "Item: Set crossfade shape to type 2 (slight convex, equal power)", W),
    ("41530", "Item: Set crossfade shape to type 3 (slight concave)", W),
    ("41531", "Item: Set crossfade shape to type 4 (sharp convex)", W),
    ("41532", "Item: Set crossfade shape to type 5 (sharp concave)", W),
    ("41533", "Item: Set crossfade shape to type 6 (slight S-curve)", W),
    ("41838", "Item: Set crossfade shape to type 7 (sharp S-curve)", W),
    ("41534", "Item: Cycle through crossfade shapes", W),
    ("40119", "Item edit: Move items/envelope points right", W),
    ("40120", "Item edit: Move items/envelope points left", W),
    ("40793", "Item edit: Move items/envelope points left by grid size", W),
    ("40794", "Item edit: Move items/envelope points right by grid size", W),
    ("41249", "Item edit: Nudge right by last nudge dialog settings", W),
    ("41250", "Item edit: Nudge left by last nudge dialog settings", W),
    ("40225", "Item edit: Grow left edge of items", W),
    ("40226", "Item edit: Shrink left edge of items", W),
    ("40227", "Item edit: Shrink right edge of items", W),
    ("40228", "Item edit: Grow right edge of items", W),
    ("40375", "Item navigation: Move cursor to next transient in items", W),
    ("40376", "Item navigation: Move cursor to previous transient in items", W),
    ("42399", "Razor edit: Move areas forwards without contents", W),
    ("42400", "Razor edit: Move areas backwards without contents", W),
    ("42402", "Razor edit: Move areas up without contents", W),
    ("42403", "Razor edit: Move areas down without contents", W),
    ("42406", "Razor edit: Clear all areas", W),
    ("40760", "Edit: Dynamic split items...", W),
    ("41827", "View: Show crossfade editor window", W),
    ("41226", "Item edit: Nudge/Set...", W),
    ("40071", "Options: Show snap/grid settings", W),
    ("40277", "Options: Show lock settings", W),
    // Scripts.
    ("41060", "ReaScript: Run ReaScript (EEL2 or Lua)...", W),
    ("41061", "ReaScript: Run last ReaScript (EEL2 or Lua)", W),
    ("41898", "ReaScript: Close all running ReaScripts", W),
    // Live.
    ("_SCYTHE_SETLIST_PLAY", "Setlist: play the current song", S),
    ("_SCYTHE_SETLIST_STOP", "Setlist: stop", S),
    ("_SCYTHE_SETLIST_PLAY_STOP", "Setlist: play the current song, or stop when playing", S),
    ("_SCYTHE_SETLIST_NEXT", "Setlist: next song (cued when stopped, played when playing)", S),
    ("_SCYTHE_SETLIST_PREVIOUS", "Setlist: previous song", S),
    ("_SCYTHE_SETLIST_RESTART", "Setlist: play the current song again from its start", S),
    ("_SCYTHE_TOGGLE_SETLIST", "View: Toggle setlist", W),
    ("_SCYTHE_TOGGLE_MONITOR_MIXES", "View: Toggle monitor mixes (in-ear)", W),
    ("_SCYTHE_NEW_MONITOR_MIX", "Monitor: New monitor mix for a musician...", W),
    ("_SCYTHE_CREATE_CUE_BUS", "Monitor: Create cue bus from track selection...", W),
    ("_SCYTHE_NAM_PANEL", "NAM: Open the Neural Amp Modeler panel of the selected track", W),
    ("_SCYTHE_NAM_PREVIOUS", "NAM: Previous model", W),
    ("_SCYTHE_NAM_NEXT", "NAM: Next model", W),
    ("_SCYTHE_NAM_SNAPSHOT_A", "NAM: Switch to snapshot A", W),
    ("_SCYTHE_NAM_SNAPSHOT_B", "NAM: Switch to snapshot B", W),
    ("_SCYTHE_NAM_SNAPSHOT_C", "NAM: Switch to snapshot C", W),
    ("_SCYTHE_NAM_SNAPSHOT_D", "NAM: Switch to snapshot D", W),
    ("65535", "No-op (no action)", S),
];

/// REAPER's first "Screenset: Load window set #01" and "Save window set #01".
pub const LOAD_SCREENSET: u32 = 40454;
pub const SAVE_SCREENSET: u32 = 40474;
pub const SCREENSETS: u32 = 10;
/// The first id Scythe gives its own named commands.
const FIRST_SCYTHE_ID: u32 = 63000;

static BUILTIN: LazyLock<Vec<ActionInfo>> = LazyLock::new(|| {
    let info = |command: String, name: String, runs| ActionInfo {
        command,
        name,
        section: SECTION_MAIN,
        runs,
        kind: Kind::Builtin,
    };
    let mut out: Vec<ActionInfo> = FIXED.iter().map(|(c, n, r)| info((*c).into(), (*n).into(), *r)).collect();
    for n in 1..=9u32 {
        out.push(info((40160 + n).to_string(), format!("Markers: Go to marker {n:02}"), S));
    }
    for n in 1..=scythe_core::menus::FLOATING_TOOLBARS {
        let id = scythe_core::menus::open_toolbar_command(n).unwrap_or_default();
        out.push(info(id.to_string(), format!("Toolbar: Open/close toolbar {n}"), W));
    }
    for n in 1..=SCREENSETS {
        out.push(info((LOAD_SCREENSET + n - 1).to_string(), format!("Screenset: Load window set #{n:02}"), W));
        out.push(info((SAVE_SCREENSET + n - 1).to_string(), format!("Screenset: Save window set #{n:02}"), W));
    }
    let midi = scythe_core::midi::actions::ids().into_iter().filter_map(|id| {
        let name = scythe_core::midi::actions::name(id)?;
        Some(ActionInfo { section: keymap::SECTION_MIDI_EDITOR, ..info(id.to_string(), name, W) })
    });
    out.extend(midi);
    out.extend(crate::explorer::ACTIONS.iter().map(|(id, name)| ActionInfo {
        section: keymap::SECTION_MEDIA_EXPLORER,
        ..info(id.to_string(), (*name).into(), S)
    }));
    out
});

/// The built-in actions.
pub fn builtin() -> &'static [ActionInfo] {
    &BUILTIN
}

fn find_builtin(command: &str) -> Option<&'static ActionInfo> {
    BUILTIN.iter().find(|a| a.section == SECTION_MAIN && a.command.eq_ignore_ascii_case(command))
}

/// `40044` or `_ID` as given; a named command without its `_` gets one.
pub fn normalize(command: &str) -> String {
    let c = command.trim();
    if c.is_empty() || c.starts_with('_') || c.parse::<u32>().is_ok() { c.to_owned() } else { format!("_{c}") }
}

impl Session {
    /// Every action: built-in, custom, scripts.
    pub fn action_list(&self) -> Vec<ActionInfo> {
        let mut out: Vec<ActionInfo> = BUILTIN.to_vec();
        for line in &self.keymap.lines {
            match line {
                Line::Act(a) => out.push(ActionInfo {
                    command: a.named_command(),
                    name: a.name.clone(),
                    section: a.section,
                    runs: if self.custom_needs_window(&a.commands, 0) { W } else { S },
                    kind: Kind::Custom,
                }),
                Line::Scr(s) => out.push(ActionInfo {
                    command: s.named_command(),
                    name: script_name(&s.name),
                    section: s.section,
                    runs: W,
                    kind: Kind::Script,
                }),
                _ => {}
            }
        }
        out
    }

    /// The action's name, as the action list shows it.
    pub fn action_name(&self, command: &str) -> Option<String> {
        let c = normalize(command);
        if let Some(a) = find_builtin(&c) {
            return Some(a.name.clone());
        }
        match self.keymap.named(&c, SECTION_MAIN)? {
            Line::Act(a) => Some(a.name.clone()),
            Line::Scr(s) => Some(script_name(&s.name)),
            _ => None,
        }
    }

    fn custom_needs_window(&self, members: &[String], depth: usize) -> bool {
        depth < 8
            && members.iter().any(|m| {
                let m = normalize(m);
                match self.keymap.named(&m, SECTION_MAIN) {
                    Some(Line::Act(a)) => self.custom_needs_window(&a.commands, depth + 1),
                    Some(_) => true,
                    None => find_builtin(&m).is_some_and(|a| a.runs == W),
                }
            })
    }

    /// The command id REAPER's API uses for `command`: the number itself, or for a named
    /// command the id it has this session (custom actions and scripts from 53000 in the order
    /// `reaper-kb.ini` defines them, as REAPER numbers them). 0 when there is no such action.
    pub fn command_id(&self, command: &str) -> u32 {
        let c = normalize(command);
        if let Ok(id) = c.parse::<u32>() {
            return id;
        }
        if let Some(i) = self.keymap.named_commands().iter().position(|(_, n)| n.eq_ignore_ascii_case(&c)) {
            return keymap::FIRST_NAMED_ID + i as u32;
        }
        BUILTIN
            .iter()
            .filter(|a| a.command.starts_with('_'))
            .position(|a| a.command.eq_ignore_ascii_case(&c))
            .map_or(0, |i| FIRST_SCYTHE_ID + i as u32)
    }

    /// The command behind a command id: the number, or the named command (with its `_`).
    pub fn command_for_id(&self, id: u32) -> Option<String> {
        if id >= FIRST_SCYTHE_ID {
            return BUILTIN
                .iter()
                .filter(|a| a.command.starts_with('_'))
                .nth((id - FIRST_SCYTHE_ID) as usize)
                .map(|a| a.command.clone());
        }
        if id >= keymap::FIRST_NAMED_ID {
            return self.keymap.named_commands().get((id - keymap::FIRST_NAMED_ID) as usize).map(|(_, n)| n.clone());
        }
        Some(id.to_string())
    }

    /// `GetToggleCommandState`: 1 on, 0 off, -1 for an action without a toggle state.
    pub fn toggle_state(&self, command: &str) -> i32 {
        self.toggle_state_at(&normalize(command), 0)
    }

    fn toggle_state_at(&self, c: &str, depth: usize) -> i32 {
        if let Some(Line::Act(a)) = self.keymap.named(c, SECTION_MAIN) {
            if depth > 8 {
                return -1;
            }
            return keymap::combined_toggle(a.commands.iter().map(|m| self.toggle_state_at(&normalize(m), depth + 1)));
        }
        if let Some(&on) = self.host_toggles.get(c) {
            return on as i32;
        }
        let sel = self.selected_track.and_then(|t| self.project.track(t));
        let state = match c {
            "1068" => self.project.loop_enabled,
            "40364" => self.metronome(),
            "1007" => self.is_playing(),
            "1013" => self.is_recording(),
            "40620" => self.project.timecode_sync.enabled(),
            "40252" | "40076" | "40253" => {
                use scythe_core::recording::ProjectRecordMode;
                let want = match c {
                    "40252" => ProjectRecordMode::Normal,
                    "40076" => ProjectRecordMode::TimeSelectionPunch,
                    _ => ProjectRecordMode::ItemPunch,
                };
                ProjectRecordMode::of(&self.project) == want
            }
            "6" => sel.is_some_and(|t| t.muted),
            "7" => sel.is_some_and(|t| t.solo),
            "9" => sel.is_some_and(|t| t.armed),
            "_SCYTHE_TOGGLE_AUTO_PUNCH" => {
                scythe_core::recording::ProjectRecordMode::of(&self.project)
                    == scythe_core::recording::ProjectRecordMode::TimeSelectionPunch
            }
            "_SCYTHE_TOGGLE_MARKER_ACTIONS" => self.engine.marker_actions(),
            "40876" | "40878" | "40879" | "40880" | "40881" | "42022" | "40882" | "40885" => {
                override_for(c).is_some_and(|o| o == self.project.global_automation)
            }
            "40908" => self.project.global_automation == Some(GlobalAutomation::Bypass),
            _ => match c.parse::<u32>().ok().and_then(|id| edit_actions::toggle_state(&self.project, id)) {
                Some(on) => on,
                None => return -1,
            },
        };
        state as i32
    }

    /// The window's actions waiting for it to run them, as commands.
    pub fn take_host_commands(&mut self) -> Vec<String> {
        std::mem::take(&mut self.host_commands)
    }
}

/// A script action's name as REAPER's action list shows it ("Script: x.lua" for the
/// "Custom: x.lua" the file has).
fn script_name(name: &str) -> String {
    match name.strip_prefix("Custom: ") {
        Some(rest) => format!("Script: {rest}"),
        None => name.to_owned(),
    }
}

/// The actions of the main section a surface, a learned control or the control API can run.
pub fn list(session: &Session) -> Vec<ActionInfo> {
    session.action_list().into_iter().filter(|a| a.section == SECTION_MAIN).collect()
}

/// Runs `action`: a REAPER command id (`1007`), a named command (`_SCYTHE_SETLIST_NEXT`), a
/// custom action (`_<id>`) or a script's command (`_RS…`).
pub fn run(session: &mut Session, action: &str) -> Result<(), ApiError> {
    run_at(session, action, 0)
}

fn run_at(session: &mut Session, action: &str, depth: usize) -> Result<(), ApiError> {
    let c = normalize(action);
    if let Some(line) = session.keymap.named(&c, SECTION_MAIN).cloned() {
        return match line {
            Line::Act(a) if session.host_actions && session.custom_needs_window(&a.commands, 0) => {
                session.host_commands.push(c);
                Ok(())
            }
            Line::Act(a) => {
                if depth > 8 {
                    return Err(ApiError::invalid(format!("custom action \"{}\" nests too deeply", a.name)));
                }
                let mark = session.history.depth();
                for m in &a.commands {
                    if let Err(e) = run_at(session, m, depth + 1) {
                        session.notify(format!("{}: {e}", a.name));
                    }
                }
                if a.consolidate_undo() {
                    session.history.consolidate(mark, &a.name);
                }
                Ok(())
            }
            _ if session.host_actions => {
                session.host_commands.push(c);
                Ok(())
            }
            _ => Err(ApiError::invalid("scripts run only in the desktop app")),
        };
    }
    if let Some(result) = run_here(session, &c) {
        return result;
    }
    match find_builtin(&c) {
        Some(_) if session.host_actions => {
            session.host_commands.push(c);
            Ok(())
        }
        Some(a) => Err(ApiError::invalid(format!("\"{}\" needs the desktop app", a.name))),
        None if c.parse::<u32>().is_ok() => Err(ApiError::not_found(format!("action {c} is not available in Scythe"))),
        None => Err(ApiError::not_found(format!("no action \"{action}\""))),
    }
}

fn exec(session: &mut Session, command: Value) -> Result<(), ApiError> {
    api::execute_json(session, command).map(|_| ())
}

/// Runs an action the session has; `None` for one it does not.
pub(crate) fn run_here(session: &mut Session, c: &str) -> Option<Result<(), ApiError>> {
    let cmd = |c: &str| json!({ "cmd": c });
    let result = match c.to_ascii_uppercase().as_str() {
        "_SCYTHE_SETLIST_PLAY" => exec(session, cmd("setlist.play")),
        "_SCYTHE_SETLIST_STOP" => exec(session, cmd("setlist.stop")),
        "_SCYTHE_SETLIST_PLAY_STOP" => {
            let c = if session.is_playing() { "setlist.stop" } else { "setlist.play" };
            exec(session, cmd(c))
        }
        "_SCYTHE_SETLIST_NEXT" => exec(session, cmd("setlist.next")),
        "_SCYTHE_SETLIST_PREVIOUS" => exec(session, cmd("setlist.previous")),
        "_SCYTHE_SETLIST_RESTART" => {
            if session.is_playing()
                && let Err(e) = exec(session, cmd("setlist.stop"))
            {
                return Some(Err(e));
            }
            exec(session, cmd("setlist.play"))
        }
        "_SCYTHE_TOGGLE_AUTO_PUNCH" => {
            use scythe_core::recording::ProjectRecordMode;
            let next = match ProjectRecordMode::of(&session.project) {
                ProjectRecordMode::TimeSelectionPunch => ProjectRecordMode::Normal,
                _ => ProjectRecordMode::TimeSelectionPunch,
            };
            session.set_record_mode(next);
            Ok(())
        }
        "_SCYTHE_TOGGLE_MARKER_ACTIONS" => {
            let on = !session.engine.marker_actions();
            session.engine.set_marker_actions(on);
            session.notify(if on { "Marker actions on" } else { "Marker actions off" });
            Ok(())
        }
        "40908" => {
            let on = session.project.global_automation == Some(GlobalAutomation::Bypass);
            session.edit("Global automation override", |p| {
                p.global_automation = (!on).then_some(GlobalAutomation::Bypass);
            });
            Ok(())
        }
        id if override_for(id).is_some() => {
            let over = override_for(id).flatten();
            session.edit("Global automation override", |p| p.global_automation = over);
            Ok(())
        }
        id if track_mode_for(id).is_some() => {
            let (mode, all) = track_mode_for(id).unwrap_or_default();
            let sel = session.selected_track;
            session.edit("Automation mode", |p| {
                for t in p.tracks.iter_mut().filter(|t| all || Some(t.id) == sel) {
                    t.auto_mode = mode;
                }
            });
            Ok(())
        }
        _ => return c.parse::<u32>().ok().and_then(|id| run_id(session, id)),
    };
    Some(result)
}

fn run_id(session: &mut Session, id: u32) -> Option<Result<(), ApiError>> {
    let cmd = |c: &str| json!({ "cmd": c });
    let playing = session.is_playing() || session.is_recording();
    let selected = session.selected_track;
    Some(match id {
        ids::PLAY => exec(session, cmd("transport.play")),
        ids::PAUSE => exec(session, cmd("transport.pause")),
        ids::RECORD => exec(session, cmd("transport.record")),
        ids::STOP => exec(session, cmd("transport.stop")),
        ids::PLAY_STOP | ids::PLAY_STOP_MOVE_CURSOR => {
            if playing {
                let position = session.position();
                let stopped = exec(session, cmd("transport.stop"));
                if id == ids::PLAY_STOP_MOVE_CURSOR && stopped.is_ok() {
                    exec(session, json!({ "cmd": "transport.seek", "position": position }))
                } else {
                    stopped
                }
            } else {
                exec(session, cmd("transport.play"))
            }
        }
        ids::PLAY_PAUSE => exec(session, cmd(if playing { "transport.pause" } else { "transport.play" })),
        ids::TOGGLE_REPEAT => {
            let on = !session.project.loop_enabled;
            exec(session, json!({ "cmd": "transport.loop", "enabled": on }))
        }
        ids::GO_TO_START => exec(session, json!({ "cmd": "transport.seek", "position": 0.0 })),
        ids::GO_TO_END => {
            let end = session.project.length();
            exec(session, json!({ "cmd": "transport.seek", "position": end }))
        }
        ids::GO_TO_LOOP_START => match session.project.time_selection {
            Some(range) => exec(session, json!({ "cmd": "transport.seek", "position": range.start })),
            None => Ok(()),
        },
        40029 => exec(session, cmd("project.undo")),
        40030 => exec(session, cmd("project.redo")),
        40026 => exec(session, cmd("project.save")),
        40172 | 40173 => {
            let here = session.position();
            let target = marker_step(session, here, id == 40173);
            exec(session, json!({ "cmd": "transport.seek", "position": target }))
        }
        40157 => {
            let at = session.position();
            session.edit("Insert marker", |p| {
                p.add_marker(at, "");
            });
            Ok(())
        }
        40613 => {
            let at = session.position();
            let nearest = session
                .project
                .markers
                .iter()
                .enumerate()
                .min_by(|a, b| (a.1.position - at).abs().total_cmp(&(b.1.position - at).abs()))
                .map(|(i, _)| i);
            if let Some(i) = nearest {
                session.edit("Delete marker", |p| {
                    p.markers.remove(i);
                });
            }
            Ok(())
        }
        40161..=40169 => {
            let number = id - 40160;
            exec(session, json!({ "cmd": "marker.goto", "marker": number, "region": false }))
        }
        40339 | 40340 => {
            let label = if id == 40339 { "Unmute all tracks" } else { "Unsolo all tracks" };
            session.edit(label, |p| {
                for t in &mut p.tracks {
                    if id == 40339 {
                        t.muted = false;
                    } else {
                        t.solo = false;
                    }
                }
            });
            Ok(())
        }
        6 | 7 | 9 => {
            let Some(t) = selected else { return Some(Ok(())) };
            let label = match id {
                6 => "Toggle mute",
                7 => "Toggle solo",
                _ => "Toggle record arm",
            };
            session.edit(label, |p| {
                if let Some(t) = p.track_mut(t) {
                    match id {
                        6 => t.muted ^= true,
                        7 => t.solo ^= true,
                        _ => t.armed ^= true,
                    }
                }
            });
            Ok(())
        }
        40285 | 40286 => {
            let tracks: Vec<_> = session.project.tracks.iter().map(|t| t.id).collect();
            if !tracks.is_empty() {
                let at = selected.and_then(|s| tracks.iter().position(|t| *t == s));
                let next = match (id, at) {
                    (40286, Some(i)) => i.saturating_sub(1),
                    (_, Some(i)) => (i + 1).min(tracks.len() - 1),
                    (_, None) => 0,
                };
                session.selected_track = Some(tracks[next]);
                session.sync();
            }
            Ok(())
        }
        40364 => {
            session.toggle_metronome();
            Ok(())
        }
        40252 | 40076 | 40253 => {
            use scythe_core::recording::ProjectRecordMode;
            session.set_record_mode(match id {
                40252 => ProjectRecordMode::Normal,
                40076 => ProjectRecordMode::TimeSelectionPunch,
                _ => ProjectRecordMode::ItemPunch,
            });
            Ok(())
        }
        40620 => {
            let on = !session.project.timecode_sync.enabled();
            session.edit("Toggle external timecode synchronization", |p| p.timecode_sync.set_enabled(on));
            Ok(())
        }
        65535 => Ok(()),
        _ if edit_actions::SESSION_IDS.contains(&id) => {
            let name = session.action_name(&id.to_string()).unwrap_or_default();
            let cursor = session.project.cursor;
            let none = edit_actions::no_transients;
            let mut ctx = edit_actions::EditContext::new(Vec::new(), cursor, &none);
            session.edit(&name, |p| {
                edit_actions::run(p, &mut ctx, id);
            });
            Ok(())
        }
        _ => return None,
    })
}

/// The marker (or region start) before or after `at`, else the project start or end, as
/// REAPER's previous/next marker actions go.
pub fn marker_step(session: &Session, at: f64, forward: bool) -> f64 {
    const EPS: f64 = 1e-6;
    let points = session.project.markers.iter().map(|m| m.position);
    if forward {
        points
            .filter(|p| *p > at + EPS)
            .fold(None, |a: Option<f64>, p| Some(a.map_or(p, |a| a.min(p))))
            .unwrap_or_else(|| session.project.length())
    } else {
        points.filter(|p| *p < at - EPS).fold(None, |a: Option<f64>, p| Some(a.map_or(p, |a| a.max(p)))).unwrap_or(0.0)
    }
}

/// The global override a REAPER override action sets (`Some(None)` for "no override").
fn override_for(c: &str) -> Option<Option<GlobalAutomation>> {
    let mode = |m| Some(Some(GlobalAutomation::Mode(m)));
    match c {
        "40876" => Some(None),
        "40878" => mode(AutoMode::TrimRead),
        "40879" => mode(AutoMode::Read),
        "40880" => mode(AutoMode::Touch),
        "40881" => mode(AutoMode::Latch),
        "42022" => mode(AutoMode::LatchPreview),
        "40882" => mode(AutoMode::Write),
        "40885" => Some(Some(GlobalAutomation::Bypass)),
        _ => None,
    }
}

/// The mode a REAPER automation mode action sets, and whether on every track (else the
/// selected one).
fn track_mode_for(c: &str) -> Option<(AutoMode, bool)> {
    Some(match c {
        "40400" => (AutoMode::TrimRead, false),
        "40401" => (AutoMode::Read, false),
        "40402" => (AutoMode::Touch, false),
        "40403" => (AutoMode::Write, false),
        "40404" => (AutoMode::Latch, false),
        "42023" => (AutoMode::LatchPreview, false),
        "40088" => (AutoMode::TrimRead, true),
        "40086" => (AutoMode::Read, true),
        "40087" => (AutoMode::Touch, true),
        "40090" => (AutoMode::Write, true),
        "40266" => (AutoMode::Latch, true),
        "42024" => (AutoMode::LatchPreview, true),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_commands_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for a in builtin() {
            assert!(seen.insert((a.section, a.command.to_ascii_uppercase())), "{} twice", a.command);
        }
    }

    /// REAPER 7.73's names for the command ids Scythe registers, as `kbd_enumerateActions`
    /// listed them.
    #[test]
    fn reaper_ids_carry_reapers_names() {
        let measured = include_str!("../testdata/reaper-action-names.txt");
        for line in measured.lines() {
            let (id, name) = line.split_once('\t').unwrap();
            let ours = find_builtin(id).unwrap_or_else(|| panic!("{id} not registered"));
            assert_eq!(ours.name, name, "{id}");
        }
        let numeric =
            builtin().iter().filter(|a| a.section == SECTION_MAIN && a.command.parse::<u32>().is_ok()).count();
        assert_eq!(measured.lines().count(), numeric);
    }
}
