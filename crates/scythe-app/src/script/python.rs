//! Python scripts: `from reaper_python import *`, then `RPR_Name(every parameter)`, returning
//! the value alone, or with outputs a tuple of the value and every parameter as the call left
//! it. Pointers are strings such as `(MediaTrack*)0x0000010000000002`, as REAPER's are.
//!
//! The script runs in a `python3` child process. Scythe hands it a `reaper_python` module whose
//! functions send each call as a JSON line on the child's stdout and read the answer from its
//! stdin, so the calls run here against the script's state like Lua's and EEL's.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

use super::{API, Engine, Kind, Shared, Val, call, from_pointer, to_pointer};

/// Runs in the child: sets up the module, runs the script, then deferred code on request.
/// Stdout carries the calls, so `print` is sent as a message for Scythe to write to its own
/// stdout, where REAPER's goes. As in REAPER, what a run writes to stderr is shown only above
/// the traceback when the run fails.
const RUNNER: &str = r#"
import io, json, os, sys, traceback, types
_in, _out = sys.stdin, sys.stdout
sys.stdin = open(os.devnull)

def send(msg):
    _out.write(json.dumps(msg) + "\n")
    _out.flush()

class Out(io.TextIOBase):
    def writable(self):
        return True
    def write(self, s):
        if s:
            send({"out": s})
        return len(s)

sys.stdout = Out()

def recv():
    line = _in.readline()
    if not line:
        os._exit(0)
    return json.loads(line)

init = recv()
rp = types.ModuleType("reaper_python")
rp._send, rp._recv = send, recv
exec(init["module"], rp.__dict__)
sys.modules["reaper_python"] = rp
name = init["name"]
rp._globals.update({"__name__": "__main__", "__file__": name, "__builtins__": __builtins__})

def report(e):
    tb = e.__traceback__
    while tb is not None and tb.tb_frame.f_code.co_filename != name:
        tb = tb.tb_next
    return "".join(traceback.format_exception(type(e), e, tb))

def run(fns):
    sys.stderr = io.StringIO()
    for f in fns:
        try:
            f()
        except SystemExit:
            pass
        except BaseException as e:
            return sys.stderr.getvalue() + report(e)
    return None

error = run([lambda: exec(compile(init["source"], name, "exec"), rp._globals)])
while error is None:
    send({"done": len(rp._deferred), "error": None})
    msg = recv()
    if msg.get("exit"):
        rp._deferred.clear()
        due, rp._at_exit[:] = list(rp._at_exit), []
        send({"done": 0, "error": run(due)})
        sys.exit(0)
    due, rp._deferred[:] = list(rp._deferred), []
    error = run(due)
send({"done": 0, "error": error})
"#;

/// The part of `reaper_python` that is not one function per API call.
const MODULE: &str = r#"
_deferred = []
_at_exit = []
_globals = {}

def _call(name, args):
    _send({"call": name, "args": list(args)})
    reply = _recv()
    if "error" in reply:
        raise RuntimeError(reply["error"])
    r = reply["ret"]
    return tuple(r) if isinstance(r, list) else r

def _code(code):
    if callable(code):
        return code
    return lambda: exec(code, _globals)

def RPR_defer(code):
    _deferred.append(_code(code))
    return True

RPR_runloop = RPR_defer

def RPR_atexit(code):
    _at_exit.append(_code(code))
    return True
"#;

/// Functions REAPER's Python module answers with a tuple although they have no outputs: their
/// optional `noSort` is a pointer in C (seen for `RPR_MIDI_InsertNote` in REAPER 7.73).
const TUPLE_WITHOUT_OUTPUTS: &[&str] = &["MIDI_InsertNote", "MIDI_SetNote", "MIDI_SetCC"];

fn module() -> String {
    let mut src = MODULE.to_owned();
    for sig in API {
        let params: Vec<String> = (0..sig.params.len()).map(|i| format!("p{i}")).collect();
        let tuple = if params.len() == 1 { "p0,".to_owned() } else { params.join(", ") };
        src +=
            &format!("\ndef RPR_{}({}):\n    return _call({:?}, ({tuple}))\n", sig.name, params.join(", "), sig.name);
    }
    src
}

pub(super) struct PythonEngine {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    state: Shared,
    pending: bool,
    finished: bool,
}

pub(super) fn start(source: &str, name: &str, state: &Shared) -> Result<PythonEngine, String> {
    let python = if cfg!(windows) { "python" } else { "python3" };
    let mut child = Command::new(python)
        .args(["-u", "-c", RUNNER])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                format!("Python ReaScripts need Python 3: `{python}` was not found on the PATH.")
            }
            _ => format!("Could not start {python}: {e}"),
        })?;
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err("Could not talk to Python".to_owned());
    };
    let mut engine = PythonEngine {
        child,
        stdin,
        stdout: BufReader::new(stdout),
        state: state.clone(),
        pending: false,
        finished: false,
    };
    engine.converse(&json!({"module": module(), "source": source, "name": name}))?;
    Ok(engine)
}

impl PythonEngine {
    /// Sends `msg`, then answers the script's calls until it has done what `msg` asked.
    fn converse(&mut self, msg: &Value) -> Result<(), String> {
        self.send(msg)?;
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).map_err(|e| e.to_string())?;
            if read == 0 {
                self.finished = true;
                self.pending = false;
                return Err("Python exited unexpectedly".to_owned());
            }
            let msg: Value = serde_json::from_str(&line).map_err(|e| format!("Bad message from Python: {e}"))?;
            if let Some(text) = msg.get("out").and_then(Value::as_str) {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
                continue;
            }
            if let Some(name) = msg.get("call").and_then(Value::as_str) {
                let args = msg.get("args").and_then(Value::as_array).cloned().unwrap_or_default();
                let reply = self.answer(name, args);
                self.send(&reply)?;
                continue;
            }
            self.pending = msg.get("done").and_then(Value::as_u64).unwrap_or(0) > 0;
            if let Some(err) = msg.get("error").and_then(Value::as_str) {
                self.finished = true;
                self.pending = false;
                return Err(err.trim_end().to_owned());
            }
            return Ok(());
        }
    }

    fn send(&mut self, msg: &Value) -> Result<(), String> {
        writeln!(self.stdin, "{msg}")
            .and_then(|()| self.stdin.flush())
            .map_err(|e| format!("Python stopped listening: {e}"))
    }

    fn answer(&mut self, name: &str, mut args: Vec<Value>) -> Value {
        let Some(sig) = API.iter().find(|s| s.name == name) else {
            return json!({"error": format!("RPR_{name} is not supported")});
        };
        args.resize(sig.params.len(), Value::Null);
        let inputs: Vec<Val> = sig.inputs().map(|(i, kind)| from_json(kind, &args[i])).collect();
        let (ret, outs) = call(&mut self.state.borrow_mut(), sig, &inputs);
        if sig.outputs().next().is_none() && !TUPLE_WITHOUT_OUTPUTS.contains(&sig.name) {
            return json!({"ret": to_json(sig.ret, &ret)});
        }
        for ((i, kind), v) in sig.outputs().zip(outs) {
            if v != Val::Nil {
                args[i] = to_json(kind, &v);
            }
        }
        if sig.ret != Kind::Void {
            args.insert(0, to_json(sig.ret, &ret));
        }
        json!({"ret": args})
    }
}

fn from_json(kind: Kind, v: &Value) -> Val {
    let num = || v.as_f64().or_else(|| v.as_bool().map(|b| b as u8 as f64));
    match kind {
        Kind::Proj | Kind::Void => Val::Nil,
        k if k.is_pointer() => {
            let pointer = v.as_str().and_then(|s| {
                let (ty, hex) = s.strip_prefix('(')?.split_once(")0x")?;
                (k == Kind::Ptr || ty == kind.pointer_type()).then(|| u64::from_str_radix(hex, 16).ok())?
            });
            pointer.map_or(Val::Nil, |p| from_pointer(kind, p))
        }
        Kind::Str => match v {
            Value::String(s) => Val::Str(s.clone()),
            Value::Null => Val::Nil,
            v => Val::Str(v.to_string()),
        },
        Kind::Bool => num().map_or(Val::Nil, |x| Val::Bool(x != 0.0)),
        Kind::Int => v.as_i64().map(Val::Int).or_else(|| num().map(|x| Val::Int(x as i64))).unwrap_or(Val::Nil),
        _ => num().map_or(Val::Nil, Val::Num),
    }
}

/// A value as REAPER's Python module returns it: booleans as 0 or 1.
fn to_json(kind: Kind, v: &Val) -> Value {
    match (kind, v) {
        (k, v) if k.is_pointer() => {
            json!(format!("({})0x{:016X}", kind.pointer_type(), to_pointer(v)))
        }
        (Kind::Void, _) => Value::Null,
        (_, Val::Nil) => to_json(kind, &kind.zero()),
        (_, Val::Bool(b)) => json!(*b as u8),
        (_, Val::Int(i)) => json!(i),
        (_, Val::Num(x)) => json!(x),
        (_, Val::Str(s)) => json!(s),
        (_, v) => json!(to_pointer(v)),
    }
}

impl Engine for PythonEngine {
    fn pending(&self) -> bool {
        self.pending
    }

    fn run_deferred(&mut self) -> Result<(), String> {
        self.converse(&json!({"tick": true}))
    }

    fn run_at_exit(&mut self) -> Result<(), String> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        let result = self.converse(&json!({"exit": true}));
        self.pending = false;
        result
    }
}

impl Drop for PythonEngine {
    fn drop(&mut self) {
        if !self.finished || self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{assert_tidied, project};
    use super::super::*;

    /// Runs a Python script, or `None` when there is no Python to run it with.
    fn run_py(src: &str, name: &str) -> Option<Result<(ScriptOutcome, Option<RunningScript>), ScriptError>> {
        match run(src, name, &project(), ScriptContext::default()) {
            Err(e) if e.contains("was not found on the PATH") => {
                eprintln!("skipped: {e}");
                None
            }
            r => Some(r),
        }
    }

    #[test]
    fn edits_tracks_items_and_markers() {
        let script = r#"
from reaper_python import *

RPR_Undo_BeginBlock()
for i in range(RPR_CountTracks(0)):
    tr = RPR_GetTrack(0, i)
    (ok, _, name, _) = RPR_GetTrackName(tr, "", 512)
    RPR_ShowConsoleMsg(name + "\n")
    RPR_SetMediaTrackInfo_Value(tr, "D_VOL", 0.5)
item = RPR_GetMediaItem(0, 0)
RPR_SetMediaItemInfo_Value(item, "D_POSITION", 2)
right = RPR_SplitMediaItem(item, 3)
assert right.startswith("(MediaItem*)0x") and right != "(MediaItem*)0x0000000000000000", right
RPR_AddProjectMarker(0, False, 1.5, 0, "Verse", -1)
assert RPR_CountProjectMarkers(0, 0, 0) == (1, 0, 1, 0)
assert RPR_EnumProjectMarkers(0, 0, 0, 0, "", 0) == (1, 0, 0, 1.5, 0.0, "Verse", 1)
assert RPR_GetAppVersion().startswith("7.73")
RPR_InsertTrackAtIndex(0, True)
assert RPR_GetSetMediaTrackInfo_String(RPR_GetTrack(0, 0), "P_NAME", "Click", True)[0] == 1
assert RPR_GetTrack(0, 9) == "(MediaTrack*)0x0000000000000000"
assert RPR_GetMediaItemInfo_Value(right, "D_POSITION") == 3.0
assert RPR_SetMediaTrackInfo_Value(right, "D_VOL", 1) == 0
assert RPR_ShowConsoleMsg("") is None
RPR_Undo_EndBlock("Tidy up", -1)
"#;
        let Some(result) = run_py(script, "test.py") else { return };
        let (out, running) = result.unwrap();
        assert!(running.is_none());
        assert_tidied(&out);
    }

    #[test]
    fn errors_name_the_line() {
        let script = "from reaper_python import *\nimport sys\nRPR_ShowConsoleMsg('a')\nprint('to stdout')\nsys.stderr.write('warned\\n')\nnot_a_function()";
        let Some(result) = run_py(script, "x.py") else {
            return;
        };
        let err = result.unwrap_err();
        assert!(err.starts_with("awarned\nTraceback"), "the console, then stderr, then the traceback: {err}");
        assert!(!err.contains("to stdout"), "{err}");
        assert!(err.contains("File \"x.py\", line 6"), "{err}");
        assert!(err.ends_with("NameError: name 'not_a_function' is not defined"), "{err}");
        let err = run_py("x = (", "s.py").unwrap().unwrap_err();
        assert!(err.contains("s.py") && err.contains("SyntaxError"), "{err}");
        let quiet =
            "from reaper_python import *\nimport sys\nprint('n', RPR_CountTracks(0))\nsys.stderr.write('dropped')";
        let (out, _) = run_py(quiet, "y.py").unwrap().unwrap();
        assert!(out.project.is_none());
        assert!(out.console.is_empty(), "print and stderr stay out of the console: {:?}", out.console);
    }

    #[test]
    fn a_failed_script_keeps_what_it_changed_before_the_error() {
        let script = "from reaper_python import *\nRPR_InsertTrackAtIndex(0, True)\nraise RuntimeError('boom')";
        let Some(result) = run_py(script, "x.py") else {
            return;
        };
        let err = result.unwrap_err();
        assert!(err.ends_with("RuntimeError: boom"), "{err}");
        assert_eq!(err.outcome.project.unwrap().tracks.len(), 3);
    }

    #[test]
    fn takes_envelopes_and_markers() {
        let script = r#"
from reaper_python import *

a, b = RPR_GetTrack(0, 0), RPR_GetTrack(0, 1)
item = RPR_CreateNewMIDIItemInProj(b, 0.0, 2.0, False)
take = RPR_GetActiveTake(item)
assert take.startswith("(MediaItem_Take*)0x"), take
assert RPR_MIDI_InsertNote(take, False, False, 0, 480, 0, 64, 90, False)[0] == 1
r = RPR_MIDI_GetNote(take, 0, 0, 0, 0, 0, 0, 0, 0)
assert r[0] == 1 and r[5:9] == (0.0, 480.0, 0, 64), r
assert RPR_MIDI_CountEvts(take, 0, 0, 0) == (1, take, 1, 0, 0)
assert RPR_ValidatePtr(take, "MediaItem_Take*") == 1
assert RPR_ValidatePtr(take, "MediaTrack*") == 0
RPR_SetMediaTrackInfo_Value(a, "I_SELECTED", 1)
assert RPR_GetSelectedTrack(0, 0) == a
assert RPR_AddProjectMarker2(0, True, 1.0, 3.0, "Chorus", -1, RPR_ColorToNative(255, 0, 0) | 0x1000000) == 1
r = RPR_EnumProjectMarkers3(0, 0, 0, 0, 0, "", 0, 0)
assert r[:2] == (1, 0) and r[3:] == (1, 1.0, 3.0, "Chorus", 1, RPR_ColorToNative(255, 0, 0) | 0x1000000), r
assert RPR_GetSet_LoopTimeRange(True, False, 1.5, 2.5, False) == (True, False, 1.5, 2.5, False)
assert RPR_GetExtState("ScythePythonTest", "nope") == ""
assert RPR_GetProjExtState(0, "ScythePythonTest", "k", "", 64)[0] == 0
proj = RPR_EnumProjects(-1, "", 512)[0]
assert proj.startswith("(ReaProject*)0x"), proj
"#;
        let Some(result) = run_py(script, "t.py") else { return };
        let (out, _) = result.unwrap();
        let p = out.project.unwrap();
        assert_eq!(p.markers[0].name, "Chorus");
        assert_eq!(out.selection.unwrap().0.len(), 1);
    }

    #[test]
    fn deferred_code_runs_each_tick_until_it_stops() {
        let script = r#"
from reaper_python import *
n = 0
def step():
    global n
    n += 1
    RPR_ShowConsoleMsg("%d@%.1f " % (n, RPR_GetPlayPosition()))
    if n < 3:
        RPR_defer("step()")
def done():
    RPR_SetMediaTrackInfo_Value(RPR_GetTrack(0, 0), "D_VOL", 0.25)
RPR_atexit("done()")
RPR_defer("step()")
"#;
        let Some(result) = run_py(script, "d.py") else { return };
        let (out, running) = result.unwrap();
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
