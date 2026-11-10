//! An agent driving the window's session over MCP (Scythe's `/mcp`), as Claude Code does: the
//! light in the top bar and the agents window follow it, and pause and undo hold it back.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use serde_json::{Value, json};

use super::*;

/// One MCP message over HTTP: the session id the server gave (on `initialize`) and the answer.
fn post(addr: SocketAddr, session: Option<&str>, message: &Value) -> (Option<String>, Value) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let body = message.to_string();
    let session = session.map(|s| format!("Mcp-Session-Id: {s}\r\n")).unwrap_or_default();
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nAccept: application/json, text/event-stream\r\n\
         MCP-Protocol-Version: 2025-11-25\r\n{session}Content-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (head, rest) = response.split_once("\r\n\r\n").unwrap();
    let id = head.lines().find_map(|l| {
        let (name, value) = l.split_once(':')?;
        name.eq_ignore_ascii_case("mcp-session-id").then(|| value.trim().to_owned())
    });
    (id, serde_json::from_str(rest).unwrap_or(Value::Null))
}

fn call(id: u64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": arguments}})
}

/// The text a tool answered, and whether it failed.
fn answer(body: &Value) -> (bool, String) {
    let result = &body["result"];
    (result["isError"] == true, result["content"][0]["text"].as_str().unwrap_or_default().to_owned())
}

/// Runs the agent's steps on a thread of its own: each waits for the window to run its
/// commands, so the test keeps drawing frames meanwhile.
fn agent(addr: SocketAddr, steps: Vec<(&'static str, Value)>) -> Receiver<Vec<(bool, String)>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "claude-code", "title": "Claude Code", "version": "2.1.0"}}});
        let (session, _) = post(addr, None, &init);
        let session = session.expect("an MCP session id");
        let answers = steps
            .into_iter()
            .enumerate()
            .map(|(i, (tool, args))| answer(&post(addr, Some(&session), &call(i as u64 + 2, tool, args)).1))
            .collect();
        let _ = tx.send(answers);
    });
    rx
}

struct Served {
    _server: scythe_server::ControlServer,
    pump: scythe_session::control::ControlPump,
    addr: SocketAddr,
}

fn serve() -> Served {
    let config = scythe_server::ServerConfig::new("127.0.0.1:0".parse().unwrap(), None);
    let (server, pump) = scythe_server::ControlServer::start(config, Arc::new(|| {})).expect("a control server");
    let addr = server.addr();
    Served { _server: server, pump, addr }
}

impl TestApp {
    /// Draws frames, running the control API's commands, until the agent is done.
    fn wait_for(&mut self, served: &mut Served, agent: &Receiver<Vec<(bool, String)>>) -> Vec<(bool, String)> {
        let start = Instant::now();
        loop {
            served.pump.pump(&mut self.app().session);
            self.h.step();
            if let Ok(answers) = agent.try_recv() {
                served.pump.pump(&mut self.app().session);
                self.frames(2);
                return answers;
            }
            assert!(start.elapsed() < Duration::from_secs(60), "the agent did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn an_agent_over_mcp_lights_the_dot_and_fills_the_agents_window() {
    let mut t = TestApp::live("agents_http");
    let mut served = serve();
    assert!(!t.labels().iter().any(|l| l.starts_with("Agent activity")), "no light without an agent");

    let steps = vec![
        ("set_status", json!({"text": "Balancing the mix against the reference"})),
        ("track_volume", json!({"track": "Drums", "db": -3})),
        ("track_pan", json!({"track": "Bass", "pan": -0.2})),
    ];
    let running = agent(served.addr, steps);
    let answers = t.wait_for(&mut served, &running);
    assert!(answers.iter().all(|(error, _)| !error), "{answers:?}");
    assert!(t.has("Agent activity (working)"), "{:#?}", t.labels());
    assert_eq!((20.0 * t.track("Drums").volume.log10()).round(), -3.0);

    t.click("Agent activity (working)");
    for label in [
        "Claude Code",
        "Balancing the mix against the reference",
        "Set Drums volume to -3 dB",
        "Pan Bass 20% left",
        "Undo: Track pan",
    ] {
        assert!(t.has(label), "{label} missing: {:#?}", t.labels());
    }
    assert_eq!(t.app().history.undo_by(), Some("Claude Code"));

    t.click("Pause agents");
    assert!(t.app().agents.paused());
    let steps = vec![("track_mute", json!({"track": "Keys", "muted": true})), ("get_project", json!({}))];
    let running = agent(served.addr, steps);
    let answers = t.wait_for(&mut served, &running);
    assert!(answers[0].0 && answers[0].1.contains("paused by the user"), "{answers:?}");
    assert!(!answers[1].0, "reading works while paused: {answers:?}");
    assert!(!t.track("Keys").muted);
    assert!(t.has("Agent activity (error)"), "{:#?}", t.labels());

    t.click("Undo last agent change");
    assert_eq!(t.track("Bass").pan, 0.0);
    t.click("Undo last agent change");
    assert_eq!(t.track("Drums").volume, 1.0);
    t.click("Resume agents");
    assert!(!t.app().agents.paused());
    t.until("the light to settle", 10.0, |t| t.has("Agent activity (idle)"));

    t.click("Disconnect all");
    t.frames(2);
    assert!(!t.labels().iter().any(|l| l.starts_with("Agent activity")), "{:#?}", t.labels());
    assert!(t.has("No agent is connected."));
}
