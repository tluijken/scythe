//! The desktop app as a process on an X server of its own, driven through its control API.

use std::time::{Duration, Instant};

use scythe_itest::{ADMIN, Scythe, Xvfb, http, live_fixture, test_dir, x11};
use serde_json::{Value, json};

/// Round trips of `n` requests as the web UI makes them, a fader move and the snapshot by
/// turns: the slowest and the median, in milliseconds.
fn latency(s: &Scythe, n: usize) -> (f64, f64) {
    let mut times: Vec<f64> = (0..n)
        .map(|i| {
            let start = Instant::now();
            if i % 2 == 0 {
                s.ok(json!({"cmd": "track.volume", "track": "Keys", "db": -(i as f64) / 4.0}));
            } else {
                let (status, _) = http(s.addr, "GET", "/api/snapshot", Some(ADMIN), None);
                assert_eq!(status, 200);
            }
            start.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    times.sort_by(f64::total_cmp);
    (times[n - 1], times[n / 2])
}

#[test]
fn the_window_serves_a_live_session_shown_and_hidden() {
    let dir = test_dir("app_window");
    let Some(x) = Xvfb::start(&dir, "the_window_serves_a_live_session_shown_and_hidden") else { return };
    let project = live_fixture(&dir);
    let mut s = Scythe::app(&dir, &project, &x.display);

    let snap = s.snapshot();
    assert_eq!(snap["name"], "live", "{}", s.proc.tail());
    s.ok(json!({"cmd": "track.add", "name": "Vox"}));
    s.ok(json!({"cmd": "track.input", "track": "Vox", "input": 1}));
    s.ok(json!({"cmd": "mix.create", "musician": "Drummer", "own": ["Drums"]}));
    s.ok(json!({"cmd": "mix.more_me", "mix": "MON Drummer", "db": 6.0}));
    s.ok(json!({"cmd": "setlist.create", "name": "Gig", "songs": ["Opener", "Ballad"]}));
    let (status, link) = s.send(ADMIN, json!({"cmd": "access.create", "role": "musician", "musician": "Drummer"}));
    assert_eq!(status, 200, "{link}");
    let drummer = link["result"]["token"].as_str().unwrap().to_owned();
    assert_eq!(s.send(&drummer, json!({"cmd": "track.add"})).0, 403);
    assert_eq!(s.send(&drummer, json!({"cmd": "mix.more_me", "mix": "MON Drummer", "db": 3.0})).0, 200);

    let shown = latency(&s, 20);
    eprintln!("command round trip, window shown: max {:.1} ms, median {:.1} ms", shown.0, shown.1);

    // Opener plays from 0 s to its stop marker at 4 s; the playhead then returns to its start.
    let (played, t) = s.play_until_stopped(0);
    assert!((3.6..=5.0).contains(&played), "Opener played {played:.2} s in the window's session: {t}");

    let mut display = x11::Display::connect(&x.display).expect("X connection");
    let windows = display.top_level().unwrap();
    assert!(!windows.is_empty(), "the app shows no window");
    for w in &windows {
        display.unmap(*w).unwrap();
    }
    display.sync().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let hidden = latency(&s, 20);
    eprintln!("command round trip, window hidden: max {:.1} ms, median {:.1} ms", hidden.0, hidden.1);
    s.ok(json!({"cmd": "transport.seek", "position": 6.0}));
    s.ok(json!({"cmd": "transport.play"}));
    let t = s.wait_for("playback while hidden", Duration::from_secs(5), json!({"cmd": "transport.state"}), |t| {
        t["position"].as_f64().unwrap() > 6.5
    });
    assert_eq!(t["playing"], true);
    s.ok(json!({"cmd": "transport.stop"}));
    for w in &windows {
        display.map(*w).unwrap();
    }
    display.sync().unwrap();

    std::thread::sleep(Duration::from_secs(1));
    assert!(s.proc.alive(), "the app died:\n{}", s.proc.tail());
    let snap: Value = s.snapshot();
    let names: Vec<&str> = snap["tracks"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Drums", "Bass", "Keys", "Vox", "MON Drummer"]);
    assert!(shown.0 < 250.0 && hidden.0 < 250.0, "slow round trips: shown {shown:?}, hidden {hidden:?} (ms)");
}
