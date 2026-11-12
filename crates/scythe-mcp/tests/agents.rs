//! `scythe-mcp --headless` as an agent building a session sees it: the tool selection and its
//! changes, notifications for subscribed resources, measuring tools that wait for their job,
//! and the prompts.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use scythe_core::Project;
use serde_json::{Value, json};

struct Client {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<Value>,
    notifications: Vec<Value>,
    next: u64,
}

impl Client {
    fn start(args: &[&str]) -> Self {
        let dir = std::env::temp_dir().join(format!("scythe-mcp-agents-{}", std::process::id()));
        let mut child = Command::new(env!("CARGO_BIN_EXE_scythe-mcp"))
            .args(args)
            .env("XDG_DATA_HOME", dir.join("data"))
            .env("XDG_CONFIG_HOME", dir.join("config"))
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("scythe-mcp runs");
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if tx.send(serde_json::from_str::<Value>(&line).unwrap_or_else(|e| panic!("{e}: {line}"))).is_err() {
                    return;
                }
            }
        });
        let stdin = child.stdin.take().unwrap();
        Self { child, stdin, lines, notifications: Vec::new(), next: 0 }
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let message = json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params});
        writeln!(self.stdin, "{message}").unwrap();
        loop {
            let line = self.lines.recv_timeout(Duration::from_secs(120)).expect("an answer");
            if line.get("id").is_none() {
                self.notifications.push(line);
                continue;
            }
            assert_eq!(line["id"], self.next, "{line}");
            return line;
        }
    }

    fn call(&mut self, name: &str, arguments: Value) -> (bool, String) {
        let answer = self.request("tools/call", json!({"name": name, "arguments": arguments}));
        let result = &answer["result"];
        (result["isError"] == true, result["content"][0]["text"].as_str().unwrap_or_default().to_owned())
    }

    fn tool_names(&mut self) -> Vec<String> {
        let tools = self.request("tools/list", json!({}));
        tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_owned()).collect()
    }

    /// Waits for a notification with `method`.
    fn notified(&mut self, method: &str) -> Value {
        if let Some(at) = self.notifications.iter().position(|n| n["method"] == method) {
            return self.notifications.remove(at);
        }
        loop {
            let line = self.lines.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|_| panic!("no {method}"));
            if line["method"] == method {
                return line;
            }
            self.notifications.push(line);
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn project(dir: &std::path::Path) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join("live.RPP");
    let mut project = Project::default();
    project.add_track("Vox");
    scythe_core::rpp::save_project(&project, &path).unwrap();
    path
}

#[test]
fn compact_tools_by_default_and_more_on_request() {
    let dir = std::env::temp_dir().join(format!("scythe-mcp-tools-{}", std::process::id()));
    let path = project(&dir);
    let path = path.to_str().unwrap();
    let mut client = Client::start(&["--headless", "--null-audio", path]);
    let init = client.request("initialize", json!({"protocolVersion": "2025-11-25", "capabilities": {}}));
    let caps = &init["result"]["capabilities"];
    assert_eq!(
        (caps["tools"]["listChanged"].as_bool(), caps["resources"]["subscribe"].as_bool()),
        (Some(true), Some(true))
    );
    let names = client.tool_names();
    assert!(names.len() < 50, "{} tools by default", names.len());
    for name in ["get_project", "scythe_command", "scythe_tools", "media_import", "analysis_match_eq", "events_since"] {
        assert!(names.contains(&name.to_owned()), "{name} missing: {names:?}");
    }
    assert!(!names.contains(&"mix_more_me".to_owned()));
    let (error, text) = client.call("mix_create", json!({"musician": "Drummer"}));
    assert!(!error, "an unlisted tool still runs: {text}");

    let (error, text) = client.call("scythe_tools", json!({"enable": ["mix", "access"]}));
    assert!(!error, "{text}");
    let changed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(changed["selection"], "default,access,mix");
    client.notified("notifications/tools/list_changed");
    let names = client.tool_names();
    assert!(names.contains(&"mix_more_me".to_owned()) && names.contains(&"access_create".to_owned()));
    let (error, text) = client.call("access_list", json!({}));
    assert!(error && text.contains("scythe-server"), "{text}");
    let (error, text) = client.call("scythe_tools", json!({"enable": ["bogus"]}));
    assert!(error && text.contains("no tool domain"), "{text}");
    let (error, _) = client.call("script_run", json!({"code": "reaper.ShowConsoleMsg('hi')"}));
    assert!(error, "the headless session runs no scripts");

    let subscribed = client.request("resources/subscribe", json!({"uri": "scythe://project"}));
    assert_eq!(subscribed["result"], json!({}));
    std::thread::sleep(Duration::from_millis(400));
    let (error, _) = client.call("track_add", json!({"name": "Bass"}));
    assert!(!error);
    let (error, text) = client.call("track_pan", json!({"track": "Bass", "pan": 0.5, "width": 1.0}));
    assert!(!error && text.contains("ignored") && text.contains("\"width\""), "{text}");
    let (error, text) = client.call("track_mute", json!({"track": "Bass", "mute": true}));
    assert!(error && text.starts_with("invalid") && text.contains("\"muted\""), "{text}");
    let updated = client.notified("notifications/resources/updated");
    assert_eq!(updated["params"]["uri"], "scythe://project");
    let bad = client.request("resources/subscribe", json!({"uri": "scythe://docs/mcp"}));
    assert_eq!(bad["error"]["code"], -32002);
    assert_eq!(client.request("logging/setLevel", json!({"level": "info"}))["result"], json!({}));
    assert_eq!(client.request("logging/setLevel", json!({"level": "loud"}))["error"]["code"], -32602);

    let (error, text) = client.call("events_since", json!({}));
    assert!(!error && text.contains("project_changed"), "{text}");
    let (error, text) = client.call("events_meters", json!({}));
    assert!(!error && text.contains("\"Bass\""), "{text}");

    let prompts = client.request("prompts/list", json!({}));
    let names: Vec<&str> =
        prompts["result"]["prompts"].as_array().unwrap().iter().filter_map(|p| p["name"].as_str()).collect();
    for name in ["build_live_session", "setlist_from_backing_tracks", "match_reference"] {
        assert!(names.contains(&name), "{names:?}");
    }
    let prompt = client
        .request("prompts/get", json!({"name": "match_reference", "arguments": {"reference": "/refs/song.flac"}}));
    let text = prompt["result"]["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(text.contains("/refs/song.flac") && text.contains("analysis_match_eq"), "{text}");
    let prompt =
        client.request("prompts/get", json!({"name": "build_live_session", "arguments": {"band": "Vox on 1"}}));
    assert!(prompt["result"]["messages"][0]["content"]["text"].as_str().unwrap().contains("Vox on 1"));
    drop(client);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn measuring_tools_wait_for_their_result() {
    let dir = std::env::temp_dir().join(format!("scythe-mcp-measure-{}", std::process::id()));
    let path = project(&dir);
    let tone = dir.join("tone.wav");
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(&tone, spec).unwrap();
    for i in 0..48_000 * 4 {
        let s = (0.1 * (std::f64::consts::TAU * 997.0 * i as f64 / 48_000.0).sin() * 32767.0) as i16;
        w.write_sample(s).unwrap();
        w.write_sample(s).unwrap();
    }
    w.finalize().unwrap();
    let mut client =
        Client::start(&["--headless", "--null-audio", "--tools", "media,analysis", path.to_str().unwrap()]);
    client.request("initialize", json!({"protocolVersion": "2025-11-25", "capabilities": {}}));
    let names = client.tool_names();
    assert!(names.contains(&"analysis_job".to_owned()) && !names.contains(&"track_add".to_owned()), "{names:?}");

    let (error, text) = client.call("media_probe", json!({"path": tone}));
    assert!(!error, "{text}");
    let probe: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(probe["status"], "done", "{probe}");
    assert!((probe["result"]["integrated_lufs"].as_f64().unwrap() + 20.0).abs() < 0.2, "{probe}");
    let (error, text) = client.call("media_import", json!({"paths": [tone], "track": "Vox"}));
    assert!(!error, "{text}");
    let (error, text) = client.call("analysis_mix", json!({}));
    assert!(!error, "{text}");
    let mix: Value = serde_json::from_str(&text).unwrap();
    assert!((mix["result"]["integrated_lufs"].as_f64().unwrap() + 20.0).abs() < 0.3, "{mix}");
    let (error, text) =
        client.call("analysis_compare", json!({"reference": {"file": tone}, "mix": {"job": mix["job"]}}));
    assert!(!error, "{text}");
    assert!(text.contains("Loudness matches"), "{text}");
    let (error, text) = client.call("analysis_file", json!({"path": dir.join("missing.wav")}));
    assert!(error && text.starts_with("not_found"), "{text}");
    drop(client);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn unknown_tool_domains_are_refused_at_start() {
    let status = Command::new(env!("CARGO_BIN_EXE_scythe-mcp"))
        .args(["--headless", "--null-audio", "--tools", "default,nonsense"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
}

#[test]
fn the_session_sees_the_agent_and_its_status() {
    let dir = std::env::temp_dir().join(format!("scythe-mcp-status-{}", std::process::id()));
    let path = project(&dir);
    let mut client = Client::start(&["--headless", "--null-audio", path.to_str().unwrap()]);
    let init = client.request(
        "initialize",
        json!({"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "probe", "version": "0.1"}}),
    );
    assert!(init["result"]["instructions"].as_str().unwrap().contains("set_status"));
    assert!(client.tool_names().contains(&"set_status".to_owned()));
    let (error, text) = client.call("set_status", json!({"text": "Checking the vocal chain"}));
    assert!(!error, "{text}");
    let (error, text) = client.call("track_volume", json!({"track": "Vox", "db": -4.5}));
    assert!(!error, "{text}");
    let (error, text) = client.call("scythe_command", json!({"command": {"cmd": "agents.activity"}}));
    assert!(!error, "{text}");
    let activity: Value = serde_json::from_str(&text).unwrap();
    let agent = &activity["clients"][0];
    assert_eq!((agent["name"].as_str(), agent["transport"].as_str()), (Some("probe"), Some("stdio")));
    assert_eq!(activity["status"], "Checking the vocal chain");
    let calls = activity["calls"].as_array().unwrap();
    assert_eq!(calls[0]["summary"], "Set Vox volume to -4.5 dB");
    assert_eq!(calls[0]["undo"], json!(["Track volume"]));
    let (error, text) = client.call("scythe_command", json!({"command": {"cmd": "agents.pause"}}));
    assert!(error && text.contains("for the people using Scythe"), "{text}");
    let (error, _) = client.call("set_status", json!({"text": ""}));
    assert!(!error);
    let (_, text) = client.call("scythe_command", json!({"command": {"cmd": "agents.activity"}}));
    assert!(serde_json::from_str::<Value>(&text).unwrap()["status"].is_null(), "{text}");
}
