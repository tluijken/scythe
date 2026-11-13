//! Helpers for the end-to-end tests: the real binaries started as processes with folders and
//! ports of their own, spoken to over HTTP, WebSocket, MCP and the Chrome DevTools Protocol.

pub mod cdp;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

pub const ADMIN: &str = "itest-admin-token";

pub fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// A binary of the workspace, from `SCYTHE_BIN_DIR` or `target/debug`.
pub fn binary(name: &str) -> PathBuf {
    let dir = std::env::var_os("SCYTHE_BIN_DIR").map(PathBuf::from).unwrap_or_else(|| workspace().join("target/debug"));
    let path = dir.join(name);
    assert!(
        path.is_file(),
        "{} is missing: run `devbox run -- scripts/cargo.sh build -p scythe-server -p scythe-app -p scythe-mcp` \
         first (or `devbox run itest`, which does)",
        path.display()
    );
    path
}

/// A fresh folder for one test under `target/itest/e2e` (or `SCYTHE_ITEST_DIR`).
pub fn test_dir(name: &str) -> PathBuf {
    let root =
        std::env::var_os("SCYTHE_ITEST_DIR").map(PathBuf::from).unwrap_or_else(|| workspace().join("target/itest"));
    let dir = root.join("e2e").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A port nobody listens on right now.
pub fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Copies the `live.RPP` fixture (Drums, Bass, Keys; songs "Opener" 0-4 s and "Ballad" 6-10 s,
/// each ending at a `!1016` stop marker) with its audio into `dir`.
pub fn live_fixture(dir: &Path) -> PathBuf {
    let path = dir.join("live.RPP");
    std::fs::write(&path, include_str!("../../scythe-app/tests/fixtures/itest/live.RPP")).unwrap();
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(dir.join("tone.wav"), spec).unwrap();
    for i in 0..48_000 * 4 {
        let s = (i as f32 * 220.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.25;
        w.write_sample((s * i16::MAX as f32) as i16).unwrap();
    }
    w.finalize().unwrap();
    path
}

/// A program to find on `PATH` or through the environment variable `var`.
pub fn find_tool(var: &str, name: &str) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| p.is_file())
}

/// A child process killed (and reaped) when dropped, its output in `<dir>/<name>.log`.
pub struct Proc {
    pub child: Child,
    pub log: PathBuf,
}

impl Proc {
    /// Starts `program` with the XDG folders inside `dir`, no Wayland and no window unless
    /// `display` names an X server.
    pub fn spawn(dir: &Path, name: &str, program: &Path, args: &[&str], display: Option<&str>) -> Self {
        Self::spawn_with(dir, name, Command::new(program), args, display)
    }

    /// Starts a program from outside the workspace (Xvfb, Chromium), without the devbox
    /// library path, which carries libraries newer than such a program's own.
    pub fn spawn_tool(dir: &Path, name: &str, program: &Path, args: &[&str]) -> Self {
        let mut command = Command::new(program);
        command.env_remove("LD_LIBRARY_PATH");
        Self::spawn_with(dir, name, command, args, None)
    }

    fn spawn_with(dir: &Path, name: &str, mut command: Command, args: &[&str], display: Option<&str>) -> Self {
        let log = dir.join(format!("{name}.log"));
        let out = std::fs::File::create(&log).unwrap();
        let program = PathBuf::from(command.get_program());
        command.args(args).current_dir(dir);
        isolate(&mut command, dir, display);
        let child = command
            .stdin(Stdio::null())
            .stdout(out.try_clone().unwrap())
            .stderr(out)
            .spawn()
            .unwrap_or_else(|e| panic!("starting {}: {e}", program.display()));
        Self { child, log }
    }

    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// The end of the process's output, for a failure message.
    pub fn tail(&self) -> String {
        let text = std::fs::read_to_string(&self.log).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        lines[lines.len().saturating_sub(30)..].join("\n")
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        // SIGTERM first: a killed Xvfb leaves its socket and lock file behind in /tmp.
        #[cfg(unix)]
        // SAFETY: kill only signals the child this Proc owns.
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM)
        };
        let start = Instant::now();
        while matches!(self.child.try_wait(), Ok(None)) && start.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Gives `command` folders of its own for settings, data and caches.
pub fn isolate(command: &mut Command, dir: &Path, display: Option<&str>) {
    for (var, sub) in [("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"), ("XDG_CACHE_HOME", "cache")] {
        let p = dir.join(sub);
        std::fs::create_dir_all(&p).unwrap();
        command.env(var, p);
    }
    command.env_remove("WAYLAND_DISPLAY").env_remove("SCYTHE_CONTROL").env_remove("SCYTHE_CONTROL_TOKEN");
    command.env("SCYTHE_SYSTEM_PLUGINS", "0");
    match display {
        Some(d) => command.env("DISPLAY", d),
        None => command.env_remove("DISPLAY"),
    };
}

/// A Scythe serving its control API: `scythe-server` or the desktop app with `--control`.
pub struct Scythe {
    pub proc: Proc,
    pub addr: SocketAddr,
    pub dir: PathBuf,
}

impl Scythe {
    /// `scythe-server --null-audio` on a free port with the admin token, opening `project`.
    pub fn server(dir: &Path, project: Option<&Path>) -> Self {
        let port = free_port();
        let control = format!("127.0.0.1:{port}");
        let access = dir.join("access.json");
        let mut args = vec!["--null-audio", "--control", &control, "--token", ADMIN, "--access-file"];
        let access = access.to_str().unwrap();
        args.push(access);
        let folder = dir.to_str().unwrap();
        args.extend(["--folder", folder]);
        let project = project.map(|p| p.to_str().unwrap().to_owned());
        if let Some(p) = &project {
            args.push(p);
        }
        let proc = Proc::spawn(dir, "scythe-server", &binary("scythe-server"), &args, None);
        Self::ready(proc, control.parse().unwrap(), dir, Duration::from_secs(30))
    }

    /// The desktop app on the X server `display`, serving its control API.
    pub fn app(dir: &Path, project: &Path, display: &str) -> Self {
        let port = free_port();
        let control = format!("127.0.0.1:{port}");
        let args =
            ["--null-audio", "--no-input", "--control", &control, "--control-token", ADMIN, project.to_str().unwrap()];
        let proc = Proc::spawn(dir, "scythe-app", &binary("scythe-app"), &args, Some(display));
        Self::ready(proc, control.parse().unwrap(), dir, Duration::from_secs(60))
    }

    fn ready(mut proc: Proc, addr: SocketAddr, dir: &Path, timeout: Duration) -> Self {
        let start = Instant::now();
        loop {
            assert!(proc.alive(), "the process ended before serving:\n{}", proc.tail());
            if TcpStream::connect(addr).is_ok() && http(addr, "GET", "/api/health", None, None).0 == 200 {
                break;
            }
            assert!(start.elapsed() < timeout, "no /api/health after {timeout:?}:\n{}", proc.tail());
            std::thread::sleep(Duration::from_millis(50));
        }
        Self { proc, addr, dir: dir.to_owned() }
    }

    /// Sends a command with `token`: the HTTP status and the answer.
    pub fn send(&self, token: &str, command: Value) -> (u16, Value) {
        http(self.addr, "POST", "/api/command", Some(token), Some(&command))
    }

    /// Sends a command as admin and returns its result, failing the test when it fails.
    #[track_caller]
    pub fn ok(&self, command: Value) -> Value {
        let (status, body) = self.send(ADMIN, command.clone());
        assert!(status == 200 && body["ok"] == true, "{command} answered {status} {body}\n{}", self.proc.tail());
        body["result"].clone()
    }

    pub fn snapshot(&self) -> Value {
        self.ok(json!({"cmd": "snapshot"}))
    }

    /// Polls `check` on fresh answers of `command` until it holds.
    #[track_caller]
    pub fn wait_for(&self, what: &str, timeout: Duration, command: Value, check: impl Fn(&Value) -> bool) -> Value {
        let start = Instant::now();
        loop {
            let v = self.ok(command.clone());
            if check(&v) {
                return v;
            }
            assert!(start.elapsed() < timeout, "timed out waiting for {what}; last answer {v}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Plays setlist entry `entry` and waits for it to stop by itself: how long it played and
    /// the transport state after.
    #[track_caller]
    pub fn play_until_stopped(&self, entry: usize) -> (f64, Value) {
        self.ok(json!({"cmd": "setlist.play", "entry": entry}));
        self.wait_for("playback", Duration::from_secs(5), json!({"cmd": "transport.state"}), |t| t["playing"] == true);
        let started = Instant::now();
        loop {
            let t = self.ok(json!({"cmd": "transport.state"}));
            if t["playing"] == false {
                return (started.elapsed().as_secs_f64(), t);
            }
            assert!(started.elapsed() < Duration::from_secs(15), "still playing past the stop marker: {t}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn ws(&self, token: &str) -> Ws {
        Ws::connect(self.addr, token)
    }
}

/// A minimal HTTP/1.1 exchange: the status code and the JSON body.
pub fn http(addr: SocketAddr, method: &str, path: &str, token: Option<&str>, body: Option<&Value>) -> (u16, Value) {
    let auth = token.map(|t| format!("Bearer {t}"));
    let headers: Vec<(&str, &str)> = auth.iter().map(|a| ("Authorization", a.as_str())).collect();
    let (status, _, body) = request(addr, method, path, &headers, body.map(|b| b.to_string()).as_deref());
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

/// An HTTP request with any headers and body: the status, the lower-cased head and the body.
pub fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> (u16, String, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let body = body.unwrap_or_default();
    let headers: String = headers.iter().map(|(name, value)| format!("{name}: {value}\r\n")).collect();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).unwrap();
    // Read up to the end of the body: Chromium's DevTools endpoint keeps the connection open.
    let mut response = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = stream.read(&mut chunk).unwrap();
        response.extend_from_slice(&chunk[..n]);
        if n == 0 || complete(&response) {
            break;
        }
    }
    let split = response.windows(4).position(|w| w == b"\r\n\r\n").expect("an HTTP response");
    let head = String::from_utf8_lossy(&response[..split]).to_ascii_lowercase();
    let status = head.split_whitespace().nth(1).and_then(|s| s.parse().ok()).expect("a status code");
    let rest = response[split + 4..].to_vec();
    let body = if head.contains("transfer-encoding: chunked") { dechunk(&rest) } else { rest };
    (status, head, body)
}

/// Whether `response` holds its whole body, as its `Content-Length` tells.
fn complete(response: &[u8]) -> bool {
    let Some(split) = response.windows(4).position(|w| w == b"\r\n\r\n") else { return false };
    let head = String::from_utf8_lossy(&response[..split]).to_ascii_lowercase();
    let length =
        head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse::<usize>().ok());
    length.is_some_and(|n| response.len() >= split + 4 + n)
}

fn dechunk(mut rest: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(eol) = rest.windows(2).position(|w| w == b"\r\n") {
        let n = usize::from_str_radix(String::from_utf8_lossy(&rest[..eol]).trim(), 16).unwrap_or(0);
        if n == 0 {
            break;
        }
        out.extend_from_slice(&rest[eol + 2..eol + 2 + n]);
        rest = &rest[eol + 4 + n..];
    }
    out
}

type Socket = tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>;

/// A WebSocket client of `/api/ws`.
pub struct Ws {
    socket: Socket,
    /// Events read while waiting for replies.
    pub events: Vec<Value>,
    pub hello: Value,
}

impl Ws {
    pub fn connect(addr: SocketAddr, token: &str) -> Self {
        let url = format!("ws://{addr}/api/ws?token={token}");
        let (mut socket, _) = tungstenite::connect(url).expect("WebSocket connects");
        if let tungstenite::stream::MaybeTlsStream::Plain(s) = socket.get_mut() {
            s.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        }
        let mut ws = Self { socket, events: Vec::new(), hello: Value::Null };
        let start = Instant::now();
        while ws.hello.is_null() {
            assert!(start.elapsed() < Duration::from_secs(10), "no hello");
            if let Some(m) = ws.read() {
                if m["type"] == "hello" {
                    ws.hello = m;
                } else {
                    ws.events.push(m);
                }
            }
        }
        ws
    }

    /// The next message, or `None` after the read timeout.
    pub fn read(&mut self) -> Option<Value> {
        match self.socket.read() {
            Ok(tungstenite::Message::Text(t)) => Some(serde_json::from_str(t.as_str()).expect("JSON message")),
            Ok(_) => None,
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
            {
                None
            }
            Err(e) => panic!("WebSocket: {e}"),
        }
    }

    pub fn send(&mut self, message: &Value) {
        self.socket.send(tungstenite::Message::text(message.to_string())).expect("WebSocket send");
    }

    /// Sends `commands` with ids 1.., and returns the replies in the order they came.
    pub fn exchange(&mut self, commands: &[Value]) -> Vec<Value> {
        for (i, c) in commands.iter().enumerate() {
            let mut c = c.clone();
            c["id"] = json!(i + 1);
            self.send(&c);
        }
        let mut replies = Vec::new();
        let start = Instant::now();
        while replies.len() < commands.len() {
            assert!(start.elapsed() < Duration::from_secs(20), "only {} of {} replies", replies.len(), commands.len());
            match self.read() {
                Some(m) if m["type"] == "reply" => replies.push(m),
                Some(m) => self.events.push(m),
                None => {}
            }
        }
        replies
    }

    /// Reads for `time`, keeping the events.
    pub fn listen(&mut self, time: Duration) {
        let start = Instant::now();
        while start.elapsed() < time {
            if let Some(m) = self.read() {
                self.events.push(m);
            }
        }
    }

    pub fn close(mut self) {
        let _ = self.socket.close(None);
        let _ = self.socket.flush();
    }

    pub fn saw(&self, event: &str) -> bool {
        self.events.iter().any(|e| e["type"] == "event" && e["event"] == event)
    }
}

/// An X server of the test's own, stopped when dropped.
pub struct Xvfb {
    _proc: Proc,
    pub display: String,
}

impl Xvfb {
    /// Starts Xvfb on a free display, or `None` (with a message saying the test is skipped)
    /// when there is no Xvfb.
    pub fn start(dir: &Path, test: &str) -> Option<Self> {
        let Some(xvfb) = find_tool("SCYTHE_XVFB", "Xvfb") else {
            eprintln!("SKIPPED {test}: no Xvfb on PATH and SCYTHE_XVFB not set");
            return None;
        };
        let n = (90..140).find(|n| {
            !Path::new(&format!("/tmp/.X11-unix/X{n}")).exists() && !Path::new(&format!("/tmp/.X{n}-lock")).exists()
        })?;
        let display = format!(":{n}");
        let proc =
            Proc::spawn_tool(dir, "xvfb", &xvfb, &[&display, "-screen", "0", "1600x1000x24", "-nolisten", "tcp"]);
        let start = Instant::now();
        while !Path::new(&format!("/tmp/.X11-unix/X{n}")).exists() {
            assert!(start.elapsed() < Duration::from_secs(10), "Xvfb did not start:\n{}", proc.tail());
            std::thread::sleep(Duration::from_millis(50));
        }
        Some(Self { _proc: proc, display })
    }
}

/// Just enough of the X11 protocol to hide and show the top-level windows of an X server, so
/// a test can see what a window does while it gets no frames.
#[cfg(unix)]
pub mod x11 {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    pub struct Display {
        stream: UnixStream,
        root: u32,
    }

    impl Display {
        /// Connects to the local display `:n` (an Xvfb without access control).
        pub fn connect(display: &str) -> std::io::Result<Self> {
            let n = display.trim_start_matches(':');
            let mut stream = UnixStream::connect(format!("/tmp/.X11-unix/X{n}"))?;
            stream.write_all(&[b'l', 0, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0])?;
            let mut head = [0u8; 8];
            stream.read_exact(&mut head)?;
            let extra = u16::from_le_bytes([head[6], head[7]]) as usize * 4;
            let mut body = vec![0u8; extra];
            stream.read_exact(&mut body)?;
            if head[0] != 1 {
                return Err(std::io::Error::other("the X server refused the connection"));
            }
            let vendor = u16::from_le_bytes([body[16], body[17]]) as usize;
            let formats = body[21] as usize;
            let screen = 32 + vendor.div_ceil(4) * 4 + formats * 8;
            let root = u32::from_le_bytes(body[screen..screen + 4].try_into().unwrap());
            Ok(Self { stream, root })
        }

        /// The windows directly below the root: the applications' top-level windows.
        pub fn top_level(&mut self) -> std::io::Result<Vec<u32>> {
            let mut req = vec![15u8, 0, 2, 0];
            req.extend_from_slice(&self.root.to_le_bytes());
            self.stream.write_all(&req)?;
            let mut reply = [0u8; 32];
            loop {
                self.stream.read_exact(&mut reply)?;
                let extra = u32::from_le_bytes(reply[4..8].try_into().unwrap()) as usize * 4;
                let mut body = vec![0u8; if reply[0] == 1 { extra } else { 0 }];
                self.stream.read_exact(&mut body)?;
                if reply[0] == 1 {
                    let n = u16::from_le_bytes([reply[16], reply[17]]) as usize;
                    return Ok((0..n)
                        .map(|i| u32::from_le_bytes(body[i * 4..i * 4 + 4].try_into().unwrap()))
                        .collect());
                }
                if reply[0] == 0 {
                    return Err(std::io::Error::other(format!("X error {}", reply[1])));
                }
            }
        }

        fn window_request(&mut self, opcode: u8, window: u32) -> std::io::Result<()> {
            let mut req = vec![opcode, 0, 2, 0];
            req.extend_from_slice(&window.to_le_bytes());
            self.stream.write_all(&req)
        }

        pub fn unmap(&mut self, window: u32) -> std::io::Result<()> {
            self.window_request(10, window)
        }

        pub fn map(&mut self, window: u32) -> std::io::Result<()> {
            self.window_request(8, window)
        }

        /// Waits until the server has handled every request sent so far.
        pub fn sync(&mut self) -> std::io::Result<()> {
            self.top_level().map(|_| ())
        }
    }
}
