mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::http;
use futures_util::{SinkExt, StreamExt};
use scythe_core::render_settings::ResampleMode;
use scythe_engine::{Engine, MediaPool};
use scythe_server::{ControlServer, ServerConfig};
use scythe_session::Session;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

const TOKEN: &str = "s3cret";

/// What the client saw over the WebSocket.
#[derive(Default)]
struct Seen {
    hello: bool,
    replies: Vec<Value>,
    project_changed: bool,
    playing: bool,
    meters: bool,
}

async fn websocket(addr: std::net::SocketAddr) -> Seen {
    let url = format!("ws://{addr}/api/ws?token={TOKEN}");
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.expect("websocket");
    let mut seen = Seen::default();
    for (id, cmd) in [
        (1, json!({"cmd": "track.volume", "track": "Vox", "db": -6.0})),
        (2, json!({"cmd": "track.volume", "track": 404, "db": -6.0})),
        (3, json!({"cmd": "transport.play"})),
    ] {
        let mut msg = cmd;
        msg["id"] = id.into();
        ws.send(Message::text(msg.to_string())).await.unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !(seen.replies.len() == 3 && seen.project_changed && seen.playing && seen.meters)
    {
        let Ok(Some(Ok(Message::Text(text)))) = tokio::time::timeout(Duration::from_secs(5), ws.next()).await else {
            break;
        };
        let value: Value = serde_json::from_str(text.as_str()).unwrap();
        match (value["type"].as_str(), value["event"].as_str()) {
            (Some("hello"), _) => seen.hello = true,
            (Some("reply"), _) => seen.replies.push(value),
            (Some("event"), Some("project_changed")) => seen.project_changed = true,
            (Some("event"), Some("transport")) => seen.playing |= value["playing"] == true,
            (Some("event"), Some("meters")) => seen.meters = true,
            _ => {}
        }
    }
    let _ = ws.close(None).await;
    seen
}

#[test]
fn commands_over_http_and_websocket() {
    let pool = Arc::new(MediaPool::with_resample(ResampleMode::PLAYBACK));
    let mut session = Session::new(Engine::start_offline(pool, 48_000));
    let config = ServerConfig::new("127.0.0.1:0".parse().unwrap(), Some(TOKEN.into()));
    let (server, mut pump) = ControlServer::start(config, Arc::new(|| {})).expect("server");
    let addr = server.addr();

    let client = std::thread::spawn(move || {
        let (status, body) = http(addr, "GET", "/api/health", None, None);
        assert_eq!((status, body["ok"].clone()), (200, json!(true)));
        let add = json!({"cmd": "track.add", "name": "Vox"});
        let (status, body) = http(addr, "POST", "/api/command", None, Some(&add));
        assert_eq!(status, 401, "{body}");
        let (status, body) = http(addr, "POST", "/api/command", Some("wrong"), Some(&add));
        assert_eq!(status, 401, "{body}");
        let (status, body) = http(addr, "POST", "/api/command", Some(TOKEN), Some(&add));
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["result"]["name"], "Vox");
        let (status, body) = http(addr, "POST", "/api/command", Some(TOKEN), Some(&json!({"cmd": "track.fly"})));
        assert_eq!(status, 400, "{body}");
        assert_eq!(body["error"]["kind"], "invalid");
        let missing = json!({"cmd": "track.mute", "track": 12345});
        let (status, _) = http(addr, "POST", "/api/command", Some(TOKEN), Some(&missing));
        assert_eq!(status, 404);

        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let seen = runtime.block_on(websocket(addr));
        assert!(seen.hello);
        let by_id = |id: u64| seen.replies.iter().find(|r| r["id"] == id).cloned().unwrap_or(Value::Null);
        assert_eq!(by_id(1)["ok"], true, "{:?}", seen.replies);
        assert!((by_id(1)["result"]["volume_db"].as_f64().unwrap() + 6.0).abs() < 1e-9);
        assert_eq!(by_id(2)["ok"], false);
        assert_eq!(by_id(2)["error"]["kind"], "not_found");
        assert_eq!(by_id(3)["result"]["playing"], true);
        assert!(seen.project_changed && seen.playing && seen.meters);

        let (status, body) = http(addr, "GET", "/api/snapshot", Some(TOKEN), None);
        assert_eq!(status, 200);
        assert_eq!(body["result"]["tracks"][0]["name"], "Vox");
        assert_eq!(body["result"]["transport"]["playing"], true);
    });

    while !client.is_finished() {
        session.poll();
        session.update_meters();
        pump.wait(&mut session, Duration::from_millis(10));
    }
    client.join().unwrap();
    assert!(session.is_playing());
    drop(server);
}
