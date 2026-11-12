//! The commands AI agents build sessions and match mixes with: importing media, finding songs
//! in backing tracks, measuring files and mixes offline, matching EQ, and polling events.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use scythe_core::render_settings::ResampleMode;
use scythe_engine::analysis::signals::pink;
use scythe_engine::{Engine, MediaPool};
use scythe_session::Session;
use scythe_session::api::{ErrorKind, Event, execute_json};
use scythe_session::control;
use serde_json::{Value, json};

const RATE: u32 = 48_000;

fn session() -> Session {
    let pool = Arc::new(MediaPool::with_resample(ResampleMode::PLAYBACK));
    Session::new(Engine::start_offline(pool, RATE))
}

fn ok(session: &mut Session, command: Value) -> Value {
    execute_json(session, command.clone()).unwrap_or_else(|e| panic!("{command} failed: {e}"))
}

/// Runs a command that starts a job and answers with the finished job.
fn job(session: &mut Session, command: Value) -> Value {
    let started = ok(session, command);
    result(session, started)
}

/// Waits for a job a command started and answers with its result.
fn result(session: &mut Session, started: Value) -> Value {
    let deadline = Instant::now() + Duration::from_secs(300);
    let mut answer = started;
    while answer["status"] == "running" {
        assert!(Instant::now() < deadline, "the job took too long: {answer}");
        std::thread::sleep(Duration::from_millis(20));
        answer = ok(session, json!({"cmd": "analysis.job", "job": answer["job"]}));
    }
    assert_eq!(answer["status"], "done", "{answer}");
    answer
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("scythe-agents-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_wav(path: &Path, left: &[f32], right: &[f32]) {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for (l, r) in left.iter().zip(right) {
        w.write_sample(*l).unwrap();
        w.write_sample(*r).unwrap();
    }
    w.finalize().unwrap();
}

/// A click file for a set: songs of clicks at 120, 100 and 140 BPM with 4 s of silence between.
fn click_file(path: &Path) -> Vec<(f64, f64)> {
    let mut signal = vec![0.0f32; RATE as usize];
    let mut songs = Vec::new();
    for (bpm, beats) in [(120.0, 24), (100.0, 20), (140.0, 28)] {
        let start = signal.len();
        let period = (60.0 / bpm * RATE as f64) as usize;
        for beat in 0..beats {
            let mut block = vec![0.0f32; if beat + 1 == beats { 480 } else { period }];
            for (i, s) in block.iter_mut().take(480).enumerate() {
                *s = 0.5 * (std::f32::consts::TAU * 1500.0 * i as f32 / RATE as f32).sin() * (1.0 - i as f32 / 480.0);
            }
            signal.extend(block);
        }
        songs.push((start as f64 / RATE as f64, bpm));
        signal.extend(vec![0.0f32; 4 * RATE as usize]);
    }
    write_wav(path, &signal, &signal);
    songs
}

/// Four beats of one note, as a type 0 standard MIDI file.
fn midi_file(path: &Path) {
    // Note on, note off 3840 ticks (four beats) later, end of track.
    let track = vec![0x00, 0x90, 60, 100, 0x9E, 0x00, 0x80, 60, 0, 0x00, 0xFF, 0x2F, 0x00];
    let mut bytes = b"MThd".to_vec();
    bytes.extend([0, 0, 0, 6, 0, 0, 0, 1, 0x03, 0xC0]);
    bytes.extend(b"MTrk");
    bytes.extend((track.len() as u32).to_be_bytes());
    bytes.extend(track);
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn import_probe_and_find_songs_in_a_click_file() {
    let dir = scratch("import");
    let click = dir.join("click.wav");
    let songs = click_file(&click);
    let midi = dir.join("keys.mid");
    midi_file(&midi);
    let mut s = session();

    let probe = job(&mut s, json!({"cmd": "media.probe", "path": click}));
    let info = &probe["result"];
    assert_eq!((info["sample_rate"].as_u64(), info["channels"].as_u64()), (Some(48_000), Some(2)), "{info}");
    assert!(info["integrated_lufs"].as_f64().unwrap() < -10.0, "{info}");
    let midi_probe = job(&mut s, json!({"cmd": "media.probe", "path": midi}));
    assert_eq!(midi_probe["result"]["kind"], "midi");

    let imported = ok(&mut s, json!({"cmd": "media.import", "paths": [click, midi], "position": 2.0}));
    let items = imported["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{imported}");
    assert_eq!(items[0]["track_name"], "click");
    assert_eq!(items[1]["kind"], "midi");
    assert_eq!(s.project.tracks.len(), 2);
    let length = items[0]["length"].as_f64().unwrap();
    let file_length = 1.0 + songs.len() as f64 * 4.0 + (23.0 * 0.5 + 19.0 * 0.6 + 27.0 * 60.0 / 140.0) + 3.0 * 0.01;
    assert!((length - file_length).abs() < 0.01, "{length} against {file_length}");
    assert!((items[1]["length"].as_f64().unwrap() - 2.0).abs() < 1e-6, "four beats at 120 BPM: {imported}");
    let undone = ok(&mut s, json!({"cmd": "project.undo"}));
    assert_eq!(undone["label"], "Import MIDI");
    ok(&mut s, json!({"cmd": "project.redo"}));

    let seq = ok(
        &mut s,
        json!({"cmd": "media.import", "paths": [click, click], "sequence": true, "gap": 2.0, "name": "Backing"}),
    );
    let placed = seq["items"].as_array().unwrap();
    assert_eq!(placed[0]["track"], placed[1]["track"]);
    assert!((placed[1]["position"].as_f64().unwrap() - (length + 2.0)).abs() < 1e-9, "{seq}");
    let missing = execute_json(&mut s, json!({"cmd": "media.import", "paths": ["/nowhere/x.wav"]})).unwrap_err();
    assert_eq!(missing.kind, ErrorKind::NotFound);

    let item = items[0]["item"].as_u64().unwrap();
    let found = job(&mut s, json!({"cmd": "analysis.segments", "item": item}));
    let segments = found["result"]["segments"].as_array().unwrap();
    assert_eq!(segments.len(), 3, "{found}");
    for (segment, (start, bpm)) in segments.iter().zip(&songs) {
        assert!((segment["start"].as_f64().unwrap() - start).abs() < 0.002, "{segment}");
        assert!((segment["bpm"].as_f64().unwrap() - bpm).abs() < 0.5, "{segment}");
    }
    let markers = found["result"]["markers"].as_array().unwrap();
    assert_eq!(markers.len(), 6);
    assert!((markers[0]["position"].as_f64().unwrap() - (2.0 + songs[0].0)).abs() < 0.002, "item time: {markers:?}");
    assert_eq!(markers[1]["name"], "!1016");

    let placed = ok(&mut s, json!({"cmd": "analysis.segments", "item": item, "markers": true, "name": "Track"}));
    assert_eq!(placed["result"]["added"], true);
    let setlist = ok(&mut s, json!({"cmd": "setlist.songs"}));
    let names: Vec<&str> = setlist.as_array().unwrap().iter().filter_map(|s| s["name"].as_str()).collect();
    assert_eq!(names, ["Track 1", "Track 2", "Track 3"], "{setlist}");
    assert_eq!(s.history.undo_label(), Some("Song markers"));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A reference made by putting a known ReaEQ on pink noise: matching the plain noise to it
/// through the render path, applying the proposed EQ and rendering again leaves the tonal
/// balance within half a dB.
#[test]
fn matching_a_mix_to_a_reference_converges() {
    let dir = scratch("match");
    let noise = dir.join("noise.wav");
    let p = pink(20.0, RATE, 21);
    write_wav(&noise, &p, &p);
    let mut s = session();
    ok(&mut s, json!({"cmd": "media.import", "paths": [noise], "name": "Ref"}));
    ok(&mut s, json!({"cmd": "media.import", "paths": [noise], "name": "Mix"}));
    let (ref_id, mix_id) = (s.project.tracks[0].id.0, s.project.tracks[1].id.0);
    let shaped = ok(
        &mut s,
        json!({"cmd": "analysis.apply_eq", "track": ref_id, "bands": [
            {"kind": "low_shelf", "freq": 150.0, "gain_db": 3.5},
            {"kind": "peak", "freq": 1800.0, "gain_db": -2.5, "bandwidth": 1.2},
            {"kind": "high_shelf", "freq": 7000.0, "gain_db": 2.0}]}),
    );
    assert_eq!(shaped["fx_index"], 0);
    let too_much = execute_json(
        &mut s,
        json!({"cmd": "analysis.apply_eq", "track": mix_id, "bands": [{"kind": "peak", "freq": 1000.0, "gain_db": 20.0}]}),
    )
    .unwrap_err();
    assert_eq!(too_much.kind, ErrorKind::Invalid);

    let reference = job(&mut s, json!({"cmd": "analysis.mix", "target": ref_id}));
    let reference_job = reference["job"].clone();
    assert!((reference["result"]["duration"].as_f64().unwrap() - 20.0).abs() < 0.01, "{reference}");
    let before =
        job(&mut s, json!({"cmd": "analysis.compare", "reference": {"job": reference_job}, "mix": {"mix": mix_id}}));
    assert!(before["result"]["comparison"]["max_tonal_diff_db"].as_f64().unwrap() > 2.0, "{before}");
    assert!(
        before["result"]["comparison"]["summary"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l.as_str().unwrap().contains("low end"))
    );

    let matched = ok(
        &mut s,
        json!({"cmd": "analysis.match_eq", "reference": {"job": reference_job}, "mix": {"mix": "Mix"}, "apply": "Mix"}),
    );
    let matched = result(&mut s, matched);
    assert!(matched["result"]["max_error_after_db"].as_f64().unwrap() <= 0.5, "{matched}");
    assert_eq!(matched["applied"]["track"], "Mix", "{matched}");
    assert_eq!(s.history.undo_label(), Some("Match EQ"));
    assert_eq!(s.project.tracks[1].fx.len(), 1);
    let again = ok(&mut s, json!({"cmd": "analysis.job", "job": matched["job"]}));
    assert_eq!(s.project.tracks[1].fx.len(), 1, "applied once: {again}");

    let after =
        job(&mut s, json!({"cmd": "analysis.compare", "reference": {"job": reference_job}, "mix": {"mix": "Mix"}}));
    let c = &after["result"]["comparison"];
    let worst = c["bands"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| (30.0..=16_000.0).contains(&b["hz"].as_f64().unwrap()))
        .map(|b| b["diff_db"].as_f64().unwrap().abs())
        .fold(0.0, f64::max);
    assert!(worst <= 0.5, "{worst} dB left: {c}");

    let file = job(&mut s, json!({"cmd": "analysis.file", "path": noise}));
    assert!(file["result"]["tilt_db_per_octave"].as_f64().unwrap().abs() < 0.3, "{file}");
    let cached = ok(&mut s, json!({"cmd": "analysis.file", "path": noise}));
    let cached = result(&mut s, cached);
    assert_eq!(cached["result"], file["result"]);
    let cache = scythe_session::data_dir().join("analysis-cache");
    assert!(std::fs::read_dir(&cache).unwrap().count() >= 1);
    let missing = execute_json(
        &mut s,
        json!({"cmd": "analysis.compare", "reference": {"job": 999_999}, "mix": {"mix": "master"}}),
    );
    assert_eq!(missing.unwrap_err().kind, ErrorKind::NotFound);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Renders and measurements run at the project's rate (REAPER's 44.1 kHz for a project that
/// sets none) or the rate the render settings give, whatever rate the audio device runs at,
/// and nothing after the master chain touches the samples: a limiter's ceiling holds.
#[test]
fn renders_and_measurements_run_at_the_project_rate() {
    let dir = scratch("project-rate");
    let tone = dir.join("tone.wav");
    let sine: Vec<f32> =
        (0..2 * RATE).map(|i| 0.99 * (2.0 * std::f32::consts::PI * 997.0 * i as f32 / RATE as f32).sin()).collect();
    write_wav(&tone, &sine, &sine);
    let mut s = session();
    assert_eq!(s.engine.sample_rate(), 48_000);
    ok(&mut s, json!({"cmd": "media.import", "paths": [tone], "name": "Tone"}));
    let limiter = scythe_engine::builtin::Builtin::Limiter;
    let mut state = limiter.default_state();
    state[8..16].copy_from_slice(&(-1.0f64).to_le_bytes());
    state[16..24].copy_from_slice(&(-1.0f64).to_le_bytes());
    s.edit("Limiter", |p| p.master.fx.push(limiter.slot_with(&state)));
    assert_eq!(s.project.sample_rate, None);
    let jobs = ok(&mut s, json!({"cmd": "render.settings", "file": dir.join("out"), "pattern": "mix"}));
    assert_eq!(jobs[0]["sample_rate"], 44_100, "{jobs}");

    let ceiling = 10f64.powf(-1.0 / 20.0);
    let render = |s: &Session| {
        let files = scythe_session::render::render_project(&s.project, &s.render_selection()).unwrap();
        let mut reader = hound::WavReader::open(&files[0]).unwrap();
        let full_scale = (1i64 << (reader.spec().bits_per_sample - 1)) as f64;
        let peak = reader.samples::<i32>().map(|v| (v.unwrap() as f64 / full_scale).abs()).fold(0.0, f64::max);
        (reader.spec().sample_rate, peak)
    };
    let (rate, peak) = render(&s);
    assert_eq!(rate, 44_100);
    assert!(peak > 0.5 && peak <= ceiling, "peak {} dBFS", 20.0 * peak.log10());

    let measured = job(&mut s, json!({"cmd": "analysis.mix", "target": "master"}));
    let m = &measured["result"];
    assert_eq!(m["sample_rate"], 44_100, "{m}");
    assert!(m["sample_peak_dbfs"].as_f64().unwrap() <= -1.0, "{m}");
    assert!((m["centroid_hz"].as_f64().unwrap() - 997.0).abs() < 20.0, "played at the wrong speed: {m}");

    let jobs = ok(&mut s, json!({"cmd": "render.settings", "sample_rate": 48_000}));
    assert_eq!(jobs[0]["sample_rate"], 48_000, "{jobs}");
    assert_eq!(render(&s).0, 48_000);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A bass-like tone, guitar-like noise overlapping it in the low mids and hi-hat-like noise far
/// above them: rendered group by group through their buses, the bass buries the guitars in the
/// low mids and the hats meet nobody.
#[test]
fn masking_finds_the_bass_burying_the_guitars_through_their_buses() {
    use scythe_engine::analysis::signals::{band_noise, harmonic};
    let dir = scratch("masking");
    let mut s = session();
    for (name, signal) in [
        ("Guitar", band_noise(150.0, 1500.0, 0.3, 8.0, RATE, 7)),
        ("Bass", harmonic(80.0, 6, 0.5, 8.0, RATE)),
        ("Hats", band_noise(8000.0, 16_000.0, 0.3, 8.0, RATE, 8)),
    ] {
        let path = dir.join(format!("{name}.wav"));
        write_wav(&path, &signal, &signal);
        ok(&mut s, json!({"cmd": "media.import", "paths": [path], "name": name}));
    }
    for (source, bus) in [("Guitar", "Gtr Bus"), ("Bass", "Low Bus")] {
        ok(&mut s, json!({"cmd": "track.add", "name": bus}));
        ok(&mut s, json!({"cmd": "send.add", "track": source, "dest": bus}));
        ok(&mut s, json!({"cmd": "track.main_send", "track": source, "on": false}));
    }
    let hats = s.project.tracks[2].id.0;
    let pairs = |answer: &Value| answer["result"]["pairs"].as_array().unwrap().clone();

    let named = job(
        &mut s,
        json!({"cmd": "analysis.masking", "groups": {"bass": ["Low Bus"], "guitars": ["Gtr Bus"], "hats": [hats]}}),
    );
    let r = &named["result"];
    let top = &pairs(&named)[0];
    assert_eq!(
        (&top["buried"], &top["by"], &top["region"]),
        (&json!("guitars"), &json!("bass"), &json!("low mids (160-400 Hz)")),
        "{r}"
    );
    assert!(top["share_pct"].as_f64().unwrap() > 80.0, "{top}");
    assert!((top["together_seconds"].as_f64().unwrap() - 8.0).abs() < 0.3, "{top}");
    assert!(pairs(&named).iter().all(|p| p["buried"] != "hats" && p["by"] != "hats"), "{r}");
    assert!(!pairs(&named).iter().any(|p| p["buried"] == "bass" && p["by"] == "guitars"), "{r}");
    assert!(r["summary"][0].as_str().unwrap().starts_with("bass buries guitars in the low mids"), "{r}");
    let groups: Vec<&str> = r["groups"].as_array().unwrap().iter().map(|g| g["name"].as_str().unwrap()).collect();
    assert_eq!(groups, ["bass", "guitars", "hats"]);
    assert_eq!(r["groups"][1]["tracks"], json!(["Gtr Bus"]));
    assert_eq!(r["bands_hz"].as_array().unwrap().len(), r["groups"][0]["level_db"].as_array().unwrap().len());
    assert!(s.project.tracks.iter().all(|t| !t.solo), "the project's solos are left alone");

    let derived = job(&mut s, json!({"cmd": "analysis.masking", "start": 1.0, "end": 7.0}));
    let r = &derived["result"];
    let groups: Vec<(&str, &Value)> =
        r["groups"].as_array().unwrap().iter().map(|g| (g["name"].as_str().unwrap(), &g["tracks"])).collect();
    assert_eq!(groups, [("Hats", &json!(["Hats"])), ("Gtr Bus", &json!(["Guitar"])), ("Low Bus", &json!(["Bass"]))]);
    let top = &pairs(&derived)[0];
    assert_eq!((&top["buried"], &top["by"]), (&json!("Gtr Bus"), &json!("Low Bus")), "{r}");
    assert!(top["longest"]["start"].as_f64().unwrap() >= 1.0 && top["longest"]["end"].as_f64().unwrap() <= 7.0);

    ok(&mut s, json!({"cmd": "track.volume", "track": "Low Bus", "db": -40.0}));
    let quiet = job(&mut s, json!({"cmd": "analysis.masking"}));
    assert!(
        !pairs(&quiet).iter().any(|p| p["buried"] == "Gtr Bus" && p["by"] == "Low Bus"),
        "the bass is heard through its bus: {quiet}"
    );

    let one = execute_json(&mut s, json!({"cmd": "analysis.masking", "groups": {"all": ["Bass", "Guitar"]}}));
    assert_eq!(one.unwrap_err().kind, ErrorKind::Invalid);
    let missing = execute_json(&mut s, json!({"cmd": "analysis.masking", "groups": {"a": ["Bass"], "b": ["Nope"]}}));
    assert_eq!(missing.unwrap_err().kind, ErrorKind::NotFound);
    let margin = execute_json(&mut s, json!({"cmd": "analysis.masking", "margin_db": -1}));
    assert_eq!(margin.unwrap_err().kind, ErrorKind::Invalid);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn events_can_be_polled_and_meters_read() {
    let mut s = session();
    let (_handle, mut pump) = control::channel(Arc::new(|| {}), Arc::new(|_: &Event| {}));
    pump.pump(&mut s);
    let start = ok(&mut s, json!({"cmd": "events.since"}));
    let latest = start["latest"].as_u64().unwrap();
    ok(&mut s, json!({"cmd": "track.add", "name": "Vox"}));
    s.notify("Saved");
    pump.pump(&mut s);
    let news = ok(&mut s, json!({"cmd": "events.since", "since": latest}));
    let kinds: Vec<&str> = news["events"].as_array().unwrap().iter().filter_map(|e| e["event"].as_str()).collect();
    assert!(kinds.contains(&"project_changed") && kinds.contains(&"notice"), "{news}");
    assert!(!kinds.contains(&"meters"));
    assert_eq!(news["revision"].as_u64(), Some(s.revision()));
    assert_eq!(news["missed"], false);
    let nothing = ok(&mut s, json!({"cmd": "events.since", "since": news["latest"]}));
    assert_eq!(nothing["events"], json!([]));

    let meters = ok(&mut s, json!({"cmd": "events.meters"}));
    assert_eq!(meters["tracks"][0]["name"], "Vox");
    assert_eq!(meters["tracks"][0]["peak_db"], json!([-150.0, -150.0]));
    assert!(meters["health"]["status"].is_string(), "{meters}");
}

#[test]
fn scripts_need_a_runner_and_are_one_undo_step() {
    let mut s = session();
    let refused = execute_json(&mut s, json!({"cmd": "script.run", "code": "reaper.InsertTrackAtIndex(0, true)"}));
    assert_eq!(refused.unwrap_err().kind, ErrorKind::Failed);
    let seen = Arc::new(Mutex::new(String::new()));
    let code_seen = seen.clone();
    s.script_runner = Some(Box::new(move |session, _, code| {
        *code_seen.lock().unwrap() = code.to_owned();
        let mut project = session.project.clone();
        project.add_track("From script");
        Ok(scythe_session::api::script::ScriptRun {
            project: Some(project),
            console: "hello\n".into(),
            undo: Some("Script edit".into()),
            cursor: Some(4.0),
            error: (code == "-- fails").then(|| "control.lua:2: boom".into()),
        })
    }));
    let ran = ok(&mut s, json!({"cmd": "script.run", "code": "-- anything", "language": "lua"}));
    assert_eq!((ran["console"].as_str(), ran["changed"].as_bool()), (Some("hello\n"), Some(true)));
    assert_eq!(seen.lock().unwrap().as_str(), "-- anything");
    assert_eq!(s.project.tracks[0].name, "From script");
    assert_eq!(s.history.undo_label(), Some("Script edit"));
    assert_eq!(s.project.cursor, 4.0);
    let failed = execute_json(&mut s, json!({"cmd": "script.run", "code": "-- fails"})).unwrap_err();
    assert!(failed.message.contains("boom"), "{failed:?}");
    assert_eq!(s.project.tracks.len(), 2, "what it changed before the error stays, as in REAPER");
    s.undo();
    assert_eq!(s.project.tracks.len(), 1, "and is one undo step");
}

#[test]
fn agent_calls_name_effect_parameters_and_mark_their_undo_steps() {
    let mut s = session();
    ok(&mut s, json!({"cmd": "track.add", "name": "Bass"}));
    ok(&mut s, json!({"cmd": "fx.add", "track": "Bass", "plugin": "builtin:compressor"}));
    let params = ok(&mut s, json!({"cmd": "fx.params", "track": "Bass", "index": 0}));
    let threshold =
        params.as_array().unwrap().iter().find(|p| p["name"] == "Threshold").expect("a threshold")["id"].clone();
    let agent = json!({"client": "c1", "name": "Claude Code", "transport": "stdio"});
    let mut begin = agent.clone();
    begin["cmd"] = "agents.call".into();
    begin["tool"] = "fx_param".into();
    begin["command"] = "fx.param".into();
    begin["arguments"] = json!({"track": "Bass", "index": 0, "param": threshold, "value": -18.0});
    let call = ok(&mut s, begin)["call"].clone();
    let command = json!({"cmd": "fx.param", "track": "Bass", "index": 0, "param": threshold, "value": -18.0});
    ok(&mut s, json!({"cmd": "agents.run", "call": call, "command": command}));
    ok(&mut s, json!({"cmd": "agents.done", "call": call}));
    let activity = ok(&mut s, json!({"cmd": "agents.activity"}));
    let made = &activity["calls"][0];
    let summary = made["summary"].as_str().unwrap();
    assert!(summary.starts_with("Set ") && summary.ends_with(" Threshold on Bass to -18"), "{activity}");
    assert_eq!(made["undo"], json!(["Adjust Threshold"]));
    assert_eq!(s.history.undo_by(), Some("Claude Code"));
    ok(&mut s, json!({"cmd": "agents.undo"}));
    assert_eq!(s.history.undo_label(), Some("Add plugin"));
    assert!(execute_json(&mut s, json!({"cmd": "agents.undo"})).is_err(), "the user's step is on top");
}
