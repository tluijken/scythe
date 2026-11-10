mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use common::{exchange, http, request};
use scythe_core::render_settings::ResampleMode;
use scythe_engine::{Engine, MediaPool};
use scythe_server::{ControlServer, ServerConfig};
use scythe_session::Session;
use serde_json::{Value, json};

const TOKEN: &str = "s3cret";

fn rpc(addr: SocketAddr, headers: &[(&str, &str)], message: Value) -> (u16, Value) {
    let mut all = vec![("Accept", "application/json, text/event-stream")];
    if !headers.iter().any(|(name, _)| *name == "MCP-Protocol-Version") {
        all.push(("MCP-Protocol-Version", "2025-11-25"));
    }
    all.extend_from_slice(headers);
    request(addr, "POST", "/mcp", &all, Some(&message))
}

fn call(id: u64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": arguments}})
}

/// Runs `client` against a server on a random port while this thread runs the session.
fn serve(token: Option<&str>, client: impl FnOnce(SocketAddr) + Send + 'static) -> Session {
    let pool = Arc::new(MediaPool::with_resample(ResampleMode::PLAYBACK));
    let mut session = Session::new(Engine::start_offline(pool, 48_000));
    let config = ServerConfig::new("127.0.0.1:0".parse().unwrap(), token.map(Into::into));
    let (server, mut pump) = ControlServer::start(config, Arc::new(|| {})).expect("server");
    let addr = server.addr();
    let client = std::thread::spawn(move || client(addr));
    while !client.is_finished() {
        session.poll();
        pump.wait(&mut session, Duration::from_millis(10));
    }
    client.join().unwrap();
    drop(server);
    session
}

#[test]
fn mcp_over_streamable_http() {
    let session = serve(Some(TOKEN), |addr| {
        let auth = format!("Bearer {TOKEN}");
        let auth = [("Authorization", auth.as_str())];
        let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}});

        let (status, _) = rpc(addr, &[], init.clone());
        assert_eq!(status, 401);
        let (status, body) = rpc(addr, &auth, init);
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(body["result"]["serverInfo"]["name"], "scythe");

        let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        assert_eq!(rpc(addr, &auth, initialized).0, 202);

        let (status, body) = rpc(addr, &auth, json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
        assert_eq!(status, 200);
        let tools = body["result"]["tools"].as_array().unwrap();
        assert!(tools.iter().any(|t| t["name"] == "track_add"));

        let (_, body) = rpc(addr, &auth, call(3, "track_add", json!({"name": "Vox"})));
        assert_eq!(body["result"]["isError"], false, "{body}");
        let (_, body) = rpc(addr, &auth, call(4, "track_volume", json!({"track": "Vox", "db": -6.0})));
        assert_eq!(body["result"]["isError"], false, "{body}");
        let (_, body) = rpc(addr, &auth, call(5, "get_project", json!({})));
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("\"Vox\": -6.0 dB"), "{text}");
        let (_, body) =
            rpc(addr, &auth, call(6, "scythe_command", json!({"command": {"cmd": "track.mute", "track": "Vox"}})));
        assert_eq!(body["result"]["isError"], false, "{body}");
        let (_, body) = rpc(addr, &auth, call(7, "track_delete", json!({"track": 999})));
        assert_eq!(body["result"]["isError"], true, "{body}");

        let read =
            json!({"jsonrpc": "2.0", "id": 8, "method": "resources/read", "params": {"uri": "scythe://project"}});
        let (_, body) = rpc(addr, &auth, read);
        let snapshot: Value = serde_json::from_str(body["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(snapshot["tracks"][0]["muted"], true);

        let (status, _) = rpc(
            addr,
            &[auth[0], ("MCP-Protocol-Version", "1999-01-01")],
            json!({"jsonrpc": "2.0", "id": 9, "method": "ping"}),
        );
        assert_eq!(status, 400);
        assert_eq!(http(addr, "DELETE", "/mcp", Some(TOKEN), None).0, 404, "no session to end");
    });
    assert_eq!(session.project.tracks.len(), 1);
    assert!(session.project.tracks[0].muted);
}

#[test]
fn without_a_token_only_local_pages_may_call() {
    serve(None, |addr| {
        let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});
        let (status, body) = rpc(addr, &[], ping.clone());
        assert_eq!((status, body["result"].clone()), (200, json!({})));
        assert_eq!(rpc(addr, &[("Origin", "http://localhost:5173")], ping.clone()).0, 200);
        assert_eq!(rpc(addr, &[("Origin", "https://evil.example")], ping).0, 403);
    });
}

/// Holds `GET /mcp` open and hands on each server-sent event's data.
fn notifications(addr: SocketAddr) -> std::sync::mpsc::Receiver<Value> {
    use std::io::{BufRead, BufReader, Write};
    let mut stream = std::net::TcpStream::connect(addr).unwrap();
    let request = format!(
        "GET /mcp HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {TOKEN}\r\nAccept: text/event-stream\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { return };
            if let Some(data) = line.strip_prefix("data: ")
                && let Ok(message) = serde_json::from_str::<Value>(data)
                && tx.send(message).is_err()
            {
                return;
            }
        }
    });
    rx
}

#[test]
fn tool_selection_notifications_and_access_links() {
    serve(Some(TOKEN), |addr| {
        let auth = format!("Bearer {TOKEN}");
        let auth = [("Authorization", auth.as_str())];
        let list = |path: &str| {
            let message = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
            let mut headers =
                vec![("Accept", "application/json, text/event-stream"), ("MCP-Protocol-Version", "2025-11-25")];
            headers.extend_from_slice(&auth);
            let (_, body) = request(addr, "POST", path, &headers, Some(&message));
            body["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        let default = list("/mcp");
        assert!(default.contains(&"media_import".to_owned()) && !default.contains(&"mix_more_me".to_owned()));
        let all = list("/mcp?tools=all");
        assert_eq!(all.len(), scythe_mcp::tools::all().len());
        let mixes = list("/mcp?tools=mix");
        assert!(mixes.contains(&"mix_more_me".to_owned()) && !mixes.contains(&"track_add".to_owned()), "{mixes:?}");
        assert_eq!(rpc(addr, &auth, json!({"jsonrpc": "2.0", "id": 1, "method": "ping"})).0, 200);
        let (status, _) = request(
            addr,
            "POST",
            "/mcp?tools=bogus",
            &[auth[0], ("MCP-Protocol-Version", "2025-11-25")],
            Some(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"})),
        );
        assert_eq!(status, 400);

        let events = notifications(addr);
        std::thread::sleep(Duration::from_millis(200));
        let (_, body) = rpc(addr, &auth, call(2, "scythe_tools", json!({"enable": ["setlist"]})));
        assert_eq!(body["result"]["isError"], false, "{body}");
        let changed = events.recv_timeout(Duration::from_secs(5)).expect("a notification");
        assert_eq!(changed["method"], "notifications/tools/list_changed");
        assert!(list("/mcp").contains(&"setlist_next".to_owned()));

        let subscribe =
            json!({"jsonrpc": "2.0", "id": 3, "method": "resources/subscribe", "params": {"uri": "scythe://project"}});
        assert_eq!(rpc(addr, &auth, subscribe).1["result"], json!({}));
        std::thread::sleep(Duration::from_millis(400));
        rpc(addr, &auth, call(4, "track_add", json!({"name": "Click"})));
        let updated = events.recv_timeout(Duration::from_secs(5)).expect("a resource update");
        assert_eq!(updated["method"], "notifications/resources/updated", "{updated}");
        assert_eq!(updated["params"]["uri"], "scythe://project");

        let (_, body) =
            rpc(addr, &auth, call(5, "scythe_command", json!({"command": {"cmd": "access.create", "role": "stage"}})));
        let link: Value = serde_json::from_str(body["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(link["role"], "stage", "{link}");
        assert!(link["path"].as_str().unwrap().starts_with("/setlist?token="));
        assert!(link["urls"].is_array());
        let (_, body) = rpc(addr, &auth, call(6, "events_since", json!({})));
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("project_changed"), "{text}");
    });
}

#[test]
fn agents_show_in_the_session_and_obey_the_pause() {
    let session = serve(Some(TOKEN), |addr| {
        let auth = format!("Bearer {TOKEN}");
        let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "claude-code", "title": "Claude Code", "version": "2.1.0"}}});
        let headers = [
            ("Authorization", auth.as_str()),
            ("Accept", "application/json, text/event-stream"),
            ("MCP-Protocol-Version", "2025-11-25"),
        ];
        let (status, head, _) = exchange(addr, "POST", "/mcp", &headers, Some(&init));
        assert_eq!(status, 200);
        let id =
            head.lines().find_map(|l| l.strip_prefix("mcp-session-id: ")).expect("an MCP session id").trim().to_owned();
        let session = [auth.as_str(), id.as_str()];
        let mcp = |message: Value| rpc(addr, &[("Authorization", session[0]), ("Mcp-Session-Id", session[1])], message);
        let control = |command: Value| http(addr, "POST", "/api/command", Some(TOKEN), Some(&command)).1;
        let text = |body: &Value| body["result"]["content"][0]["text"].as_str().unwrap_or_default().to_owned();

        let (_, body) = mcp(call(2, "set_status", json!({"text": "Building the drum bus"})));
        assert_eq!(body["result"]["isError"], false, "{body}");
        let (_, body) = mcp(call(3, "track_add", json!({"name": "Vox"})));
        assert_eq!(body["result"]["isError"], false, "{body}");
        let (_, body) = mcp(call(4, "track_volume", json!({"track": "Vox", "db": -3})));
        assert_eq!(body["result"]["isError"], false, "{body}");

        let activity = control(json!({"cmd": "agents.activity"}))["result"].clone();
        let client = &activity["clients"][0];
        assert_eq!((client["name"].as_str(), client["transport"].as_str()), (Some("Claude Code"), Some("http")));
        assert_eq!((client["version"].as_str(), client["role"].as_str()), (Some("2.1.0"), Some("admin")));
        assert_eq!(activity["status"], "Building the drum bus");
        let calls = activity["calls"].as_array().unwrap();
        assert_eq!(calls.len(), 2, "set_status is no call: {calls:?}");
        assert_eq!(calls[0]["summary"], "Add track Vox");
        assert_eq!(calls[1]["summary"], "Set Vox volume to -3 dB");
        assert_eq!((calls[1]["ok"].as_bool(), calls[1]["changed"].as_bool()), (Some(true), Some(true)));
        assert_eq!(calls[1]["undo"], json!(["Track volume"]));
        assert_eq!(activity["undo"], json!({"label": "Track volume", "agent": "Claude Code"}));

        assert_eq!(control(json!({"cmd": "agents.pause"}))["result"]["paused"], true);
        let (_, body) = mcp(call(5, "track_volume", json!({"track": "Vox", "db": -10})));
        assert_eq!(body["result"]["isError"], true);
        assert!(text(&body).contains("paused by the user"), "{body}");
        let (_, body) = mcp(call(6, "scythe_command", json!({"command": {"cmd": "access.create", "role": "stage"}})));
        assert!(text(&body).contains("paused by the user"), "{body}");
        let (_, body) = mcp(call(7, "get_project", json!({})));
        assert_eq!(body["result"]["isError"], false, "reading still works: {body}");
        let (_, body) = mcp(call(8, "scythe_command", json!({"command": {"cmd": "agents.pause", "paused": false}})));
        assert_eq!(body["result"]["isError"], true, "an agent cannot resume itself: {body}");
        control(json!({"cmd": "agents.pause", "paused": false}));
        let activity = control(json!({"cmd": "agents.activity"}))["result"].clone();
        let failed = activity["calls"].as_array().unwrap().iter().filter(|c| c["ok"] == false).count();
        assert_eq!(failed, 3);

        let undone = control(json!({"cmd": "agents.undo"}));
        assert_eq!(undone["result"]["label"], "Track volume", "{undone}");
        assert_eq!(control(json!({"cmd": "track.volume", "track": "Vox", "db": -1}))["ok"], true);
        let refused = control(json!({"cmd": "agents.undo"}));
        assert_eq!(refused["ok"], false, "the user's own edit is on top: {refused}");

        control(json!({"cmd": "agents.disconnect", "client": id}));
        let (_, body) = mcp(call(9, "track_add", json!({"name": "Gtr"})));
        assert!(text(&body).contains("disconnected"), "{body}");
        assert_eq!(control(json!({"cmd": "agents.activity"}))["result"]["clients"], json!([]));
        assert_eq!(
            exchange(addr, "DELETE", "/mcp", &[("Authorization", session[0]), ("Mcp-Session-Id", session[1])], None).0,
            204
        );
        assert_eq!(mcp(json!({"jsonrpc": "2.0", "id": 10, "method": "ping"})).0, 404, "an ended session");
    });
    let names: Vec<&str> = session.project.tracks.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["Vox"]);
    assert_eq!(session.history.undo_label(), Some("Track volume"));
    assert_eq!(session.history.undo_by(), None);
}
