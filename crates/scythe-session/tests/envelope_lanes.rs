//! Take and tempo envelopes and per-send automation modes through the envelope commands.

use std::sync::Arc;

use scythe_core::render_settings::ResampleMode;
use scythe_engine::{Engine, MediaPool};
use scythe_session::Session;
use scythe_session::api::execute_json;
use serde_json::{Value, json};

const PROJECT: &str = "<REAPER_PROJECT 0.1 \"7.73\" 0\n  TEMPO 120 4 4\n  <TRACK\n    NAME a\n    <ITEM\n      POSITION 2\n      LENGTH 6\n      NAME riff\n      <VOLENV\n        ACT 1 -1\n        VIS 1 1 1\n        PT 0 1 0\n        PT 3 0.5 0\n      >\n      <SOURCE MIDI\n        HASDATA 1 960 QN\n        E 7680 b0 7b 00\n      >\n    >\n  >\n  <TRACK\n    NAME b\n    AUXRECV 0 0 1 0 0 0 0 0 0 -1:U 0 -1 ''\n  >\n>\n";

fn with(at: &Value, mut command: Value) -> Value {
    for (k, v) in at.as_object().unwrap() {
        command[k] = v.clone();
    }
    command
}

fn ok(s: &mut Session, command: Value) -> Value {
    execute_json(s, command.clone()).unwrap_or_else(|e| panic!("{command} failed: {e}"))
}

#[test]
fn take_tempo_and_send_mode_envelopes() {
    let dir = std::env::temp_dir().join(format!("scythe-envlanes-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("e.RPP");
    std::fs::write(&path, PROJECT).unwrap();
    let pool = Arc::new(MediaPool::with_resample(ResampleMode::PLAYBACK));
    let mut s = Session::new(Engine::start_offline(pool, 48_000));
    s.open_path(&path).unwrap();
    let (a, item) = (s.project.tracks[0].id.0, s.project.tracks[0].items[0].id.0);

    let take = json!({"track": a, "envelope": "volume", "item": item});
    let got = ok(&mut s, with(&take, json!({"cmd": "envelope.get"})));
    let times: Vec<f64> = got["points"].as_array().unwrap().iter().map(|p| p["time"].as_f64().unwrap()).collect();
    assert_eq!(times, [2.0, 5.0], "take points in project seconds");
    ok(&mut s, with(&take, json!({"cmd": "envelope.point_add", "time": 7.0, "value": 0.25})));
    let env = &s.project.tracks[0].items[0].takes[0].envelopes[0];
    assert_eq!(env.points.last().map(|p| (p.time, p.value)), Some((5.0, 0.25)), "stored from the item start");

    let tempo = json!({"track": a, "envelope": "tempo"});
    ok(&mut s, with(&tempo, json!({"cmd": "envelope.point_add", "time": 4.0, "value": 90.0, "shape": "square"})));
    assert_eq!(s.project.tempo.points.iter().map(|p| (p.time, p.bpm)).collect::<Vec<_>>(), [(0.0, 120.0), (4.0, 90.0)]);

    let m = ok(&mut s, json!({"cmd": "envelope.mode", "track": a, "send": 0, "mode": "write"}));
    assert_eq!((m["mode"].as_str(), m["effective"].as_str()), (Some("write"), Some("write")));
    let text = scythe_core::rpp::project_to_rpp(&s.project);
    assert!(text.contains("-1:U 0 3"), "REAPER's send I_AUTOMODE field: {text}");
    let m = ok(&mut s, json!({"cmd": "envelope.mode", "track": a, "send": 0, "mode": "track"}));
    assert_eq!(m["mode"].as_str(), Some("track"));
    let _ = std::fs::remove_dir_all(dir);
}
