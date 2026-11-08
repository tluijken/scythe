//! ReaScript in Lua 5.4, EEL2 and Python: a subset of REAPER's API over a copy of the project,
//! so simple REAPER scripts run unchanged. The script's changes come back as one project to
//! apply as a single undo step. A script that calls `defer` keeps running: the app calls its
//! deferred code once per frame, each time over a fresh copy of the project.
//!
//! The API is implemented once, here, over language-neutral values; each language passes
//! arguments and returns results its own way, as REAPER's signatures for it say (`API`).

mod eel;
mod lua;
mod python;

use std::cell::RefCell;
use std::rc::Rc;

use scythe_core::model::MIN_ITEM_LENGTH;
use scythe_core::{ItemId, Marker, Project, Track, TrackId};

/// What a script sees besides the project, and what it asks of the app.
#[derive(Debug, Clone, Default)]
pub struct ScriptContext {
    pub selected_track: Option<TrackId>,
    pub selected_items: Vec<ItemId>,
    pub cursor: f64,
    pub playing: bool,
    /// The playhead while playing, else the cursor.
    pub play_position: f64,
}

#[derive(Debug, Default)]
pub struct ScriptOutcome {
    pub project: Option<Project>,
    pub console: String,
    /// The undo description given to `Undo_EndBlock`.
    pub undo: Option<String>,
    pub cursor: Option<f64>,
    pub play: Option<bool>,
}

struct State {
    project: Project,
    ctx: ScriptContext,
    out: ScriptOutcome,
    changed: bool,
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
    let state: Shared =
        Rc::new(RefCell::new(State { project: project.clone(), ctx, out: ScriptOutcome::default(), changed: false }));
    let ext = std::path::Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
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
        self.reset(project, ctx);
        let result = self.engine.run_deferred();
        self.result(result)
    }

    /// Ends the script, running what it registered with `atexit`.
    pub fn stop(mut self, project: &Project, ctx: ScriptContext) -> Result<ScriptOutcome, String> {
        self.finish(Some((project, ctx)))
    }

    fn finish(&mut self, fresh: Option<(&Project, ScriptContext)>) -> Result<ScriptOutcome, String> {
        if let Some((project, ctx)) = fresh {
            self.reset(project, ctx);
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

    fn reset(&mut self, project: &Project, ctx: ScriptContext) {
        let mut s = self.state.borrow_mut();
        s.project = project.clone();
        s.ctx = ctx;
        s.changed = false;
    }

    /// What the script did since the last outcome was taken.
    fn outcome(&mut self) -> ScriptOutcome {
        let mut s = self.state.borrow_mut();
        let mut out = std::mem::take(&mut s.out);
        if std::mem::take(&mut s.changed) {
            out.project = Some(s.project.clone());
        }
        out
    }
}

// ---- the API -------------------------------------------------------------------------------

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

    fn track(&self) -> Option<TrackId> {
        if let Val::Track(t) = self { Some(*t) } else { None }
    }

    fn item(&self) -> Option<ItemId> {
        if let Val::Item(i) = self { Some(*i) } else { None }
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
    /// A `ReaProject*`; always the current project.
    Proj,
}

impl Kind {
    /// The C type, as Python's pointer strings name it.
    fn pointer_type(self) -> &'static str {
        match self {
            Kind::Track => "MediaTrack*",
            Kind::Item => "MediaItem*",
            _ => "ReaProject*",
        }
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
const B: Param = (Kind::Bool, Dir::In);
const Z: Param = (Kind::Int, Dir::In);
const N: Param = (Kind::Num, Dir::In);
const S: Param = (Kind::Str, Dir::In);
const BO: Param = (Kind::Bool, Dir::Out);
const ZO: Param = (Kind::Int, Dir::Out);
const NO: Param = (Kind::Num, Dir::Out);
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
    "ShowConsoleMsg" Void [S];
    "ClearConsole" Void [];
    "GetAppVersion" Str [];
    "Undo_BeginBlock" Void [];
    "Undo_BeginBlock2" Void [P];
    "Undo_EndBlock" Void [S, Z];
    "Undo_EndBlock2" Void [P, S, Z];
    "UpdateArrange" Void [];
    "UpdateTimeline" Void [];
    "TrackList_AdjustWindows" Void [B];
    "PreventUIRefresh" Void [Z];
    "time_precise" Num [];
    "GetCursorPosition" Num [];
    "SetEditCurPos" Void [N, B, B];
    "GetPlayState" Int [];
    "GetPlayPosition" Num [];
    "GetPlayPosition2" Num [];
    "GetPlayPositionEx" Num [P];
    "GetPlayPosition2Ex" Num [P];
    "OnPlayButton" Void [];
    "OnStopButton" Void [];
    "GetProjectLength" Num [P];
    "Master_GetTempo" Num [];
    "TimeMap2_timeToQN" Num [P, N];
    "TimeMap2_QNToTime" Num [P, N];
    "CountTracks" Int [P];
    "GetTrack" Track [P, Z];
    "GetMasterTrack" Track [P];
    "CountSelectedTracks" Int [P];
    "GetSelectedTrack" Track [P, Z];
    "InsertTrackAtIndex" Void [Z, B];
    "DeleteTrack" Void [T];
    "GetTrackName" Bool [T, SO, SZ];
    "GetSetMediaTrackInfo_String" Bool [T, S, SIO, B];
    "GetMediaTrackInfo_Value" Num [T, S];
    "SetMediaTrackInfo_Value" Bool [T, S, N];
    "TrackFX_GetCount" Int [T];
    "TrackFX_GetFXName" Bool [T, Z, SO, SZ];
    "TrackFX_GetEnabled" Bool [T, Z];
    "TrackFX_SetEnabled" Void [T, Z, B];
    "CountMediaItems" Int [P];
    "GetMediaItem" Item [P, Z];
    "CountTrackMediaItems" Int [T];
    "GetTrackMediaItem" Item [T, Z];
    "GetMediaItem_Track" Track [I];
    "CountSelectedMediaItems" Int [P];
    "GetSelectedMediaItem" Item [P, Z];
    "GetMediaItemInfo_Value" Num [I, S];
    "SetMediaItemInfo_Value" Bool [I, S, N];
    "SplitMediaItem" Item [I, N];
    "DeleteTrackMediaItem" Bool [T, I];
    "AddProjectMarker" Int [P, B, N, N, S, Z];
    "CountProjectMarkers" Int [P, ZO, ZO];
    "EnumProjectMarkers" Int [Z, BO, NO, NO, SO, ZO];
};

/// Pointers to tracks and items as EEL and Python see them: the id, tagged by kind so a
/// track's pointer is never taken for an item.
const POINTER_TAG: u64 = 1 << 40;

fn to_pointer(v: &Val) -> u64 {
    match v {
        Val::Track(t) => POINTER_TAG | t.0,
        Val::Item(i) => (2 * POINTER_TAG) | i.0,
        _ => 0,
    }
}

fn from_pointer(kind: Kind, p: u64) -> Val {
    match (kind, p / POINTER_TAG) {
        (Kind::Track, 1) => Val::Track(TrackId(p % POINTER_TAG)),
        (Kind::Item, 2) => Val::Item(ItemId(p % POINTER_TAG)),
        _ => Val::Nil,
    }
}

fn track(p: &mut Project, id: Option<TrackId>) -> Option<&mut Track> {
    let id = id?;
    if p.master.id == id { Some(&mut p.master) } else { p.track_mut(id) }
}

fn all_items(p: &Project) -> Vec<ItemId> {
    p.tracks.iter().flat_map(|t| t.items.iter().map(|i| i.id)).collect()
}

fn index(list_len: usize, i: i64) -> Option<usize> {
    usize::try_from(i).ok().filter(|&i| i < list_len)
}

/// Calls `sig` with its inputs in order (`Sig::inputs`); returns its value and its outputs in
/// order (`Sig::outputs`), `Nil` for an output it leaves alone. Pointers that do not name a
/// track or item make the call do nothing, as REAPER's null checks do.
fn call(s: &mut State, sig: &Sig, args: &[Val]) -> (Val, Vec<Val>) {
    let nil = Val::Nil;
    let a = |i: usize| args.get(i).unwrap_or(&nil);
    let ret = |v: Val| (v, Vec::new());
    match sig.name {
        "ShowConsoleMsg" => {
            s.out.console.push_str(&a(0).text());
            ret(Val::Nil)
        }
        "ClearConsole" => {
            s.out.console.clear();
            ret(Val::Nil)
        }
        "GetAppVersion" => ret(Val::Str("7.73/scythe".to_owned())),
        "Undo_EndBlock" => {
            s.out.undo = Some(a(0).text());
            ret(Val::Nil)
        }
        "Undo_EndBlock2" => {
            s.out.undo = Some(a(1).text());
            ret(Val::Nil)
        }
        "time_precise" => ret(Val::Num(
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64()),
        )),

        // Transport and cursor.
        "GetCursorPosition" => ret(Val::Num(s.out.cursor.unwrap_or(s.ctx.cursor))),
        "SetEditCurPos" => {
            s.out.cursor = Some(a(0).num().max(0.0));
            ret(Val::Nil)
        }
        "GetPlayState" => ret(Val::Int(s.out.play.unwrap_or(s.ctx.playing) as i64)),
        "GetPlayPosition" | "GetPlayPosition2" | "GetPlayPositionEx" | "GetPlayPosition2Ex" => {
            ret(Val::Num(s.ctx.play_position))
        }
        "OnPlayButton" => {
            s.out.play = Some(true);
            ret(Val::Nil)
        }
        "OnStopButton" => {
            s.out.play = Some(false);
            ret(Val::Nil)
        }
        "GetProjectLength" => ret(Val::Num(s.project.length())),
        "Master_GetTempo" => ret(Val::Num(s.project.tempo.bpm_at(0.0))),
        "TimeMap2_timeToQN" => ret(Val::Num(s.project.tempo.timeline().qn_at(a(1).num()))),
        "TimeMap2_QNToTime" => ret(Val::Num(s.project.tempo.timeline().time_at_qn(a(1).num()))),

        // Tracks.
        "CountTracks" => ret(Val::Int(s.project.tracks.len() as i64)),
        "GetTrack" => {
            ret(index(s.project.tracks.len(), a(1).int()).map_or(Val::Nil, |i| Val::Track(s.project.tracks[i].id)))
        }
        "GetMasterTrack" => ret(Val::Track(s.project.master.id)),
        "CountSelectedTracks" => ret(Val::Int(s.ctx.selected_track.is_some() as i64)),
        "GetSelectedTrack" => ret(s.ctx.selected_track.filter(|_| a(1).int() == 0).map_or(Val::Nil, Val::Track)),
        "InsertTrackAtIndex" => {
            s.project.add_track("");
            let t = s.project.tracks.pop().unwrap();
            let at = (a(0).int().max(0) as usize).min(s.project.tracks.len());
            s.project.tracks.insert(at, t);
            s.changed = true;
            ret(Val::Nil)
        }
        "DeleteTrack" => {
            if let Some(t) = a(0).track() {
                s.project.remove_track(t);
                s.changed = true;
            }
            ret(Val::Nil)
        }
        "GetTrackName" => {
            let number = a(0).track().and_then(|t| s.project.track_index(t)).map_or(0, |i| i + 1);
            let master = s.project.master.id;
            match track(&mut s.project, a(0).track()) {
                Some(t) if t.id == master => (Val::Bool(true), vec![Val::Str("MASTER".to_owned())]),
                Some(t) if t.name.is_empty() => (Val::Bool(true), vec![Val::Str(format!("Track {number}"))]),
                Some(t) => (Val::Bool(true), vec![Val::Str(t.name.clone())]),
                None => (Val::Bool(false), vec![Val::Str(String::new())]),
            }
        }
        "GetSetMediaTrackInfo_String" => {
            let set = a(3).truthy();
            let Some(t) = track(&mut s.project, a(0).track()) else {
                return (Val::Bool(false), vec![Val::Str(String::new())]);
            };
            let value = match a(1).text().as_str() {
                "P_NAME" if set => {
                    t.name = a(2).text();
                    s.changed = true;
                    t.name.clone()
                }
                "P_NAME" => t.name.clone(),
                "GUID" => t.guid.clone(),
                _ => return (Val::Bool(false), vec![Val::Str(String::new())]),
            };
            (Val::Bool(true), vec![Val::Str(value)])
        }
        "GetMediaTrackInfo_Value" => {
            let number = a(0).track().and_then(|t| s.project.track_index(t)).map_or(-1.0, |i| i as f64 + 1.0);
            let Some(t) = track(&mut s.project, a(0).track()) else { return ret(Val::Num(0.0)) };
            ret(Val::Num(match a(1).text().as_str() {
                "D_VOL" => t.volume,
                "D_PAN" => t.pan,
                "B_MUTE" => t.muted as u8 as f64,
                "I_SOLO" => t.solo as u8 as f64,
                "I_RECARM" => t.armed as u8 as f64,
                "I_RECINPUT" => t.rec_input as f64,
                "I_RECMON" => t.monitor as u8 as f64,
                "B_MAINSEND" => t.main_send as u8 as f64,
                "I_FOLDERDEPTH" => t.folder_depth as f64,
                "I_AUTOMODE" => t.auto_mode.rpp_code() as f64,
                "I_NCHAN" => 2.0,
                "IP_TRACKNUMBER" => number,
                "I_MIDIHWOUT" => t.midi_out.map_or(-1.0, |(d, c)| (d as i64 * 32 + c as i64) as f64),
                _ => 0.0,
            }))
        }
        "SetMediaTrackInfo_Value" => {
            let v = a(2).num();
            let Some(t) = track(&mut s.project, a(0).track()) else { return ret(Val::Bool(false)) };
            match a(1).text().as_str() {
                "D_VOL" => t.volume = v.max(0.0),
                "D_PAN" => t.pan = v.clamp(-1.0, 1.0),
                "B_MUTE" => t.muted = v != 0.0,
                "I_SOLO" => t.solo = v != 0.0,
                "I_RECARM" => t.armed = v != 0.0,
                "I_RECINPUT" => t.rec_input = v as i64,
                "I_RECMON" => t.monitor = v != 0.0,
                "B_MAINSEND" => t.main_send = v != 0.0,
                "I_FOLDERDEPTH" => t.folder_depth = v as i32,
                "I_AUTOMODE" => t.auto_mode = scythe_core::AutoMode::from_rpp(v as i64),
                "I_MIDIHWOUT" => {
                    t.midi_out = (v >= 0.0).then_some(((v as i64 >> 5) as u8, (v as i64 & 31) as u8));
                }
                _ => return ret(Val::Bool(false)),
            }
            s.changed = true;
            ret(Val::Bool(true))
        }
        "TrackFX_GetCount" => ret(Val::Int(track(&mut s.project, a(0).track()).map_or(0, |t| t.fx.len() as i64))),
        "TrackFX_GetFXName" => {
            let fx = a(1).int();
            let name =
                track(&mut s.project, a(0).track()).and_then(|t| index(t.fx.len(), fx).map(|i| t.fx[i].name.clone()));
            (Val::Bool(name.is_some()), vec![Val::Str(name.unwrap_or_default())])
        }
        "TrackFX_GetEnabled" => {
            let fx = a(1).int();
            let on = track(&mut s.project, a(0).track()).and_then(|t| index(t.fx.len(), fx).map(|i| t.fx[i].enabled));
            ret(Val::Bool(on.unwrap_or(false)))
        }
        "TrackFX_SetEnabled" => {
            let (fx, on) = (a(1).int(), a(2).truthy());
            if let Some(t) = track(&mut s.project, a(0).track())
                && let Some(i) = index(t.fx.len(), fx)
            {
                t.fx[i].enabled = on;
                s.changed = true;
            }
            ret(Val::Nil)
        }

        // Items.
        "CountMediaItems" => ret(Val::Int(all_items(&s.project).len() as i64)),
        "GetMediaItem" => {
            let items = all_items(&s.project);
            ret(index(items.len(), a(1).int()).map_or(Val::Nil, |i| Val::Item(items[i])))
        }
        "CountTrackMediaItems" => {
            ret(Val::Int(track(&mut s.project, a(0).track()).map_or(0, |t| t.items.len() as i64)))
        }
        "GetTrackMediaItem" => {
            let i = a(1).int();
            let item = track(&mut s.project, a(0).track()).and_then(|t| index(t.items.len(), i).map(|i| t.items[i].id));
            ret(item.map_or(Val::Nil, Val::Item))
        }
        "GetMediaItem_Track" => {
            ret(a(0).item().and_then(|i| s.project.find_item(i)).map_or(Val::Nil, |(t, _)| Val::Track(t)))
        }
        "CountSelectedMediaItems" => ret(Val::Int(s.ctx.selected_items.len() as i64)),
        "GetSelectedMediaItem" => {
            ret(index(s.ctx.selected_items.len(), a(1).int()).map_or(Val::Nil, |i| Val::Item(s.ctx.selected_items[i])))
        }
        "GetMediaItemInfo_Value" => {
            let Some((_, item)) = a(0).item().and_then(|i| s.project.find_item(i)) else { return ret(Val::Num(0.0)) };
            ret(Val::Num(match a(1).text().as_str() {
                "D_POSITION" => item.position,
                "D_LENGTH" => item.length,
                "B_MUTE" => item.muted as u8 as f64,
                "D_VOL" => item.volume,
                "B_LOOPSRC" => item.looped as u8 as f64,
                "D_FADEINLEN" => item.fade_in.length,
                "D_FADEOUTLEN" => item.fade_out.length,
                "I_CURTAKE" => item.active_take as f64,
                _ => 0.0,
            }))
        }
        "SetMediaItemInfo_Value" => {
            let v = a(2).num();
            let Some(id) = a(0).item() else { return ret(Val::Bool(false)) };
            let Some(item) = s.project.tracks.iter_mut().find_map(|t| t.item_mut(id)) else {
                return ret(Val::Bool(false));
            };
            match a(1).text().as_str() {
                "D_POSITION" => item.position = v.max(0.0),
                "D_LENGTH" => item.length = v.max(MIN_ITEM_LENGTH),
                "B_MUTE" => item.muted = v != 0.0,
                "D_VOL" => item.volume = v.max(0.0),
                "B_LOOPSRC" => item.looped = v != 0.0,
                "D_FADEINLEN" => item.fade_in.length = v.max(0.0),
                "D_FADEOUTLEN" => item.fade_out.length = v.max(0.0),
                "I_CURTAKE" => item.active_take = (v.max(0.0) as usize).min(item.takes.len().saturating_sub(1)),
                _ => return ret(Val::Bool(false)),
            }
            s.changed = true;
            ret(Val::Bool(true))
        }
        "SplitMediaItem" => {
            let Some(id) = a(0).item() else { return ret(Val::Nil) };
            let Some((tid, _)) = s.project.find_item(id) else { return ret(Val::Nil) };
            let t = s.project.track_mut(tid).unwrap();
            let Some(right) = t.item_mut(id).and_then(|i| i.split_at(a(1).num())) else { return ret(Val::Nil) };
            let right_id = right.id;
            t.items.push(right);
            s.changed = true;
            ret(Val::Item(right_id))
        }
        "DeleteTrackMediaItem" => {
            let item = a(1).item();
            let Some(t) = track(&mut s.project, a(0).track()) else { return ret(Val::Bool(false)) };
            let before = t.items.len();
            t.items.retain(|i| Some(i.id) != item);
            let removed = t.items.len() != before;
            s.changed |= removed;
            ret(Val::Bool(removed))
        }

        // Markers and regions.
        "AddProjectMarker" => {
            let region = a(1).truthy();
            let want = a(5).int();
            let index = if want > 0 {
                want as u32
            } else {
                s.project
                    .markers
                    .iter()
                    .filter(|m| m.region_end.is_some() == region)
                    .map(|m| m.index)
                    .max()
                    .unwrap_or(0)
                    + 1
            };
            s.project.markers.push(Marker {
                index,
                position: a(2).num(),
                name: a(4).text(),
                color: None,
                region_end: region.then_some(a(3).num()),
                lines: Default::default(),
            });
            s.project.markers.sort_by(|a, b| a.position.total_cmp(&b.position));
            s.changed = true;
            ret(Val::Int(index as i64))
        }
        "CountProjectMarkers" => {
            let regions = s.project.markers.iter().filter(|m| m.region_end.is_some()).count() as i64;
            let total = s.project.markers.len() as i64;
            (Val::Int(total), vec![Val::Int(total - regions), Val::Int(regions)])
        }
        "EnumProjectMarkers" => {
            let i = a(0).int();
            match index(s.project.markers.len(), i).map(|i| &s.project.markers[i]) {
                Some(m) => (
                    Val::Int(i + 1),
                    vec![
                        Val::Bool(m.region_end.is_some()),
                        Val::Num(m.position),
                        Val::Num(m.region_end.unwrap_or(0.0)),
                        Val::Str(m.name.clone()),
                        Val::Int(m.index as i64),
                    ],
                ),
                None => (Val::Int(0), vec![Val::Nil; 5]),
            }
        }

        // Undo blocks, refreshes and the like have nothing to do here.
        _ => ret(Val::Nil),
    }
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
    }

    #[test]
    fn unnamed_tracks_are_named_by_number_as_reaper_does() {
        let mut p = project();
        p.add_track("");
        let id = p.tracks[2].id;
        let mut s = State { project: p, ctx: ScriptContext::default(), out: ScriptOutcome::default(), changed: false };
        let sig = API.iter().find(|s| s.name == "GetTrackName").unwrap();
        assert_eq!(call(&mut s, sig, &[Val::Track(id)]), (Val::Bool(true), vec![Val::Str("Track 3".into())]));
    }
}
