//! EEL2 scripts: the API's names without `reaper.`, every parameter passed (outputs as
//! variables or `#strings`), pointers as numbers, and `defer("code")` taking code to compile.
//! Besides the API there are EEL2's own ReaScript functions: `eval`, `extension_api`, the file
//! functions (`fopen` …) and the TCP ones (`tcp_connect` …); `gfx_*` draw in the script's
//! window.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::rc::Rc;

use scythe_engine::eel::{Code, Function, Host, HostArgs, Machine, Vars, compile_with_host};

use super::{API, Dir, Engine, GfxWindow, Kind, ScriptGfx, Shared, Sig, Val, call, from_pointer, to_pointer};

#[derive(Default)]
struct Queues {
    deferred: Vec<String>,
    at_exit: Vec<String>,
}

/// EEL2's ReaScript functions, after the API in the host names.
const OWN: &[&str] = &[
    "defer",
    "runloop",
    "atexit",
    "eval",
    "extension_api",
    "fopen",
    "fclose",
    "fread",
    "fgets",
    "fgetc",
    "fwrite",
    "fprintf",
    "fseek",
    "ftell",
    "feof",
    "fflush",
    "tcp_listen",
    "tcp_listen_end",
    "tcp_connect",
    "tcp_send",
    "tcp_recv",
    "tcp_set_block",
    "tcp_close",
];

/// The host names: the API, then EEL2's own functions.
fn host_names() -> Vec<&'static str> {
    API.iter().map(|s| s.name).chain(OWN.iter().copied()).collect()
}

/// An open file and whether a read has reached its end.
struct OpenFile {
    file: std::fs::File,
    eof: bool,
}

/// What the script's EEL functions keep between calls.
#[derive(Default)]
struct Resources {
    files: Vec<Option<OpenFile>>,
    sockets: Vec<Option<TcpStream>>,
    listeners: HashMap<u16, TcpListener>,
}

impl Resources {
    fn file(&mut self, handle: f64) -> Option<&mut OpenFile> {
        self.files.get_mut((handle as usize).checked_sub(1)?)?.as_mut()
    }

    fn socket(&mut self, handle: f64) -> Option<&mut TcpStream> {
        self.sockets.get_mut((handle as usize).checked_sub(1)?)?.as_mut()
    }

    fn add_socket(&mut self, s: TcpStream) -> f64 {
        self.sockets.push(Some(s));
        self.sockets.len() as f64
    }
}

#[derive(Clone)]
struct ScriptHost {
    state: Shared,
    queues: Rc<RefCell<Queues>>,
    functions: Rc<RefCell<HashMap<String, Function>>>,
    resources: Rc<RefCell<Resources>>,
}

impl Host for ScriptHost {
    fn call(&mut self, index: usize, args: &mut HostArgs) -> f64 {
        match API.get(index) {
            Some(sig) => self.api(sig, args),
            None => self.own(OWN[index - API.len()], args),
        }
    }
}

impl ScriptHost {
    fn api(&mut self, sig: &Sig, args: &mut HostArgs) -> f64 {
        // EEL has no buffer sizes, and takes a string return as a first `#retval` parameter.
        let returns_string = sig.ret == Kind::Str;
        let slots: Vec<(Kind, Dir)> = returns_string
            .then_some((Kind::Str, Dir::Out))
            .into_iter()
            .chain(sig.params.iter().copied().filter(|(_, d)| *d != Dir::Size))
            .collect();
        let passed = args.len();
        let inputs: Vec<Val> = slots
            .iter()
            .enumerate()
            .filter(|(_, (_, d))| matches!(d, Dir::In | Dir::InOut))
            .map(|(at, (kind, _))| {
                match kind {
                    // Optional parameters left out are absent, not zero.
                    _ if at >= passed => Val::Nil,
                    Kind::Str => Val::Str(args.string(at)),
                    k if k.is_pointer() => from_pointer(*k, args.value(at) as u64),
                    Kind::Bool => Val::Bool(args.value(at) != 0.0),
                    Kind::Int => Val::Int(args.value(at) as i64),
                    _ => Val::Num(args.value(at)),
                }
            })
            .collect();
        let (ret, outs) = call(&mut self.state.borrow_mut(), sig, &inputs);
        let (ret, written) = if returns_string {
            (Val::Bool(ret != Val::Nil), std::iter::once(ret).chain(outs).collect())
        } else {
            (ret, outs)
        };
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

    fn own(&mut self, name: &str, args: &mut HostArgs) -> f64 {
        match name {
            "defer" | "runloop" | "atexit" => {
                let code = args.string(0);
                let mut q = self.queues.borrow_mut();
                if name == "atexit" {
                    q.at_exit.push(code)
                } else {
                    q.deferred.push(code)
                }
                1.0
            }
            "eval" => {
                let src = args.string(0);
                // Functions the code defines stay its own, as EEL2's do.
                let mut functions = self.functions.borrow().clone();
                let host = Box::new(self.clone());
                let m = args.machine();
                match compile_with_host(&src, &mut m.vars, &mut functions, &host_names()) {
                    Ok(code) => {
                        m.run_hosted(&code, host);
                        1.0
                    }
                    Err(_) => 0.0,
                }
            }
            // REAPER 7.73 reaches only functions extensions registered this way, not its own API,
            // and Scythe has no extensions.
            "extension_api" => 0.0,
            _ if name.starts_with("tcp_") => self.tcp(name, args),
            _ => self.file(name, args),
        }
    }

    fn file(&mut self, name: &str, args: &mut HostArgs) -> f64 {
        let mut res = self.resources.borrow_mut();
        if name == "fopen" {
            let (path, mode) = (args.string(0), args.string(1));
            let mut opts = std::fs::OpenOptions::new();
            match mode.trim_end_matches(['b', 't']) {
                "w" => opts.write(true).create(true).truncate(true),
                "a" => opts.append(true).create(true),
                "r+" => opts.read(true).write(true),
                "w+" => opts.read(true).write(true).create(true).truncate(true),
                "a+" => opts.read(true).append(true).create(true),
                _ => opts.read(true),
            };
            return match opts.open(path) {
                Ok(file) => {
                    res.files.push(Some(OpenFile { file, eof: false }));
                    res.files.len() as f64
                }
                Err(_) => 0.0,
            };
        }
        let handle = args.value(0);
        if name == "fclose" {
            return match res.files.get_mut((handle as usize).wrapping_sub(1)) {
                Some(f @ Some(_)) => {
                    *f = None;
                    0.0
                }
                _ => -1.0,
            };
        }
        let Some(f) = res.file(handle) else { return -1.0 };
        match name {
            "fread" => {
                let mut buf = vec![0; args.value(2).max(0.0) as usize];
                let n = read_full(f, &mut buf);
                args.set_string(1, String::from_utf8_lossy(&buf[..n]).into_owned());
                n as f64
            }
            "fgets" => {
                let mut line = Vec::new();
                let mut byte = [0u8];
                while read_full(f, &mut byte) == 1 {
                    line.push(byte[0]);
                    if byte[0] == b'\n' {
                        break;
                    }
                }
                let n = line.len();
                args.set_string(1, String::from_utf8_lossy(&line).into_owned());
                n as f64
            }
            "fgetc" => {
                let mut byte = [0u8];
                if read_full(f, &mut byte) == 1 { byte[0] as f64 } else { -1.0 }
            }
            "fwrite" | "fprintf" => {
                let text = if name == "fprintf" {
                    args.format(1)
                } else {
                    let s = args.string(1);
                    let len = args.value(2);
                    if len >= 1.0 { s.chars().take(len as usize).collect() } else { s }
                };
                match f.file.write_all(text.as_bytes()) {
                    Ok(()) => text.len() as f64,
                    Err(_) => -1.0,
                }
            }
            "fseek" => {
                let (offset, whence) = (args.value(1) as i64, args.value(2));
                let to = if whence < 0.0 {
                    SeekFrom::Start(offset.max(0) as u64)
                } else if whence > 0.0 {
                    SeekFrom::End(offset)
                } else {
                    SeekFrom::Current(offset)
                };
                f.eof = false;
                if f.file.seek(to).is_ok() { 0.0 } else { -1.0 }
            }
            "ftell" => f.file.stream_position().map_or(-1.0, |p| p as f64),
            "feof" => f.eof as u8 as f64,
            "fflush" => f.file.flush().map_or(-1.0, |()| 0.0),
            _ => 0.0,
        }
    }

    fn tcp(&mut self, name: &str, args: &mut HostArgs) -> f64 {
        let mut res = self.resources.borrow_mut();
        match name {
            "tcp_connect" => {
                let addr = format!("{}:{}", args.string(0), args.value(1) as u16);
                let Some(to) = std::net::ToSocketAddrs::to_socket_addrs(&addr).ok().and_then(|mut a| a.next()) else {
                    return -1.0;
                };
                match TcpStream::connect_timeout(&to, std::time::Duration::from_secs(5)) {
                    Ok(s) => {
                        let _ = s.set_nonblocking(args.len() > 2 && args.value(2) == 0.0);
                        res.add_socket(s)
                    }
                    Err(_) => -1.0,
                }
            }
            "tcp_listen" => {
                let port = args.value(0) as u16;
                if let std::collections::hash_map::Entry::Vacant(e) = res.listeners.entry(port) {
                    let interface = if args.len() > 1 { args.string(1) } else { String::new() };
                    let ip = if interface.is_empty() { "0.0.0.0".to_owned() } else { interface };
                    let Ok(l) = TcpListener::bind((ip.as_str(), port)) else { return -1.0 };
                    let _ = l.set_nonblocking(true);
                    e.insert(l);
                }
                match res.listeners[&port].accept() {
                    Ok((s, peer)) => {
                        let _ = s.set_nonblocking(false);
                        if args.len() > 2 {
                            args.set_string(2, peer.ip().to_string());
                        }
                        res.add_socket(s)
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => 0.0,
                    Err(_) => -1.0,
                }
            }
            "tcp_listen_end" => {
                res.listeners.remove(&(args.value(0) as u16));
                0.0
            }
            "tcp_close" => {
                if let Some(s) = res.sockets.get_mut((args.value(0) as usize).wrapping_sub(1)) {
                    *s = None;
                }
                0.0
            }
            _ => {
                let Some(s) = res.socket(args.value(0)) else { return -1.0 };
                match name {
                    "tcp_set_block" => {
                        let _ = s.set_nonblocking(args.value(1) == 0.0);
                        0.0
                    }
                    "tcp_send" => {
                        let text = args.string(1);
                        let len = args.value(2);
                        let bytes = text.as_bytes();
                        let bytes = if len >= 1.0 { &bytes[..(len as usize).min(bytes.len())] } else { bytes };
                        match s.write(bytes) {
                            Ok(n) => n as f64,
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => 0.0,
                            Err(_) => -1.0,
                        }
                    }
                    "tcp_recv" => {
                        let max = if args.len() > 2 && args.value(2) >= 1.0 { args.value(2) as usize } else { 4096 };
                        let mut buf = vec![0; max];
                        match s.read(&mut buf) {
                            Ok(0) => -1.0,
                            Ok(n) => {
                                args.set_string(1, String::from_utf8_lossy(&buf[..n]).into_owned());
                                n as f64
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => 0.0,
                            Err(_) => -1.0,
                        }
                    }
                    _ => 0.0,
                }
            }
        }
    }
}

/// Reads as much of `buf` as the file has, noting when it ran out.
fn read_full(f: &mut OpenFile, buf: &mut [u8]) -> usize {
    let mut n = 0;
    while n < buf.len() {
        match f.file.read(&mut buf[n..]) {
            Ok(0) | Err(_) => {
                f.eof = true;
                break;
            }
            Ok(k) => n += k,
        }
    }
    n
}

fn number(v: &Val) -> f64 {
    match v {
        Val::Nil | Val::Bool(_) | Val::Int(_) | Val::Num(_) | Val::Str(_) => v.num(),
        v => to_pointer(v) as f64,
    }
}

pub(super) struct EelEngine {
    machine: Machine,
    /// Deferred code by its text, compiled once.
    compiled: HashMap<String, Code>,
    host: ScriptHost,
    name: String,
    gfx: ScriptGfx,
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
    let host = ScriptHost {
        state: state.clone(),
        queues: Default::default(),
        functions: Rc::new(RefCell::new(functions)),
        resources: Default::default(),
    };
    let mut engine =
        EelEngine { machine, compiled: HashMap::new(), host, name: name.to_owned(), gfx: ScriptGfx::default() };
    engine.machine.run_hosted(&code, Box::new(engine.host.clone()));
    engine.gfx.end(&mut engine.machine);
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
    fn run_code(&mut self, src: String) -> Result<(), String> {
        if !self.compiled.contains_key(&src) {
            let mut functions = self.host.functions.borrow_mut();
            let code = compile_with_host(&src, &mut self.machine.vars, &mut functions, &host_names())
                .map_err(|e| format!("{}: {e} (in \"{src}\")", self.name))?;
            drop(functions);
            self.compiled.insert(src.clone(), code);
        }
        self.machine.run_hosted(&self.compiled[&src], Box::new(self.host.clone()));
        Ok(())
    }
}

impl Engine for EelEngine {
    fn pending(&self) -> bool {
        !self.host.queues.borrow().deferred.is_empty()
    }

    fn run_deferred(&mut self) -> Result<(), String> {
        let due = std::mem::take(&mut self.host.queues.borrow_mut().deferred);
        let ctx = self.host.state.borrow().ctx.clone();
        self.gfx.begin(&mut self.machine, &ctx);
        let result = due.into_iter().try_for_each(|code| self.run_code(code));
        self.gfx.end(&mut self.machine);
        result
    }

    fn run_at_exit(&mut self) -> Result<(), String> {
        let at_exit = {
            let mut q = self.host.queues.borrow_mut();
            q.deferred.clear();
            std::mem::take(&mut q.at_exit)
        };
        at_exit.into_iter().try_for_each(|code| self.run_code(code))
    }

    fn gfx_window(&mut self) -> Option<GfxWindow> {
        self.gfx.take()
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

    #[test]
    fn eval_extension_api_and_optional_arguments() {
        let script = r#"
            function twice(x) ( x * 2 );
            eval("v = twice(21)") != 1 || v != 42 ? ShowConsoleMsg("bad eval ");
            eval("(") != 0 ? ShowConsoleMsg("bad code evaluates ");
            #m = "from eval ";
            eval("ShowConsoleMsg(#m)");
            extension_api("CountTracks", 0) != 0 ? ShowConsoleMsg("REAPER's own API is no extension ");
            item = CreateNewMIDIItemInProj(GetTrack(0, 1), 0, 2);
            take = GetActiveTake(item);
            MIDI_InsertNote(take, 0, 0, 0, 960, 0, 60, 100);
            MIDI_SetNote(take, 0, 1);
            MIDI_GetNote(take, 0, sel, muted, s, e, ch, pitch, vel);
            sel != 1 || pitch != 60 || e != 960 ? ShowConsoleMsg("MIDI_SetNote changed more than asked ");
            GetTakeName(#tn, take) ? ShowConsoleMsg("named ");
        "#;
        let (out, _) = run(script, "e.eel", &project(), ScriptContext::default()).unwrap();
        assert_eq!(out.console, "from eval named ");
    }

    #[test]
    fn files_read_and_write() {
        let path = std::env::temp_dir().join(format!("scythe-eel-file-{}.txt", std::process::id()));
        let script = format!(
            r#"
            fp = fopen("{p}", "w");
            fprintf(fp, "%d tracks\n", CountTracks(0));
            fwrite(fp, "second line\nthird", 0);
            fclose(fp);
            fp = fopen("{p}", "r");
            fgets(fp, #a);
            fgets(fp, #b);
            ShowConsoleMsg(#a); ShowConsoleMsg(#b);
            pos = ftell(fp);
            fread(fp, #c, 100);
            feof(fp) ? ShowConsoleMsg("eof ");
            fseek(fp, 2, -1);
            fgetc(fp) == 't' ? ShowConsoleMsg("seek ");
            fclose(fp);
            fopen("/nonexistent/dir/x", "r") == 0 ? ShowConsoleMsg("missing");
            "#,
            p = path.display()
        );
        let (out, _) = run(&script, "f.eel", &project(), ScriptContext::default()).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(out.console, "2 tracks\nsecond line\neof seek missing");
    }

    #[test]
    fn tcp_connects_sends_and_receives() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 5];
            std::io::Read::read_exact(&mut s, &mut buf).unwrap();
            std::io::Write::write_all(&mut s, b"pong").unwrap();
            buf
        });
        let script = format!(
            r#"
            c = tcp_connect("127.0.0.1", {port});
            c > 0 ? tcp_send(c, "ping!");
            n = tcp_recv(c, #got);
            ShowConsoleMsg(#got);
            tcp_close(c);
            "#
        );
        let (out, _) = run(&script, "t.eel", &project(), ScriptContext::default()).unwrap();
        assert_eq!(&server.join().unwrap(), b"ping!");
        assert_eq!(out.console, "pong");
    }

    #[test]
    fn gfx_window_until_closed() {
        let script = r#"
            gfx_init("Scope", 40, 20);
            function frame() (
                gfx_set(0, 1, 0);
                gfx_rect(0, 0, 4, 4);
                gfx_getchar() >= 0 ? defer("frame()") : ShowConsoleMsg("closed");
            );
            frame();
        "#;
        let (_, running) = run(script, "g.eel", &project(), ScriptContext::default()).unwrap();
        let mut running = running.unwrap();
        assert_eq!(running.gfx_window().map(|w| (w.title, w.w, w.h)), Some(("Scope".into(), 40, 20)));
        running.tick(&project(), ScriptContext::default()).unwrap();
        let frame = running.gfx_window().unwrap().frame.unwrap();
        assert_eq!(frame.px[0], [0, 255, 0, 255]);
        let out = running.tick(&project(), ScriptContext { gfx_closed: true, ..Default::default() }).unwrap();
        assert_eq!(out.console, "closed");
        assert!(!running.gfx_window().unwrap().open);
    }
}
