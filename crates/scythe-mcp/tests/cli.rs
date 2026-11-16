//! `--version` and `--help` print and exit, without a window, a device or a server.

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn run(flag: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_scythe-mcp"))
        .arg(flag)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("scythe-mcp runs");
    let deadline = Instant::now() + Duration::from_secs(20);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            child.kill().unwrap();
            panic!("scythe-mcp {flag} did not exit");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn version_prints_the_version() {
    let out = run("--version");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), concat!("scythe-mcp ", env!("CARGO_PKG_VERSION"), "\n"));
}

#[test]
fn help_prints_the_usage() {
    let out = run("--help");
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.starts_with("usage: scythe-mcp") && text.contains("--version"), "{text}");
}
