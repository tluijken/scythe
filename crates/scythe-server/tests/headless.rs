#![cfg(unix)]
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

    let status = Command::new(env!("CARGO_BIN_EXE_scythe-server"))
        .args(["--status", "--control", &addr.to_string()])
        .env("XDG_CONFIG_HOME", &dir)
        .output()
        .expect("scythe-server --status runs");
    let report = String::from_utf8_lossy(&status.stdout);
    assert!(status.status.success(), "{report}");
    assert!(report.contains("0 client(s) connected") && report.contains("status: silent"), "{report}");
    assert!(report.contains("transport: stopped at 1.1.00") && report.contains("xruns 0"), "{report}");

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

#[test]
fn installs_a_user_service_running_the_same_session() {
    let dir = std::env::temp_dir().join(format!("scythe-service-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_scythe-server"))
        .args(["--install-service", "set.RPP", "--output-device", "Scarlett", "--null-audio"])
        .current_dir(&dir)
        .env("XDG_CONFIG_HOME", &dir)
        .output()
        .expect("scythe-server --install-service runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("loginctl enable-linger"));
    let unit = std::fs::read_to_string(dir.join("systemd/user/scythe-server.service")).unwrap();
    let exec = format!(
        "ExecStart={} {} --output-device Scarlett --null-audio\n",
        env!("CARGO_BIN_EXE_scythe-server"),
        dir.join("set.RPP").display()
    );
    assert!(unit.contains(&exec), "{unit}");
    assert!(unit.contains("Restart=on-failure\n") && unit.contains("WantedBy=default.target\n"), "{unit}");
    std::fs::remove_dir_all(&dir).unwrap();
}
