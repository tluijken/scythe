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
fn chains_take_wet_parallel_and_containers() {
    let mut s = session();
    let t = add_track(&mut s, "Bass");
    ok(&mut s, json!({"cmd": "fx.add", "track": t, "plugin": "builtin:compressor"}));
    ok(&mut s, json!({"cmd": "fx.add", "track": t, "plugin": "container"}));
    let chain = ok(&mut s, json!({"cmd": "fx.container.move_in", "track": t, "index": 0, "container": 1}));
    assert_eq!(chain.as_array().unwrap().len(), 1);
    assert_eq!(chain[0]["container"]["effects"][0]["name"], "VST: ReaComp (Cockos)");
    ok(&mut s, json!({"cmd": "fx.add", "track": t, "plugin": "Equalizer", "inside": [0]}));
    let chain = ok(&mut s, json!({"cmd": "fx.parallel", "track": t, "index": 1, "mode": "parallel", "inside": [0]}));
    assert_eq!(chain[0]["container"]["effects"][1]["parallel"], "parallel");
    let chain = ok(&mut s, json!({"cmd": "fx.wet", "track": t, "index": 0, "wet": 0.25, "inside": [0]}));
    assert_eq!(chain[0]["container"]["effects"][0]["wet"], 0.25);
    let chain = ok(&mut s, json!({"cmd": "fx.container.config", "track": t, "index": 0, "channels": 4}));
    assert_eq!(chain[0]["container"]["channels"], 4);
    ok(&mut s, json!({"cmd": "fx.container.pins", "track": t, "index": 0, "pins": [[2], [1]]}));
    assert_eq!(s.project.tracks[0].fx[0].listed_pins(false), Some(vec![2, 1]));
    let chain = ok(&mut s, json!({"cmd": "fx.offline", "track": t, "index": 1, "inside": [0]}));
    assert_eq!(chain[0]["container"]["effects"][1]["offline"], true);
    let first = execute_json(&mut s, json!({"cmd": "fx.parallel", "track": t, "index": 0, "mode": "parallel"}));
    assert_eq!(first.unwrap_err().kind, ErrorKind::Invalid);
    let chain = ok(&mut s, json!({"cmd": "fx.container.move_out", "track": t, "index": 0, "child": 0}));
    assert_eq!(chain.as_array().unwrap().len(), 2);

    let input = ok(&mut s, json!({"cmd": "fx.add", "track": t, "plugin": "builtin:compressor", "chain": "input"}));
    assert_eq!(input.as_array().unwrap().len(), 1);
    assert_eq!(s.project.tracks[0].input_fx().len(), 1);
    let item = Item::new(0.0, 1.0, Take::from_file("/nonexistent/take.wav".into()));
    let id = item.id.0;
    s.edit("Add item", |p| p.tracks[0].items.push(item));
    ok(&mut s, json!({"cmd": "fx.add", "track": t, "plugin": "builtin:compressor", "chain": "take", "item": id}));
    assert_eq!(s.project.tracks[0].items[0].takes[0].fx().len(), 1);
    ok(&mut s, json!({"cmd": "fx.take_channels", "item": id, "channels": 4}));
    assert_eq!(s.project.tracks[0].items[0].takes[0].fx_channels(), Some(4));
    let monitor =
        ok(&mut s, json!({"cmd": "fx.add", "track": "master", "plugin": "builtin:compressor", "chain": "monitor"}));
    assert_eq!(monitor.as_array().unwrap().len(), 1);
    assert_eq!(s.monitor_fx().len(), 1);
    assert!(s.project.master.fx.is_empty());
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

/// Three short songs on one timeline, each ending at a `!1016` stop marker, with a cue inside
/// the second.
fn live_set(s: &mut Session) {
    for (position, name) in [
        (0.0, "Opener"),
        (0.15, "!1016"),
        (0.3, "Ballad"),
        (0.35, "solo"),
        (0.45, "!1016"),
        (0.6, "Closer"),
        (0.75, "!1016"),
    ] {
        ok(s, json!({"cmd": "marker.add", "position": position, "name": name}));
    }
}

/// Lets the null engine play for `ms`, then lets the session follow up on marker actions.
fn play_for(s: &mut Session, ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
    s.poll();
}

#[test]
fn setlists_are_built_from_marker_songs() {
    let mut s = session();
    live_set(&mut s);
    let songs = ok(&mut s, json!({"cmd": "setlist.songs"}));
    let names: Vec<&str> = songs.as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Opener", "Ballad", "Closer"]);
    assert_eq!(songs[1]["ends_at"], "stop_marker");
    assert_eq!(songs[1]["sections"][0]["name"], "solo");
    assert!((songs[1]["duration"].as_f64().unwrap() - 0.15).abs() < 1e-9);

    let gig = ok(&mut s, json!({"cmd": "setlist.create", "name": "Gig", "songs": ["closer", 0]}));
    assert_eq!(gig["entries"][0]["name"], "Closer");
    assert_eq!(gig["entries"][1]["song"], 0);
    let copy = ok(&mut s, json!({"cmd": "setlist.duplicate", "setlist": "Gig"}));
    assert_eq!(copy["name"], "Gig copy");
    let renamed = ok(&mut s, json!({"cmd": "setlist.rename", "setlist": 1, "name": "Gig"}));
    assert_eq!(renamed["name"], "Gig 2");
    ok(&mut s, json!({"cmd": "setlist.set", "setlist": 1, "songs": [0, 1, 2]}));
    let moved = ok(&mut s, json!({"cmd": "setlist.move", "setlist": 1, "from": 2, "to": 0}));
    let order: Vec<&str> = moved["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
    assert_eq!(order, ["Closer", "Opener", "Ballad"]);
    let text = ok(&mut s, json!({"cmd": "setlist.export", "setlist": 1}));
    assert_eq!(text["text"], "Gig 2\n1. Closer\n2. Opener\n3. Ballad\n");

    let imported = ok(&mut s, json!({"cmd": "setlist.import", "text": "Encore\n- Ballad\n- Missing song\n"}));
    assert_eq!(imported["name"], "Encore");
    assert_eq!(imported["entries"][1]["song"], Value::Null);
    let list = ok(&mut s, json!({"cmd": "setlist.list"}));
    assert_eq!(list["setlists"].as_array().unwrap().len(), 3);
    assert_eq!(list["current"], 2);
    ok(&mut s, json!({"cmd": "setlist.delete", "setlist": "Encore"}));
    assert_eq!(ok(&mut s, json!({"cmd": "setlist.list"}))["setlists"].as_array().unwrap().len(), 2);
    ok(&mut s, json!({"cmd": "project.undo"}));
    assert_eq!(ok(&mut s, json!({"cmd": "setlist.list"}))["setlists"].as_array().unwrap().len(), 3);

    let bad = execute_json(&mut s, json!({"cmd": "setlist.create", "songs": ["Nope"]}));
    assert_eq!(bad.unwrap_err().kind, ErrorKind::NotFound);
    let bad = execute_json(&mut s, json!({"cmd": "setlist.move", "setlist": 0, "from": 0, "to": 9}));
    assert_eq!(bad.unwrap_err().kind, ErrorKind::Invalid);
}

#[test]
fn a_setlist_plays_to_the_stop_marker_and_cues_the_next_song() {
    let mut s = session();
    live_set(&mut s);
    ok(&mut s, json!({"cmd": "setlist.create", "name": "Gig", "songs": ["Ballad", "Closer"]}));
    let st = ok(&mut s, json!({"cmd": "setlist.auto_advance", "enabled": true}));
    assert_eq!(st["current"]["name"], "Ballad");
    assert_eq!(st["next"]["name"], "Closer");

    let st = ok(&mut s, json!({"cmd": "setlist.play"}));
    assert_eq!(st["playing"], true);
    play_for(&mut s, 50);
    let st = ok(&mut s, json!({"cmd": "setlist.status"}));
    assert!(st["elapsed"].as_f64().unwrap() > 0.0 && st["remaining"].as_f64().unwrap() < 0.15);
    play_for(&mut s, 400);
    let st = ok(&mut s, json!({"cmd": "setlist.status"}));
    assert_eq!(st["playing"], false, "stopped at the song's stop marker");
    assert_eq!(st["current"]["name"], "Closer", "auto-advance cued the next song");
    assert_eq!(s.project.cursor, 0.6);

    let st = ok(&mut s, json!({"cmd": "setlist.previous"}));
    assert_eq!(st["current"]["name"], "Ballad");
    assert_eq!(s.project.cursor, 0.3);
    ok(&mut s, json!({"cmd": "setlist.cue", "entry": 1}));
    assert_eq!(s.project.cursor, 0.6);

    ok(&mut s, json!({"cmd": "setlist.auto_advance", "enabled": false}));
    let st = ok(&mut s, json!({"cmd": "setlist.marker_actions", "enabled": false}));
    assert_eq!(st["marker_actions"], false);
    ok(&mut s, json!({"cmd": "setlist.play", "entry": 0}));
    play_for(&mut s, 300);
    assert!(s.is_playing(), "without marker actions playback runs past the stop marker");
    let st = ok(&mut s, json!({"cmd": "setlist.stop"}));
    assert_eq!(st["playing"], false);
    assert_eq!(st["current"]["name"], "Ballad");
    ok(&mut s, json!({"cmd": "setlist.marker_actions", "enabled": true}));
    ok(&mut s, json!({"cmd": "setlist.play"}));
    play_for(&mut s, 400);
    assert!(!s.is_playing());
    assert_eq!(s.project.cursor, 0.3, "Stop returns to the edit cursor, the song's start");

    let missing = execute_json(&mut s, json!({"cmd": "setlist.play", "entry": 5}));
    assert_eq!(missing.unwrap_err().kind, ErrorKind::NotFound);
}

#[test]
fn setlist_changes_are_published() {
    let mut s = session();
    live_set(&mut s);
    let events: Arc<Mutex<Vec<Event>>> = Arc::default();
    let seen = events.clone();
    let (_handle, mut pump) =
        control::channel(Arc::new(|| {}), Arc::new(move |e: &Event| seen.lock().unwrap().push(e.clone())));
    pump.pump(&mut s);
    ok(&mut s, json!({"cmd": "setlist.create", "name": "Gig", "songs": [0, 1]}));
    pump.pump(&mut s);
    pump.pump(&mut s);
    ok(&mut s, json!({"cmd": "setlist.next"}));
    pump.pump(&mut s);
    let events = events.lock().unwrap();
    let statuses: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::Setlist(st) => Some(st.current.as_ref().map(|c| c.name.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(statuses, [None, Some("Opener".into()), Some("Ballad".into())]);
}

#[test]
fn tempo_changes_and_bars_follow() {
    let mut s = session();
    let snap = ok(&mut s, json!({"cmd": "snapshot"}));
    assert_eq!(snap["bpm"], 120.0);
    assert_eq!(snap["transport"]["bar_beat"], "1.1.00");
    let bars = snap["bars"].as_array().unwrap();
    assert_eq!(bars[0], 0.0);
    assert!((bars[1].as_f64().unwrap() - 2.0).abs() < 1e-9);

    let answer = ok(&mut s, json!({"cmd": "project.tempo", "bpm": 60.0}));
    assert_eq!(answer["bpm"], 60.0);
    let snap = ok(&mut s, json!({"cmd": "snapshot"}));
    assert!((snap["bars"][1].as_f64().unwrap() - 4.0).abs() < 1e-9);
    ok(&mut s, json!({"cmd": "transport.seek", "position": 5.0}));
    assert_eq!(ok(&mut s, json!({"cmd": "transport.state"}))["bar_beat"], "2.2.00");

    let err = execute_json(&mut s, json!({"cmd": "project.tempo", "bpm": 1000.0})).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Invalid);
    assert_eq!(ok(&mut s, json!({"cmd": "project.undo"}))["label"], "Change tempo");
    assert_eq!(s.project.tempo.bpm_at(0.0), 120.0);
}

#[test]
fn tempo_markers_are_listed_added_edited_and_deleted() {
    let mut s = session();
    let map = ok(&mut s, json!({"cmd": "tempo.list"}));
    assert_eq!((map["bpm"].as_f64(), map["markers"].as_array().map(Vec::len)), (Some(120.0), Some(0)));

    let map = ok(&mut s, json!({"cmd": "tempo.add", "time": 4.0, "bpm": 90.0}));
    let markers = map["markers"].as_array().unwrap();
    assert_eq!(markers.len(), 2, "a marker at the start keeps the base tempo: {map}");
    assert_eq!((markers[0]["time"].as_f64(), markers[0]["bpm"].as_f64()), (Some(0.0), Some(120.0)));
    assert_eq!((markers[1]["bpm"].as_f64(), markers[1]["bar_beat"].as_str()), (Some(90.0), Some("3.1.00")));

    // A time signature starts a bar: 5 s at 90 bpm lands mid-bar and moves to the next line.
    let map = ok(&mut s, json!({"cmd": "tempo.add", "time": 5.0, "time_signature": [7, 8]}));
    let seven = &map["markers"][2];
    assert_eq!(seven["time_signature"], json!([7, 8]));
    assert_eq!(seven["bar_beat"].as_str().map(|b| b.ends_with(".1.00")), Some(true));
    assert!(seven["time"].as_f64().unwrap() > 5.0);
    assert_eq!(seven["bpm"].as_f64(), Some(90.0));

    let undo_before = s.history.undo_label().map(str::to_owned);
    for bpm in [100.0, 110.0, 128.0] {
        ok(&mut s, json!({"cmd": "tempo.edit", "index": 1, "bpm": bpm, "time": 4.0}));
    }
    assert_eq!(s.project.tempo.points[1].bpm, 128.0);
    assert_eq!(ok(&mut s, json!({"cmd": "project.undo"}))["label"], "Edit tempo marker");
    assert_eq!(s.project.tempo.points[1].bpm, 90.0, "a dragged marker is one undo step");
    assert_eq!(s.history.undo_label().map(str::to_owned), undo_before);

    let map = ok(&mut s, json!({"cmd": "tempo.edit", "index": 2, "time_signature": null}));
    assert_eq!(map["markers"][2]["time_signature"], Value::Null);
    let map = ok(&mut s, json!({"cmd": "tempo.edit", "index": 0, "bpm": 140.0, "time_signature": [3, 4]}));
    assert_eq!((map["bpm"].as_f64(), map["time_signature"].clone()), (Some(140.0), json!([3, 4])));
    // The later markers keep their beat: 8 quarter notes at 140 bpm.
    let second = map["markers"][1]["time"].as_f64().unwrap();
    assert!((second - 8.0 * 60.0 / 140.0).abs() < 1e-9, "{map}");
    assert_eq!(ok(&mut s, json!({"cmd": "snapshot"}))["tempo"].as_array().map(Vec::len), Some(3));

    let map = ok(&mut s, json!({"cmd": "tempo.delete", "index": 2}));
    assert_eq!(map["markers"].as_array().map(Vec::len), Some(2));
    for bad in [
        json!({"cmd": "tempo.delete", "index": 9}),
        json!({"cmd": "tempo.edit", "index": 1, "bpm": 5.0}),
        json!({"cmd": "tempo.add", "time": 1.0, "time_signature": [4, 3]}),
        json!({"cmd": "tempo.add", "time": second}),
    ] {
        assert!(execute_json(&mut s, bad.clone()).is_err(), "{bad}");
    }
}

/// A 16-bit mono WAV of `samples`.
fn write_wav(path: &std::path::Path, rate: u32, samples: &[f32]) {
    let data: Vec<u8> = samples.iter().flat_map(|s| ((s * 32767.0) as i16).to_le_bytes()).collect();
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    std::fs::write(path, out).unwrap();
}

#[test]
fn item_peaks_follow_the_audio() {
    let dir = std::env::temp_dir().join(format!("scythe-peaks-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("half.wav");
    // One second of silence, then one second at half scale.
    let samples: Vec<f32> = (0..96_000).map(|i| if i < 48_000 { 0.0 } else { 0.5 }).collect();
    write_wav(&wav, 48_000, &samples);
    let mut s = session();
    s.import_files(vec![wav], None, 0.0);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while s.project.tracks.first().is_none_or(|t| t.items.is_empty()) && std::time::Instant::now() < deadline {
        s.poll();
        std::thread::sleep(Duration::from_millis(5));
    }
    let item = s.project.tracks[0].items[0].id.0;
    let peaks = ok(&mut s, json!({"cmd": "item.peaks", "item": item, "width": 8}));
    assert_eq!(peaks["state"], "ready");
    let max: Vec<f64> = peaks["max"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    assert_eq!(max.len(), 8);
    assert!(max[..3].iter().all(|m| *m < 0.01), "{max:?}");
    assert!(max[4..].iter().all(|m| (m - 0.5).abs() < 0.01), "{max:?}");
    let part = ok(&mut s, json!({"cmd": "item.peaks", "item": item, "from": 1.0, "to": 1.5, "width": 2}));
    assert_eq!((part["from"].as_f64(), part["to"].as_f64()), (Some(1.0), Some(1.5)));
    assert!(part["max"].as_array().unwrap().iter().all(|v| (v.as_f64().unwrap() - 0.5).abs() < 0.01));

    ok(&mut s, json!({"cmd": "item.volume", "item": item, "volume": 0.5}));
    let quieter = ok(&mut s, json!({"cmd": "item.peaks", "item": item, "width": 4}));
    assert!((quieter["max"][3].as_f64().unwrap() - 0.25).abs() < 0.01);
    for bad in [
        json!({"cmd": "item.peaks", "item": item, "width": 0}),
        json!({"cmd": "item.peaks", "item": 999_999, "width": 8}),
    ] {
        assert!(execute_json(&mut s, bad.clone()).is_err(), "{bad}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

mod scopes {
    use scythe_session::access::{self, Scope};

    use super::*;

    fn band() -> Session {
        let mut s = session();
        add_track(&mut s, "Kick");
        add_track(&mut s, "Vox");
        ok(&mut s, json!({"cmd": "mix.create", "musician": "Drummer", "own": ["Kick"]}));
        ok(&mut s, json!({"cmd": "mix.create", "musician": "Singer", "own": ["Vox"]}));
        live_set(&mut s);
        ok(&mut s, json!({"cmd": "setlist.create", "name": "Gig", "songs": [0, 1, 2]}));
        s
    }

    fn run(s: &mut Session, scope: &Scope, command: Value) -> Result<Value, scythe_session::api::ApiError> {
        access::run(s, scope, serde_json::from_value(command).unwrap())
    }

    fn forbidden(s: &mut Session, scope: &Scope, command: Value) {
        let err = run(s, scope, command.clone()).expect_err(&command.to_string());
        assert_eq!(err.kind, ErrorKind::Forbidden, "{command}: {err}");
    }

    #[test]
    fn a_musician_reaches_only_their_own_mix() {
        let mut s = band();
        let drummer = Scope::Musician { musician: "drummer".into() };
        let mixes = run(&mut s, &drummer, json!({"cmd": "mix.list"})).unwrap();
        assert_eq!(mixes.as_array().map(Vec::len), Some(1));
        assert_eq!(mixes[0]["musician"], "Drummer");
        let own = mixes[0]["id"].as_u64().unwrap();
        let singer = s.project.monitor_mixes().into_iter().find(|m| m.musician == "Singer").unwrap().track.0;

        for allowed in [
            json!({"cmd": "mix.get", "mix": own}),
            json!({"cmd": "mix.level", "mix": own, "source": "Vox", "db": -3.0}),
            json!({"cmd": "mix.pan", "mix": "MON Drummer", "source": "Vox", "pan": 0.5}),
            json!({"cmd": "mix.mute", "mix": own, "source": "Vox", "muted": true}),
            json!({"cmd": "mix.more_me", "mix": own, "db": 6.0}),
            json!({"cmd": "mix.volume", "mix": own, "db": -2.0}),
            json!({"cmd": "mix.click", "mix": own, "db": -6.0}),
            json!({"cmd": "transport.state"}),
            json!({"cmd": "setlist.status"}),
        ] {
            run(&mut s, &drummer, allowed.clone()).unwrap_or_else(|e| panic!("{allowed}: {e}"));
        }
        let snap = run(&mut s, &drummer, json!({"cmd": "snapshot"})).unwrap();
        assert!(snap.get("tracks").is_none() && snap.get("path").is_none(), "{snap}");
        assert_eq!(snap["access"], json!({"role": "musician", "musician": "drummer"}));

        for refused in [
            json!({"cmd": "mix.level", "mix": singer, "source": "Vox", "db": 6.0}),
            json!({"cmd": "mix.get", "mix": "MON Singer"}),
            json!({"cmd": "mix.output", "mix": own, "output": 4}),
            json!({"cmd": "mix.limiter", "mix": own, "on": false}),
            json!({"cmd": "mix.add_source", "mix": own, "source": "Kick"}),
            json!({"cmd": "mix.delete", "mix": own}),
            json!({"cmd": "mix.create", "musician": "Me"}),
            json!({"cmd": "track.volume", "track": "Vox", "db": -6.0}),
            json!({"cmd": "transport.play"}),
            json!({"cmd": "setlist.next"}),
            json!({"cmd": "project.save", "path": "/tmp/x.RPP"}),
            json!({"cmd": "project.open", "path": "/etc/passwd"}),
            json!({"cmd": "item.list"}),
            json!({"cmd": "mix.click", "mix": singer, "db": 0.0}),
            json!({"cmd": "transport.health"}),
            json!({"cmd": "transport.count_in", "playback": true}),
            json!({"cmd": "tempo.list"}),
        ] {
            forbidden(&mut s, &drummer, refused);
        }
        let singer_mix = s.project.monitor_mix(scythe_core::TrackId(singer)).unwrap();
        assert_eq!(singer_mix.more_me, 0.0);

        let nobody = Scope::Musician { musician: "Keys".into() };
        assert_eq!(run(&mut s, &nobody, json!({"cmd": "mix.list"})).unwrap(), json!([]));
        let err = run(&mut s, &nobody, json!({"cmd": "mix.get", "mix": own})).unwrap_err();
        assert_eq!(err.kind, ErrorKind::NotFound);
    }

    #[test]
    fn the_stage_plays_the_setlist_without_editing_it() {
        let mut s = band();
        for allowed in [
            json!({"cmd": "setlist.list"}),
            json!({"cmd": "setlist.songs"}),
            json!({"cmd": "setlist.get"}),
            json!({"cmd": "setlist.status"}),
            json!({"cmd": "setlist.export"}),
            json!({"cmd": "setlist.select", "setlist": 0}),
            json!({"cmd": "setlist.cue", "entry": 1}),
            json!({"cmd": "setlist.next"}),
            json!({"cmd": "setlist.previous"}),
            json!({"cmd": "setlist.auto_advance", "enabled": true}),
            json!({"cmd": "setlist.count_in", "enabled": true}),
            json!({"cmd": "transport.health"}),
            json!({"cmd": "setlist.play"}),
            json!({"cmd": "setlist.stop"}),
            json!({"cmd": "transport.play"}),
            json!({"cmd": "transport.stop"}),
            json!({"cmd": "transport.state"}),
        ] {
            run(&mut s, &Scope::Stage, allowed.clone()).unwrap_or_else(|e| panic!("{allowed}: {e}"));
        }
        for refused in [
            json!({"cmd": "setlist.create", "name": "New"}),
            json!({"cmd": "setlist.rename", "name": "Renamed"}),
            json!({"cmd": "setlist.duplicate"}),
            json!({"cmd": "setlist.delete"}),
            json!({"cmd": "setlist.set", "songs": [0]}),
            json!({"cmd": "setlist.move", "from": 0, "to": 1}),
            json!({"cmd": "setlist.import", "text": "Set\nOpener"}),
            json!({"cmd": "setlist.marker_actions", "enabled": false}),
            json!({"cmd": "transport.record"}),
            json!({"cmd": "transport.seek", "position": 3.0}),
            json!({"cmd": "mix.list"}),
            json!({"cmd": "track.add"}),
            json!({"cmd": "marker.add"}),
            json!({"cmd": "project.undo"}),
            json!({"cmd": "project.new"}),
            json!({"cmd": "item.peaks", "item": 1, "width": 10}),
            json!({"cmd": "transport.count_in", "playback": true}),
            json!({"cmd": "transport.metronome", "enabled": true}),
            json!({"cmd": "transport.metronome_output", "output": 2}),
            json!({"cmd": "mix.click", "mix": "MON Drummer", "db": 0.0}),
        ] {
            forbidden(&mut s, &Scope::Stage, refused);
        }
        assert!(!s.project.ext_state_section("SCYTHE_SETLISTS").is_empty());
        let snap = run(&mut s, &Scope::Stage, json!({"cmd": "snapshot"})).unwrap();
        assert_eq!(snap["access"]["role"], "stage");
        assert!(snap.get("tracks").is_none());
    }

    #[test]
    fn events_are_narrowed_to_the_scope() {
        let mut s = band();
        let events: Arc<Mutex<Vec<Event>>> = Arc::default();
        let seen = events.clone();
        let (_handle, mut pump) =
            control::channel(Arc::new(|| {}), Arc::new(move |e: &Event| seen.lock().unwrap().push(e.clone())));
        pump.wait(&mut s, Duration::from_millis(100));
        let events = events.lock().unwrap();
        let meters = events.iter().find(|e| matches!(e, Event::Meters(_))).unwrap();
        let Event::Meters(all) = meters else { unreachable!() };
        assert_eq!(all.mixes.len(), 2);

        let drummer = Scope::Musician { musician: "Drummer".into() };
        let Some(Event::Meters(mine)) = drummer.event(meters) else { panic!("a musician hears their meters") };
        assert_eq!(mine.mixes.len(), 1);
        assert_eq!(mine.mixes[0].musician.as_deref(), Some("Drummer"));
        assert!(mine.tracks.is_empty());
        assert!(drummer.event(&Event::ProjectChanged { revision: 3 }).is_some());
        assert!(drummer.event(&Event::Notice { message: "Saved /home/band/set.RPP".into() }).is_none());
        let transport = events.iter().find(|e| matches!(e, Event::Transport(_))).unwrap();
        assert!(drummer.event(transport).is_none());

        assert!(Scope::Stage.event(meters).is_none());
        assert!(Scope::Stage.event(transport).is_some());
        let setlist = events.iter().find(|e| matches!(e, Event::Setlist(_))).unwrap();
        assert!(Scope::Stage.event(setlist).is_some());
        assert!(drummer.event(setlist).is_none());
        let health = events.iter().find(|e| matches!(e, Event::Health(_))).unwrap();
        assert!(Scope::Stage.event(health).is_some(), "the stage sees when the audio is in trouble");
        assert!(drummer.event(health).is_none());
        assert!(Scope::Admin.event(&Event::Notice { message: "x".into() }).is_some());
    }

    #[test]
    fn the_pump_checks_the_scope_of_each_command() {
        let mut s = band();
        let (handle, mut pump) = control::channel(Arc::new(|| {}), Arc::new(|_: &Event| {}));
        let (tx, rx) = std::sync::mpsc::channel();
        let command = |v: Value| serde_json::from_value(v).unwrap();
        let out = tx.clone();
        handle.send_as(Scope::Stage, command(json!({"cmd": "track.add"})), move |a| out.send(a).unwrap());
        handle.send(command(json!({"cmd": "track.add", "name": "Admin"})), move |a| tx.send(a).unwrap());
        pump.pump(&mut s);
        assert_eq!(rx.recv().unwrap().unwrap_err().kind, ErrorKind::Forbidden);
        assert_eq!(rx.recv().unwrap().unwrap()["name"], "Admin");
    }
}

#[test]
fn tempo_markers_survive_saving() {
    let dir = std::env::temp_dir().join(format!("scythe-tempo-save-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tempo.RPP");
    let mut s = session();
    ok(&mut s, json!({"cmd": "tempo.add", "time": 4.0, "bpm": 90.0}));
    ok(&mut s, json!({"cmd": "tempo.add", "time": 9.0, "time_signature": [7, 8]}));
    let before = ok(&mut s, json!({"cmd": "tempo.list"}));
    ok(&mut s, json!({"cmd": "project.save", "path": path}));
    ok(&mut s, json!({"cmd": "project.open", "path": path}));
    assert_eq!(ok(&mut s, json!({"cmd": "tempo.list"})), before);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn count_in_and_metronome_settings() {
    let mut s = session();
    let st = ok(&mut s, json!({"cmd": "transport.count_in", "playback": true, "measures": 1}));
    assert_eq!(st["count_in"]["playback"], true);
    assert_eq!(st["count_in"]["record"], false);
    assert_eq!(st["count_in"]["measures"], 1.0);
    assert_eq!(s.project.metronome.flags, 6 | 8, "REAPER's count-in before playback bit");
    let bad = execute_json(&mut s, json!({"cmd": "transport.count_in", "measures": 0}));
    assert_eq!(bad.unwrap_err().kind, ErrorKind::Invalid);

    let st = ok(&mut s, json!({"cmd": "transport.metronome", "db": -6.0}));
    assert_eq!(st["metronome"], false, "the level alone leaves it off");
    assert!((st["metronome_volume"].as_f64().unwrap() - 0.501).abs() < 1e-3);
    let st = ok(&mut s, json!({"cmd": "transport.metronome_output", "output": 4, "mono": true}));
    assert_eq!(st["metronome_outputs"], json!([{"output": 4, "mono": true}]));
    assert_eq!(s.project.metronome.outputs, [1028]);
    let st = ok(&mut s, json!({"cmd": "transport.metronome_output", "output": null}));
    assert_eq!(st["metronome_outputs"], json!([]));

    // As in REAPER, a disabled metronome does not count in.
    ok(&mut s, json!({"cmd": "transport.seek", "position": 3.0}));
    let st = ok(&mut s, json!({"cmd": "transport.play"}));
    assert_eq!((st["playing"].clone(), st["count_in"]["position"].clone()), (json!(true), Value::Null));
    ok(&mut s, json!({"cmd": "transport.stop"}));

    // At 120 bpm one 4/4 bar is two seconds; the playhead waits at the cursor meanwhile.
    ok(&mut s, json!({"cmd": "transport.metronome", "enabled": true}));
    let st = ok(&mut s, json!({"cmd": "transport.play"}));
    assert_eq!(st["playing"], true);
    let position = st["count_in"]["position"].as_f64().unwrap();
    assert!((-2.0..=-1.5).contains(&position), "{position}");
    assert_eq!(st["count_in"]["beats"], 4);
    assert_eq!(st["position"], 3.0);
    std::thread::sleep(Duration::from_millis(150));
    let st = ok(&mut s, json!({"cmd": "transport.state"}));
    assert!(st["count_in"]["position"].as_f64().unwrap() > position);
    assert_eq!(st["position"], 3.0);
    let st = ok(&mut s, json!({"cmd": "transport.stop"}));
    assert_eq!(st["count_in"]["position"], Value::Null);
    assert!(s.dirty, "the settings are saved with the project");
}

#[test]
fn preroll_and_count_in_preferences() {
    let mut s = session();
    let dir = std::env::temp_dir().join(format!("scythe-prefs-{}", std::process::id()));
    let file = dir.join("preferences.ini");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&file, "[REAPER]\nother=1\n").unwrap();
    s.load_preferences(file.clone());
    let st = ok(&mut s, json!({"cmd": "transport.state"}));
    assert_eq!(st["preroll"], json!({"playback": false, "record": false, "measures": 2.0, "measure_start": true}));
    assert_eq!(st["metronome_default_output"], Value::Null);

    let st = ok(&mut s, json!({"cmd": "transport.preroll", "playback": true, "measures": 1}));
    assert_eq!(st["preroll"], json!({"playback": true, "record": false, "measures": 1.0, "measure_start": true}));
    let st = ok(&mut s, json!({"cmd": "transport.count_in", "measure_start": true}));
    assert_eq!(st["count_in"]["measure_start"], true);
    let st = ok(&mut s, json!({"cmd": "transport.metronome_output", "output": 2, "default": true}));
    assert_eq!(st["metronome_default_output"], json!({"output": 2, "mono": false}));
    assert_eq!(st["metronome_outputs"], json!([]), "the project's own stays empty");
    assert!(!s.dirty, "preferences are not part of the project");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert_eq!(saved, "[REAPER]\nother=1\npreroll=9\nprerollmeas=1.00000000000000\nmetronome_defout=2\n");
    let bad = execute_json(&mut s, json!({"cmd": "transport.preroll", "measures": 17}));
    assert_eq!(bad.unwrap_err().kind, ErrorKind::Invalid);

    // Bar 3 beat 3 at 120 bpm pre-rolls from the bar line a bar back: bar 2, 2.0 s.
    ok(&mut s, json!({"cmd": "transport.seek", "position": 5.0}));
    let st = ok(&mut s, json!({"cmd": "transport.play"}));
    assert_eq!(st["playing"], true);
    assert!((2.0..2.2).contains(&st["position"].as_f64().unwrap()), "{}", st["position"]);
    ok(&mut s, json!({"cmd": "transport.stop"}));
    assert_eq!(ok(&mut s, json!({"cmd": "transport.state"}))["position"], 5.0, "stop returns to the cursor");

    // From the project start the pre-roll waits a bar before the playhead moves.
    ok(&mut s, json!({"cmd": "transport.seek", "position": 0.0}));
    let st = ok(&mut s, json!({"cmd": "transport.play"}));
    let lead = st["count_in"]["position"].as_f64().unwrap();
    assert!((-2.0..=-1.8).contains(&lead), "{lead}");
    assert_eq!(st["position"], 0.0);
    ok(&mut s, json!({"cmd": "transport.stop"}));

    let mut reloaded = session();
    reloaded.load_preferences(file);
    assert_eq!(reloaded.prefs, s.prefs);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn setlist_songs_count_in() {
    let mut s = session();
    live_set(&mut s);
    ok(&mut s, json!({"cmd": "setlist.create", "name": "Gig", "songs": ["Ballad"]}));
    let list = ok(&mut s, json!({"cmd": "setlist.count_in", "enabled": true}));
    assert_eq!(list["count_in"], true);
    assert_eq!(ok(&mut s, json!({"cmd": "setlist.status"}))["count_in"], true);
    let st = ok(&mut s, json!({"cmd": "setlist.play"}));
    assert_eq!(st["playing"], true);
    let state = ok(&mut s, json!({"cmd": "transport.state"}));
    assert!(state["count_in"]["position"].as_f64().unwrap() < 0.0, "the drummer does not count");
    assert_eq!(state["position"], 0.3);
    ok(&mut s, json!({"cmd": "setlist.stop"}));
    let list = ok(&mut s, json!({"cmd": "setlist.count_in"}));
    assert_eq!(list["count_in"], false);
    ok(&mut s, json!({"cmd": "setlist.play"}));
    assert_eq!(ok(&mut s, json!({"cmd": "transport.state"}))["count_in"]["position"], Value::Null);
}

#[test]
fn a_monitor_mix_takes_the_click_at_its_own_level() {
    let mut s = session();
    add_track(&mut s, "Kick");
    let mix = ok(&mut s, json!({"cmd": "mix.create", "musician": "Drummer"}));
    assert_eq!(mix["click"], Value::Null);
    let mix = ok(&mut s, json!({"cmd": "mix.click", "mix": "MON Drummer", "db": -6.0}));
    assert!((mix["click"].as_f64().unwrap() - 0.501).abs() < 1e-3);
    let mix = ok(&mut s, json!({"cmd": "mix.click", "mix": "MON Drummer", "volume": 0}));
    assert_eq!(mix["click"], Value::Null);
    ok(&mut s, json!({"cmd": "project.undo"}));
    assert!(ok(&mut s, json!({"cmd": "mix.get", "mix": "MON Drummer"}))["click"].is_number());
}

#[test]
fn health_reports_the_device() {
    let mut s = session();
    let h = ok(&mut s, json!({"cmd": "transport.health"}));
    assert_eq!(h["status"], "silent");
    assert_eq!(h["sample_rate"], 48_000);
    assert_eq!(h["block_size"], 512);
    assert!((h["buffer_ms"].as_f64().unwrap() - 10.67).abs() < 0.01);
    assert_eq!(h["xruns"], 0);

    let events: Arc<Mutex<Vec<Event>>> = Arc::default();
    let seen = events.clone();
    let (_handle, mut pump) =
        control::channel(Arc::new(|| {}), Arc::new(move |e: &Event| seen.lock().unwrap().push(e.clone())));
    pump.pump(&mut s);
    let events = events.lock().unwrap();
    assert!(events.iter().any(|e| matches!(e, Event::Health(h) if h.status == "silent" && h.cpu >= 0.0)));
}

#[test]
fn groups_carry_edits_to_followers() {
    let mut s = session();
    let lead = add_track(&mut s, "Drums");
    let kick = add_track(&mut s, "Kick");
    let snare = add_track(&mut s, "Snare");
    ok(&mut s, json!({"cmd": "group.name", "group": 1, "name": "Kit"}));
    ok(
        &mut s,
        json!({"cmd": "group.set", "track": lead, "group": 1, "params": ["volume_lead", "mute_lead", "pan_lead"]}),
    );
    ok(&mut s, json!({"cmd": "group.set", "track": kick, "group": 1, "params": ["VOLUME_FOLLOW", "MUTE_FOLLOW"]}));
    let groups = ok(
        &mut s,
        json!({"cmd": "group.set", "track": snare, "group": 1, "params": ["volume_follow", "volume_reverse", "pan_follow"]}),
    );
    assert_eq!(groups[0]["name"], "Kit");
    assert_eq!(groups[0]["members"].as_array().map(Vec::len), Some(3));
    ok(&mut s, json!({"cmd": "track.volume", "track": lead, "volume": 0.5}));
    let get = |s: &mut Session, id: u64| ok(s, json!({"cmd": "track.get", "track": id}));
    assert_eq!(get(&mut s, kick)["volume"], 0.5);
    assert_eq!(get(&mut s, snare)["volume"], 2.0);
    ok(&mut s, json!({"cmd": "track.mute", "track": lead, "muted": true}));
    assert_eq!(get(&mut s, kick)["muted"], true);
    assert_eq!(get(&mut s, snare)["muted"], false);
    ok(&mut s, json!({"cmd": "track.pan", "track": lead, "pan": 0.25}));
    assert_eq!(get(&mut s, snare)["pan"], 0.25);
    assert_eq!(get(&mut s, kick)["pan"], 0.0);
    assert_eq!(get(&mut s, kick)["groups"][0]["params"], json!(["VOLUME_FOLLOW", "MUTE_FOLLOW"]));
    ok(&mut s, json!({"cmd": "group.clear", "group": 1}));
    assert_eq!(ok(&mut s, json!({"cmd": "group.list"})), json!([]));
    let err = execute_json(&mut s, json!({"cmd": "group.set", "track": lead, "group": 65, "params": ["pan_lead"]}));
    assert_eq!(err.unwrap_err().kind, ErrorKind::Invalid);
}

#[test]
fn polarity_pan_modes_and_solo_through_the_api() {
    let mut s = session();
    let t = add_track(&mut s, "Keys");
    let info = ok(&mut s, json!({"cmd": "track.phase", "track": t}));
    assert_eq!(info["phase"], true);
    let info = ok(&mut s, json!({"cmd": "track.pan_mode", "track": t, "mode": "stereo"}));
    assert_eq!(info["pan_mode"], "stereo");
    let info = ok(&mut s, json!({"cmd": "track.width", "track": t, "width": 0.3}));
    assert_eq!(info["width"], 0.3);
    let info = ok(&mut s, json!({"cmd": "track.dual_pan", "track": t, "left": -0.5, "right": 0.25}));
    assert_eq!(info["dual_pan"], json!([-0.5, 0.25]));
    let info = ok(&mut s, json!({"cmd": "track.pan_mode", "track": t, "mode": null}));
    assert_eq!(info["pan_mode"], Value::Null);
    let info = ok(&mut s, json!({"cmd": "track.solo", "track": t, "solo": true, "mode": "ignore_routing"}));
    assert_eq!((info["solo"].clone(), info["solo_mode"].clone()), (json!(true), json!("ignore_routing")));
    let info = ok(&mut s, json!({"cmd": "track.solo_defeat", "track": t}));
    assert_eq!(info["solo_defeat"], true);
    let bad = execute_json(&mut s, json!({"cmd": "track.pan_mode", "track": t, "mode": "surround"}));
    assert_eq!(bad.unwrap_err().kind, ErrorKind::Invalid);
    let bad = execute_json(&mut s, json!({"cmd": "track.width", "track": t, "width": 2.0}));
    assert_eq!(bad.unwrap_err().kind, ErrorKind::Invalid);
}

#[test]
fn control_surfaces_are_kept_as_reaper_ini_lines() {
    let dir = std::env::temp_dir().join(format!("scythe-surface-api-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let prefs = dir.join("preferences.ini");
    std::fs::write(&prefs, "[REAPER]\ncsurf_cnt=1\ncsurf_0=BCF2000 0 1\nother=1\n").unwrap();
    let mut s = session();
    s.load_preferences(prefs.clone());
    let osc =
        json!({"type": "osc", "name": "Tablet", "local_port": 8000, "device_ip": "10.0.0.5", "device_port": 9000});
    let list = ok(&mut s, json!({"cmd": "surface.add", "surface": osc}));
    assert_eq!(list[1]["line"], "OSC \"Tablet\" 3 8000 \"10.0.0.5\" 9000 1024 10 \"\"");
    let mcu = json!({"type": "midi", "kind": "mcu", "input": 2, "output": 3});
    ok(&mut s, json!({"cmd": "surface.add", "surface": mcu}));
    let xt = json!({"type": "line", "line": "MCUEX 8 0 4 5 0"});
    let list = ok(&mut s, json!({"cmd": "surface.set", "index": 0, "surface": xt}));
    assert_eq!(
        list[0]["surface"],
        json!({"type": "midi", "kind": "mcu_extender", "input": 4, "output": 5, "offset": 8, "size": 0, "flags": 0})
    );
    let text = std::fs::read_to_string(&prefs).unwrap();
    assert!(text.contains("other=1"), "{text}");
    for line in [
        "csurf_0=MCUEX 8 0 4 5 0",
        "csurf_1=OSC \"Tablet\" 3 8000 \"10.0.0.5\" 9000 1024 10 \"\"",
        "csurf_2=MCU 0 0 2 3 0",
        "csurf_cnt=3",
    ] {
        assert!(text.lines().any(|l| l == line), "{line} in\n{text}");
    }
    ok(&mut s, json!({"cmd": "surface.remove", "index": 0}));
    assert_eq!(s.prefs.surfaces.len(), 2);
    let err = execute_json(&mut s, json!({"cmd": "surface.remove", "index": 5})).unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);
    let actions = ok(&mut s, json!({"cmd": "surface.action_list"}));
    assert!(actions.as_array().unwrap().iter().any(|a| a["id"] == "_SCYTHE_SETLIST_NEXT"));
    ok(&mut s, json!({"cmd": "surface.run_action", "action": "40364"}));
    assert!(s.metronome());
    let err = execute_json(&mut s, json!({"cmd": "surface.run_action", "action": "99999"})).unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);
}

#[test]
fn midi_routing_and_sync_through_the_api() {
    let mut s = session();
    let keys = add_track(&mut s, "Keys");
    let synth = add_track(&mut s, "Synth");
    let t = ok(&mut s, json!({"cmd": "track.midi_input", "track": keys, "device": "keyboard", "channel": 2}));
    assert_eq!(t["input"], 4096 + (62 << 5) + 2);
    let t = ok(&mut s, json!({"cmd": "track.midi_input", "track": keys, "device": 5}));
    assert_eq!(t["input"], 4096 + (5 << 5));
    let bad = execute_json(&mut s, json!({"cmd": "track.midi_input", "track": keys, "device": 63}));
    assert_eq!(bad.unwrap_err().kind, ErrorKind::Invalid);

    ok(&mut s, json!({"cmd": "send.add", "track": keys, "dest": synth}));
    let list = ok(&mut s, json!({"cmd": "send.list", "track": keys}));
    assert!(list[0]["midi"].is_null(), "a new send carries no MIDI");
    let send = ok(
        &mut s,
        json!({"cmd": "send.midi", "track": keys, "send": 0, "enabled": true, "audio": false, "dst_channel": 10, "dst_bus": 2}),
    );
    assert_eq!(send["audio"], false);
    assert_eq!(send["midi"]["dst_channel"], 10);
    assert_eq!(send["midi"]["dst_bus"], 2);
    assert_eq!(send["midi"]["src_channel"], 0);
    let flags = s.project.tracks[0].sends[0].midi_flags;
    assert_eq!(flags, 10 << 5 | 2 << 22);
    assert!(scythe_core::rpp::project_to_rpp(&s.project).contains(&format!("-1:U {flags} ")));

    let sync = ok(&mut s, json!({"cmd": "sync.set", "enabled": true, "source": "mtc", "device": 3, "offset": 3600.0}));
    assert_eq!(
        (sync["enabled"].as_bool(), sync["source"].as_str(), sync["device"].as_u64()),
        (Some(true), Some("mtc"), Some(3))
    );
    assert_eq!(s.project.timecode_sync.input, 4096 + (3 << 5));
    let sync = ok(&mut s, json!({"cmd": "sync.set", "source": "clock", "device": 63}));
    assert_eq!((sync["source"].as_str(), sync["device"].as_u64()), (Some("clock"), None));
    let sync = ok(&mut s, json!({"cmd": "sync.set", "source": "ltc", "channel": 2, "frame_rate": 25.0}));
    assert_eq!((sync["source"].as_str(), sync["channel"].as_u64()), (Some("ltc"), Some(2)));
    assert!(execute_json(&mut s, json!({"cmd": "sync.set", "frame_rate": 31.0})).is_err());
    assert_eq!(ok(&mut s, json!({"cmd": "sync.get"}))["frame_rate"], 25.0);
    ok(&mut s, json!({"cmd": "midi.keyboard", "note": 60, "velocity": 100}));
    assert!(execute_json(&mut s, json!({"cmd": "midi.keyboard", "note": 60, "velocity": 0, "channel": 17})).is_err());
    let devices = ok(&mut s, json!({"cmd": "midi.devices"}));
    assert!(devices["inputs"].is_array() && devices["outputs"].is_array());
    let missing = execute_json(&mut s, json!({"cmd": "midi.device", "kind": "output", "number": 120, "clock": true}));
    assert_eq!(missing.unwrap_err().kind, ErrorKind::NotFound);
}

#[test]
fn midi_device_settings_are_kept_in_the_preferences() {
    let dir = std::env::temp_dir().join(format!("scythe-midi-prefs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("reaper.ini");
    std::fs::write(dir.join("reaper-midihw-linux.ini"), "[mididevcache]\not0=1\non0=Synth Out\nit0=1\nin0=Keys In\n")
        .unwrap();
    let mut s = session();
    s.load_preferences(file.clone());
    assert_eq!(s.midi_devices.output(0).map(|p| p.name.as_str()), Some("Synth Out"));
    ok(
        &mut s,
        json!({"cmd": "midi.device", "kind": "output", "number": 0, "clock": true, "spp": false, "alias": "Modular"}),
    );
    ok(&mut s, json!({"cmd": "midi.device", "kind": "input", "number": 0, "enabled": false}));
    let ini = std::fs::read_to_string(&file).unwrap();
    assert!(
        ini.contains("midiouts_clock=1\n") && ini.contains("midiouts_clock_nospp=1\n") && ini.contains("midiins=-2\n"),
        "{ini}"
    );
    let cache = std::fs::read_to_string(dir.join("reaper-midihw-linux.ini")).unwrap();
    assert!(cache.contains("oa0=Modular"), "{cache}");
    let mut again = session();
    again.load_preferences(file);
    let devices = ok(&mut again, json!({"cmd": "midi.devices"}));
    assert_eq!(devices["outputs"][0]["alias"], "Modular");
    assert_eq!(
        (devices["outputs"][0]["clock"].as_bool(), devices["outputs"][0]["spp"].as_bool()),
        (Some(true), Some(false))
    );
    assert_eq!(devices["inputs"][0]["enabled"], false);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The positions REAPER 7.73 gives the same items after `SetTempoTimeSigMarker` halves the
/// starting tempo of a map with markers at 0 and 4 s.
#[test]
fn items_and_markers_follow_tempo_edits_by_their_timebase() {
    let mut s = session();
    add_track(&mut s, "A");
    ok(&mut s, json!({"cmd": "tempo.add", "time": 4.0, "bpm": 120.0}));
    ok(&mut s, json!({"cmd": "marker.add", "position": 3.0}));
    let mut ids = Vec::new();
    for timebase in ["project", "time", "beats", "beats_position"] {
        let item = Item::new(2.0, 1.0, Take::from_file("/nonexistent/take.wav".into()));
        ids.push(item.id.0);
        s.edit("Add item", |p| p.tracks[0].items.push(item));
        ok(&mut s, json!({"cmd": "item.timebase", "item": ids.last(), "timebase": timebase}));
    }
    let tb = ok(&mut s, json!({"cmd": "project.timebase"}));
    assert_eq!((tb["timebase"].as_str(), tb["tempo_follows_beats"].as_bool()), (Some("beats"), Some(true)));
    ok(&mut s, json!({"cmd": "tempo.edit", "index": 0, "bpm": 60.0}));
    let at = |s: &Session, id: u64| {
        let it = s.project.find_item(scythe_core::ItemId(id)).unwrap().1;
        (it.position, it.length, it.takes[0].playrate)
    };
    assert_eq!(at(&s, ids[0]), (4.0, 2.0, 0.5));
    assert_eq!(at(&s, ids[1]), (2.0, 1.0, 1.0));
    assert_eq!(at(&s, ids[2]), (4.0, 2.0, 0.5));
    assert_eq!(at(&s, ids[3]), (4.0, 1.0, 1.0));
    assert_eq!(s.project.tempo.points[1].time, 8.0);
    assert_eq!(s.project.markers[0].position, 6.0);

    ok(&mut s, json!({"cmd": "project.undo"}));
    ok(&mut s, json!({"cmd": "project.timebase", "timebase": "time", "tempo_follows_beats": false}));
    ok(&mut s, json!({"cmd": "tempo.edit", "index": 0, "bpm": 60.0}));
    assert_eq!(at(&s, ids[0]), (2.0, 1.0, 1.0));
    assert_eq!(s.project.tempo.points[1].time, 4.0);
    assert_eq!(s.project.markers[0].position, 3.0);
}

#[test]
fn stretch_markers_channel_modes_and_play_rate() {
    let mut s = session();
    add_track(&mut s, "A");
    let item = Item::new(0.0, 2.0, Take::from_file("/nonexistent/take.wav".into()));
    let id = item.id.0;
    s.edit("Add item", |p| p.tracks[0].items.push(item));

    ok(&mut s, json!({"cmd": "item.stretch_markers.add", "item": id, "position": 0.5}));
    let list = ok(&mut s, json!({"cmd": "item.stretch_markers.add", "item": id, "position": 1.5, "source": 1.3}));
    assert_eq!(list.as_array().map(Vec::len), Some(2));
    assert_eq!((list[0]["position"].as_f64(), list[0]["source"].as_f64()), (Some(0.5), Some(0.5)));
    let list = ok(&mut s, json!({"cmd": "item.stretch_markers.move", "item": id, "index": 0, "source": 0.8}));
    assert_eq!(list[0]["source"].as_f64(), Some(0.8));
    let list = ok(&mut s, json!({"cmd": "item.stretch_markers.move", "item": id, "index": 0, "position": 0.6}));
    assert_eq!((list[0]["position"].as_f64(), list[0]["source"].as_f64()), (Some(0.6), Some(0.8)));
    let crossing = json!({"cmd": "item.stretch_markers.move", "item": id, "index": 0, "position": 1.7});
    assert_eq!(execute_json(&mut s, crossing).unwrap_err().kind, ErrorKind::Invalid);
    let list = ok(&mut s, json!({"cmd": "item.stretch_markers.slope", "item": id, "index": 0, "slope": 0.5}));
    assert_eq!(list[0]["slope"].as_f64(), Some(0.5));
    let take = &s.project.tracks[0].items[0].takes[0];
    assert!((take.source_pos(1.6) - 1.4).abs() < 1e-9);
    let list = ok(&mut s, json!({"cmd": "item.stretch_markers.delete", "item": id, "index": 1}));
    assert_eq!(list.as_array().map(Vec::len), Some(1));
    ok(&mut s, json!({"cmd": "item.stretch_markers.delete", "item": id}));
    assert!(s.project.tracks[0].items[0].takes[0].stretch_markers.is_empty());

    let mode = ok(&mut s, json!({"cmd": "take.channel_mode", "item": id, "mode": "mono", "channel": 2}));
    assert_eq!((mode["code"].as_i64(), mode["label"].as_str()), (Some(4), Some("Mono (right)")));
    let mode = ok(&mut s, json!({"cmd": "take.channel_mode", "item": id, "mode": "stereo", "channel": 3}));
    assert_eq!(mode["code"].as_i64(), Some(69));

    let rate = ok(&mut s, json!({"cmd": "project.playrate", "rate": 1.5, "preserve_pitch": true}));
    assert_eq!((rate["rate"].as_f64(), rate["preserve_pitch"].as_bool()), (Some(1.5), Some(true)));
    assert_eq!(
        execute_json(&mut s, json!({"cmd": "project.playrate", "rate": 9.0})).unwrap_err().kind,
        ErrorKind::Invalid
    );
}

#[test]
fn render_matrix_settings_and_queue() {
    let mut s = session();
    let kick = add_track(&mut s, "Kick");
    s.project.tracks[0].items.push(Item::new(0.0, 4.0, Take::from_file("kick.wav".into())));
    ok(&mut s, json!({"cmd": "marker.add", "position": 0.0, "end": 2.0, "name": "Intro"}));
    ok(&mut s, json!({"cmd": "render.matrix", "region": 1, "track": kick, "on": true}));
    ok(&mut s, json!({"cmd": "render.matrix", "region": 1, "track": "master", "on": true}));
    let dir = std::env::temp_dir().join(format!("scythe-render-api-{}", std::process::id()));
    let jobs = ok(
        &mut s,
        json!({"cmd": "render.settings", "source": "matrix", "pattern": "$region-$track", "file": dir.join("out")}),
    );
    let names: Vec<_> = jobs
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["path"].as_str().unwrap().rsplit('/').next().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["Intro-Master.wav", "Intro-Kick.wav"]);
    assert_eq!(jobs[1]["source"]["kind"], "stem");

    let queued = ok(&mut s, json!({"cmd": "render.queue", "dir": dir}));
    let path = queued["queued"].as_str().unwrap().to_owned();
    assert!(std::fs::read_to_string(&path).unwrap().contains("QUEUED_RENDER_OUTFILE"));
    let list = ok(&mut s, json!({"cmd": "render.queue_list", "dir": dir}));
    assert_eq!(list.as_array().unwrap().len(), 1);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn modulation_and_learn_are_stored_beside_the_plugin() {
    let mut s = session();
    let t = add_track(&mut s, "Synth");
    ok(&mut s, json!({"cmd": "fx.add", "track": t, "plugin": "builtin:compressor"}));
    let set = ok(
        &mut s,
        json!({"cmd": "fx.modulation.set", "track": t, "index": 0, "param": 1, "base": 0.25,
               "lfo": {"active": true, "shape": "triangle", "hz": 2.0, "strength": 0.3, "direction": 0},
               "audio": {"active": true, "channel": 2, "attack_ms": 10.0}}),
    );
    assert_eq!(set["lfo"]["shape"], "triangle");
    assert!((set["lfo"]["hz"].as_f64().unwrap() - 2.0).abs() < 1e-9);
    assert_eq!(set["audio"]["channel"], 2);
    let synced =
        ok(&mut s, json!({"cmd": "fx.modulation.set", "track": t, "index": 0, "param": 1, "lfo": {"period_qn": 2.0}}));
    assert_eq!(synced["lfo"]["tempo_sync"], true);
    assert!((synced["lfo"]["period_qn"].as_f64().unwrap() - 2.0).abs() < 1e-9);
    ok(&mut s, json!({"cmd": "fx.add", "track": t, "plugin": "builtin:compressor"}));
    let linked = ok(
        &mut s,
        json!({"cmd": "fx.modulation.set", "track": t, "index": 1, "param": 0,
               "link": {"fx": 0, "param": 1, "scale": 0.5, "offset": -0.1}}),
    );
    assert_eq!(linked["link"]["fx"], 0);
    let learned = ok(
        &mut s,
        json!({"cmd": "fx.learn.set", "track": t, "index": 0, "param": 1,
               "midi": {"type": "cc", "number": 7, "channel": 2}, "mode": "relative2", "soft_takeover": true}),
    );
    assert_eq!(learned["describe"], "CC 7, ch 2");
    let list = ok(&mut s, json!({"cmd": "fx.modulation.list", "track": t, "index": 0}));
    assert_eq!(list[0]["param"], 1);
    assert_eq!(list[0]["learn"]["mode"], "relative2");
    let text = scythe_core::rpp::project_to_rpp(&s.project);
    assert!(text.contains("<PROGRAMENV 1 0"), "{text}");
    assert!(text.contains("PARMLEARN 1 133041 2"), "{text}");
    assert!(text.contains("PLINK 0.5 0:-1 1 -0.1"), "{text}");
    let back = scythe_core::rpp::project_from_rpp(&text).unwrap();
    assert_eq!(scythe_core::rpp::project_to_rpp(&back), text);
    ok(&mut s, json!({"cmd": "fx.learn.remove", "track": t, "index": 0, "param": 1}));
    ok(&mut s, json!({"cmd": "fx.modulation.remove", "track": t, "index": 0, "param": 1}));
    let list = ok(&mut s, json!({"cmd": "fx.modulation.list", "track": t, "index": 0}));
    assert_eq!(list, json!([]));

    let heard = execute_json(&mut s, json!({"cmd": "action.learn.add", "action": "40364", "heard": true}));
    assert_eq!(heard.unwrap_err().kind, ErrorKind::Failed);
    let bound = ok(
        &mut s,
        json!({"cmd": "action.learn.add", "action": "_SCYTHE_SETLIST_NEXT", "midi": {"type": "note", "number": 60}}),
    );
    assert_eq!(bound["describe"], "Note 60, ch 1");
    assert_eq!(ok(&mut s, json!({"cmd": "action.learn.list"})).as_array().unwrap().len(), 1);
    let removed = ok(&mut s, json!({"cmd": "action.learn.remove", "action": "_SCYTHE_SETLIST_NEXT"}));
    assert_eq!(removed["removed"], 1);
}

#[test]
fn project_tabs() {
    let mut s = session();
    add_track(&mut s, "One");
    let tabs = ok(&mut s, json!({"cmd": "project.tab_new"}));
    assert_eq!(tabs.as_array().unwrap().len(), 2);
    assert_eq!(tabs[1]["active"], true);
    assert!(s.project.tracks.is_empty());
    ok(&mut s, json!({"cmd": "project.tab_switch", "index": 0}));
    assert_eq!(s.project.tracks[0].name, "One");
    let err = execute_json(&mut s, json!({"cmd": "project.tab_switch", "index": 5})).unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);
    let tabs = ok(&mut s, json!({"cmd": "project.tab_close", "index": 1}));
    assert_eq!(tabs.as_array().unwrap().len(), 1);
    assert_eq!(ok(&mut s, json!({"cmd": "project.tabs"}))[0]["dirty"], true);
}
