//! EEL2 scripts: the API's names without `reaper.`, every parameter passed (outputs as
//! variables or `#strings`), pointers as numbers, and `defer("code")` taking code to compile.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use scythe_engine::eel::{Code, Function, Host, HostArgs, Machine, Vars, compile_with_host};

use super::{API, Dir, Engine, Kind, Shared, Val, call, from_pointer, to_pointer};

#[derive(Default)]
struct Queues {
    deferred: Vec<String>,
    at_exit: Vec<String>,
}

/// The host names: the API, then `defer`, `runloop` and `atexit`.
fn host_names() -> Vec<&'static str> {
    API.iter().map(|s| s.name).chain(["defer", "runloop", "atexit"]).collect()
}

struct ScriptHost {
    state: Shared,
    queues: Rc<RefCell<Queues>>,
}

impl Host for ScriptHost {
    fn call(&mut self, index: usize, args: &mut HostArgs) -> f64 {
        let Some(sig) = API.get(index) else {
            let code = args.string(0);
            let mut q = self.queues.borrow_mut();
            if index == API.len() + 2 {
                q.at_exit.push(code)
            } else {
                q.deferred.push(code)
            }
            return 1.0;
        };
        // EEL has no buffer sizes, and takes a string return as a first `#retval` parameter.
        let returns_string = sig.ret == Kind::Str;
        let slots: Vec<(Kind, Dir)> = returns_string
            .then_some((Kind::Str, Dir::Out))
            .into_iter()
            .chain(sig.params.iter().copied().filter(|(_, d)| *d != Dir::Size))
            .collect();
        let inputs: Vec<Val> = slots
            .iter()
            .enumerate()
            .filter(|(_, (_, d))| matches!(d, Dir::In | Dir::InOut))
            .map(|(i, (kind, _))| match kind {
                Kind::Str => Val::Str(args.string(i)),
                Kind::Track | Kind::Item => from_pointer(*kind, args.value(i) as u64),
                Kind::Proj | Kind::Void => Val::Nil,
                Kind::Bool => Val::Bool(args.value(i) != 0.0),
                Kind::Int => Val::Int(args.value(i) as i64),
                Kind::Num => Val::Num(args.value(i)),
            })
            .collect();
        let (ret, outs) = call(&mut self.state.borrow_mut(), sig, &inputs);
        let (ret, written) =
            if returns_string { (Val::Bool(true), std::iter::once(ret).chain(outs).collect()) } else { (ret, outs) };
        let out_slots = slots.iter().enumerate().filter(|(_, (_, d))| matches!(d, Dir::Out | Dir::InOut));
        for ((i, _), v) in out_slots.zip(written) {
            match v {
                Val::Nil => {}
                Val::Str(s) => args.set_string(i, s),
                v => args.set_value(i, number(&v)),
            }
        }
        number(&ret)
    }
}

fn number(v: &Val) -> f64 {
    match v {
        Val::Track(_) | Val::Item(_) => to_pointer(v) as f64,
        v => v.num(),
    }
}

pub(super) struct EelEngine {
    machine: Machine,
    functions: HashMap<String, Function>,
    /// Deferred code by its text, compiled once.
    compiled: HashMap<String, Code>,
    queues: Rc<RefCell<Queues>>,
    state: Shared,
    name: String,
}

pub(super) fn start(source: &str, name: &str, state: &Shared) -> Result<EelEngine, String> {
    let mut machine = Machine::new(Vars::default());
    let mut functions = HashMap::new();
    let code =
        compile_with_host(source, &mut machine.vars, &mut functions, &host_names()).map_err(|e| {
            match error_line(source, &e) {
                Some(line) => format!("{name}:{line}: {e}"),
                None => format!("{name}: {e}"),
            }
        })?;
    let mut engine = EelEngine {
        machine,
        functions,
        compiled: HashMap::new(),
        queues: Default::default(),
        state: state.clone(),
        name: name.to_owned(),
    };
    engine.machine.run_hosted(&code, engine.host());
    Ok(engine)
}

/// The line a compile error is on: the first whose source up to it fails the same way.
fn error_line(source: &str, err: &str) -> Option<usize> {
    let names = host_names();
    let lines: Vec<&str> = source.lines().collect();
    (1..=lines.len()).find(|&n| {
        let prefix = lines[..n].join("\n");
        compile_with_host(&prefix, &mut Vars::default(), &mut HashMap::new(), &names).err().as_deref() == Some(err)
    })
}

impl EelEngine {
    fn host(&self) -> Box<dyn Host> {
        Box::new(ScriptHost { state: self.state.clone(), queues: self.queues.clone() })
    }

    fn run_code(&mut self, src: String) -> Result<(), String> {
        if !self.compiled.contains_key(&src) {
            let code = compile_with_host(&src, &mut self.machine.vars, &mut self.functions, &host_names())
                .map_err(|e| format!("{}: {e} (in \"{src}\")", self.name))?;
            self.compiled.insert(src.clone(), code);
        }
        let host = self.host();
        self.machine.run_hosted(&self.compiled[&src], host);
        Ok(())
    }
}

impl Engine for EelEngine {
    fn pending(&self) -> bool {
        !self.queues.borrow().deferred.is_empty()
    }

    fn run_deferred(&mut self) -> Result<(), String> {
        let due = std::mem::take(&mut self.queues.borrow_mut().deferred);
        due.into_iter().try_for_each(|code| self.run_code(code))
    }

    fn run_at_exit(&mut self) -> Result<(), String> {
        let at_exit = {
            let mut q = self.queues.borrow_mut();
            q.deferred.clear();
            std::mem::take(&mut q.at_exit)
        };
        at_exit.into_iter().try_for_each(|code| self.run_code(code))
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{assert_tidied, project};
    use super::super::*;

    #[test]
    fn edits_tracks_items_and_markers() {
        let script = r#"
            Undo_BeginBlock();
            i = 0;
            loop(CountTracks(0),
                tr = GetTrack(0, i);
                GetTrackName(tr, #name);
                ShowConsoleMsg(#name);
                ShowConsoleMsg("\n");
                SetMediaTrackInfo_Value(tr, "D_VOL", 0.5);
                i += 1;
            );
            item = GetMediaItem(0, 0);
            SetMediaItemInfo_Value(item, "D_POSITION", 2);
            right = SplitMediaItem(item, 3);
            right == 0 ? ShowConsoleMsg("no split");
            AddProjectMarker(0, 0, 1.5, 0, "Verse", -1);
            CountProjectMarkers(0, markers, regions) != 1 || markers != 1 || regions != 0 ? ShowConsoleMsg("bad count");
            EnumProjectMarkers(0, isrgn, pos, end, #mname, idx);
            isrgn != 0 || pos != 1.5 || strcmp(#mname, "Verse") != 0 ? ShowConsoleMsg("bad marker");
            GetAppVersion(#version);
            match("7.73*", #version) ? 0 : ShowConsoleMsg("bad version");
            InsertTrackAtIndex(0, 1);
            #click = "Click";
            GetSetMediaTrackInfo_String(GetTrack(0, 0), "P_NAME", #click, 1);
            GetTrack(0, 9) != 0 ? ShowConsoleMsg("not null");
            GetMediaItem_Track(right) != GetTrack(0, 1) ? ShowConsoleMsg("bad track");
            GetMediaTrackInfo_Value(right, "D_VOL") != 0 ? ShowConsoleMsg("an item is no track");
            Undo_EndBlock("Tidy up", -1);
        "#;
        let (out, running) = run(script, "test.eel", &project(), ScriptContext::default()).unwrap();
        assert!(running.is_none());
        assert_tidied(&out);
    }

    #[test]
    fn errors_name_the_line_and_change_nothing() {
        let err = run("x = 1;\nnot_a_function();", "x.eel", &project(), ScriptContext::default()).unwrap_err();
        assert_eq!(err, "x.eel:2: unknown function not_a_function()");
        let err = run("x = 1;\ny = (2;\nz = 3;", "p.eel", &project(), ScriptContext::default()).unwrap_err();
        assert!(err.starts_with("p.eel:"), "{err}");
        let (out, _) = run("n = CountTracks(0);", "y.eel", &project(), ScriptContext::default()).unwrap();
        assert!(out.project.is_none());
    }

    #[test]
    fn deferred_code_runs_each_tick_until_it_stops() {
        let script = r#"
            function step() (
                n += 1;
                sprintf(#msg, "%d@%.1f ", n, GetPlayPosition());
                ShowConsoleMsg(#msg);
                n < 3 ? defer("step()");
            );
            function done() ( SetMediaTrackInfo_Value(GetTrack(0, 0), "D_VOL", 0.25); );
            atexit("done()");
            defer("step();");
        "#;
        let (out, running) = run(script, "d.eel", &project(), ScriptContext::default()).unwrap();
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
