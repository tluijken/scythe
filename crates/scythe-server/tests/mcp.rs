mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use common::{http, request};
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
    let config = ServerConfig { addr: "127.0.0.1:0".parse().unwrap(), token: token.map(Into::into) };
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
        assert_eq!(http(addr, "GET", "/mcp", Some(TOKEN), None).0, 405);
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
