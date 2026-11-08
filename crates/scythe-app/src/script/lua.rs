//! Lua scripts: `reaper.Name(inputs)` returns the value, then the outputs.

use std::cell::RefCell;
use std::rc::Rc;

use mlua::{Function, Lua, MultiValue, UserData, Value};

use super::{API, Engine, Kind, Shared, Sig, Val};

#[derive(Clone, Copy)]
struct TrackHandle(scythe_core::TrackId);
impl UserData for TrackHandle {}

#[derive(Clone, Copy)]
struct ItemHandle(scythe_core::ItemId);
impl UserData for ItemHandle {}

#[derive(Default)]
struct Queues {
    deferred: Vec<Function>,
    at_exit: Vec<Function>,
}

pub(super) struct LuaEngine {
    /// Holds the deferred functions' interpreter alive.
    _lua: Lua,
    queues: Rc<RefCell<Queues>>,
}

pub(super) fn start(source: &str, name: &str, state: &Shared) -> Result<LuaEngine, String> {
    let lua = Lua::new();
    let queues = Rc::new(RefCell::new(Queues::default()));
    register(&lua, state, &queues).map_err(|e| e.to_string())?;
    lua.load(source).set_name(name).exec().map_err(|e| e.to_string())?;
    Ok(LuaEngine { _lua: lua, queues })
}

impl Engine for LuaEngine {
    fn pending(&self) -> bool {
        !self.queues.borrow().deferred.is_empty()
    }

    fn run_deferred(&mut self) -> Result<(), String> {
        let due = std::mem::take(&mut self.queues.borrow_mut().deferred);
        due.into_iter().try_for_each(|f| f.call::<()>(())).map_err(|e| e.to_string())
    }

    fn run_at_exit(&mut self) -> Result<(), String> {
        let mut q = self.queues.borrow_mut();
        q.deferred.clear();
        let at_exit = std::mem::take(&mut q.at_exit);
        drop(q);
        at_exit.into_iter().try_for_each(|f| f.call::<()>(())).map_err(|e| e.to_string())
    }
}

/// Reads argument `n` (from 1) of `sig` as `kind`; a wrong type is an error, as in REAPER.
fn to_val(sig: &Sig, n: usize, kind: Kind, v: &Value) -> mlua::Result<Val> {
    let bad =
        |expected: &str| mlua::Error::runtime(format!("bad argument #{n} to '{}' ({expected} expected)", sig.name));
    Ok(match (kind, v) {
        (Kind::Proj, _) => Val::Nil,
        (Kind::Track, Value::UserData(u)) => Val::Track(u.borrow::<TrackHandle>().map_err(|_| bad("MediaTrack"))?.0),
        (Kind::Track, _) => return Err(bad("MediaTrack")),
        (Kind::Item, Value::UserData(u)) => Val::Item(u.borrow::<ItemHandle>().map_err(|_| bad("MediaItem"))?.0),
        (Kind::Item, _) => return Err(bad("MediaItem")),
        (_, Value::Nil) => Val::Nil,
        (Kind::Bool, Value::Boolean(b)) => Val::Bool(*b),
        (Kind::Bool, _) => return Err(bad("boolean")),
        (Kind::Str, Value::String(s)) => Val::Str(s.to_string_lossy()),
        (Kind::Str, Value::Integer(i)) => Val::Str(i.to_string()),
        (Kind::Str, Value::Number(x)) => Val::Str(x.to_string()),
        (Kind::Str, _) => return Err(bad("string")),
        (_, Value::Integer(i)) => Val::Int(*i),
        (_, Value::Number(x)) => Val::Num(*x),
        _ => return Err(bad("number")),
    })
}

fn to_lua(lua: &Lua, kind: Kind, v: Val) -> mlua::Result<Value> {
    Ok(match v {
        Val::Nil if kind == Kind::Track || kind == Kind::Item => Value::Nil,
        Val::Nil => return to_lua(lua, kind, kind.zero()),
        Val::Bool(b) => Value::Boolean(b),
        Val::Int(i) => Value::Integer(i),
        Val::Num(x) => Value::Number(x),
        Val::Str(s) => Value::String(lua.create_string(&s)?),
        Val::Track(t) => Value::UserData(lua.create_userdata(TrackHandle(t))?),
        Val::Item(i) => Value::UserData(lua.create_userdata(ItemHandle(i))?),
    })
}

fn register(lua: &Lua, state: &Shared, queues: &Rc<RefCell<Queues>>) -> mlua::Result<()> {
    let reaper = lua.create_table()?;
    for sig in API {
        let st = state.clone();
        let f = lua.create_function(move |lua, args: MultiValue| {
            let args: Vec<Value> = args.into_iter().collect();
            let inputs = sig
                .inputs()
                .enumerate()
                .map(|(n, (_, kind))| to_val(sig, n + 1, kind, args.get(n).unwrap_or(&Value::Nil)))
                .collect::<mlua::Result<Vec<_>>>()?;
            let (ret, outs) = super::call(&mut st.borrow_mut(), sig, &inputs);
            let mut results = Vec::new();
            if sig.ret != Kind::Void {
                results.push(to_lua(lua, sig.ret, ret)?);
            }
            for ((_, kind), v) in sig.outputs().zip(outs) {
                results.push(to_lua(lua, kind, v)?);
            }
            Ok(MultiValue::from_iter(results))
        })?;
        reaper.set(sig.name, f)?;
    }
    for (name, at_exit) in [("defer", false), ("runloop", false), ("atexit", true)] {
        let q = queues.clone();
        let f = lua.create_function(move |_, f: Function| {
            let mut q = q.borrow_mut();
            if at_exit {
                q.at_exit.push(f)
            } else {
                q.deferred.push(f)
            }
            Ok(true)
        })?;
        reaper.set(name, f)?;
    }
    lua.globals().set("reaper", reaper)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::tests::{assert_tidied, project};
    use super::super::*;

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
            local n, markers, regions = reaper.CountProjectMarkers(0)
            assert(n == 1 and markers == 1 and regions == 0)
            local _, isrgn, pos, _, name = reaper.EnumProjectMarkers(0)
            assert(isrgn == false and pos == 1.5 and name == "Verse")
            reaper.InsertTrackAtIndex(0, true)
            reaper.GetSetMediaTrackInfo_String(reaper.GetTrack(0, 0), "P_NAME", "Click", true)
            assert(reaper.GetTrack(0, 9) == nil)
            reaper.Undo_EndBlock("Tidy up", -1)
        "#;
        let (out, _) = run(script, "test.lua", &project(), ScriptContext::default()).unwrap();
        assert_tidied(&out);
    }

    #[test]
    fn errors_name_the_line_and_change_nothing() {
        let err = run("reaper.ShowConsoleMsg('a')\nnot_a_function()", "x.lua", &project(), ScriptContext::default())
            .unwrap_err();
        assert!(err.starts_with('a'), "the console comes first: {err}");
        assert!(err.contains("x.lua\"]:2") || err.contains("x.lua:2"), "{err}");
        let err = run("reaper.GetTrackName(nil)", "t.lua", &project(), ScriptContext::default()).unwrap_err();
        assert!(err.contains("bad argument #1 to 'GetTrackName' (MediaTrack expected)"), "{err}");
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
