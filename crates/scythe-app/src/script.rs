//! ReaScript in Lua 5.4: a subset of REAPER's `reaper.*` API over a copy of the project, so
//! simple REAPER scripts run unchanged. The script's changes come back as one project to apply
//! as a single undo step. A script that calls `reaper.defer` keeps running: the app calls its
//! deferred functions once per frame, each over a fresh copy of the project.

use std::cell::RefCell;
use std::rc::Rc;

use mlua::{Lua, MultiValue, UserData, UserDataRef, Value};
use scythe_core::model::MIN_ITEM_LENGTH;
use scythe_core::{ItemId, Marker, Project, Track, TrackId};

#[derive(Clone, Copy)]
struct TrackHandle(TrackId);
impl UserData for TrackHandle {}

#[derive(Clone, Copy)]
struct ItemHandle(ItemId);
impl UserData for ItemHandle {}

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
    deferred: Vec<mlua::Function>,
    at_exit: Vec<mlua::Function>,
}

type Shared = Rc<RefCell<State>>;

fn track(p: &mut Project, id: TrackId) -> Option<&mut Track> {
    if p.master.id == id { Some(&mut p.master) } else { p.track_mut(id) }
}

fn all_items(p: &Project) -> Vec<ItemId> {
    p.tracks.iter().flat_map(|t| t.items.iter().map(|i| i.id)).collect()
}

/// Runs `source` and returns what it did, and the script itself when it deferred work to
/// later frames. Errors carry Lua's message (with the line).
pub fn run(
    source: &str,
    name: &str,
    project: &Project,
    ctx: ScriptContext,
) -> Result<(ScriptOutcome, Option<RunningScript>), String> {
    let state: Shared = Rc::new(RefCell::new(State {
        project: project.clone(),
        ctx,
        out: ScriptOutcome::default(),
        changed: false,
        deferred: Vec::new(),
        at_exit: Vec::new(),
    }));
    let lua = Lua::new();
    register(&lua, &state).map_err(|e| e.to_string())?;
    let result = lua.load(source).set_name(name).exec();
    let mut script = RunningScript { _lua: lua, state };
    if let Err(e) = result {
        return Err(format!("{}{e}", script.outcome().console));
    }
    if script.is_running() {
        let out = script.outcome();
        return Ok((out, Some(script)));
    }
    let out = script.finish(None)?;
    Ok((out, None))
}

/// A script waiting for its deferred functions to run.
pub struct RunningScript {
    /// Holds the deferred functions' interpreter alive.
    _lua: Lua,
    state: Shared,
}

impl std::fmt::Debug for RunningScript {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningScript").field("deferred", &self.state.borrow().deferred.len()).finish()
    }
}

impl RunningScript {
    pub fn is_running(&self) -> bool {
        !self.state.borrow().deferred.is_empty()
    }

    /// Runs the functions deferred in the last frame over `project`.
    pub fn tick(&mut self, project: &Project, ctx: ScriptContext) -> Result<ScriptOutcome, String> {
        self.reset(project, ctx);
        let due = std::mem::take(&mut self.state.borrow_mut().deferred);
        for f in due {
            if let Err(e) = f.call::<()>(()) {
                return Err(format!("{}{e}", self.outcome().console));
            }
        }
        Ok(self.outcome())
    }

    /// Ends the script, running what it registered with `atexit`.
    pub fn stop(mut self, project: &Project, ctx: ScriptContext) -> Result<ScriptOutcome, String> {
        self.state.borrow_mut().deferred.clear();
        self.finish(Some((project, ctx)))
    }

    fn finish(&mut self, fresh: Option<(&Project, ScriptContext)>) -> Result<ScriptOutcome, String> {
        if let Some((project, ctx)) = fresh {
            self.reset(project, ctx);
        }
        let at_exit = std::mem::take(&mut self.state.borrow_mut().at_exit);
        for f in at_exit {
            if let Err(e) = f.call::<()>(()) {
                return Err(format!("{}{e}", self.outcome().console));
            }
        }
        Ok(self.outcome())
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

fn register(lua: &Lua, state: &Shared) -> mlua::Result<()> {
    let reaper = lua.create_table()?;
    macro_rules! func {
        ($name:literal, |$s:ident, $args:tt: $ty:ty| $body:expr) => {{
            let st = state.clone();
            reaper.set(
                $name,
                lua.create_function(move |_, $args: $ty| {
                    let mut guard = st.borrow_mut();
                    let $s: &mut State = &mut guard;
                    $body
                })?,
            )?;
        }};
    }

    // Console, undo and UI calls.
    func!("ShowConsoleMsg", |s, msg: String| {
        s.out.console.push_str(&msg);
        Ok(())
    });
    func!("ClearConsole", |s, (): ()| {
        s.out.console.clear();
        Ok(())
    });
    func!("GetAppVersion", |_s, (): ()| Ok("7.73/scythe".to_owned()));
    func!("Undo_BeginBlock", |_s, (): ()| Ok(()));
    func!("Undo_BeginBlock2", |_s, _p: Value| Ok(()));
    func!("Undo_EndBlock", |s, (desc, _flags): (String, Option<i64>)| {
        s.out.undo = Some(desc);
        Ok(())
    });
    func!("Undo_EndBlock2", |s, (_p, desc, _flags): (Value, String, Option<i64>)| {
        s.out.undo = Some(desc);
        Ok(())
    });
    for noop in ["UpdateArrange", "UpdateTimeline", "TrackList_AdjustWindows", "PreventUIRefresh"] {
        reaper.set(noop, lua.create_function(|_, _: MultiValue| Ok(()))?)?;
    }
    func!("defer", |s, f: mlua::Function| {
        s.deferred.push(f);
        Ok(true)
    });
    func!("runloop", |s, f: mlua::Function| {
        s.deferred.push(f);
        Ok(true)
    });
    func!("atexit", |s, f: mlua::Function| {
        s.at_exit.push(f);
        Ok(true)
    });
    func!("time_precise", |_s, (): ()| {
        Ok(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64()))
    });

    // Transport and cursor.
    func!("GetCursorPosition", |s, (): ()| Ok(s.out.cursor.unwrap_or(s.ctx.cursor)));
    func!("SetEditCurPos", |s, (t, _view, _seek): (f64, Option<bool>, Option<bool>)| {
        s.out.cursor = Some(t.max(0.0));
        Ok(())
    });
    func!("GetPlayState", |s, (): ()| Ok(s.out.play.unwrap_or(s.ctx.playing) as i64));
    func!("GetPlayPosition", |s, (): ()| Ok(s.ctx.play_position));
    func!("GetPlayPosition2", |s, (): ()| Ok(s.ctx.play_position));
    func!("GetPlayPositionEx", |s, _p: Value| Ok(s.ctx.play_position));
    func!("GetPlayPosition2Ex", |s, _p: Value| Ok(s.ctx.play_position));
    func!("OnPlayButton", |s, (): ()| {
        s.out.play = Some(true);
        Ok(())
    });
    func!("OnStopButton", |s, (): ()| {
        s.out.play = Some(false);
        Ok(())
    });
    func!("GetProjectLength", |s, _p: Value| Ok(s.project.length()));
    func!("Master_GetTempo", |s, (): ()| Ok(s.project.tempo.bpm_at(0.0)));
    func!("TimeMap2_timeToQN", |s, (_p, t): (Value, f64)| Ok(s.project.tempo.timeline().qn_at(t)));
    func!("TimeMap2_QNToTime", |s, (_p, q): (Value, f64)| Ok(s.project.tempo.timeline().time_at_qn(q)));

    // Tracks.
    func!("CountTracks", |s, _p: Value| Ok(s.project.tracks.len() as i64));
    func!("GetTrack", |s, (_p, i): (Value, i64)| {
        Ok(s.project.tracks.get(i.max(0) as usize).filter(|_| i >= 0).map(|t| TrackHandle(t.id)))
    });
    func!("GetMasterTrack", |s, _p: Value| Ok(TrackHandle(s.project.master.id)));
    func!("CountSelectedTracks", |s, _p: Value| Ok(s.ctx.selected_track.is_some() as i64));
    func!("GetSelectedTrack", |s, (_p, i): (Value, i64)| {
        Ok(s.ctx.selected_track.filter(|_| i == 0).map(TrackHandle))
    });
    func!("InsertTrackAtIndex", |s, (idx, _defaults): (i64, Option<bool>)| {
        s.project.add_track("");
        let t = s.project.tracks.pop().unwrap();
        let at = (idx.max(0) as usize).min(s.project.tracks.len());
        s.project.tracks.insert(at, t);
        s.changed = true;
        Ok(())
    });
    func!("DeleteTrack", |s, tr: UserDataRef<TrackHandle>| {
        s.project.remove_track(tr.0);
        s.changed = true;
        Ok(())
    });
    func!("GetTrackName", |s, tr: UserDataRef<TrackHandle>| {
        let master = s.project.master.id;
        let name = match track(&mut s.project, tr.0) {
            Some(t) if t.id == master => "MASTER".to_owned(),
            Some(t) => t.name.clone(),
            None => String::new(),
        };
        Ok((true, name))
    });
    func!(
        "GetSetMediaTrackInfo_String",
        |s, (tr, parm, value, set): (UserDataRef<TrackHandle>, String, String, bool)| {
            let Some(t) = track(&mut s.project, tr.0) else { return Ok((false, String::new())) };
            match parm.as_str() {
                "P_NAME" if set => {
                    t.name = value.clone();
                    s.changed = true;
                    Ok((true, value))
                }
                "P_NAME" => Ok((true, t.name.clone())),
                "GUID" => Ok((true, t.guid.clone())),
                _ => Ok((false, String::new())),
            }
        }
    );
    func!("GetMediaTrackInfo_Value", |s, (tr, parm): (UserDataRef<TrackHandle>, String)| {
        let index = s.project.track_index(tr.0).map_or(-1.0, |i| i as f64 + 1.0);
        let Some(t) = track(&mut s.project, tr.0) else { return Ok(0.0) };
        Ok(match parm.as_str() {
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
            "IP_TRACKNUMBER" => index,
            "I_MIDIHWOUT" => t.midi_out.map_or(-1.0, |(d, c)| (d as i64 * 32 + c as i64) as f64),
            _ => 0.0,
        })
    });
    func!("SetMediaTrackInfo_Value", |s, (tr, parm, v): (UserDataRef<TrackHandle>, String, f64)| {
        let Some(t) = track(&mut s.project, tr.0) else { return Ok(false) };
        match parm.as_str() {
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
            "I_MIDIHWOUT" => t.midi_out = (v >= 0.0).then_some(((v as i64 >> 5) as u8, (v as i64 & 31) as u8)),
            _ => return Ok(false),
        }
        s.changed = true;
        Ok(true)
    });
    func!("TrackFX_GetCount", |s, tr: UserDataRef<TrackHandle>| {
        Ok(track(&mut s.project, tr.0).map_or(0, |t| t.fx.len() as i64))
    });
    func!("TrackFX_GetFXName", |s, (tr, fx, _buf): (UserDataRef<TrackHandle>, i64, Option<String>)| {
        let name = track(&mut s.project, tr.0).and_then(|t| t.fx.get(fx.max(0) as usize)).map(|f| f.name.clone());
        Ok((name.is_some(), name.unwrap_or_default()))
    });
    func!("TrackFX_GetEnabled", |s, (tr, fx): (UserDataRef<TrackHandle>, i64)| {
        Ok(track(&mut s.project, tr.0).and_then(|t| t.fx.get(fx.max(0) as usize)).is_some_and(|f| f.enabled))
    });
    func!("TrackFX_SetEnabled", |s, (tr, fx, on): (UserDataRef<TrackHandle>, i64, bool)| {
        if let Some(f) = track(&mut s.project, tr.0).and_then(|t| t.fx.get_mut(fx.max(0) as usize)) {
            f.enabled = on;
            s.changed = true;
        }
        Ok(())
    });

    // Items.
    func!("CountMediaItems", |s, _p: Value| Ok(all_items(&s.project).len() as i64));
    func!("GetMediaItem", |s, (_p, i): (Value, i64)| {
        Ok(all_items(&s.project).get(i.max(0) as usize).filter(|_| i >= 0).map(|id| ItemHandle(*id)))
    });
    func!("CountTrackMediaItems", |s, tr: UserDataRef<TrackHandle>| {
        Ok(track(&mut s.project, tr.0).map_or(0, |t| t.items.len() as i64))
    });
    func!("GetTrackMediaItem", |s, (tr, i): (UserDataRef<TrackHandle>, i64)| {
        Ok(track(&mut s.project, tr.0).and_then(|t| t.items.get(i.max(0) as usize)).map(|it| ItemHandle(it.id)))
    });
    func!("GetMediaItem_Track", |s, it: UserDataRef<ItemHandle>| {
        Ok(s.project.find_item(it.0).map(|(t, _)| TrackHandle(t)))
    });
    func!("CountSelectedMediaItems", |s, _p: Value| Ok(s.ctx.selected_items.len() as i64));
    func!("GetSelectedMediaItem", |s, (_p, i): (Value, i64)| {
        Ok(s.ctx.selected_items.get(i.max(0) as usize).filter(|_| i >= 0).map(|id| ItemHandle(*id)))
    });
    func!("GetMediaItemInfo_Value", |s, (it, parm): (UserDataRef<ItemHandle>, String)| {
        let Some((_, item)) = s.project.find_item(it.0) else { return Ok(0.0) };
        Ok(match parm.as_str() {
            "D_POSITION" => item.position,
            "D_LENGTH" => item.length,
            "B_MUTE" => item.muted as u8 as f64,
            "D_VOL" => item.volume,
            "B_LOOPSRC" => item.looped as u8 as f64,
            "D_FADEINLEN" => item.fade_in.length,
            "D_FADEOUTLEN" => item.fade_out.length,
            "I_CURTAKE" => item.active_take as f64,
            _ => 0.0,
        })
    });
    func!("SetMediaItemInfo_Value", |s, (it, parm, v): (UserDataRef<ItemHandle>, String, f64)| {
        let Some(item) = s.project.tracks.iter_mut().find_map(|t| t.item_mut(it.0)) else { return Ok(false) };
        match parm.as_str() {
            "D_POSITION" => item.position = v.max(0.0),
            "D_LENGTH" => item.length = v.max(MIN_ITEM_LENGTH),
            "B_MUTE" => item.muted = v != 0.0,
            "D_VOL" => item.volume = v.max(0.0),
            "B_LOOPSRC" => item.looped = v != 0.0,
            "D_FADEINLEN" => item.fade_in.length = v.max(0.0),
            "D_FADEOUTLEN" => item.fade_out.length = v.max(0.0),
            "I_CURTAKE" => item.active_take = (v.max(0.0) as usize).min(item.takes.len().saturating_sub(1)),
            _ => return Ok(false),
        }
        s.changed = true;
        Ok(true)
    });
    func!("SplitMediaItem", |s, (it, at): (UserDataRef<ItemHandle>, f64)| {
        let Some((tid, _)) = s.project.find_item(it.0) else { return Ok(None) };
        let t = s.project.track_mut(tid).unwrap();
        let Some(right) = t.item_mut(it.0).and_then(|i| i.split_at(at)) else { return Ok(None) };
        let id = right.id;
        t.items.push(right);
        s.changed = true;
        Ok(Some(ItemHandle(id)))
    });
    func!("DeleteTrackMediaItem", |s, (tr, it): (UserDataRef<TrackHandle>, UserDataRef<ItemHandle>)| {
        let Some(t) = track(&mut s.project, tr.0) else { return Ok(false) };
        let before = t.items.len();
        t.items.retain(|i| i.id != it.0);
        let removed = t.items.len() != before;
        s.changed |= removed;
        Ok(removed)
    });

    // Markers and regions.
    func!("AddProjectMarker", |s, (_p, region, pos, end, name, want): (Value, bool, f64, f64, String, i64)| {
        let index = if want > 0 {
            want as u32
        } else {
            s.project.markers.iter().filter(|m| m.region_end.is_some() == region).map(|m| m.index).max().unwrap_or(0)
                + 1
        };
        s.project.markers.push(Marker {
            index,
            position: pos,
            name,
            color: None,
            region_end: region.then_some(end),
            lines: Default::default(),
        });
        s.project.markers.sort_by(|a, b| a.position.total_cmp(&b.position));
        s.changed = true;
        Ok(index as i64)
    });
    func!("CountProjectMarkers", |s, _p: Value| {
        let regions = s.project.markers.iter().filter(|m| m.region_end.is_some()).count() as i64;
        let total = s.project.markers.len() as i64;
        Ok((total, total - regions, regions))
    });
    func!("EnumProjectMarkers", |s, i: i64| {
        Ok(match s.project.markers.get(i.max(0) as usize).filter(|_| i >= 0) {
            Some(m) => {
                (i + 1, m.region_end.is_some(), m.position, m.region_end.unwrap_or(0.0), m.name.clone(), m.index as i64)
            }
            None => (0, false, 0.0, 0.0, String::new(), 0),
        })
    });

    lua.globals().set("reaper", reaper)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use scythe_core::{Item, Take};

    fn project() -> Project {
        let mut p = Project::default();
        let a = p.add_track("Guitar");
        p.add_track("Bass");
        p.track_mut(a).unwrap().items.push(Item::new(1.0, 4.0, Take::from_file("a.wav".into())));
        p
    }

    #[test]
    fn edits_tracks_items_and_markers() {
        let script = r#"
            reaper.Undo_BeginBlock()
            for i = 0, reaper.CountTracks(0) - 1 do
                local tr = reaper.GetTrack(0, i)
                local _, name = reaper.GetTrackName(tr)
                reaper.ShowConsoleMsg(name .. "\n")
                reaper.SetMediaTrackInfo_Value(tr, "D_VOL", 0.5)
            end
            local item = reaper.GetMediaItem(0, 0)
            reaper.SetMediaItemInfo_Value(item, "D_POSITION", 2)
            local right = reaper.SplitMediaItem(item, 3)
            assert(right ~= nil)
            reaper.AddProjectMarker(0, false, 1.5, 0, "Verse", -1)
            reaper.InsertTrackAtIndex(0, true)
            reaper.GetSetMediaTrackInfo_String(reaper.GetTrack(0, 0), "P_NAME", "Click", true)
            reaper.Undo_EndBlock("Tidy up", -1)
        "#;
        let (out, _) = run(script, "test.lua", &project(), ScriptContext::default()).unwrap();
        assert_eq!(out.console, "Guitar\nBass\n");
        assert_eq!(out.undo.as_deref(), Some("Tidy up"));
        let p = out.project.unwrap();
        assert_eq!(p.tracks.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["Click", "Guitar", "Bass"]);
        assert_eq!(p.tracks[1].volume, 0.5);
        let spans: Vec<_> = p.tracks[1].items.iter().map(|i| (i.position, i.end())).collect();
        assert_eq!(spans, [(2.0, 3.0), (3.0, 6.0)]);
        assert_eq!((p.markers[0].name.as_str(), p.markers[0].position), ("Verse", 1.5));
    }

    #[test]
    fn errors_name_the_line_and_change_nothing() {
        let err = run("reaper.ShowConsoleMsg('a')\nnot_a_function()", "x.lua", &project(), ScriptContext::default())
            .unwrap_err();
        assert!(err.contains("x.lua\"]:2") || err.contains("x.lua:2"), "{err}");
        let (out, _) = run("local n = reaper.CountTracks(0)", "y.lua", &project(), ScriptContext::default()).unwrap();
        assert!(out.project.is_none());
    }

    #[test]
    fn deferred_functions_run_each_tick_until_they_stop() {
        let script = r#"
            local n = 0
            local function step()
                n = n + 1
                reaper.ShowConsoleMsg(n .. "@" .. reaper.GetPlayPosition() .. " ")
                if n < 3 then reaper.defer(step) end
            end
            reaper.atexit(function() reaper.SetMediaTrackInfo_Value(reaper.GetTrack(0, 0), "D_VOL", 0.25) end)
            reaper.defer(step)
        "#;
        let (out, running) = run(script, "d.lua", &project(), ScriptContext::default()).unwrap();
        assert!(out.console.is_empty());
        let mut running = running.expect("the script deferred work");
        let mut console = String::new();
        for t in 1..=3 {
            let ctx = ScriptContext { play_position: t as f64, ..Default::default() };
            console += &running.tick(&project(), ctx).unwrap().console;
        }
        assert_eq!(console, "1@1.0 2@2.0 3@3.0 ");
        assert!(!running.is_running());
        let out = running.stop(&project(), ScriptContext::default()).unwrap();
        assert_eq!(out.project.unwrap().tracks[0].volume, 0.25, "atexit runs when the script ends");
    }
}
