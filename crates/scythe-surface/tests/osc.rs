mod common;

use std::collections::HashMap;
use std::net::UdpSocket;
use std::time::{Duration, Instant};

use scythe_core::surface::{OscSurface as OscConfig, Surface};
use scythe_session::Session;
use scythe_surface::SurfaceHost;
use scythe_surface::osc::codec::{self, Arg, Message};
use scythe_surface::osc::pattern::PatternFile;

/// A UDP device: sends messages to Scythe and keeps the latest value of each feedback address.
struct Device {
    socket: UdpSocket,
    host: u16,
    seen: HashMap<String, Arg>,
}

impl Device {
    fn new() -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_read_timeout(Some(Duration::from_millis(5))).unwrap();
        Self { socket, host: 0, seen: HashMap::new() }
    }

    fn port(&self) -> u16 {
        self.socket.local_addr().unwrap().port()
    }

    fn send(&self, address: &str, args: Vec<Arg>) {
        let bytes = Message::new(address, args).encode();
        self.socket.send_to(&bytes, ("127.0.0.1", self.host)).unwrap();
    }

    /// Pumps the host for `ms` milliseconds, collecting feedback.
    fn run(&mut self, host: &mut SurfaceHost, s: &mut Session, ms: u64) {
        let until = Instant::now() + Duration::from_millis(ms);
        let mut buf = [0u8; 65536];
        while Instant::now() < until {
            host.pump(s);
            while let Ok(n) = self.socket.recv(&mut buf) {
                for m in codec::decode(&buf[..n]).unwrap() {
                    self.seen.insert(m.address, m.args[0].clone());
                }
            }
        }
    }

    fn float(&self, address: &str) -> f32 {
        match self.seen.get(address) {
            Some(Arg::Float(f)) => *f,
            other => panic!("{address}: {other:?}"),
        }
    }

    fn text(&self, address: &str) -> &str {
        match self.seen.get(address) {
            Some(Arg::Str(s)) => s,
            other => panic!("{address}: {other:?}"),
        }
    }
}

fn free_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn setup(name: &str) -> (Session, SurfaceHost, Device) {
    let mut s = common::session(name);
    let mut device = Device::new();
    device.host = free_port();
    s.prefs.surfaces = vec![Surface::Osc(OscConfig {
        name: "test".into(),
        local_port: device.host,
        device_port: device.port(),
        ..Default::default()
    })];
    let mut host = SurfaceHost::new(std::sync::Arc::new(|| {}));
    device.run(&mut host, &mut s, 300);
    (s, host, device)
}

#[test]
fn feedback_matches_what_reaper_sends() {
    let (s, _host, d) = setup("osc-feedback");
    assert!(s.surface_status[0].running, "{:?}", s.surface_status);
    // Values REAPER 7.73 sent for the same project with its Default.ReaperOSC.
    assert_eq!(d.text("/track/1/name"), "Kick");
    assert_eq!(d.text("/track/1/number/str"), "1");
    assert!((d.float("/track/1/volume") - 0.716).abs() < 1e-6);
    assert_eq!(d.text("/track/1/volume/str"), "0.00dB");
    assert!((d.float("/track/2/volume") - 0.592449).abs() < 5e-5);
    assert_eq!(d.text("/track/2/volume/str"), "-6.02dB");
    assert!((d.float("/track/2/volume/db") + 6.0206).abs() < 1e-3);
    assert_eq!(d.float("/track/2/pan"), 0.375);
    assert_eq!(d.text("/track/2/pan/str"), "25%L");
    assert_eq!(d.float("/track/2/mute"), 1.0);
    assert_eq!(d.float("/track/2/mute/toggle"), 1.0);
    assert!((d.float("/track/3/volume") - 0.852774).abs() < 5e-5);
    assert_eq!(d.text("/track/3/volume/str"), "+6.02dB");
    assert_eq!(d.text("/track/3/pan/str"), "50%R");
    assert_eq!(d.float("/track/3/solo"), 1.0);
    assert_eq!(d.float("/anysolo"), 1.0);
    // Empty bank slots and the device's selected track (the master at first).
    assert_eq!(d.text("/track/4/name"), "Track 4");
    assert_eq!(d.text("/track/4/volume/str"), "-inf dB");
    assert!((d.float("/track/4/volume") - 0.002514).abs() < 1e-6);
    assert_eq!(d.float("/track/4/volume/db"), -150.0);
    assert_eq!(d.text("/track/name"), "MASTER");
    assert_eq!(d.text("/track/number/str"), "0");
    assert!((d.float("/master/volume") - 0.716).abs() < 1e-6);
    assert_eq!(d.text("/master/pan/str"), "C");
    assert_eq!(d.float("/tempo"), 0.3125);
    assert_eq!(d.float("/tempo/raw"), 120.0);
    assert_eq!(d.text("/tempo/str"), "120");
    assert_eq!(d.float("/stop"), 1.0);
    assert_eq!(d.float("/play"), 0.0);
    assert_eq!(d.float("/click"), 0.0);
    assert_eq!(d.float("/device/track/select/1"), 0.0);
    assert_eq!(d.text("/track/1/send/1/name"), "Send 1");
    assert_eq!(d.text("/track/1/fx/1/name"), "");
}

#[test]
fn device_messages_change_the_session() {
    let (mut s, mut host, mut d) = setup("osc-input");
    d.send("/track/1/volume", vec![Arg::Float(0.5)]);
    d.send("/track/2/mute", vec![Arg::Float(0.0)]);
    d.send("/track/3/solo/toggle", vec![]);
    d.send("/track/1/pan", vec![Arg::Float(0.25)]);
    d.send("/track/2/volume/db", vec![Arg::Float(-12.0)]);
    d.send("/track/1/recarm", vec![Arg::Float(1.0)]);
    d.send("/track/3/name", vec![Arg::Str("Lead".into())]);
    d.send("/action", vec![Arg::Int(40364)]);
    d.send("/marker/2", vec![]);
    d.send("/tempo/raw", vec![Arg::Float(100.0)]);
    d.run(&mut host, &mut s, 300);
    let t = |i: usize| common::track(&s, i).clone();
    assert!((scythe_surface::view::gain_to_fader(t(0).volume) - 0.5).abs() < 1e-6);
    assert!(!t(1).muted);
    assert!(!t(2).solo);
    assert_eq!(t(0).pan, -0.5);
    assert!((scythe_core::units::gain_to_db(t(1).volume) + 12.0).abs() < 1e-9);
    assert!(t(0).armed);
    assert_eq!(t(2).name, "Lead");
    assert!(s.metronome());
    assert_eq!(s.project.cursor, 8.0);
    assert_eq!(s.project.tempo.bpm, 100.0);
    // Feedback follows.
    assert_eq!(d.float("/click"), 1.0);
    assert_eq!(d.text("/track/3/name"), "Lead");
    assert_eq!(d.float("/anysolo"), 0.0);
    assert_eq!(d.text("/track/1/pan/str"), "50%L");
    assert_eq!(d.text("/tempo/str"), "100");
    // A toggle released (argument 0) does nothing.
    d.send("/track/1/mute/toggle", vec![Arg::Float(0.0)]);
    d.run(&mut host, &mut s, 100);
    assert!(!common::track(&s, 0).muted);
}

#[test]
fn device_selection_banks_and_actions() {
    let (mut s, mut host, mut d) = setup("osc-device");
    d.send("/device/track/select/2", vec![]);
    d.run(&mut host, &mut s, 150);
    assert_eq!(d.text("/track/name"), "Bass");
    assert_eq!(d.float("/device/track/select/2"), 1.0);
    d.send("/track/volume", vec![Arg::Float(0.716)]);
    d.send("/device/track/+", vec![]);
    d.run(&mut host, &mut s, 150);
    assert!((common::track(&s, 1).volume - 1.0).abs() < 1e-5);
    assert_eq!(d.text("/track/name"), "Vox");
    // A bank of two tracks: the second bank starts at track 3.
    d.send("/device/track/count", vec![Arg::Int(2)]);
    d.send("/device/track/bank/+", vec![]);
    d.run(&mut host, &mut s, 150);
    assert_eq!(d.text("/track/1/name"), "Vox");
    assert_eq!(d.text("/track/2/name"), "Track 4");
    // Scythe's named commands through /action/str, and REAPER's ids through /action/<id>.
    scythe_session::api::execute_json(&mut s, serde_json::json!({"cmd": "setlist.create", "songs": [0, 1]})).unwrap();
    d.send("/action/str", vec![Arg::Str("_SCYTHE_SETLIST_PLAY".into())]);
    d.run(&mut host, &mut s, 100);
    assert!(s.is_playing());
    assert_eq!(d.float("/play"), 1.0);
    assert_eq!(d.text("/setlist/song/name"), "Verse");
    assert_eq!(d.text("/setlist/song/number/str"), "1");
    assert_eq!(d.text("/setlist/next/name"), "Chorus");
    d.send("/action/1016", vec![]);
    d.run(&mut host, &mut s, 100);
    assert!(!s.is_playing());
    assert_eq!(d.float("/stop"), 1.0);
}

#[test]
fn reads_reapers_own_pattern_file() {
    let path = std::path::Path::new("/nix/store")
        .read_dir()
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().join("opt/REAPER/Plugins/Default.ReaperOSC"))
        .find(|p| p.exists());
    let Some(path) = path else {
        eprintln!("REAPER is not installed; skipped");
        return;
    };
    let f = PatternFile::parse(&std::fs::read_to_string(path).unwrap());
    assert!(f.warnings.is_empty(), "{:?}", f.warnings);
    let ours = PatternFile::parse(scythe_surface::osc::pattern::DEFAULT);
    assert!(ours.warnings.is_empty(), "{:?}", ours.warnings);
    // Every pattern Scythe's default lists is REAPER's too, apart from Scythe's additions.
    for (action, patterns) in &ours.actions {
        if action.starts_with("SETLIST_") {
            continue;
        }
        for p in patterns {
            assert!(f.patterns(action).contains(p), "{action} {:?}", p.segments);
        }
    }
    assert_eq!(f.settings, ours.settings);
}
