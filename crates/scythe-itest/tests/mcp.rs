//! The MCP server three ways: `scythe-mcp` over stdio against a running server, `scythe-mcp
//! --headless` with a session of its own, and the server's own `/mcp` over HTTP.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use scythe_itest::{ADMIN, Scythe, binary, isolate, live_fixture, request, test_dir};
use serde_json::{Value, json};

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next: u64,
}

impl Mcp {
    fn start(dir: &std::path::Path, args: &[&str]) -> Self {
        let mut command = Command::new(binary("scythe-mcp"));
        command.args(args).current_dir(dir);
        isolate(&mut command, dir, None);
        let log = std::fs::File::create(dir.join("scythe-mcp.log")).unwrap();
        let mut child =
            command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(log).spawn().expect("scythe-mcp runs");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in stdout.lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self { child, stdin, lines, next: 0 }
    }

    fn send(&mut self, message: &Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params}));
        loop {
            let line =
                self.lines.recv_timeout(Duration::from_secs(60)).unwrap_or_else(|_| panic!("no answer to {method}"));
            let answer: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line:?}"));
            // Notifications may come in between.
            if answer["id"] == self.next {
                return answer;
            }
        }
    }

    fn call(&mut self, name: &str, arguments: Value) -> (bool, String) {
        let answer = self.request("tools/call", json!({"name": name, "arguments": arguments}));
        let result = &answer["result"];
        assert!(result.is_object(), "{name}: {answer}");
        (result["isError"] == true, result["content"][0]["text"].as_str().unwrap_or_default().to_owned())
    }

    fn initialize(&mut self) -> Value {
        let init = self.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "itest", "version": "1"}}),
        );
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        init
    }

    fn tools(&mut self) -> Vec<String> {
        let tools = self.request("tools/list", json!({}));
        tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_owned()).collect()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn stdio_against_a_running_server() {
    let dir = test_dir("mcp_stdio_remote");
    let project = live_fixture(&dir);
    let s = Scythe::server(&dir, Some(&project));
    let url = format!("http://{}", s.addr);
    let mut mcp = Mcp::start(&dir, &["--url", &url, "--token", ADMIN]);
    let init = mcp.initialize();
    assert!(init["result"]["capabilities"]["tools"].is_object(), "{init}");
    let tools = mcp.tools();
    for t in ["get_project", "track_add", "mix_create", "setlist_create"] {
        assert!(tools.iter().any(|n| n == t), "no {t} in {tools:?}");
    }
    let (error, text) = mcp.call("get_project", json!({}));
    assert!(!error && text.contains("Drums"), "{text}");
    let (error, text) = mcp.call("track_add", json!({"name": "Guitar"}));
    assert!(!error, "{text}");
    let (error, text) = mcp.call("track_volume", json!({"track": "Guitar", "db": -4.5}));
    assert!(!error, "{text}");
    let (error, text) = mcp.call("mix_create", json!({"musician": "Guitarist", "own": ["Guitar"]}));
    assert!(!error, "{text}");
    let (error, text) = mcp.call("setlist_create", json!({"name": "Agent set", "songs": ["Opener"]}));
    assert!(!error, "{text}");
    let (error, text) = mcp.call("track_delete", json!({"track": 999}));
    assert!(error, "deleting a missing track should be an error: {text}");

    let snap = s.snapshot();
    let guitar = snap["tracks"].as_array().unwrap().iter().find(|t| t["name"] == "Guitar").expect("Guitar added");
    assert!((guitar["volume_db"].as_f64().unwrap() + 4.5).abs() < 0.01, "{guitar}");
    assert!(snap["mixes"].as_array().unwrap().iter().any(|m| m["musician"] == "Guitarist"));
    let list = s.ok(json!({"cmd": "setlist.get", "setlist": "Agent set"}));
    assert_eq!(list["entries"][0]["name"], "Opener", "{list}");
}

#[test]
fn stdio_with_a_headless_session_of_its_own() {
    let dir = test_dir("mcp_stdio_headless");
    let project = live_fixture(&dir);
    let mut mcp = Mcp::start(&dir, &["--headless", "--null-audio", project.to_str().unwrap()]);
    mcp.initialize();
    assert!(mcp.tools().iter().any(|n| n == "scythe_command"));
    let (error, text) = mcp.call("track_list", json!({}));
    assert!(!error && text.contains("Keys"), "{text}");
    let (error, text) = mcp.call("track_rename", json!({"track": "Keys", "name": "Synth"}));
    assert!(!error, "{text}");
    let saved = dir.join("agent.RPP");
    let (error, text) = mcp.call("project_save", json!({"path": saved}));
    assert!(!error, "{text}");
    let rpp = std::fs::read_to_string(&saved).expect("the project was saved");
    assert!(rpp.contains("NAME Synth"), "{rpp}");
    let (error, text) = mcp.call("scythe_command", json!({"command": {"cmd": "setlist.songs"}}));
    assert!(!error && text.contains("Ballad"), "{text}");
}

fn rpc(s: &Scythe, token: Option<&str>, message: Value) -> (u16, Value) {
    let auth = token.map(|t| format!("Bearer {t}"));
    let mut headers = vec![("Accept", "application/json, text/event-stream"), ("MCP-Protocol-Version", "2025-11-25")];
    if let Some(a) = &auth {
        headers.push(("Authorization", a));
    }
    let (status, _, body) = request(s.addr, "POST", "/mcp", &headers, Some(&message.to_string()));
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

#[test]
fn http_on_the_server_with_its_token() {
    let dir = test_dir("mcp_http");
    let project = live_fixture(&dir);
    let s = Scythe::server(&dir, Some(&project));
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "itest", "version": "1"}}});
    assert_eq!(rpc(&s, None, init.clone()).0, 401);
    assert_eq!(rpc(&s, Some("wrong"), init.clone()).0, 401);
    let (status, body) = rpc(&s, Some(ADMIN), init);
    assert_eq!(status, 200, "{body}");
    assert_eq!(rpc(&s, Some(ADMIN), json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).0, 202);
    let call = |id: u64, name: &str, arguments: Value| {
        rpc(
            &s,
            Some(ADMIN),
            json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": arguments}}),
        )
    };
    let (_, body) = call(2, "mix_create", json!({"musician": "Keys player", "own": ["Keys"]}));
    assert_eq!(body["result"]["isError"], false, "{body}");
    let (_, body) = call(3, "mix_more_me", json!({"mix": "MON Keys player", "db": 4.0}));
    assert_eq!(body["result"]["isError"], false, "{body}");
    let mix = s.ok(json!({"cmd": "mix.get", "mix": "MON Keys player"}));
    assert_eq!(mix["more_me"], 4.0, "{mix}");
    let (_, body) = call(4, "project_undo", json!({}));
    assert_eq!(body["result"]["isError"], false, "{body}");
    let mix = s.ok(json!({"cmd": "mix.get", "mix": "MON Keys player"}));
    assert_eq!(mix["more_me"], 0.0, "undo over MCP undoes the last step: {mix}");
}
