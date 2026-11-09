use std::io::{BufRead, BufReader, Write};
use std::process::{ChildStdin, Command, Stdio};

use scythe_core::Project;
use serde_json::{Value, json};

struct Client {
    stdin: ChildStdin,
    stdout: Box<dyn BufRead>,
    next: u64,
}

impl Client {
    fn send(&mut self, message: &Value) {
        writeln!(self.stdin, "{message}").unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params}));
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        let answer: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line:?}"));
        assert_eq!(answer["id"], self.next, "{answer}");
        answer
    }

    fn call(&mut self, name: &str, arguments: Value) -> (bool, String) {
        let answer = self.request("tools/call", json!({"name": name, "arguments": arguments}));
        let result = &answer["result"];
        (result["isError"] == true, result["content"][0]["text"].as_str().unwrap_or_default().to_owned())
    }
}

#[test]
fn stdio_against_an_in_process_session() {
    let dir = std::env::temp_dir().join(format!("scythe-mcp-stdio-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("band.RPP");
    let mut project = Project::default();
    project.add_track("Drums");
    project.add_track("Bass");
    scythe_core::rpp::save_project(&project, &path).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_scythe-mcp"))
        .args(["--headless", "--null-audio"])
        .arg(&path)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("scythe-mcp runs");
    let mut client = Client {
        stdin: child.stdin.take().unwrap(),
        stdout: Box::new(BufReader::new(child.stdout.take().unwrap())),
        next: 0,
    };

    let init = client.request(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}),
    );
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    let newer = client.request("initialize", json!({"protocolVersion": "2099-01-01", "capabilities": {}}));
    assert_eq!(newer["result"]["protocolVersion"], scythe_mcp::PROTOCOL_VERSIONS[0]);
    client.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    assert_eq!(client.request("ping", json!({}))["result"], json!({}));

    let tools = client.request("tools/list", json!({}));
    let names: Vec<&str> =
        tools["result"]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
    assert_eq!(names.first(), Some(&"get_project"));
    assert!(names.contains(&"track_volume") && names.contains(&"scythe_command"), "{names:?}");

    let (error, text) = client.call("get_project", json!({}));
    assert!(!error, "{text}");
    assert!(text.starts_with("Project \"band\""), "{text}");
    assert!(text.contains("\"Drums\"") && text.contains("\"Bass\""), "{text}");

    let (error, text) = client.call("track_volume", json!({"track": "Bass", "db": -3.0}));
    assert!(!error, "{text}");
    let track: Value = serde_json::from_str(&text).unwrap();
    assert!((track["volume_db"].as_f64().unwrap() + 3.0).abs() < 1e-9);
    let (error, text) = client.call("fx_catalog", json!({"query": "compressor"}));
    assert!(!error && text.contains("builtin:compressor"), "{text}");
    let (error, text) = client.call("fx_add", json!({"track": "Bass", "plugin": "builtin:compressor"}));
    assert!(!error, "{text}");
    let (error, text) = client.call("track_volume", json!({"track": "Nobody", "db": -3.0}));
    assert!(error && text.starts_with("not_found"), "{text}");
    let (error, text) = client.call("track_pan", json!({"track": "Bass", "pan": "left"}));
    assert!(error && text.starts_with("invalid"), "{text}");
    let (error, text) = client.call("scythe_command", json!({"command": {"cmd": "project.undo"}}));
    assert!(!error && text.contains("Add plugin"), "{text}");
    let unknown = client.request("tools/call", json!({"name": "launch_rockets", "arguments": {}}));
    assert_eq!(unknown["error"]["code"], -32602);

    let resources = client.request("resources/list", json!({}));
    let uris: Vec<&str> =
        resources["result"]["resources"].as_array().unwrap().iter().filter_map(|r| r["uri"].as_str()).collect();
    assert!(uris.contains(&"scythe://project") && uris.contains(&"scythe://docs/control-api"));
    let docs = client.request("resources/read", json!({"uri": "scythe://docs/control-api"}));
    assert!(docs["result"]["contents"][0]["text"].as_str().unwrap().starts_with("# Control API"));
    let summary = client.request("resources/read", json!({"uri": "scythe://project/summary"}));
    assert!(summary["result"]["contents"][0]["text"].as_str().unwrap().contains("-3.0 dB"));
    let missing = client.request("resources/read", json!({"uri": "scythe://nothing"}));
    assert_eq!(missing["error"]["code"], -32002);

    let prompts = client.request("prompts/list", json!({}));
    assert_eq!(prompts["result"]["prompts"][0]["name"], "mix_check");
    let prompt = client.request("prompts/get", json!({"name": "mix_check", "arguments": {"focus": "the bass"}}));
    assert!(prompt["result"]["messages"][0]["content"]["text"].as_str().unwrap().contains("the bass"));
    assert_eq!(client.request("tools/frobnicate", json!({}))["error"]["code"], -32601);

    writeln!(client.stdin, "this is not json").unwrap();
    let mut line = String::new();
    client.stdout.read_line(&mut line).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["error"]["code"], -32700);

    drop(client);
    let status = child.wait().unwrap();
    assert!(status.success(), "{status}");
    std::fs::remove_dir_all(&dir).unwrap();
}
