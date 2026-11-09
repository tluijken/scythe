use std::sync::{Arc, Mutex};
use std::time::Duration;

use scythe_core::render_settings::ResampleMode;
use scythe_core::{Item, Take};
use scythe_engine::{Engine, MediaPool};
use scythe_session::Session;
use scythe_session::api::{ErrorKind, Event, execute_json};
use scythe_session::control;
use serde_json::{Value, json};

fn session() -> Session {
    let pool = Arc::new(MediaPool::with_resample(ResampleMode::PLAYBACK));
    Session::new(Engine::start_offline(pool, 48_000))
}

fn ok(session: &mut Session, command: Value) -> Value {
    execute_json(session, command.clone()).unwrap_or_else(|e| panic!("{command} failed: {e}"))
}

fn add_track(session: &mut Session, name: &str) -> u64 {
    ok(session, json!({"cmd": "track.add", "name": name}))["id"].as_u64().unwrap()
}

#[test]
fn tracks_are_added_changed_and_undone() {
    let mut s = session();
    let drums = add_track(&mut s, "Drums");
    let bass = add_track(&mut s, "Bass");
    let first = ok(&mut s, json!({"cmd": "track.add", "name": "Click", "index": 0}));
    assert_eq!(first["index"], 0);

    let renamed = ok(&mut s, json!({"cmd": "track.rename", "track": drums, "name": "Kit"}));
    assert_eq!(renamed["name"], "Kit");
    let v = ok(&mut s, json!({"cmd": "track.volume", "track": "Kit", "db": -6.0}));
    assert!((v["volume_db"].as_f64().unwrap() + 6.0).abs() < 1e-9);
    ok(&mut s, json!({"cmd": "track.pan", "track": bass, "pan": -0.5}));
    ok(&mut s, json!({"cmd": "track.mute", "track": bass}));
    ok(&mut s, json!({"cmd": "track.solo", "track": bass, "solo": true}));
    ok(&mut s, json!({"cmd": "track.arm", "track": bass, "armed": true}));
    ok(&mut s, json!({"cmd": "track.input", "track": bass, "input": 1025}));
    let c = ok(&mut s, json!({"cmd": "track.color", "track": bass, "color": "#ff8000"}));
    assert_eq!(c["color"], "#ff8000");
    ok(&mut s, json!({"cmd": "track.folder", "track": "Click", "depth": 1}));
    let moved = ok(&mut s, json!({"cmd": "track.move", "track": bass, "index": 0}));
    assert_eq!(moved["index"], 0);

    let bass_info = ok(&mut s, json!({"cmd": "track.get", "track": bass}));
    assert_eq!(bass_info["muted"], true);
    assert_eq!(bass_info["solo"], true);
    assert_eq!(bass_info["armed"], true);
    assert_eq!(bass_info["input"], 1025);
    assert_eq!(bass_info["pan"], -0.5);

    let master = ok(&mut s, json!({"cmd": "track.volume", "track": "master", "volume": 0.5}));
    assert_eq!(master["volume"], 0.5);
    assert_eq!(s.project.master.volume, 0.5);

    let undone = ok(&mut s, json!({"cmd": "project.undo"}));
    assert_eq!(undone["label"], "Track volume");
    assert_eq!(s.project.master.volume, 1.0);
    ok(&mut s, json!({"cmd": "project.redo"}));
    assert_eq!(s.project.master.volume, 0.5);

    ok(&mut s, json!({"cmd": "track.delete", "track": drums}));
    assert_eq!(ok(&mut s, json!({"cmd": "track.list"})).as_array().unwrap().len(), 2);
}

#[test]
fn a_remote_fader_is_one_undo_step() {
    let mut s = session();
    let t = add_track(&mut s, "Vox");
    for db in [-10.0, -8.0, -6.0, -4.0] {
        ok(&mut s, json!({"cmd": "track.volume", "track": t, "db": db}));
    }
    ok(&mut s, json!({"cmd": "project.undo"}));
    assert_eq!(s.project.tracks[0].volume, 1.0);
    assert_eq!(s.history.undo_label(), Some("Add track"));
}

#[test]
fn errors_say_what_is_wrong() {
    let mut s = session();
    let err = execute_json(&mut s, json!({"cmd": "track.volume", "track": 99, "db": 0})).unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);
    let err = execute_json(&mut s, json!({"cmd": "nonsense.do"})).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Invalid);
    let err = execute_json(&mut s, json!({"cmd": "track.add", "name": 5})).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Invalid);
    let err = execute_json(&mut s, json!({"no": "cmd"})).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Invalid);
    let t = add_track(&mut s, "A");
    let err = execute_json(&mut s, json!({"cmd": "track.pan", "track": t, "pan": 3})).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Invalid);
    let err = execute_json(&mut s, json!({"cmd": "transport.record"})).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Failed);
    let err = execute_json(&mut s, json!({"cmd": "project.redo"})).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Failed);
}

#[test]
fn sends_route_between_tracks() {
    let mut s = session();
    let vox = add_track(&mut s, "Vox");
    let mon = add_track(&mut s, "IEM Singer");
    let send = ok(&mut s, json!({"cmd": "send.add", "track": vox, "dest": mon}));
    assert_eq!(send["dest"], mon);
    assert_eq!(send["index"], 0);
    let v = ok(&mut s, json!({"cmd": "send.volume", "track": vox, "send": 0, "db": -3.0}));
    assert!((v["volume_db"].as_f64().unwrap() + 3.0).abs() < 1e-9);
    ok(&mut s, json!({"cmd": "send.pan", "track": vox, "send": 0, "pan": 0.25}));
    let m = ok(&mut s, json!({"cmd": "send.mute", "track": vox, "send": 0}));
    assert_eq!(m["muted"], true);
    assert_eq!(
        execute_json(&mut s, json!({"cmd": "send.add", "track": vox, "dest": vox})).unwrap_err().kind,
        ErrorKind::Invalid
    );
    let missing =
        execute_json(&mut s, json!({"cmd": "send.volume", "track": vox, "send": 4, "volume": 1})).unwrap_err();
    assert_eq!(missing.kind, ErrorKind::NotFound);
    let left = ok(&mut s, json!({"cmd": "send.remove", "track": vox, "send": 0}));
    assert_eq!(left.as_array().unwrap().len(), 0);
}

#[test]
fn effects_are_added_bypassed_and_removed() {
    let mut s = session();
    let t = add_track(&mut s, "Guitar");
    let chain = ok(&mut s, json!({"cmd": "fx.add", "track": t, "plugin": "builtin:compressor"}));
    assert_eq!(chain.as_array().unwrap().len(), 1);
    let chain = ok(&mut s, json!({"cmd": "fx.add", "track": t, "plugin": "Equalizer", "index": 0}));
    assert_eq!(chain[0]["name"], "VST: ReaEQ (Cockos)");
    assert_eq!(chain[0]["running"], true);
    let chain = ok(&mut s, json!({"cmd": "fx.bypass", "track": t, "index": 1, "bypassed": true}));
    assert_eq!(chain[1]["enabled"], false);
    let chain = ok(&mut s, json!({"cmd": "fx.remove", "track": t, "index": 0}));
    assert_eq!(chain.as_array().unwrap().len(), 1);
    ok(&mut s, json!({"cmd": "fx.add", "track": "master", "plugin": "builtin:delay"}));
    assert_eq!(s.project.master.fx.len(), 1);
    let unknown = execute_json(&mut s, json!({"cmd": "fx.add", "track": t, "plugin": "builtin:nothing-like-it"}));
    assert_eq!(unknown.unwrap_err().kind, ErrorKind::NotFound);
}

#[test]
fn items_move_split_and_go() {
    let mut s = session();
    let a = add_track(&mut s, "A");
    let b = add_track(&mut s, "B");
    let item = Item::new(1.0, 4.0, Take::from_file("/nonexistent/take.wav".into()));
    let id = item.id.0;
    s.edit("Add item", |p| p.tracks[0].items.push(item));

    let listed = ok(&mut s, json!({"cmd": "item.list", "track": a}));
    assert_eq!(listed[0]["id"], id);
    let moved = ok(&mut s, json!({"cmd": "item.move", "item": id, "position": 2.0, "track": b}));
    assert_eq!(moved["track"], b);
    assert_eq!(moved["position"], 2.0);
    let halves = ok(&mut s, json!({"cmd": "item.split", "item": id, "at": 3.0}));
    assert_eq!(halves[0]["length"], 1.0);
    assert_eq!(halves[1]["position"], 3.0);
    let outside = execute_json(&mut s, json!({"cmd": "item.split", "item": id, "at": 30.0})).unwrap_err();
    assert_eq!(outside.kind, ErrorKind::Invalid);
    let v = ok(&mut s, json!({"cmd": "item.volume", "item": id, "volume": 0.5}));
    assert_eq!(v["volume"], 0.5);
    ok(&mut s, json!({"cmd": "item.delete", "item": id}));
    assert_eq!(ok(&mut s, json!({"cmd": "item.list"})).as_array().unwrap().len(), 1);
}

#[test]
fn markers_and_regions() {
    let mut s = session();
    let intro = ok(&mut s, json!({"cmd": "marker.add", "position": 0.0, "name": "Intro"}));
    let song = ok(&mut s, json!({"cmd": "marker.add", "position": 10.0, "end": 200.0, "name": "Song 1"}));
    assert_eq!(song["region"], true);
    let edited =
        ok(&mut s, json!({"cmd": "marker.edit", "marker": song["number"], "name": "Opener", "color": "#00ff00"}));
    assert_eq!(edited["name"], "Opener");
    assert_eq!(edited["end"], 200.0);
    let at = ok(&mut s, json!({"cmd": "marker.goto", "marker": song["number"], "region": true}));
    assert_eq!(at["cursor"], 10.0);
    assert_eq!(s.project.cursor, 10.0);
    let bad = execute_json(&mut s, json!({"cmd": "marker.edit", "marker": intro["number"], "end": 5.0}));
    assert_eq!(bad.unwrap_err().kind, ErrorKind::Invalid);
    ok(&mut s, json!({"cmd": "marker.delete", "marker": intro["number"]}));
    let list = ok(&mut s, json!({"cmd": "marker.list"}));
    assert_eq!(list.as_array().unwrap().len(), 1);
}

#[test]
fn transport_seeks_loops_and_plays() {
    let mut s = session();
    let state = ok(&mut s, json!({"cmd": "transport.seek", "position": 12.5}));
    assert_eq!(state["cursor"], 12.5);
    let state = ok(&mut s, json!({"cmd": "transport.loop", "enabled": true, "start": 4.0, "end": 8.0}));
    assert_eq!(state["loop_enabled"], true);
    assert_eq!(state["loop_range"], json!([4.0, 8.0]));
    let state = ok(&mut s, json!({"cmd": "transport.play"}));
    assert_eq!(state["playing"], true);
    let state = ok(&mut s, json!({"cmd": "transport.stop"}));
    assert_eq!(state["playing"], false);
    let state = ok(&mut s, json!({"cmd": "transport.metronome", "enabled": true}));
    assert_eq!(state["metronome"], true);
}

#[test]
fn projects_save_and_open() {
    let dir = std::env::temp_dir().join(format!("scythe-api-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("set.RPP");
    let mut s = session();
    add_track(&mut s, "Keys");
    assert_eq!(execute_json(&mut s, json!({"cmd": "project.save"})).unwrap_err().kind, ErrorKind::Invalid);
    let saved = ok(&mut s, json!({"cmd": "project.save", "path": path}));
    assert_eq!(saved["dirty"], false);

    let fresh = ok(&mut s, json!({"cmd": "project.new"}));
    assert_eq!(fresh["tracks"].as_array().unwrap().len(), 0);
    let opened = ok(&mut s, json!({"cmd": "project.open", "path": path}));
    assert_eq!(opened["tracks"][0]["name"], "Keys");
    let snap = ok(&mut s, json!({"cmd": "snapshot"}));
    assert_eq!(snap["name"], "set");
    assert!(snap["revision"].as_u64().unwrap() > 0);
    let missing = execute_json(&mut s, json!({"cmd": "project.open", "path": dir.join("gone.RPP")}));
    assert_eq!(missing.unwrap_err().kind, ErrorKind::Failed);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_pump_answers_other_threads_and_publishes_events() {
    let mut s = session();
    let events: Arc<Mutex<Vec<Event>>> = Arc::default();
    let seen = events.clone();
    let (handle, mut pump) =
        control::channel(Arc::new(|| {}), Arc::new(move |e: &Event| seen.lock().unwrap().push(e.clone())));
    let (tx, rx) = std::sync::mpsc::channel();
    let client = std::thread::spawn(move || {
        let command = serde_json::from_value(json!({"cmd": "track.add", "name": "Remote"})).unwrap();
        assert!(handle.send(command, move |answer| tx.send(answer).unwrap()));
    });
    client.join().unwrap();
    pump.wait(&mut s, Duration::from_millis(100));
    let answer = rx.recv_timeout(Duration::from_secs(1)).unwrap().unwrap();
    assert_eq!(answer["name"], "Remote");
    let events = events.lock().unwrap();
    assert!(events.iter().any(|e| matches!(e, Event::ProjectChanged { revision } if *revision == s.revision())));
    assert!(events.iter().any(|e| matches!(e, Event::Transport(t) if !t.playing)));
    assert!(events.iter().any(|e| matches!(e, Event::Meters(m) if m.tracks.len() == 1)));
}

#[test]
fn monitor_mixes_through_the_api() {
    let mut s = session();
    let ids: Vec<u64> = ["Kick", "Bass", "Vocal", "Click"].iter().map(|n| add_track(&mut s, n)).collect();
    let mix = ok(&mut s, json!({"cmd": "mix.create", "musician": "Drummer", "own": [ids[0]], "output": 2}));
    let id = mix["id"].as_u64().unwrap();
    assert_eq!(mix["name"], "MON Drummer");
    assert_eq!(mix["output"]["label"], "Out 3/4");
    assert_eq!(mix["limiter"], true);
    assert_eq!(mix["sources"].as_array().unwrap().len(), 4);
    assert!(mix["sources"].as_array().unwrap().iter().all(|x| x["mode"] == "post_fx"));
    assert_eq!(mix["sources"][0]["own"], true);

    let m = ok(&mut s, json!({"cmd": "mix.level", "mix": id, "source": "Click", "db": -6.0}));
    assert!((m["sources"][3]["volume_db"].as_f64().unwrap() + 6.0).abs() < 1e-9);
    ok(&mut s, json!({"cmd": "mix.pan", "mix": "MON Drummer", "source": ids[1], "pan": -0.5}));
    ok(&mut s, json!({"cmd": "mix.mute", "mix": id, "source": "Vocal"}));
    let m = ok(&mut s, json!({"cmd": "mix.more_me", "mix": id, "db": 6.0}));
    assert_eq!(m["more_me"], 6.0);
    assert!((m["sources"][0]["volume_db"].as_f64().unwrap() - 3.0).abs() < 1e-9);
    assert!((m["sources"][1]["volume_db"].as_f64().unwrap() + 3.0).abs() < 1e-9);
    assert_eq!(m["sources"][1]["pan"], -0.5);
    assert_eq!(m["sources"][2]["muted"], true);
    let m = ok(&mut s, json!({"cmd": "mix.own", "mix": id, "source": "Bass", "own": true}));
    assert!((m["sources"][1]["volume_db"].as_f64().unwrap() - 3.0).abs() < 1e-9);
    let m = ok(&mut s, json!({"cmd": "mix.output", "mix": id, "output": 5, "mono": true}));
    assert_eq!(m["output"]["label"], "Out 6 (mono)");
    let m = ok(&mut s, json!({"cmd": "mix.limiter", "mix": id}));
    assert_eq!(m["limiter"], false);
    let m = ok(&mut s, json!({"cmd": "mix.volume", "mix": id, "db": -3.0}));
    assert!((m["volume_db"].as_f64().unwrap() + 3.0).abs() < 1e-9);
    let m = ok(&mut s, json!({"cmd": "mix.remove_source", "mix": id, "source": "Click"}));
    assert_eq!(m["sources"].as_array().unwrap().len(), 3);
    ok(&mut s, json!({"cmd": "mix.add_source", "mix": id, "source": "Click"}));
    let again = execute_json(&mut s, json!({"cmd": "mix.add_source", "mix": id, "source": "Click"}));
    assert_eq!(again.unwrap_err().kind, ErrorKind::Invalid);

    let copy = ok(&mut s, json!({"cmd": "mix.copy", "mix": id, "musician": "Singer"}));
    let copy_id = copy["id"].as_u64().unwrap();
    assert_eq!(copy["output"]["label"], "Out 3 (mono)", "the copy takes the next free output");
    ok(&mut s, json!({"cmd": "mix.rename", "mix": copy_id, "musician": "Lead singer"}));
    let mixes = ok(&mut s, json!({"cmd": "mix.list"}));
    assert_eq!(mixes.as_array().unwrap().len(), 2);
    assert_eq!(mixes[1]["musician"], "Lead singer");
    assert_eq!(ok(&mut s, json!({"cmd": "snapshot"}))["mixes"].as_array().unwrap().len(), 2);
    let not_a_mix = execute_json(&mut s, json!({"cmd": "mix.get", "mix": "Kick"}));
    assert_eq!(not_a_mix.unwrap_err().kind, ErrorKind::Invalid);

    // A remote "more me" drag is one undo step.
    for db in [7.0, 8.0, 9.0] {
        ok(&mut s, json!({"cmd": "mix.more_me", "mix": copy_id, "db": db}));
    }
    ok(&mut s, json!({"cmd": "project.undo"}));
    assert_eq!(ok(&mut s, json!({"cmd": "mix.get", "mix": copy_id}))["more_me"], 6.0);

    let left = ok(&mut s, json!({"cmd": "mix.delete", "mix": id}));
    assert_eq!(left.as_array().unwrap().len(), 1);
    assert!(s.project.tracks.iter().all(|t| t.sends.iter().all(|x| x.dest.0 != id)));

    let bus = ok(
        &mut s,
        json!({"cmd": "mix.cue_bus", "sources": ["Bass", "Vocal"], "mode": "pre_fx", "volumes_from_tracks": true, "output": 8}),
    );
    assert_eq!(bus["name"], "Cue Buss");
    assert_eq!(bus["main_send"], false);
    assert_eq!(bus["hw_outs"][0]["label"], "Out 9/10");
    let receives = s.project.monitor_sources(scythe_core::TrackId(bus["id"].as_u64().unwrap()));
    assert_eq!(receives.len(), 2);
    assert!(receives.iter().all(|r| r.mode == scythe_core::SendMode::PreFx));
}

#[test]
fn hardware_outputs_and_send_modes_through_the_api() {
    let mut s = session();
    let a = add_track(&mut s, "Click");
    let b = add_track(&mut s, "Bus");
    let hw = ok(&mut s, json!({"cmd": "send.hw_add", "track": a, "output": 6, "mono": true}));
    assert_eq!(hw["label"], "Out 7 (mono)");
    let set = json!({"cmd": "send.hw_set", "track": "Click", "hw": 0, "mode": "post_fx", "db": -6.0, "pan": 0.25});
    let hw = ok(&mut s, set);
    assert_eq!((hw["mode"].as_str(), hw["pan"].as_f64()), (Some("post_fx"), Some(0.25)));
    assert!((hw["volume_db"].as_f64().unwrap() + 6.0).abs() < 1e-9);
    let hw = ok(&mut s, json!({"cmd": "send.hw_set", "track": a, "hw": 0, "mono": false, "muted": true}));
    assert_eq!((hw["label"].as_str(), hw["muted"].as_bool()), (Some("Out 7/8"), Some(true)));
    let next = ok(&mut s, json!({"cmd": "send.hw_add", "track": b}));
    assert_eq!(next["output"], 2, "the next pair after the master's 1/2");
    let t = ok(&mut s, json!({"cmd": "track.main_send", "track": a, "on": false}));
    assert_eq!(t["main_send"], false);
    assert_eq!(t["hw_outs"].as_array().unwrap().len(), 1);

    let master = ok(&mut s, json!({"cmd": "send.hw_list", "track": "master"}));
    assert_eq!(master[0]["label"], "Out 1/2");
    ok(&mut s, json!({"cmd": "send.hw_add", "track": "master", "output": 10}));
    ok(&mut s, json!({"cmd": "send.hw_remove", "track": "master", "hw": 0}));
    assert_eq!(s.project.master.hw_outs[0].first_output(), 10);
    let missing = execute_json(&mut s, json!({"cmd": "send.hw_remove", "track": a, "hw": 3}));
    assert_eq!(missing.unwrap_err().kind, ErrorKind::NotFound);

    ok(&mut s, json!({"cmd": "send.add", "track": a, "dest": b}));
    let send = ok(&mut s, json!({"cmd": "send.mode", "track": a, "send": 0, "mode": "pre_fx", "phase": true}));
    assert_eq!(send["mode"], "pre_fx");
    assert_eq!((send["phase"].as_bool(), send["mono"].as_bool()), (Some(true), Some(false)));
    assert_eq!(s.project.tracks[0].sends[0].mode, scythe_core::SendMode::PreFx);
}

#[test]
fn meters_carry_the_monitor_mixes() {
    let mut s = session();
    add_track(&mut s, "Kick");
    ok(&mut s, json!({"cmd": "mix.create", "musician": "Drummer"}));
    let events: Arc<Mutex<Vec<Event>>> = Arc::default();
    let seen = events.clone();
    let (_handle, mut pump) =
        control::channel(Arc::new(|| {}), Arc::new(move |e: &Event| seen.lock().unwrap().push(e.clone())));
    pump.wait(&mut s, Duration::from_millis(100));
    let events = events.lock().unwrap();
    assert!(events.iter().any(|e| matches!(e, Event::Meters(m) if m.tracks.len() == 2 && m.mixes.len() == 1)));
}
