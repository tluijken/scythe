mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{get_raw, http};
use futures_util::{SinkExt, StreamExt};
use scythe_core::render_settings::ResampleMode;
use scythe_engine::{Engine, MediaPool};
use scythe_server::{ControlServer, ServerConfig};
use scythe_session::Session;
use scythe_session::api::execute_json;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

const ADMIN: &str = "admin-s3cret";

fn band() -> Session {
    let pool = Arc::new(MediaPool::with_resample(ResampleMode::PLAYBACK));
    let mut s = Session::new(Engine::start_offline(pool, 48_000));
    for command in [
        json!({"cmd": "track.add", "name": "Kick"}),
        json!({"cmd": "track.add", "name": "Vox"}),
        json!({"cmd": "mix.create", "musician": "Drummer", "own": ["Kick"]}),
        json!({"cmd": "mix.create", "musician": "Singer", "own": ["Vox"]}),
        json!({"cmd": "marker.add", "position": 0.0, "name": "Opener"}),
        json!({"cmd": "marker.add", "position": 30.0, "name": "Closer"}),
        json!({"cmd": "setlist.create", "name": "Gig", "songs": [0, 1]}),
    ] {
        execute_json(&mut s, command.clone()).unwrap_or_else(|e| panic!("{command}: {e}"));
    }
    s
}

fn command(addr: SocketAddr, token: &str, command: Value) -> (u16, Value) {
    http(addr, "POST", "/api/command", Some(token), Some(&command))
}

/// The messages a WebSocket client with `token` receives within `wait`, after sending
/// `commands`.
async fn listen(addr: SocketAddr, token: &str, commands: &[Value], wait: Duration) -> Vec<Value> {
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/ws?token={token}")).await.unwrap();
    for (id, c) in commands.iter().enumerate() {
        let mut c = c.clone();
        c["id"] = id.into();
        ws.send(Message::text(c.to_string())).await.unwrap();
    }
    let mut seen = Vec::new();
    let deadline = Instant::now() + wait;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match tokio::time::timeout(left, ws.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => seen.push(serde_json::from_str(text.as_str()).unwrap()),
            Ok(Some(Ok(_))) => {}
            _ => break,
        }
    }
    let _ = ws.close(None).await;
    seen
}

fn events<'a>(seen: &'a [Value], name: &str) -> Vec<&'a Value> {
    seen.iter().filter(|m| m["type"] == "event" && m["event"] == name).collect()
}

#[test]
fn every_role_stays_inside_its_bounds() {
    let mut session = band();
    let base = std::env::temp_dir().join(format!("scythe-access-test-{}", std::process::id()));
    let live = base.join("live");
    std::fs::create_dir_all(live.join("set")).unwrap();
    std::fs::write(live.join("set").join("Set.RPP"), "").unwrap();
    std::fs::create_dir_all(base.join("private")).unwrap();
    let config = ServerConfig {
        access_file: Some(base.join("access.json")),
        folders: vec![live.clone()],
        ..ServerConfig::new("127.0.0.1:0".parse().unwrap(), Some(ADMIN.into()))
    };
    let (server, mut pump) = ControlServer::start(config.clone(), Arc::new(|| {})).expect("server");
    let addr = server.addr();
    let item_less_peaks = json!({"cmd": "item.peaks", "item": 1, "width": 8});

    let client = std::thread::spawn(move || {
        let (status, drummer) =
            command(addr, ADMIN, json!({"cmd": "access.create", "role": "musician", "musician": "Drummer"}));
        assert_eq!(status, 200, "{drummer}");
        let drummer = drummer["result"].clone();
        assert!(drummer["path"].as_str().unwrap().starts_with("/monitor?token="));
        let drummer_token = drummer["token"].as_str().unwrap().to_owned();
        let (_, stage) = command(addr, ADMIN, json!({"cmd": "access.create", "role": "stage", "name": "Drum tablet"}));
        let stage_token = stage["result"]["token"].as_str().unwrap().to_owned();
        let (_, listed) = command(addr, ADMIN, json!({"cmd": "access.list"}));
        assert_eq!(listed["result"]["links"].as_array().map(Vec::len), Some(2));
        assert_eq!(listed["result"]["origins"], json!([]), "a loopback server has no address for phones");

        // The musician: their own mix, nothing else.
        let (status, snap) = http(addr, "GET", "/api/snapshot", Some(&drummer_token), None);
        assert_eq!(status, 200);
        assert!(snap["result"].get("tracks").is_none(), "{snap}");
        assert_eq!(snap["result"]["access"]["musician"], "Drummer");
        let (_, mixes) = command(addr, &drummer_token, json!({"cmd": "mix.list"}));
        let mixes = mixes["result"].as_array().unwrap().clone();
        assert_eq!(mixes.len(), 1);
        let own = mixes[0]["id"].clone();
        let (status, body) = command(addr, &drummer_token, json!({"cmd": "mix.more_me", "mix": own, "db": 4.0}));
        assert_eq!(status, 200, "{body}");
        for refused in [
            json!({"cmd": "mix.more_me", "mix": "MON Singer", "db": 4.0}),
            json!({"cmd": "mix.output", "mix": own, "output": 6}),
            json!({"cmd": "track.mute", "track": "Vox"}),
            json!({"cmd": "transport.play"}),
            json!({"cmd": "project.open", "path": "/etc/hosts"}),
            json!({"cmd": "access.list"}),
            json!({"cmd": "access.create", "role": "stage"}),
            item_less_peaks.clone(),
        ] {
            let (status, body) = command(addr, &drummer_token, refused.clone());
            assert_eq!(status, 403, "{refused}: {body}");
            assert_eq!(body["error"]["kind"], "forbidden");
        }
        let (_, me) = command(addr, &drummer_token, json!({"cmd": "access.whoami"}));
        assert_eq!(me["result"], json!({"role": "musician", "musician": "Drummer"}));

        // The stage: the setlist and play/stop.
        for allowed in [json!({"cmd": "setlist.status"}), json!({"cmd": "setlist.cue", "entry": 1})] {
            let (status, body) = command(addr, &stage_token, allowed.clone());
            assert_eq!(status, 200, "{allowed}: {body}");
        }
        for refused in [
            json!({"cmd": "setlist.delete"}),
            json!({"cmd": "setlist.import", "text": "X\nOpener"}),
            json!({"cmd": "mix.list"}),
            json!({"cmd": "track.add"}),
            json!({"cmd": "project.save", "path": "/tmp/stage.RPP"}),
        ] {
            let (status, body) = command(addr, &stage_token, refused.clone());
            assert_eq!(status, 403, "{refused}: {body}");
        }

        // Only the admin browses files, peeks at waveforms or speaks MCP.
        for token in [&drummer_token, &stage_token] {
            let (status, _) = http(addr, "GET", "/api/files", Some(token), None);
            assert_eq!(status, 403);
            let (status, _) = http(addr, "GET", "/api/peaks?item=1&width=10", Some(token), None);
            assert_eq!(status, 403);
            let init = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
            let (status, _) = http(addr, "POST", "/mcp", Some(token), Some(&init));
            assert_eq!(status, 403);
        }
        let (status, roots) = http(addr, "GET", "/api/files", Some(ADMIN), None);
        assert_eq!(status, 200, "{roots}");
        let root = roots["result"]["entries"][0]["path"].as_str().unwrap().to_owned();
        let (status, set) = http(addr, "GET", &format!("/api/files?path={root}/set"), Some(ADMIN), None);
        assert_eq!(status, 200, "{set}");
        assert_eq!(set["result"]["entries"][0]["name"], "Set.RPP");
        let (status, _) = http(addr, "GET", &format!("/api/files?path={root}/../private"), Some(ADMIN), None);
        assert_eq!(status, 403);
        let (status, body) = http(addr, "GET", "/api/peaks?item=424242&width=10", Some(ADMIN), None);
        assert_eq!(status, 404, "{body}");

        // Events: the musician hears their own mix's meters, the stage the transport and setlist.
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let wait = Duration::from_millis(600);
        let musician = runtime.block_on(listen(addr, &drummer_token, &[json!({"cmd": "mix.list"})], wait));
        assert_eq!(musician[0]["access"]["role"], "musician");
        let meters = events(&musician, "meters");
        assert!(!meters.is_empty());
        for m in &meters {
            assert_eq!(m["tracks"], json!([]));
            assert_eq!(m["mixes"].as_array().map(Vec::len), Some(1), "{m}");
            assert_eq!(m["mixes"][0]["musician"], "Drummer");
        }
        assert!(events(&musician, "transport").is_empty() && events(&musician, "setlist").is_empty());
        let stage = runtime.block_on(listen(addr, &stage_token, &[json!({"cmd": "transport.play"})], wait));
        assert!(stage.iter().any(|m| m["type"] == "reply" && m["ok"] == true));
        assert!(events(&stage, "meters").is_empty());
        assert!(!events(&stage, "transport").is_empty());
        let admin = runtime.block_on(listen(addr, ADMIN, &[json!({"cmd": "transport.stop"})], wait));
        assert_eq!(admin[0]["access"]["role"], "admin");
        assert!(events(&admin, "meters").iter().any(|m| m["tracks"].as_array().is_some_and(|t| t.len() == 4)));

        // A deleted link stops working at once; the admin's never needs one.
        let id = drummer["id"].as_str().unwrap();
        command(addr, ADMIN, json!({"cmd": "access.delete", "id": id}));
        let (status, _) = command(addr, &drummer_token, json!({"cmd": "mix.list"}));
        assert_eq!(status, 401);
        let (status, _) = command(addr, "made-up", json!({"cmd": "transport.state"}));
        assert_eq!(status, 401);
        assert_eq!(get_raw(addr, "/monitor", "").0, 200, "the pages themselves need no token");
        stage_token
    });

    while !client.is_finished() {
        session.poll();
        session.update_meters();
        pump.wait(&mut session, Duration::from_millis(10));
    }
    let stage_token = client.join().unwrap();
    drop(server);

    // Links survive a restart.
    let (server, mut pump) = ControlServer::start(config, Arc::new(|| {})).expect("server");
    let addr = server.addr();
    let client = std::thread::spawn(move || command(addr, &stage_token, json!({"cmd": "setlist.status"})).0);
    while !client.is_finished() {
        pump.wait(&mut session, Duration::from_millis(10));
    }
    assert_eq!(client.join().unwrap(), 200);
    drop(server);
    std::fs::remove_dir_all(&base).unwrap();
}
