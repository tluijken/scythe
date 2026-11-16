use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use scythe_core::render_settings::ResampleMode;
use scythe_engine::{Engine, MediaPool};
use scythe_mcp::{Backend, Remote};
use scythe_server::{ControlServer, ServerConfig};
use scythe_session::Session;
use serde_json::{Value, json};

const TOKEN: &str = "s3cret";

/// `scythe-mcp --url` driving a running control server.
#[test]
fn stdio_against_a_running_scythe() {
    let pool = Arc::new(MediaPool::with_resample(ResampleMode::PLAYBACK));
    let mut session = Session::new(Engine::start_offline(pool, 48_000));
    let config = ServerConfig::new("127.0.0.1:0".parse().unwrap(), Some(TOKEN.into()));
    let (server, mut pump) = ControlServer::start(config, Arc::new(|| {})).expect("server");
    let url = format!("http://{}", server.addr());

    let client = std::thread::spawn(move || {
        let wrong = Remote::new(&url, Some("wrong".into())).command(json!({"cmd": "track.list"}));
        assert!(wrong.unwrap_err().message.contains("--token"));
        let gone = Remote::new("http://127.0.0.1:1", None).command(json!({"cmd": "track.list"}));
        assert!(gone.unwrap_err().message.starts_with("cannot reach Scythe"));

        let private = std::env::temp_dir().join(format!("scythe-mcp-remote-{}", std::process::id()));
        let mut child = Command::new(env!("CARGO_BIN_EXE_scythe-mcp"))
            .args(["--url", &url, "--token", TOKEN])
            .env("XDG_CONFIG_HOME", private.join("config"))
            .env("XDG_CACHE_HOME", private.join("cache"))
            .env("XDG_DATA_HOME", private.join("data"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("scythe-mcp runs");
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut answer = |message: Value| {
            writeln!(stdin, "{message}").unwrap();
            let mut line = String::new();
            stdout.read_line(&mut line).unwrap();
            serde_json::from_str::<Value>(&line).unwrap()
        };
        let call = |id: u64, name: &str, arguments: Value| json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": arguments}});
        let init = answer(
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-11-25", "capabilities": {}}}),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "scythe");
        let added = answer(call(2, "track_add", json!({"name": "Keys"})));
        assert_eq!(added["result"]["isError"], false, "{added}");
        let panned = answer(call(5, "track_pan", json!({"track": "Keys", "pan": 0.5, "width": 1.0})));
        let text = panned["result"]["content"][0]["text"].as_str().unwrap();
        assert!(panned["result"]["isError"] == false && text.contains("ignored") && text.contains("width"), "{panned}");
        let muted = answer(call(6, "track_mute", json!({"track": "Keys", "mute": true})));
        assert_eq!(muted["result"]["isError"], true, "{muted}");
        let missing = answer(call(3, "send_add", json!({"track": "Keys", "dest": "Nowhere"})));
        assert_eq!(missing["result"]["content"][0]["text"], "not_found: no track named \"Nowhere\"");
        let opened = answer(call(4, "project_new", json!({})));
        assert!(opened["result"]["content"][0]["text"].as_str().unwrap().contains("Tracks (0)"), "{opened}");
        drop(stdin);
        assert!(child.wait().unwrap().success());
    });
    while !client.is_finished() {
        session.poll();
        pump.wait(&mut session, Duration::from_millis(10));
    }
    client.join().unwrap();
    drop(server);
    assert!(session.project.tracks.is_empty());
}
