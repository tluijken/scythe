//! ReaScript in Lua 5.4, EEL2 and Python: a subset of REAPER's API over a copy of the project,
//! so simple REAPER scripts run unchanged. The script's changes come back as one project to
//! apply as a single undo step. A script that calls `defer` keeps running: the app calls its
//! deferred code once per frame, each time over a fresh copy of the project.
//!
//! The API is implemented once, in `tracks`, `items` and `project`, over language-neutral
//! values; each language passes arguments and returns results its own way, as REAPER's
//! signatures for it say (`API`).

mod eel;
mod items;
mod lua;
mod project;
mod python;
mod tracks;

pub(crate) use tracks::fx_name_matches;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use scythe_core::{EnvelopeKind, FxSlot, ItemId, Project, Source, Track, TrackId};
use scythe_engine::eel::Machine;
use scythe_engine::eel::gfx::{Bitmap, Input};

/// What the app does for a script: dialogs, plugins and media it knows about.
pub trait Services {
    /// `ShowMessageBox`: the button pressed, as REAPER numbers them (1 OK, 2 Cancel, 6 Yes…).
    fn message_box(&self, text: &str, title: &str, kind: i64) -> i64;
    /// `GetUserInputs`: the values entered, or `None` when the user cancelled.
    fn user_inputs(&self, title: &str, captions: &[String], values: &[String]) -> Option<Vec<String>>;
    /// A new slot for the installed plugin named `name` (`ReaEQ`, `VST3: Surge XT`…).
    fn plugin(&self, name: &str) -> Option<FxSlot>;
    /// The length of a media file in seconds.
    fn media_length(&self, path: &Path) -> Option<f64>;
}

/// A parameter of a running effect.
#[derive(Debug, Clone, PartialEq)]
pub struct FxParam {
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub value: f64,
    /// The value as the effect shows it.
    pub text: String,
}

/// A running effect whose parameters a script reads and sets.
pub trait LiveFx {
    fn params(&self) -> Vec<FxParam>;
    fn set_param(&self, index: usize, value: f64);
    fn is_instrument(&self) -> bool;
}

/// What a script sees besides the project, and what it asks of the app.
#[derive(Clone, Default)]
pub struct ScriptContext {
    pub selected_tracks: Vec<TrackId>,
    pub selected_items: Vec<ItemId>,
    pub cursor: f64,
    pub playing: bool,
    /// The playhead while playing, else the cursor.
    pub play_position: f64,
    /// The running effects of each track, in slot order (`None` for one not running).
    pub fx: HashMap<TrackId, Vec<Option<Rc<dyn LiveFx>>>>,
    pub services: Option<Rc<dyn Services>>,
    /// What `GetResourcePath` names; presets are looked up under it.
    pub resource_dir: Option<PathBuf>,
    /// Where `SetExtState` keeps what persists, like REAPER's `reaper-extstate.ini`.
    pub ext_state_file: Option<PathBuf>,
    /// The undo and redo steps' names, for `Undo_CanUndo2` and `Undo_CanRedo2`.
    pub undo: Option<String>,
    pub redo: Option<String>,
    pub dirty: bool,
    /// The mouse and keys over the script's graphics window, and whether the user closed it.
    pub gfx: Input,
    pub gfx_closed: bool,
}

#[derive(Debug, Default)]
pub struct ScriptOutcome {
    pub project: Option<Project>,
    pub console: String,
    /// The undo description given to `Undo_EndBlock`.
    pub undo: Option<String>,
    pub cursor: Option<f64>,
    pub play: Option<bool>,
    /// The track and item selection, when the script changed it.
    pub selection: Option<(Vec<TrackId>, Vec<ItemId>)>,
    /// REAPER command ids passed to `Main_OnCommand` for the app to run once the project is
    /// applied (those the script could not run itself).
    pub commands: Vec<i64>,
    /// An effect parameter was set.
    pub fx_touched: bool,
    pub dirty: bool,
}

/// A script's graphics window and what it drew last.
#[derive(Debug, Clone)]
pub struct GfxWindow {
    pub title: String,
    pub w: u32,
    pub h: u32,
    pub open: bool,
    pub frame: Option<Bitmap>,
}

struct State {
    project: Project,
    ctx: ScriptContext,
    out: ScriptOutcome,
    changed: bool,
    sel_tracks: Vec<TrackId>,
    sel_items: Vec<ItemId>,
    sel_changed: bool,
    /// `PCM_source*` handles: sources a script made or asked for, by handle.
    sources: Vec<Source>,
}

impl State {
    fn new(project: &Project, ctx: ScriptContext) -> Self {
        let mut s = State {
            project: project.clone(),
            ctx: ScriptContext::default(),
            out: ScriptOutcome::default(),
            changed: false,
            sel_tracks: Vec::new(),
            sel_items: Vec::new(),
            sel_changed: false,
            sources: Vec::new(),
        };
        s.reset(project, ctx);
        s
    }

    fn reset(&mut self, project: &Project, ctx: ScriptContext) {
        self.project = project.clone();
        self.sel_tracks = ctx.selected_tracks.clone();
        self.sel_items = ctx.selected_items.clone();
        self.sel_changed = false;
        self.changed = false;
        self.ctx = ctx;
    }

    /// The edit cursor as the script left it.
    fn cursor(&self) -> f64 {
        self.out.cursor.unwrap_or(self.ctx.cursor)
    }

    fn playing(&self) -> bool {
        self.out.play.unwrap_or(self.ctx.playing)
    }
}

type Shared = Rc<RefCell<State>>;

/// A running interpreter: the part of a script that differs by language.
trait Engine {
    /// Whether code is deferred to the next frame.
    fn pending(&self) -> bool;
    /// Runs the code deferred in the last frame.
    fn run_deferred(&mut self) -> Result<(), String>;
    /// Runs the code registered with `atexit`.
    fn run_at_exit(&mut self) -> Result<(), String>;
    /// The script's graphics window, if it opened one, with the frame drawn since the last call.
    fn gfx_window(&mut self) -> Option<GfxWindow> {
        None
    }
}

/// The graphics window of a Lua or EEL script, which an EEL machine's `gfx_*` draw on (Lua's
/// `gfx.*` included).
#[derive(Default)]
struct ScriptGfx {
    window: Option<GfxWindow>,
}

impl ScriptGfx {
    /// Before a run: hands the open window's mouse and keys to the machine, or closes it.
    fn begin(&mut self, m: &mut Machine, ctx: &ScriptContext) {
        let Some(w) = m.gfx_window().filter(|w| w.open) else { return };
        if ctx.gfx_closed {
            m.close_gfx_window();
            return;
        }
        let mut input = ctx.gfx.clone();
        if input.w <= 0.0 || input.h <= 0.0 {
            (input.w, input.h) = (w.w as f64, w.h as f64);
        }
        m.gfx_begin(&input);
    }

    /// The window, taking its frame so it is uploaded once.
    fn take(&mut self) -> Option<GfxWindow> {
        let w = self.window.as_mut()?;
        let frame = w.frame.take();
        Some(GfxWindow { frame, ..w.clone() })
    }

    /// After a run: draws what it queued, keeping the last frame until the app takes it.
    fn end(&mut self, m: &mut Machine) {
        let Some(w) = m.gfx_window() else { return };
        let frame = if w.open { m.gfx_end().render() } else { None };
        let kept = self.window.as_mut().and_then(|x| x.frame.take());
        self.window = Some(GfxWindow { title: w.title, w: w.w, h: w.h, open: w.open, frame: frame.or(kept) });
    }
}

/// Runs `source` and returns what it did, and the script itself when it deferred work to
/// later frames. The language follows `name`'s extension (`.eel`, `.py`, else Lua). Errors
/// carry the interpreter's message, with the line.
pub fn run(
    source: &str,
    name: &str,
    project: &Project,
    ctx: ScriptContext,
) -> Result<(ScriptOutcome, Option<RunningScript>), String> {
    let state: Shared = Rc::new(RefCell::new(State::new(project, ctx)));
    let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let engine: Result<Box<dyn Engine>, String> = match ext.as_str() {
        "eel" => eel::start(source, name, &state).map(|e| Box::new(e) as _),
        "py" => python::start(source, name, &state).map(|e| Box::new(e) as _),
        _ => lua::start(source, name, &state).map(|e| Box::new(e) as _),
    };
    let engine = match engine {
        Ok(e) => e,
        Err(e) => return Err(format!("{}{e}", std::mem::take(&mut state.borrow_mut().out.console))),
    };
    let mut script = RunningScript { engine, state };
    if script.is_running() {
        let out = script.outcome();
        return Ok((out, Some(script)));
    }
    let out = script.finish(None)?;
    Ok((out, None))
}

/// A script waiting for its deferred code to run.
pub struct RunningScript {
    engine: Box<dyn Engine>,
    state: Shared,
}

impl std::fmt::Debug for RunningScript {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningScript").field("pending", &self.engine.pending()).finish()
    }
}

impl RunningScript {
    pub fn is_running(&self) -> bool {
        self.engine.pending()
    }

    /// Runs the code deferred in the last frame over `project`.
    pub fn tick(&mut self, project: &Project, ctx: ScriptContext) -> Result<ScriptOutcome, String> {
        self.state.borrow_mut().reset(project, ctx);
        let result = self.engine.run_deferred();
        self.result(result)
    }

    /// Ends the script, running what it registered with `atexit`.
    pub fn stop(mut self, project: &Project, ctx: ScriptContext) -> Result<ScriptOutcome, String> {
        self.finish(Some((project, ctx)))
    }

    /// The script's graphics window and its last frame, taken so it is uploaded once.
    pub fn gfx_window(&mut self) -> Option<GfxWindow> {
        self.engine.gfx_window()
    }

    fn finish(&mut self, fresh: Option<(&Project, ScriptContext)>) -> Result<ScriptOutcome, String> {
        if let Some((project, ctx)) = fresh {
            self.state.borrow_mut().reset(project, ctx);
        }
        let result = self.engine.run_at_exit();
        self.result(result)
    }

    fn result(&mut self, result: Result<(), String>) -> Result<ScriptOutcome, String> {
        match result {
            Ok(()) => Ok(self.outcome()),
            Err(e) => Err(format!("{}{e}", self.outcome().console)),
        }
    }

    /// What the script did since the last outcome was taken.
    fn outcome(&mut self) -> ScriptOutcome {
        let mut s = self.state.borrow_mut();
        let mut out = std::mem::take(&mut s.out);
        if std::mem::take(&mut s.changed) {
            out.project = Some(s.project.clone());
        }
        if std::mem::take(&mut s.sel_changed) {
            out.selection = Some((s.sel_tracks.clone(), s.sel_items.clone()));
        }
        out
    }
}

// ---- the API -------------------------------------------------------------------------------

/// An envelope a script can hold: one of a track's or one of a take's.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Env {
    Track(TrackId, EnvelopeKind),
    Take(ItemId, usize, EnvelopeKind),
}

/// A value crossing the API, whatever the language.
#[derive(Debug, Clone, PartialEq)]
enum Val {
    /// No value: an absent argument, a null pointer, or an output left unwritten.
    Nil,
    Bool(bool),
    Int(i64),
    Num(f64),
    Str(String),
    Track(TrackId),
    Item(ItemId),
    /// A take: its item and its index there.
    Take(ItemId, usize),
    Env(Env),
    /// A `PCM_source*`: its handle in `State::sources`.
    Src(usize),
    /// The current project.
    Proj,
}

impl Val {
    fn num(&self) -> f64 {
        match self {
            Val::Bool(b) => *b as u8 as f64,
            Val::Int(i) => *i as f64,
            Val::Num(x) => *x,
            _ => 0.0,
        }
    }

    fn int(&self) -> i64 {
        match self {
            Val::Int(i) => *i,
            v => v.num() as i64,
        }
    }

    fn truthy(&self) -> bool {
        self.num() != 0.0
    }

    fn text(&self) -> String {
        match self {
            Val::Str(s) => s.clone(),
            Val::Int(i) => i.to_string(),
            Val::Num(x) => x.to_string(),
            _ => String::new(),
        }
    }

    fn is_nil(&self) -> bool {
        *self == Val::Nil
    }

    fn track(&self) -> Option<TrackId> {
        if let Val::Track(t) = self { Some(*t) } else { None }
    }

    fn item(&self) -> Option<ItemId> {
        if let Val::Item(i) = self { Some(*i) } else { None }
    }

    fn take(&self) -> Option<(ItemId, usize)> {
        if let Val::Take(i, t) = self { Some((*i, *t)) } else { None }
    }

    fn env(&self) -> Option<Env> {
        if let Val::Env(e) = self { Some(*e) } else { None }
    }

    fn src(&self) -> Option<usize> {
        if let Val::Src(s) = self { Some(*s) } else { None }
    }
}

/// What a parameter or return value is.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Void,
    Bool,
    Int,
    Num,
    Str,
    Track,
    Item,
    Take,
    Env,
    Src,
    /// A `ReaProject*`; always the current project.
    Proj,
    /// A pointer of any kind (`ValidatePtr`'s).
    Ptr,
}

impl Kind {
    /// The C type, as Python's pointer strings and `ValidatePtr` name it.
    fn pointer_type(self) -> &'static str {
        match self {
            Kind::Track => "MediaTrack*",
            Kind::Item => "MediaItem*",
            Kind::Take => "MediaItem_Take*",
            Kind::Env => "TrackEnvelope*",
            Kind::Src => "PCM_source*",
            Kind::Ptr => "void*",
            _ => "ReaProject*",
        }
    }

    fn is_pointer(self) -> bool {
        matches!(self, Kind::Track | Kind::Item | Kind::Take | Kind::Env | Kind::Src | Kind::Proj | Kind::Ptr)
    }

    /// The value an output of this kind has when nothing was written.
    fn zero(self) -> Val {
        match self {
            Kind::Bool => Val::Bool(false),
            Kind::Int => Val::Int(0),
            Kind::Num => Val::Num(0.0),
            Kind::Str => Val::Str(String::new()),
            _ => Val::Nil,
        }
    }
}

/// How a parameter passes: in, out (written by the call), both, or the size of the output
/// buffer before it (only Python's signatures have those).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Dir {
    In,
    Out,
    InOut,
    Size,
}

type Param = (Kind, Dir);

/// A function's signature as REAPER's generated ReaScript documentation gives it.
struct Sig {
    name: &'static str,
    ret: Kind,
    params: &'static [Param],
}

impl Sig {
    /// The parameters a call reads.
    fn inputs(&self) -> impl Iterator<Item = (usize, Kind)> + '_ {
        self.params
            .iter()
            .enumerate()
            .filter(|(_, (_, d))| matches!(d, Dir::In | Dir::InOut))
            .map(|(i, (k, _))| (i, *k))
    }

    /// The parameters a call writes, in order.
    fn outputs(&self) -> impl Iterator<Item = (usize, Kind)> + '_ {
        self.params
            .iter()
            .enumerate()
            .filter(|(_, (_, d))| matches!(d, Dir::Out | Dir::InOut))
            .map(|(i, (k, _))| (i, *k))
    }
}

const P: Param = (Kind::Proj, Dir::In);
const T: Param = (Kind::Track, Dir::In);
const I: Param = (Kind::Item, Dir::In);
const K: Param = (Kind::Take, Dir::In);
const E: Param = (Kind::Env, Dir::In);
const R: Param = (Kind::Src, Dir::In);
const V: Param = (Kind::Ptr, Dir::In);
const B: Param = (Kind::Bool, Dir::In);
const Z: Param = (Kind::Int, Dir::In);
const N: Param = (Kind::Num, Dir::In);
const S: Param = (Kind::Str, Dir::In);
const BO: Param = (Kind::Bool, Dir::Out);
const ZO: Param = (Kind::Int, Dir::Out);
const NO: Param = (Kind::Num, Dir::Out);
const NIO: Param = (Kind::Num, Dir::InOut);
const ZIO: Param = (Kind::Int, Dir::InOut);
const BIO: Param = (Kind::Bool, Dir::InOut);
const SO: Param = (Kind::Str, Dir::Out);
const SIO: Param = (Kind::Str, Dir::InOut);
const SZ: Param = (Kind::Int, Dir::Size);

macro_rules! sigs {
    ($($name:literal $ret:ident [$($p:ident),*];)*) => {
        &[$(Sig { name: $name, ret: Kind::$ret, params: &[$($p),*] }),*]
    };
}

/// The functions scripts can call (besides `defer`, `runloop` and `atexit`, which each
/// language takes code for its own way).
static API: &[Sig] = sigs! {
    // General.
    "ShowConsoleMsg" Void [S];
    "ClearConsole" Void [];
    "GetAppVersion" Str [];
    "GetOS" Str [];
    "GetResourcePath" Str [];
    "GetExePath" Str [];
    "time_precise" Num [];
    "ShowMessageBox" Int [S, S, Z];
    "GetUserInputs" Bool [S, Z, S, SIO, SZ];
    "ValidatePtr" Bool [V, S];
    "ValidatePtr2" Bool [P, V, S];
    "DB2SLIDER" Num [N];
    "SLIDER2DB" Num [N];
    "mkvolstr" Void [SIO, N];
    "mkpanstr" Void [SIO, N];
    "ColorToNative" Int [Z, Z, Z];
    "ColorFromNative" Void [Z, ZO, ZO, ZO];
    "format_timestr" Void [N, SIO, SZ];
    "format_timestr_pos" Void [N, SIO, SZ, Z];
    "parse_timestr" Num [S];
    "parse_timestr_pos" Num [S, Z];
    "GetExtState" Str [S, S];
    "SetExtState" Void [S, S, S, B];
    "HasExtState" Bool [S, S];
    "DeleteExtState" Void [S, S, B];

    // Undo and refreshes.
    "Undo_BeginBlock" Void [];
    "Undo_BeginBlock2" Void [P];
    "Undo_EndBlock" Void [S, Z];
    "Undo_EndBlock2" Void [P, S, Z];
    "Undo_OnStateChange" Void [S];
    "Undo_OnStateChange2" Void [P, S];
    "Undo_CanUndo2" Str [P];
    "Undo_CanRedo2" Str [P];
    "Undo_DoUndo2" Int [P];
    "Undo_DoRedo2" Int [P];
    "UpdateArrange" Void [];
    "UpdateTimeline" Void [];
    "UpdateItemInProject" Void [I];
    "TrackList_AdjustWindows" Void [B];
    "PreventUIRefresh" Void [Z];
    "MarkProjectDirty" Void [P];
    "IsProjectDirty" Int [P];
    "GetProjectStateChangeCount" Int [P];

    // Project.
    "EnumProjects" Proj [Z, SO, SZ];
    "GetProjectName" Void [P, SO, SZ];
    "GetProjectPath" Void [SO, SZ];
    "GetProjectPathEx" Void [P, SO, SZ];
    "GetProjectLength" Num [P];
    "GetSetProjectInfo" Num [P, S, N, B];
    "GetSetProjectInfo_String" Bool [P, S, SIO, B];
    "GetSetProjectAuthor" Void [P, B, SIO, SZ];
    "GetSetProjectNotes" Void [P, B, SIO, SZ];
    "SetProjExtState" Int [P, S, S, S];
    "GetProjExtState" Int [P, S, S, SO, SZ];
    "EnumProjExtState" Bool [P, S, Z, SO, SZ, SO, SZ];
    "Main_OnCommand" Void [Z, Z];
    "Main_OnCommandEx" Void [Z, Z, P];
    "NamedCommandLookup" Int [S];
    "GetToggleCommandState" Int [Z];

    // Transport and cursor.
    "GetCursorPosition" Num [];
    "GetCursorPositionEx" Num [P];
    "SetEditCurPos" Void [N, B, B];
    "MoveEditCursor" Void [N, B];
    "GetPlayState" Int [];
    "GetPlayPosition" Num [];
    "GetPlayPosition2" Num [];
    "GetPlayPositionEx" Num [P];
    "GetPlayPosition2Ex" Num [P];
    "OnPlayButton" Void [];
    "OnStopButton" Void [];
    "GetSet_LoopTimeRange" Void [B, B, NIO, NIO, B];
    "GetSet_LoopTimeRange2" Void [P, B, B, NIO, NIO, B];
    "GetSetRepeat" Int [Z];
    "GetSetRepeatEx" Int [P, Z];

    // Tempo and time signature.
    "Master_GetTempo" Num [];
    "SetCurrentBPM" Void [P, N, B];
    "TimeMap2_timeToQN" Num [P, N];
    "TimeMap2_QNToTime" Num [P, N];
    "TimeMap_timeToQN" Num [N];
    "TimeMap_QNToTime" Num [N];
    "TimeMap_GetDividedBpmAtTime" Num [N];
    "TimeMap2_GetDividedBpmAtTime" Num [P, N];
    "TimeMap_GetTimeSigAtTime" Void [P, N, ZO, ZO, NO];
    "GetProjectTimeSignature2" Void [P, NO, NO];
    "TimeMap2_timeToBeats" Num [P, N, ZO, ZO, NO, ZO];
    "TimeMap2_beatsToTime" Num [P, N, Z];
    "TimeMap_GetMeasureInfo" Num [P, Z, NO, NO, ZO, ZO, NO];
    "TimeMap2_GetNextChangeTime" Num [P, N];
    "CountTempoTimeSigMarkers" Int [P];
    "GetTempoTimeSigMarker" Bool [P, Z, NO, ZO, NO, NO, ZO, ZO, BO];
    "SetTempoTimeSigMarker" Bool [P, Z, N, Z, N, N, Z, Z, B];
    "AddTempoTimeSigMarker" Bool [P, N, N, Z, Z, B];
    "DeleteTempoTimeSigMarker" Bool [P, Z];

    // Markers and regions.
    "AddProjectMarker" Int [P, B, N, N, S, Z];
    "AddProjectMarker2" Int [P, B, N, N, S, Z, Z];
    "CountProjectMarkers" Int [P, ZO, ZO];
    "EnumProjectMarkers" Int [Z, BO, NO, NO, SO, ZO];
    "EnumProjectMarkers2" Int [P, Z, BO, NO, NO, SO, ZO];
    "EnumProjectMarkers3" Int [P, Z, BO, NO, NO, SO, ZO, ZO];
    "SetProjectMarker" Bool [Z, B, N, N, S];
    "SetProjectMarker2" Bool [P, Z, B, N, N, S];
    "SetProjectMarker3" Bool [P, Z, B, N, N, S, Z];
    "SetProjectMarker4" Bool [P, Z, B, N, N, S, Z, Z];
    "SetProjectMarkerByIndex" Bool [P, Z, B, N, N, Z, S, Z];
    "SetProjectMarkerByIndex2" Bool [P, Z, B, N, N, Z, S, Z, Z];
    "DeleteProjectMarker" Bool [P, Z, B];
    "DeleteProjectMarkerByIndex" Bool [P, Z];
    "GetLastMarkerAndCurRegion" Void [P, N, ZO, ZO];

    // Tracks.
    "CountTracks" Int [P];
    "GetTrack" Track [P, Z];
    "GetMasterTrack" Track [P];
    "InsertTrackAtIndex" Void [Z, B];
    "DeleteTrack" Void [T];
    "GetTrackName" Bool [T, SO, SZ];
    "GetSetMediaTrackInfo_String" Bool [T, S, SIO, B];
    "GetMediaTrackInfo_Value" Num [T, S];
    "SetMediaTrackInfo_Value" Bool [T, S, N];
    "GetTrackColor" Int [T];
    "SetTrackColor" Void [T, Z];
    "GetParentTrack" Track [T];
    "GetTrackDepth" Int [T];
    "GetTrackUIVolPan" Bool [T, NO, NO];
    "GetSetTrackGroupMembership" Int [T, S, Z, Z];
    "GetSetTrackGroupMembershipHigh" Int [T, S, Z, Z];
    "GetSetTrackGroupMembershipEx" Int [T, S, Z, Z, Z];
    "CSurf_OnVolumeChange" Num [T, N, B];
    "CSurf_OnVolumeChangeEx" Num [T, N, B, B];
    "CSurf_OnPanChange" Num [T, N, B];
    "CSurf_OnPanChangeEx" Num [T, N, B, B];
    "CSurf_OnWidthChange" Num [T, N, B];
    "CSurf_OnWidthChangeEx" Num [T, N, B, B];
    "CSurf_OnMuteChange" Bool [T, Z];
    "CSurf_OnMuteChangeEx" Bool [T, Z, B];
    "CSurf_OnSoloChange" Bool [T, Z];
    "CSurf_OnSoloChangeEx" Bool [T, Z, B];
    "CSurf_OnRecArmChange" Bool [T, Z];
    "CSurf_OnRecArmChangeEx" Bool [T, Z, B];
    "SetTrackUIVolume" Num [T, N, B, B, Z];
    "SetTrackUIPan" Num [T, N, B, B, Z];
    "SetTrackUIWidth" Num [T, N, B, B, Z];
    "SetTrackUIMute" Int [T, Z, Z];
    "SetTrackUISolo" Int [T, Z, Z];
    "SetTrackUIRecArm" Int [T, Z, Z];
    "SetTrackUIPolarity" Int [T, Z, Z];
    "CountSelectedTracks" Int [P];
    "CountSelectedTracks2" Int [P, B];
    "GetSelectedTrack" Track [P, Z];
    "GetSelectedTrack2" Track [P, Z, B];
    "IsTrackSelected" Bool [T];
    "SetTrackSelected" Void [T, B];
    "SetOnlyTrackSelected" Void [T];
    "Track_GetPeakInfo" Num [T, Z];

    // Sends and receives.
    "CreateTrackSend" Int [T, T];
    "RemoveTrackSend" Bool [T, Z, Z];
    "GetTrackNumSends" Int [T, Z];
    "GetTrackSendInfo_Value" Num [T, Z, Z, S];
    "SetTrackSendInfo_Value" Bool [T, Z, Z, S, N];
    "GetTrackSendName" Bool [T, Z, SO, SZ];
    "GetTrackReceiveName" Bool [T, Z, SO, SZ];

    // Effects.
    "TrackFX_GetCount" Int [T];
    "TrackFX_GetFXName" Bool [T, Z, SO, SZ];
    "TrackFX_GetEnabled" Bool [T, Z];
    "TrackFX_SetEnabled" Void [T, Z, B];
    "TrackFX_GetOffline" Bool [T, Z];
    "TrackFX_GetInstrument" Int [T];
    "TrackFX_AddByName" Int [T, S, B, Z];
    "TrackFX_GetByName" Int [T, S, B];
    "TrackFX_Delete" Bool [T, Z];
    "TrackFX_CopyToTrack" Void [T, Z, T, Z, B];
    "TrackFX_GetNumParams" Int [T, Z];
    "TrackFX_GetParam" Num [T, Z, Z, NO, NO];
    "TrackFX_GetParamEx" Num [T, Z, Z, NO, NO, NO];
    "TrackFX_SetParam" Bool [T, Z, Z, N];
    "TrackFX_GetParamNormalized" Num [T, Z, Z];
    "TrackFX_SetParamNormalized" Bool [T, Z, Z, N];
    "TrackFX_GetFormattedParamValue" Bool [T, Z, Z, SO, SZ];
    "TrackFX_GetParamName" Bool [T, Z, Z, SO, SZ];
    "TrackFX_GetPreset" Bool [T, Z, SO, SZ];
    "TrackFX_SetPreset" Bool [T, Z, S];
    "TrackFX_GetPresetIndex" Int [T, Z, ZO];
    "TrackFX_SetPresetByIndex" Bool [T, Z, Z];
    "TrackFX_NavigatePresets" Bool [T, Z, Z];

    // Envelopes.
    "CountTrackEnvelopes" Int [T];
    "GetTrackEnvelope" Env [T, Z];
    "GetTrackEnvelopeByName" Env [T, S];
    "GetTrackEnvelopeByChunkName" Env [T, S];
    "CountTakeEnvelopes" Int [K];
    "GetTakeEnvelope" Env [K, Z];
    "GetTakeEnvelopeByName" Env [K, S];
    "GetEnvelopeName" Bool [E, SO, SZ];
    "GetEnvelopeScalingMode" Int [E];
    "ScaleFromEnvelopeMode" Num [Z, N];
    "ScaleToEnvelopeMode" Num [Z, N];
    "CountEnvelopePoints" Int [E];
    "CountEnvelopePointsEx" Int [E, Z];
    "GetEnvelopePoint" Bool [E, Z, NO, NO, ZO, NO, BO];
    "GetEnvelopePointEx" Bool [E, Z, Z, NO, NO, ZO, NO, BO];
    "SetEnvelopePoint" Bool [E, Z, NIO, NIO, ZIO, NIO, BIO, BIO];
    "SetEnvelopePointEx" Bool [E, Z, Z, NIO, NIO, ZIO, NIO, BIO, BIO];
    "InsertEnvelopePoint" Bool [E, N, N, Z, N, B, BIO];
    "InsertEnvelopePointEx" Bool [E, Z, N, N, Z, N, B, BIO];
    "DeleteEnvelopePointEx" Bool [E, Z, Z];
    "DeleteEnvelopePointRange" Bool [E, N, N];
    "DeleteEnvelopePointRangeEx" Bool [E, Z, N, N];
    "GetEnvelopePointByTime" Int [E, N];
    "GetEnvelopePointByTimeEx" Int [E, Z, N];
    "Envelope_Evaluate" Int [E, N, N, Z, NO, NO, NO, NO];
    "Envelope_SortPoints" Bool [E];
    "Envelope_SortPointsEx" Bool [E, Z];
    "CountAutomationItems" Int [E];

    // Items.
    "CountMediaItems" Int [P];
    "GetMediaItem" Item [P, Z];
    "CountTrackMediaItems" Int [T];
    "GetTrackNumMediaItems" Int [T];
    "GetTrackMediaItem" Item [T, Z];
    "GetMediaItem_Track" Track [I];
    "GetMediaItemTrack" Track [I];
    "CountSelectedMediaItems" Int [P];
    "GetSelectedMediaItem" Item [P, Z];
    "IsMediaItemSelected" Bool [I];
    "SetMediaItemSelected" Void [I, B];
    "SelectAllMediaItems" Void [P, B];
    "GetMediaItemInfo_Value" Num [I, S];
    "SetMediaItemInfo_Value" Bool [I, S, N];
    "GetSetMediaItemInfo_String" Bool [I, S, SIO, B];
    "SetMediaItemPosition" Bool [I, N, B];
    "SetMediaItemLength" Bool [I, N, B];
    "AddMediaItemToTrack" Item [T];
    "CreateNewMIDIItemInProj" Item [T, N, N, B];
    "SplitMediaItem" Item [I, N];
    "DeleteTrackMediaItem" Bool [T, I];
    "MoveMediaItemToTrack" Bool [I, T];

    // Takes and sources.
    "CountTakes" Int [I];
    "GetMediaItemNumTakes" Int [I];
    "GetTake" Take [I, Z];
    "GetMediaItemTake" Take [I, Z];
    "GetActiveTake" Take [I];
    "SetActiveTake" Void [K];
    "AddTakeToMediaItem" Take [I];
    "GetMediaItemTake_Item" Item [K];
    "GetMediaItemTake_Track" Track [K];
    "GetTakeName" Str [K];
    "GetSetMediaItemTakeInfo_String" Bool [K, S, SIO, B];
    "GetMediaItemTakeInfo_Value" Num [K, S];
    "SetMediaItemTakeInfo_Value" Bool [K, S, N];
    "TakeIsMIDI" Bool [K];
    "GetMediaItemTake_Source" Src [K];
    "SetMediaItemTake_Source" Bool [K, R];
    "PCM_Source_CreateFromFile" Src [S];
    "PCM_Source_Destroy" Void [R];
    "GetMediaSourceFileName" Void [R, SO, SZ];
    "GetMediaSourceType" Void [R, SO, SZ];
    "GetMediaSourceLength" Num [R, BO];

    // MIDI.
    "MIDI_CountEvts" Int [K, ZO, ZO, ZO];
    "MIDI_GetNote" Bool [K, Z, BO, BO, NO, NO, ZO, ZO, ZO];
    "MIDI_SetNote" Bool [K, Z, B, B, N, N, Z, Z, Z, B];
    "MIDI_InsertNote" Bool [K, B, B, N, N, Z, Z, Z, B];
    "MIDI_DeleteNote" Bool [K, Z];
    "MIDI_GetCC" Bool [K, Z, BO, BO, NO, ZO, ZO, ZO, ZO];
    "MIDI_SetCC" Bool [K, Z, B, B, N, Z, Z, Z, Z, B];
    "MIDI_InsertCC" Bool [K, B, B, N, Z, Z, Z, Z];
    "MIDI_DeleteCC" Bool [K, Z];
    "MIDI_Sort" Void [K];
    "MIDI_DisableSort" Void [K];
    "MIDI_GetPPQPosFromProjTime" Num [K, N];
    "MIDI_GetProjTimeFromPPQPos" Num [K, N];
    "MIDI_GetPPQPosFromProjQN" Num [K, N];
    "MIDI_GetProjQNFromPPQPos" Num [K, N];
};

/// Pointers as EEL and Python see them: a tag for the kind above 40 bits of identity, so a
/// track's pointer is never taken for an item. Takes and envelopes pack their owner's id in
/// the low 32 bits and their index or kind above it.
const POINTER_TAG: u64 = 1 << 40;
const LOW: u64 = 1 << 32;

fn env_kind_index(kind: EnvelopeKind) -> u64 {
    EnvelopeKind::ALL.iter().position(|&k| k == kind).unwrap_or(0) as u64
}

fn to_pointer(v: &Val) -> u64 {
    match v {
        Val::Track(t) => POINTER_TAG | t.0,
        Val::Item(i) => (2 * POINTER_TAG) | i.0,
        Val::Take(i, t) => (3 * POINTER_TAG) | ((*t as u64 % 256) * LOW) | (i.0 % LOW),
        Val::Env(Env::Track(t, k)) => (4 * POINTER_TAG) | (env_kind_index(*k) * LOW) | (t.0 % LOW),
        Val::Env(Env::Take(i, t, k)) => {
            (5 * POINTER_TAG) | ((*t as u64 % 32) * 8 * LOW) | (env_kind_index(*k) * LOW) | (i.0 % LOW)
        }
        Val::Src(s) => (6 * POINTER_TAG) | *s as u64,
        Val::Proj => (7 * POINTER_TAG) | 1,
        _ => 0,
    }
}

fn from_pointer(kind: Kind, p: u64) -> Val {
    let (tag, id) = (p / POINTER_TAG, p % POINTER_TAG);
    let env_kind = |id: u64| EnvelopeKind::ALL.get((id / LOW % 8) as usize).copied();
    let v = match tag {
        1 => Val::Track(TrackId(id)),
        2 => Val::Item(ItemId(id)),
        3 => Val::Take(ItemId(id % LOW), (id / LOW) as usize),
        4 => env_kind(id).map_or(Val::Nil, |k| Val::Env(Env::Track(TrackId(id % LOW), k))),
        5 => env_kind(id).map_or(Val::Nil, |k| Val::Env(Env::Take(ItemId(id % LOW), (id / LOW / 8) as usize, k))),
        6 => Val::Src(id as usize),
        7 => Val::Proj,
        _ => Val::Nil,
    };
    if kind == Kind::Ptr || value_kind(&v) == Some(kind) { v } else { Val::Nil }
}

/// The pointer kind of a value.
fn value_kind(v: &Val) -> Option<Kind> {
    Some(match v {
        Val::Track(_) => Kind::Track,
        Val::Item(_) => Kind::Item,
        Val::Take(..) => Kind::Take,
        Val::Env(_) => Kind::Env,
        Val::Src(_) => Kind::Src,
        Val::Proj => Kind::Proj,
        _ => return None,
    })
}

fn track(p: &mut Project, id: Option<TrackId>) -> Option<&mut Track> {
    let id = id?;
    if p.master.id == id { Some(&mut p.master) } else { p.track_mut(id) }
}

fn index(list_len: usize, i: i64) -> Option<usize> {
    usize::try_from(i).ok().filter(|&i| i < list_len)
}

/// A call's inputs, in order.
struct Args<'a>(&'a [Val]);

impl Args<'_> {
    fn get(&self, i: usize) -> &Val {
        self.0.get(i).unwrap_or(&Val::Nil)
    }

    fn num(&self, i: usize) -> f64 {
        self.get(i).num()
    }

    fn int(&self, i: usize) -> i64 {
        self.get(i).int()
    }

    fn bool(&self, i: usize) -> bool {
        self.get(i).truthy()
    }

    fn text(&self, i: usize) -> String {
        self.get(i).text()
    }

    /// An optional input: `None` when nil.
    fn opt(&self, i: usize) -> Option<f64> {
        (!self.get(i).is_nil()).then(|| self.num(i))
    }
}

/// What a call returns: its value and its outputs in order (`Sig::outputs`).
type Ret = (Val, Vec<Val>);

fn ret(v: Val) -> Ret {
    (v, Vec::new())
}

/// Calls `sig` with its inputs in order (`Sig::inputs`); returns its value and its outputs in
/// order (`Sig::outputs`), `Nil` for an output it leaves alone. Pointers that do not name a
/// track, item or take make the call do nothing, as REAPER's null checks do.
fn call(s: &mut State, sig: &Sig, args: &[Val]) -> Ret {
    let a = Args(args);
    tracks::call(s, sig.name, &a)
        .or_else(|| items::call(s, sig.name, &a))
        .or_else(|| project::call(s, sig.name, &a))
        .unwrap_or_else(|| ret(Val::Nil))
}

#[cfg(test)]
mod tests {
    use super::*;
    use scythe_core::{Item, Take};

    pub(super) fn project() -> Project {
        let mut p = Project::default();
        let a = p.add_track("Guitar");
        p.add_track("Bass");
        p.track_mut(a).unwrap().items.push(Item::new(1.0, 4.0, Take::from_file("a.wav".into())));
        p
    }

    /// What the scripts in each language's tests do to `project()`.
    pub(super) fn assert_tidied(out: &ScriptOutcome) {
        assert_eq!(out.console, "Guitar\nBass\n");
        assert_eq!(out.undo.as_deref(), Some("Tidy up"));
        let p = out.project.as_ref().unwrap();
        assert_eq!(p.tracks.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["Click", "Guitar", "Bass"]);
        assert_eq!(p.tracks[1].volume, 0.5);
        let spans: Vec<_> = p.tracks[1].items.iter().map(|i| (i.position, i.end())).collect();
        assert_eq!(spans, [(2.0, 3.0), (3.0, 6.0)]);
        assert_eq!((p.markers[0].name.as_str(), p.markers[0].position), ("Verse", 1.5));
    }

    pub(super) fn state(p: Project) -> State {
        State::new(&p, ScriptContext::default())
    }

    pub(super) fn api(s: &mut State, name: &str, args: &[Val]) -> Ret {
        call(s, API.iter().find(|x| x.name == name).unwrap_or_else(|| panic!("no {name}")), args)
    }

    #[test]
    fn every_function_has_one_signature() {
        let mut names: Vec<_> = API.iter().map(|s| s.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), API.len());
    }

    #[test]
    fn pointers_keep_their_kind() {
        let t = Val::Track(TrackId(7));
        assert_eq!(from_pointer(Kind::Track, to_pointer(&t)), t);
        assert_eq!(from_pointer(Kind::Item, to_pointer(&t)), Val::Nil);
        assert_eq!(from_pointer(Kind::Track, 0), Val::Nil);
        for v in [
            Val::Take(ItemId(12), 3),
            Val::Env(Env::Track(TrackId(5), EnvelopeKind::Pan)),
            Val::Env(Env::Take(ItemId(9), 2, EnvelopeKind::Mute)),
            Val::Src(4),
            Val::Proj,
        ] {
            let p = to_pointer(&v);
            assert!(p < 1 << 53, "EEL keeps pointers in doubles");
            assert_eq!(from_pointer(value_kind(&v).unwrap(), p), v);
            assert_eq!(from_pointer(Kind::Ptr, p), v);
            assert_eq!(from_pointer(Kind::Track, p), Val::Nil);
        }
    }

    #[test]
    fn unnamed_tracks_are_named_by_number_as_reaper_does() {
        let mut p = project();
        p.add_track("");
        let id = p.tracks[2].id;
        let mut s = state(p);
        assert_eq!(api(&mut s, "GetTrackName", &[Val::Track(id)]), (Val::Bool(true), vec![Val::Str("Track 3".into())]));
    }
}
