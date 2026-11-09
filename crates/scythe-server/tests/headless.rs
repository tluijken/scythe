mod common;

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::http;
use scythe_core::Project;

#[test]
fn starts_without_a_display_serves_and_stops_on_sigterm() {
    let dir = std::env::temp_dir().join(format!("scythe-headless-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("monitors.RPP");
    let mut project = Project::default();
    project.add_track("Singer IEM");
    scythe_core::rpp::save_project(&project, &path).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_scythe-server"))
        .arg(&path)
        .args(["--null-audio", "--control", "127.0.0.1:0"])
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env("XDG_CONFIG_HOME", &dir)
        .stderr(Stdio::piped())
        .spawn()
        .expect("scythe-server runs");

    let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
    let addr = lines
        .by_ref()
        .map_while(Result::ok)
        .find_map(|l| l.strip_prefix("control API: http://").and_then(|a| a.strip_suffix("/api")).map(str::to_owned))
        .expect("the server says where it listens");
    let addr = addr.parse().unwrap();
    let (status, body) = http(addr, "GET", "/api/snapshot", None, None);
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"]["tracks"][0]["name"], "Singer IEM");
    assert_eq!(body["result"]["name"], "monitors");

    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "scythe-server did not stop on SIGTERM");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success(), "{status}");
    let rest: Vec<String> = lines.map_while(Result::ok).collect();
    assert!(rest.iter().any(|l| l == "stopping"), "{rest:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}
