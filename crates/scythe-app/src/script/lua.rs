//! Lua scripts: `reaper.Name(inputs)` returns the value, then the outputs. Pointers are light
//! userdata, as REAPER's are, so the same track compares equal however it was found. `gfx.*`
//! draws through an EEL machine's `gfx_*`, which is what REAPER's Lua graphics mirror.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use mlua::{Function, LightUserData, Lua, MultiValue, Table, Value};
use scythe_engine::eel::{Code, Machine, Vars, compile};

use super::{API, Engine, Kind, ScriptGfx, Shared, Sig, Val, from_pointer, to_pointer};

#[derive(Default)]
struct Queues {
    deferred: Vec<Function>,
    at_exit: Vec<Function>,
}

/// The EEL machine behind `gfx`, with the calls compiled so far by their source.
struct Gfx {
    machine: Machine,
    compiled: HashMap<String, Code>,
    window: ScriptGfx,
}

pub(super) struct LuaEngine {
    /// Holds the deferred functions' interpreter alive.
    _lua: Lua,
    queues: Rc<RefCell<Queues>>,
    gfx: Rc<RefCell<Gfx>>,
    state: Shared,
}

pub(super) fn start(source: &str, name: &str, state: &Shared) -> Result<LuaEngine, String> {
    let lua = Lua::new();
    let queues = Rc::new(RefCell::new(Queues::default()));
    let gfx = Rc::new(RefCell::new(Gfx {
        machine: Machine::new(Vars::default()),
        compiled: HashMap::new(),
        window: ScriptGfx::default(),
    }));
    register(&lua, state, &queues, &gfx).map_err(|e| e.to_string())?;
    let mut engine = LuaEngine { _lua: lua, queues, gfx, state: state.clone() };
    let result = engine._lua.load(source).set_name(name).exec().map_err(|e| e.to_string());
    engine.end_frame();
    result.map(|()| engine)
}

impl LuaEngine {
    fn begin_frame(&mut self) {
        let ctx = self.state.borrow().ctx.clone();
        let g = &mut *self.gfx.borrow_mut();
        g.window.begin(&mut g.machine, &ctx);
    }

    fn end_frame(&mut self) {
        let g = &mut *self.gfx.borrow_mut();
        g.window.end(&mut g.machine);
    }
}

impl Engine for LuaEngine {
    fn pending(&self) -> bool {
        !self.queues.borrow().deferred.is_empty()
    }

    fn run_deferred(&mut self) -> Result<(), String> {
        let due = std::mem::take(&mut self.queues.borrow_mut().deferred);
        self.begin_frame();
        let result = due.into_iter().try_for_each(|f| f.call::<()>(())).map_err(|e| e.to_string());
        self.end_frame();
        result
    }

    fn run_at_exit(&mut self) -> Result<(), String> {
        let mut q = self.queues.borrow_mut();
        q.deferred.clear();
        let at_exit = std::mem::take(&mut q.at_exit);
        drop(q);
        at_exit.into_iter().try_for_each(|f| f.call::<()>(())).map_err(|e| e.to_string())
    }

    fn gfx_window(&mut self) -> Option<super::GfxWindow> {
        self.gfx.borrow_mut().window.take()
    }
}

/// Pointer parameters that may be nil (`CreateTrackSend`'s destination: nil makes a hardware
/// output).
fn optional_pointer(sig: &Sig, n: usize) -> bool {
    sig.name == "CreateTrackSend" && n == 2
}

/// Reads argument `n` (from 1) of `sig` as `kind`; a wrong type is an error, as in REAPER.
fn to_val(sig: &Sig, n: usize, kind: Kind, v: &Value) -> mlua::Result<Val> {
    let bad =
        |expected: &str| mlua::Error::runtime(format!("bad argument #{n} to '{}' ({expected} expected)", sig.name));
    Ok(match (kind, v) {
        (Kind::Proj, _) => Val::Nil,
        (k, Value::Nil) if k.is_pointer() && (optional_pointer(sig, n) || k == Kind::Ptr) => Val::Nil,
        (k, Value::LightUserData(p)) if k.is_pointer() => match from_pointer(k, p.0 as u64) {
            Val::Nil => return Err(bad(k.pointer_type().trim_end_matches('*'))),
            v => v,
        },
        (k, _) if k.is_pointer() => return Err(bad(k.pointer_type().trim_end_matches('*'))),
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
        Val::Nil if kind.is_pointer() || kind == Kind::Str => Value::Nil,
        Val::Nil => return to_lua(lua, kind, kind.zero()),
        Val::Bool(b) => Value::Boolean(b),
        Val::Int(i) => Value::Integer(i),
        Val::Num(x) => Value::Number(x),
        Val::Str(s) => Value::String(lua.create_string(&s)?),
        v => Value::LightUserData(LightUserData(to_pointer(&v) as usize as *mut std::ffi::c_void)),
    })
}

fn register(lua: &Lua, state: &Shared, queues: &Rc<RefCell<Queues>>, gfx: &Rc<RefCell<Gfx>>) -> mlua::Result<()> {
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
                // A string function returning nothing gives nil (`GetTakeName` of a bad take).
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
    lua.globals().set("gfx", gfx_table(lua, gfx)?)?;
    Ok(())
}

// ---- gfx -----------------------------------------------------------------------------------

/// `gfx.name` variables and the EEL variables they are.
fn gfx_var(name: &str) -> Option<String> {
    Some(match name {
        "r" | "g" | "b" | "a" | "a2" | "x" | "y" | "w" | "h" | "clear" | "texth" | "mode" | "dest" | "ext_retina" => {
            format!("gfx_{name}")
        }
        "mouse_x" | "mouse_y" | "mouse_cap" | "mouse_wheel" | "mouse_hwheel" => name.to_owned(),
        _ => return None,
    })
}

/// How a `gfx` function's results come back: from the call itself, or from output variables
/// (`gfx.measurestr` returning width and height) and output strings (`gfx.getfont`'s name).
#[derive(Clone, Copy)]
enum Outs {
    Value,
    Vars(usize),
    ValueAndString,
}

/// The `gfx` functions: their EEL name, and what they return.
fn gfx_function(name: &str) -> Option<(&'static str, Outs)> {
    Some(match name {
        "init" => ("gfx_init", Outs::Value),
        "quit" => ("gfx_quit", Outs::Value),
        "update" => ("gfx_update", Outs::Value),
        "dock" => ("gfx_dock", Outs::Value),
        "getchar" => ("gfx_getchar", Outs::Value),
        "set" => ("gfx_set", Outs::Value),
        "line" => ("gfx_line", Outs::Value),
        "lineto" => ("gfx_lineto", Outs::Value),
        "rect" => ("gfx_rect", Outs::Value),
        "rectto" => ("gfx_rectto", Outs::Value),
        "circle" => ("gfx_circle", Outs::Value),
        "arc" => ("gfx_arc", Outs::Value),
        "roundrect" => ("gfx_roundrect", Outs::Value),
        "triangle" => ("gfx_triangle", Outs::Value),
        "gradrect" => ("gfx_gradrect", Outs::Value),
        "drawstr" => ("gfx_drawstr", Outs::Value),
        "drawnumber" => ("gfx_drawnumber", Outs::Value),
        "drawchar" => ("gfx_drawchar", Outs::Value),
        "printf" => ("gfx_printf", Outs::Value),
        "measurestr" => ("gfx_measurestr", Outs::Vars(2)),
        "measurechar" => ("gfx_measurechar", Outs::Vars(2)),
        "setfont" => ("gfx_setfont", Outs::Value),
        "getfont" => ("gfx_getfont", Outs::ValueAndString),
        "setpixel" => ("gfx_setpixel", Outs::Value),
        "getpixel" => ("gfx_getpixel", Outs::Vars(3)),
        "loadimg" => ("gfx_loadimg", Outs::Value),
        "blit" => ("gfx_blit", Outs::Value),
        "blitext" => ("gfx_blitext", Outs::Value),
        "setimgdim" => ("gfx_setimgdim", Outs::Value),
        "getimgdim" => ("gfx_getimgdim", Outs::Vars(2)),
        "blurto" => ("gfx_blurto", Outs::Value),
        "muladdrect" => ("gfx_muladdrect", Outs::Value),
        "showmenu" => ("gfx_showmenu", Outs::Value),
        "getsyscol" => ("gfx_getsyscol", Outs::Value),
        "setcursor" | "getdropfile" | "deltablit" | "transformblit" => ("gfx_setcursor", Outs::Value),
        _ => return None,
    })
}

/// Calls an EEL `gfx_*` with Lua's arguments: numbers through variables, strings through named
/// strings, so the call compiles once per shape.
fn gfx_call(g: &mut Gfx, eel: &str, outs: Outs, args: &[Value]) -> Result<Vec<f64>, String> {
    let mut params = Vec::new();
    for (i, v) in args.iter().enumerate() {
        match v {
            Value::String(s) => {
                let h = g.machine.named_string(&format!("#__s{i}"));
                g.machine.set_string_at(h, s.to_string_lossy());
                params.push(format!("#__s{i}"));
            }
            v => {
                let x = match v {
                    Value::Integer(i) => *i as f64,
                    Value::Number(x) => *x,
                    Value::Boolean(b) => *b as u8 as f64,
                    _ => 0.0,
                };
                let slot = g.machine.vars.slot(&format!("__a{i}"));
                g.machine.vars.put(slot, x);
                params.push(format!("__a{i}"));
            }
        }
    }
    match outs {
        Outs::Vars(n) => params.extend((0..n).map(|i| format!("__o{i}"))),
        Outs::ValueAndString => params.push("#__o".to_owned()),
        Outs::Value => {}
    }
    let src = format!("__r = {eel}({});", params.join(", "));
    if !g.compiled.contains_key(&src) {
        let code = compile(&src, &mut g.machine.vars, &mut HashMap::new())?;
        g.compiled.insert(src.clone(), code);
    }
    g.machine.run(&g.compiled[&src]);
    let get = |m: &Machine, n: &str| m.vars.get(n).unwrap_or(0.0);
    Ok(match outs {
        Outs::Vars(n) => (0..n).map(|i| get(&g.machine, &format!("__o{i}"))).collect(),
        _ => vec![get(&g.machine, "__r")],
    })
}

fn gfx_table(lua: &Lua, gfx: &Rc<RefCell<Gfx>>) -> mlua::Result<Table> {
    let functions = lua.create_table()?;
    let names = [
        "init",
        "quit",
        "update",
        "dock",
        "getchar",
        "set",
        "line",
        "lineto",
        "rect",
        "rectto",
        "circle",
        "arc",
        "roundrect",
        "triangle",
        "gradrect",
        "drawstr",
        "drawnumber",
        "drawchar",
        "printf",
        "measurestr",
        "measurechar",
        "setfont",
        "getfont",
        "setpixel",
        "getpixel",
        "loadimg",
        "blit",
        "blitext",
        "setimgdim",
        "getimgdim",
        "blurto",
        "muladdrect",
        "showmenu",
        "getsyscol",
        "setcursor",
        "getdropfile",
        "deltablit",
        "transformblit",
    ];
    for name in names {
        let (eel, outs) = gfx_function(name).unwrap();
        let g = gfx.clone();
        let f = lua.create_function(move |lua, args: MultiValue| {
            let mut args: Vec<Value> = args.into_iter().collect();
            while matches!(args.last(), Some(Value::Nil)) {
                args.pop();
            }
            if eel == "gfx_printf" {
                // Lua formats the text itself; EEL draws it.
                let text: String = lua
                    .globals()
                    .get::<Table>("string")?
                    .get::<Function>("format")?
                    .call(MultiValue::from_iter(args))?;
                args = vec![Value::String(lua.create_string(&text)?)];
                return gfx_call(&mut g.borrow_mut(), "gfx_drawstr", Outs::Value, &args)
                    .map(|_| MultiValue::new())
                    .map_err(mlua::Error::runtime);
            }
            let mut g = g.borrow_mut();
            let values = gfx_call(&mut g, eel, outs, &args).map_err(mlua::Error::runtime)?;
            let mut out: Vec<Value> = values.into_iter().map(Value::Number).collect();
            if let Outs::ValueAndString = outs {
                let h = g.machine.named_string("#__o");
                out.push(Value::String(lua.create_string(g.machine.string_at(h))?));
            }
            Ok(MultiValue::from_iter(out))
        })?;
        functions.set(name, f)?;
    }
    let table = lua.create_table()?;
    let meta = lua.create_table()?;
    let (g, fns) = (gfx.clone(), functions.clone());
    meta.set(
        "__index",
        lua.create_function(move |_, (_, key): (Table, String)| {
            if let Ok(f @ Value::Function(_)) = fns.get::<Value>(key.as_str()) {
                return Ok(f);
            }
            Ok(match gfx_var(&key) {
                Some(v) => Value::Number(g.borrow().machine.vars.get(&v).unwrap_or(0.0)),
                None => Value::Nil,
            })
        })?,
    )?;
    let g = gfx.clone();
    meta.set(
        "__newindex",
        lua.create_function(move |_, (t, key, value): (Table, String, Value)| {
            match gfx_var(&key) {
                Some(v) => {
                    let x = match value {
                        Value::Integer(i) => i as f64,
                        Value::Number(x) => x,
                        _ => 0.0,
                    };
                    let mut g = g.borrow_mut();
                    let slot = g.machine.vars.slot(&v);
                    g.machine.vars.put(slot, x);
                }
                None => t.raw_set(key, value)?,
            }
            Ok(())
        })?,
    )?;
    table.set_metatable(Some(meta))?;
    Ok(table)
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
            assert(reaper.GetTrack(0, 1) == reaper.GetMediaItem_Track(right), "pointers compare equal")
            reaper.Undo_EndBlock("Tidy up", -1)
        "#;
        let (out, _) = run(script, "test.lua", &project(), ScriptContext::default()).unwrap();
        assert_tidied(&out);
    }

    // The same calls in REAPER 7.73 (agent probes): volume follows in dB, reversed the other
    // way, pan by offset, mute as the same state; CSurf without gang moves only the track.
    #[test]
    fn group_leads_carry_edits_over() {
        let script = r#"
            reaper.InsertTrackAtIndex(2, false)
            local lead, a, b = reaper.GetTrack(0, 0), reaper.GetTrack(0, 1), reaper.GetTrack(0, 2)
            assert(reaper.GetSetTrackGroupMembership(lead, "VOLUME_LEAD", 1, 1) == 0)
            reaper.GetSetTrackGroupMembership(lead, "MUTE_LEAD", 1, 1)
            reaper.GetSetTrackGroupMembership(a, "VOLUME_FOLLOW", 1, 1)
            reaper.GetSetTrackGroupMembership(a, "MUTE_FOLLOW", 1, 1)
            reaper.GetSetTrackGroupMembershipHigh(b, "VOLUME_FOLLOW", 1, 1)
            assert(reaper.GetSetTrackGroupMembershipEx(b, "VOLUME_FOLLOW", 32, 0, 0) == 1)
            reaper.GetSetTrackGroupMembership(b, "VOLUME_FOLLOW", 1, 1)
            reaper.GetSetTrackGroupMembership(b, "VOLUME_REVERSE", 1, 1)
            reaper.SetMediaTrackInfo_Value(a, "D_VOL", 0.5)
            reaper.SetMediaTrackInfo_Value(b, "D_VOL", 0.5)
            assert(reaper.CSurf_OnVolumeChangeEx(lead, 0.5, false, true) == 0.5)
            assert(reaper.GetMediaTrackInfo_Value(a, "D_VOL") == 0.25)
            assert(math.abs(reaper.GetMediaTrackInfo_Value(b, "D_VOL") - 1) < 1e-12)
            reaper.CSurf_OnVolumeChangeEx(lead, 1, false, false)
            assert(reaper.GetMediaTrackInfo_Value(a, "D_VOL") == 0.25)
            assert(math.abs(reaper.SetTrackUIVolume(a, 6, true, true, 0) - 0.25 * 10 ^ 0.3) < 1e-12)
            assert(reaper.SetTrackUIMute(lead, -1, 0) == 1)
            assert(reaper.GetMediaTrackInfo_Value(a, "B_MUTE") == 1)
            assert(reaper.CSurf_OnSoloChangeEx(b, 1, false) == true)
            assert(reaper.GetMediaTrackInfo_Value(b, "I_SOLO") == 2)
            reaper.SetMediaTrackInfo_Value(b, "I_PANMODE", 5)
            reaper.SetMediaTrackInfo_Value(b, "D_WIDTH", 0.5)
            reaper.SetMediaTrackInfo_Value(b, "B_PHASE", 1)
            assert(reaper.GetMediaTrackInfo_Value(b, "I_PANMODE") == 5 and reaper.GetMediaTrackInfo_Value(b, "D_WIDTH") == 0.5)
            reaper.GetSetProjectInfo_String(0, "TRACK_GROUP_NAME:1", "Kit", true)
        "#;
        let (out, _) = run(script, "groups.lua", &project(), ScriptContext::default()).unwrap();
        let p = out.project.unwrap();
        assert_eq!(p.group_name(0), Some("Kit"));
        assert!(p.tracks[2].phase && p.tracks[2].pan_mode == Some(scythe_core::fader::PanMode::Stereo));
    }

    #[test]
    fn errors_name_the_line_and_change_nothing() {
        let err = run("reaper.ShowConsoleMsg('a')\nnot_a_function()", "x.lua", &project(), ScriptContext::default())
            .unwrap_err();
        assert!(err.starts_with('a'), "the console comes first: {err}");
        assert!(err.contains("x.lua\"]:2") || err.contains("x.lua:2"), "{err}");
        let err = run("reaper.GetTrackName(nil)", "t.lua", &project(), ScriptContext::default()).unwrap_err();
        assert!(err.contains("bad argument #1 to 'GetTrackName' (MediaTrack expected)"), "{err}");
        let err = run("reaper.GetTrackName(reaper.GetMediaItem(0, 0))", "t.lua", &project(), ScriptContext::default())
            .unwrap_err();
        assert!(err.contains("(MediaTrack expected)"), "{err}");
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

    #[test]
    fn sends_envelopes_takes_and_midi() {
        let script = r#"
            local a, b = reaper.GetTrack(0, 0), reaper.GetTrack(0, 1)
            assert(reaper.CreateTrackSend(a, b) == 0)
            reaper.SetTrackSendInfo_Value(a, 0, 0, "D_VOL", 0.5)
            assert(reaper.GetTrackSendInfo_Value(b, -1, 0, "D_VOL") == 0.5)
            assert(reaper.GetTrackSendInfo_Value(a, 0, 0, "P_DESTTRACK") == b)
            local item = reaper.CreateNewMIDIItemInProj(b, 0, 2, false)
            local take = reaper.GetActiveTake(item)
            assert(reaper.TakeIsMIDI(take))
            reaper.MIDI_InsertNote(take, false, false, 0, 960, 0, 60, 100, false)
            local ok, sel, muted, s, e, ch, pitch, vel = reaper.MIDI_GetNote(take, 0)
            assert(ok and s == 0 and e == 960 and pitch == 60 and vel == 100, tostring(e))
            local _, n = reaper.MIDI_CountEvts(take)
            assert(n == 1)
            reaper.SetMediaItemSelected(item, true)
            assert(reaper.GetSelectedMediaItem(0, 0) == item)
            reaper.SetTrackSelected(a, true)
            assert(reaper.CountSelectedTracks(0) == 1)
            assert(reaper.ValidatePtr(take, "MediaItem_Take*") and not reaper.ValidatePtr(take, "MediaItem*"))
            local name = reaper.GetTakeName(take)
            assert(name == "", name)
            local r, g, bl = reaper.ColorFromNative(reaper.ColorToNative(10, 20, 30))
            assert(r == 10 and g == 20 and bl == 30)
            local start, stop = reaper.GetSet_LoopTimeRange(true, false, 1, 3, false)
            assert(start == 1 and stop == 3)
        "#;
        let (out, _) = run(script, "s.lua", &project(), ScriptContext::default()).unwrap();
        let p = out.project.unwrap();
        assert_eq!(p.tracks[0].sends[0].volume, 0.5);
        assert_eq!(p.time_selection, Some(scythe_core::TimeRange::new(1.0, 3.0)));
        let (tracks, items) = out.selection.unwrap();
        assert_eq!((tracks.len(), items.len()), (1, 1));
    }

    #[test]
    fn gfx_draws_in_a_window_until_it_is_closed() {
        let script = r#"
            gfx.init("Meter", 64, 32)
            local frames = 0
            local function loop()
                frames = frames + 1
                gfx.set(1, 0, 0)
                gfx.rect(0, 0, 10, 10)
                local w, h = gfx.measurestr("hi")
                assert(w > 0 and h > 0)
                gfx.x, gfx.y = 20, 4
                gfx.drawstr("hi")
                assert(gfx.x > 20)
                if gfx.getchar() >= 0 then reaper.defer(loop) else reaper.ShowConsoleMsg("closed after " .. frames) end
            end
            loop()
        "#;
        let (_, running) = run(script, "g.lua", &project(), ScriptContext::default()).unwrap();
        let mut running = running.expect("the loop deferred");
        let w = running.gfx_window().expect("a window");
        assert_eq!((w.title.as_str(), w.w, w.h, w.open), ("Meter", 64, 32, true));
        running.tick(&project(), ScriptContext::default()).unwrap();
        let frame = running.gfx_window().unwrap().frame.expect("a frame");
        assert_eq!((frame.w, frame.h), (64, 32));
        assert_eq!(frame.px[5 * 64 + 5], [255, 0, 0, 255]);
        let closed = ScriptContext { gfx_closed: true, ..Default::default() };
        let out = running.tick(&project(), closed).unwrap();
        assert_eq!(out.console, "closed after 3");
        assert!(!running.is_running());
    }
}
