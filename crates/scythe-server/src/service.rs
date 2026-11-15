//! Running as a service: a systemd user unit that starts the headless session at boot and
//! restarts it when it fails, and `--status` for asking a running session how it is doing.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::config::Options;

pub const UNIT_NAME: &str = "scythe-server.service";

/// Resource limits the unit may ask for: the user's hard limits, which a user unit cannot raise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Highest real-time priority allowed (`RLIMIT_RTPRIO`), 0 for none.
    pub rtprio: u64,
    /// Lockable memory in bytes, `None` for unlimited.
    pub memlock: Option<u64>,
}

impl Limits {
    #[cfg(not(any(windows, target_os = "macos")))]
    pub fn current() -> Self {
        let hard = |resource| {
            let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            // SAFETY: getrlimit only writes the struct it is given.
            let ok = unsafe { libc::getrlimit(resource, &mut limit) } == 0;
            ok.then_some(limit.rlim_max)
        };
        let rtprio = hard(libc::RLIMIT_RTPRIO).unwrap_or(0);
        let memlock = hard(libc::RLIMIT_MEMLOCK).filter(|&m| m != libc::RLIM_INFINITY);
        Self { rtprio: if rtprio == libc::RLIM_INFINITY { 99 } else { rtprio }, memlock }
    }

    /// Neither limit applies on Windows and macOS: MMCSS and the time-constraint policy grant
    /// real-time scheduling without one.
    #[cfg(any(windows, target_os = "macos"))]
    pub fn current() -> Self {
        Self { rtprio: 99, memlock: None }
    }
}

/// Quotes one `ExecStart` argument for systemd.
fn quote(arg: &str) -> String {
    let escaped = arg.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%").replace('$', "$$");
    if escaped.is_empty() || escaped.contains(|c: char| c.is_whitespace() || c == '\'' || c == ';') {
        format!("\"{escaped}\"")
    } else {
        escaped
    }
}

/// The unit that runs `exe` with `args`.
pub fn unit_text(exe: &Path, args: &[String], workdir: Option<&Path>, limits: Limits) -> String {
    let command: Vec<String> =
        std::iter::once(exe.to_string_lossy().into_owned()).chain(args.iter().cloned()).collect();
    let exec = command.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ");
    let mut service = vec![
        "Type=simple".to_owned(),
        format!("ExecStart={exec}"),
        "Restart=on-failure".to_owned(),
        "RestartSec=2".to_owned(),
        // On SIGTERM the session writes unsaved changes to the project's Backups folder.
        "TimeoutStopSec=20".to_owned(),
    ];
    if let Some(dir) = workdir {
        service.insert(2, format!("WorkingDirectory={}", quote(&dir.to_string_lossy())));
    }
    // Asking for more than the user's hard limit stops a user unit from starting at all.
    if limits.rtprio > 0 {
        service.push(format!("LimitRTPRIO={}", limits.rtprio.min(95)));
    }
    service.push(format!("LimitMEMLOCK={}", limits.memlock.map_or("infinity".to_owned(), |m| m.to_string())));
    format!(
        "[Unit]\n\
         Description=Scythe live session (headless)\n\
         Documentation=https://github.com/tluijken/scythe/blob/main/docs/live.md\n\
         After=sound.target pipewire.service network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         {}\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        service.join("\n")
    )
}

/// The arguments the service runs with: those given, without `--install-service`, with the
/// project and settings file made absolute (a service starts in another directory).
pub fn service_args(args: &[String], cwd: &Path) -> Vec<String> {
    let absolute = |p: &str| {
        let path = Path::new(p);
        if path.is_absolute() { p.to_owned() } else { cwd.join(path).to_string_lossy().into_owned() }
    };
    let mut out = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--install-service" | "--status" => {}
            "--config" => {
                out.push(arg.clone());
                if let Some(path) = it.next() {
                    out.push(absolute(path));
                }
            }
            a if a.starts_with("--config=") => out.push(format!("--config={}", absolute(&a["--config=".len()..]))),
            a if crate::config::VALUED.contains(&a) => {
                out.push(arg.clone());
                if let Some(value) = it.next() {
                    out.push(value.clone());
                }
            }
            a if a.starts_with("--") => out.push(arg.clone()),
            project => out.push(absolute(project)),
        }
    }
    out
}

/// `$XDG_CONFIG_HOME/systemd/user`.
fn unit_dir() -> Option<PathBuf> {
    scythe_core::paths::config_home().map(|c| c.join("systemd").join("user"))
}

/// Where a systemd user unit does something: Linux. Elsewhere the file would only sit there.
fn has_systemd(os: &str) -> Result<(), String> {
    if os == "linux" {
        return Ok(());
    }
    Err(format!(
        "--install-service writes a systemd user unit, which {os} does not run; start scythe-server \
         from {os}'s own service manager instead"
    ))
}

/// Writes the user unit for `args` and says how to enable it.
pub fn install(args: &[String], options: &Options) -> Result<(), String> {
    has_systemd(std::env::consts::OS)?;
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this program: {e}"))?;
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let args = service_args(args, &cwd);
    let workdir = options.project.as_ref().map(|p| cwd.join(p)).and_then(|p| p.parent().map(Path::to_path_buf));
    let dir = unit_dir().ok_or("cannot tell where systemd user units go: set HOME")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(UNIT_NAME);
    let text = unit_text(&exe, &args, workdir.as_deref(), Limits::current());
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("wrote {}", path.display());
    println!();
    println!("start it now and at every login:");
    println!("  systemctl --user daemon-reload");
    println!("  systemctl --user enable --now {UNIT_NAME}");
    println!("start it at boot, before anyone logs in:");
    println!("  sudo loginctl enable-linger $USER");
    println!("follow its log:");
    println!("  journalctl --user -u {UNIT_NAME} -f");
    if Limits::current().rtprio == 0 {
        println!();
        println!("note: you may not use real-time priority; add yourself to the audio group with");
        println!("rtprio and memlock limits (see docs/live.md) for glitch-free audio.");
    }
    Ok(())
}

/// One HTTP/1.0 exchange with the control API; the parsed JSON body.
fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&Value>,
) -> Result<Value, String> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))
        .map_err(|e| format!("no Scythe session answers at {addr}: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).map_err(|e| e.to_string())?;
    let body = body.map(Value::to_string).unwrap_or_default();
    let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    let head = format!(
        "{method} {path} HTTP/1.0\r\nHost: {addr}\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).and_then(|()| stream.write_all(body.as_bytes())).map_err(|e| e.to_string())?;
    let mut response = String::new();
    stream.read_to_string(&mut response).map_err(|e| e.to_string())?;
    let json = response.split_once("\r\n\r\n").map_or("", |(_, b)| b);
    serde_json::from_str(json).map_err(|_| format!("{addr} did not answer like a Scythe session"))
}

/// Asks the session at `options.control` how it is doing and prints it.
pub fn status(options: &Options) -> Result<(), String> {
    let addr = options.control.ok_or("the control API is off, so there is nothing to ask")?;
    let addr = if addr.ip().is_unspecified() { SocketAddr::from(([127, 0, 0, 1], addr.port())) } else { addr };
    let hello = request(addr, "GET", "/api/health", None, None)?;
    let token = options.token.as_deref();
    let ask = |cmd: &str| {
        let answer = request(addr, "POST", "/api/command", token, Some(&json!({"cmd": cmd})))?;
        if answer["ok"] == true {
            Ok(answer["result"].clone())
        } else {
            Err(format!("{cmd}: {}", answer["error"]["message"].as_str().unwrap_or("refused")))
        }
    };
    let health = ask("transport.health")?;
    let transport = ask("transport.state")?;
    let setlist = ask("setlist.status").unwrap_or(Value::Null);
    print!("{}", status_text(&hello, &health, &transport, &setlist));
    Ok(())
}

pub fn status_text(hello: &Value, health: &Value, transport: &Value, setlist: &Value) -> String {
    let num = |v: &Value| v.as_f64().unwrap_or(0.0);
    let mut out = format!(
        "scythe: protocol {}, {} client(s) connected\n",
        hello["protocol"],
        hello["clients"].as_u64().unwrap_or(0)
    );
    let state = match (transport["recording"] == true, transport["playing"] == true) {
        (true, _) => "recording",
        (_, true) => "playing",
        _ => "stopped",
    };
    out += &format!("transport: {state} at {}\n", transport["bar_beat"].as_str().unwrap_or("?"));
    if let Some(name) = setlist["name"].as_str() {
        let song = setlist["current"]["name"].as_str().map(|s| format!(", song {s}")).unwrap_or_default();
        out += &format!("setlist: {name}{song}\n");
    }
    let buffer = match (health["block_size"].as_u64(), health["buffer_ms"].as_f64()) {
        (Some(b), Some(ms)) => format!(", {b} frames ({ms} ms)"),
        _ => String::new(),
    };
    out += &format!(
        "audio: {} @ {} Hz{buffer}, output latency {} ms\n",
        health["device"].as_str().unwrap_or("?"),
        health["sample_rate"],
        num(&health["output_latency_ms"])
    );
    out += &format!(
        "status: {}{}\n",
        health["status"].as_str().unwrap_or("?"),
        health["reason"].as_str().map(|r| format!(" ({r})")).unwrap_or_default()
    );
    out += &format!(
        "cpu: {}% (peak {}%), xruns {}, overloads {}, reconnects {}\n",
        num(&health["cpu"]),
        num(&health["cpu_peak"]),
        health["xruns"],
        health["overloads"],
        health["reconnects"]
    );
    let rt = &health["realtime"];
    out += &match (rt["policy"].as_str(), rt["priority"].as_i64().unwrap_or(0)) {
        (Some(policy), p) if p > 0 => format!("scheduling: {policy} {p} ({})\n", rt["via"].as_str().unwrap_or("?")),
        (Some(_), _) => format!(
            "scheduling: no real-time priority{}\n",
            rt["reason"].as_str().map(|r| format!(" ({r})")).unwrap_or_default()
        ),
        (None, _) => "scheduling: not known yet\n".into(),
    };
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_unit_is_written_only_where_systemd_runs() {
        assert_eq!(has_systemd("linux"), Ok(()));
        for os in ["macos", "windows"] {
            let refused = has_systemd(os).unwrap_err();
            assert!(refused.contains("systemd") && refused.contains(os), "{refused}");
        }
    }

    #[test]
    fn the_unit_restarts_on_failure_within_the_user_limits() {
        let args = ["--config".to_owned(), "/etc/scythe/my gig.toml".to_owned(), "/songs/Set 100%.RPP".to_owned()];
        let limits = Limits { rtprio: 99, memlock: None };
        let text = unit_text(Path::new("/usr/bin/scythe-server"), &args, Some(Path::new("/songs")), limits);
        assert_eq!(
            text,
            "[Unit]\n\
             Description=Scythe live session (headless)\n\
             Documentation=https://github.com/tluijken/scythe/blob/main/docs/live.md\n\
             After=sound.target pipewire.service network-online.target\n\
             Wants=network-online.target\n\
             \n\
             [Service]\n\
             Type=simple\n\
             ExecStart=/usr/bin/scythe-server --config \"/etc/scythe/my gig.toml\" \"/songs/Set 100%%.RPP\"\n\
             WorkingDirectory=/songs\n\
             Restart=on-failure\n\
             RestartSec=2\n\
             TimeoutStopSec=20\n\
             LimitRTPRIO=95\n\
             LimitMEMLOCK=infinity\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n"
        );
        let plain = unit_text(Path::new("/bin/scythe"), &[], None, Limits { rtprio: 0, memlock: Some(65536) });
        assert!(!plain.contains("LimitRTPRIO") && plain.contains("LimitMEMLOCK=65536\n"), "{plain}");
        assert!(!plain.contains("WorkingDirectory"));
    }

    #[cfg(unix)]
    #[test]
    fn service_arguments_are_absolute() {
        let args: Vec<String> = [
            "--install-service",
            "--headless",
            "--config",
            "gig.toml",
            "--output-device",
            "USB",
            "set.RPP",
            "--no-input",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            service_args(&args, Path::new("/home/band")),
            [
                "--headless",
                "--config",
                "/home/band/gig.toml",
                "--output-device",
                "USB",
                "/home/band/set.RPP",
                "--no-input"
            ]
        );
    }

    #[test]
    fn status_reads_like_a_report() {
        let text = status_text(
            &json!({"ok": true, "protocol": 1, "clients": 2}),
            &json!({"device": "Scarlett", "sample_rate": 48000, "block_size": 128, "buffer_ms": 2.67,
                    "output_latency_ms": 5.1, "cpu": 12.5, "cpu_peak": 30.0, "xruns": 3, "overloads": 0,
                    "status": "running", "reason": null, "reconnects": 1,
                    "realtime": {"policy": "SCHED_FIFO", "priority": 20, "via": "rtkit", "reason": null}}),
            &json!({"playing": true, "recording": false, "bar_beat": "12.3.00"}),
            &json!({"name": "Gig", "current": {"name": "Opener"}}),
        );
        assert_eq!(
            text,
            "scythe: protocol 1, 2 client(s) connected\n\
             transport: playing at 12.3.00\n\
             setlist: Gig, song Opener\n\
             audio: Scarlett @ 48000 Hz, 128 frames (2.67 ms), output latency 5.1 ms\n\
             status: running\n\
             cpu: 12.5% (peak 30%), xruns 3, overloads 0, reconnects 1\n\
             scheduling: SCHED_FIFO 20 (rtkit)\n"
        );
        let plain = status_text(
            &json!({}),
            &json!({"realtime": {"policy": "SCHED_OTHER", "priority": 0, "via": "none", "reason": "rtprio limit is 0"}}),
            &json!({}),
            &json!({}),
        );
        assert!(plain.ends_with("scheduling: no real-time priority (rtprio limit is 0)\n"), "{plain}");
    }
}
