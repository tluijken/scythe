use std::sync::Arc;

use scythe_core::render_settings::ResampleMode;
use scythe_core::rpp::parse;
use scythe_core::{Item, Source, Take};
use scythe_engine::{Engine, MediaPool};
use scythe_session::Session;
use scythe_session::api::{ErrorKind, execute_json};
use serde_json::{Value, json};

fn session() -> Session {
    let pool = Arc::new(MediaPool::with_resample(ResampleMode::PLAYBACK));
    Session::new(Engine::start_offline(pool, 48_000))
}

fn ok(session: &mut Session, command: Value) -> Value {
    execute_json(session, command.clone()).unwrap_or_else(|e| panic!("{command} failed: {e}"))
}

/// A track with one MIDI item: the notes REAPER's editor actions were measured on.
fn midi_item(s: &mut Session) -> (u64, u64) {
    let track = ok(s, json!({"cmd": "track.add", "name": "Keys"}))["id"].as_u64().unwrap();
    let chunk = parse(
        "<SOURCE MIDI\n  HASDATA 1 960 QN\n  CCINTERP 32\n  e 10 90 3d 64\n  e 307 80 3d 00\n  e 183 90 3e 5a\n  e 314 80 3e 00\n  e 186 90 3f 50\n  e 321 80 3f 00\n  E 2519 b0 7b 00\n>\n",
    )
    .unwrap();
    let mut take = Take::from_file("".into());
    take.source = Source::Raw(chunk);
    let item = Item::new(0.0, 4.0, take);
    let id = item.id.0;
    s.edit("Add item", |p| p.tracks[0].items.push(item));
    (track, id)
}

fn starts(notes: &Value) -> Vec<f64> {
    notes.as_array().unwrap().iter().map(|n| (n["start"].as_f64().unwrap() * 960.0).round()).collect()
}

#[test]
fn notes_are_listed_edited_and_quantized() {
    let mut s = session();
    let (_, item) = midi_item(&mut s);
    let notes = ok(&mut s, json!({"cmd": "midi.notes.list", "item": item}));
    assert_eq!(starts(&notes), [10.0, 500.0, 1000.0]);
    assert_eq!(notes[0]["selected"], true);
    let q = ok(&mut s, json!({"cmd": "midi.quantize", "item": item, "grid": 0.25}));
    assert_eq!(starts(&q), [0.0, 480.0, 960.0]);
    ok(&mut s, json!({"cmd": "project.undo"}));
    let half = ok(&mut s, json!({"cmd": "midi.quantize", "item": item, "grid": 0.25, "strength": 50}));
    assert_eq!(starts(&half), [5.0, 490.0, 980.0]);
    let added = ok(
        &mut s,
        json!({"cmd": "midi.notes.add", "item": item, "notes": [{"start": 3.0, "length": 0.5, "key": 72, "channel": 10}]}),
    );
    assert_eq!(added[3]["channel"], 10);
    let t = ok(&mut s, json!({"cmd": "midi.transpose", "item": item, "semitones": 2, "scale": "Major"}));
    assert_eq!(t[0]["key"], 64, "C# snaps into C major and moves two steps");
    let legato = ok(&mut s, json!({"cmd": "midi.edit", "item": item, "op": "legato"}));
    assert_eq!((legato[0]["end"].as_f64().unwrap() * 960.0).round(), 490.0);
    let err =
        execute_json(&mut s, json!({"cmd": "midi.notes.set", "item": item, "indexes": [9], "key": 1})).unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);
}

#[test]
fn controllers_shapes_and_sysex() {
    let mut s = session();
    let (_, item) = midi_item(&mut s);
    let line = ok(
        &mut s,
        json!({"cmd": "midi.cc.line", "item": item, "lane": "cc1", "from": 0.0, "to": 2.0, "start_value": 10, "end_value": 100, "shape": "bezier", "tension": 0.5}),
    );
    assert_eq!(line[0]["shape"], "bezier");
    assert_eq!(line[1]["value"], 100);
    let chunk = scythe_core::rpp::write(s.project.midi_chunk(scythe_core::ItemId(item)).unwrap());
    assert!(chunk.contains("E 0 b0 01 0a\n  ENV 5 0 0.5\n"), "{chunk}");
    let listed = ok(&mut s, json!({"cmd": "midi.cc.list", "item": item}));
    assert_eq!(listed.as_array().unwrap().len(), 2);
    ok(&mut s, json!({"cmd": "midi.cc.set", "item": item, "lane": "pitch", "points": [{"at": 1.0, "value": 16383}]}));
    let pb = ok(&mut s, json!({"cmd": "midi.cc.list", "item": item, "lane": "pitch"}));
    assert_eq!(pb[0]["value"], 16383);
    let syx = ok(&mut s, json!({"cmd": "midi.sysex.add", "item": item, "at": 0.5, "hex": "7E 7F 06 01"}));
    assert_eq!(syx[0]["hex"], "F0 7E 7F 06 01 F7");
    let text = ok(&mut s, json!({"cmd": "midi.sysex.add", "item": item, "at": 1.0, "text": "verse", "kind": 6}));
    assert_eq!(text[1]["text"], "verse");
}

#[test]
fn editor_settings_and_note_names() {
    let mut s = session();
    let (track, item) = midi_item(&mut s);
    let v = ok(
        &mut s,
        json!({"cmd": "midi.editor.set", "item": item, "mode": "named_notes", "grid": 0.5, "swing": 0.5, "key_snap": true, "show_channels": [10], "lanes": ["velocity", "cc7", "pitch"]}),
    );
    assert_eq!(v["show_channels"], json!([10]));
    let chunk = scythe_core::rpp::write(s.project.midi_chunk(scythe_core::ItemId(item)).unwrap());
    assert!(chunk.contains("EVTFILTER 512 ") && chunk.contains("VELLANE 7 100") && chunk.contains("KEYSNAP 1"));
    let q = ok(&mut s, json!({"cmd": "midi.quantize", "item": item}));
    assert_eq!(starts(&q), [0.0, 600.0, 960.0], "the take's swing grid");
    let names =
        ok(&mut s, json!({"cmd": "midi.note_names.set", "track": track, "names": [{"key": 36, "name": "Kick"}]}));
    assert_eq!(names[0]["name"], "Kick");
    let groove = ok(&mut s, json!({"cmd": "midi.groove.extract", "item": item, "beats": 1.0}));
    assert!(groove["groove"].as_str().unwrap().starts_with("Version: 2\nNumber of beats in groove: 1\n"));
}

#[test]
fn editor_actions_by_reaper_id() {
    let mut s = session();
    let (_, item) = midi_item(&mut s);
    let q = ok(&mut s, json!({"cmd": "midi.action", "item": item, "command": 40469}));
    assert_eq!(starts(&q), [0.0, 480.0, 960.0], "1/16 is the default grid");
    ok(&mut s, json!({"cmd": "midi.action", "item": item, "command": 42471}));
    assert_eq!(ok(&mut s, json!({"cmd": "midi.editor.get", "item": item}))["key_snap"], true);
    let list = ok(&mut s, json!({"cmd": "midi.actions"}));
    assert!(list.as_array().unwrap().iter().any(|a| a["command"] == 40765));
    assert!(execute_json(&mut s, json!({"cmd": "midi.action", "item": item, "command": 1})).is_err());
}

#[test]
fn the_key_signature_lives_in_the_project() {
    let mut s = session();
    let (_, item) = midi_item(&mut s);
    let k = ok(&mut s, json!({"cmd": "midi.key"}));
    assert_eq!((k["root"].as_u64(), k["scale"].as_str()), (Some(0), Some("Major")), "C major without a <KEYSIG");
    let k = ok(&mut s, json!({"cmd": "midi.key", "root": 2, "scale": "Minor"}));
    assert_eq!((k["root"].as_u64(), k["scale"].as_str()), (Some(2), Some("Minor")));
    let text = scythe_core::rpp::project_to_rpp(&s.project);
    assert!(text.contains("<KEYSIG\n    0 2 -1 0x5AD"), "{text}");
    let edited = ok(&mut s, json!({"cmd": "midi.action", "item": item, "command": 40767}));
    assert!(edited.as_array().unwrap().iter().all(|n| [2, 4, 5, 7, 9, 10, 0].contains(&(n["key"].as_u64().unwrap() % 12))));
    ok(&mut s, json!({"cmd": "midi.edit", "item": item, "op": "legato_relative"}));
}

/// Edits the first item of `SCYTHE_MIDI_EDIT_IN` the way the REAPER render check expects and
/// saves `SCYTHE_MIDI_EDIT_OUT`, for REAPER to render and the null check to compare.
#[test]
#[ignore]
fn edit_a_project_for_reaper() {
    let (Ok(input), Ok(output)) = (std::env::var("SCYTHE_MIDI_EDIT_IN"), std::env::var("SCYTHE_MIDI_EDIT_OUT")) else {
        return;
    };
    let mut s = session();
    ok(&mut s, json!({"cmd": "project.open", "path": input}));
    let item = s.project.tracks[0].items[0].id.0;
    ok(&mut s, json!({"cmd": "midi.quantize", "item": item, "grid": 0.25, "swing": 0.5, "strength": 75}));
    ok(&mut s, json!({"cmd": "midi.transpose", "item": item, "semitones": 1, "scale": "Major", "root": 2}));
    ok(&mut s, json!({"cmd": "midi.notes.set", "item": item, "indexes": [1], "velocity": 33, "end": 1.9}));
    ok(&mut s, json!({"cmd": "midi.edit", "item": item, "op": "split", "value": 3.1}));
    ok(
        &mut s,
        json!({"cmd": "midi.cc.line", "item": item, "lane": "cc1", "from": 0.5, "to": 2.5, "start_value": 5, "end_value": 120, "shape": "bezier", "tension": -0.4}),
    );
    ok(
        &mut s,
        json!({"cmd": "midi.cc.line", "item": item, "lane": "pitch", "from": 1.0, "to": 3.5, "start_value": 16000, "end_value": 100, "shape": "slow_start_end"}),
    );
    ok(
        &mut s,
        json!({"cmd": "midi.cc.set", "item": item, "lane": "cc1", "points": [{"at": 3.0, "value": 60, "shape": "fast_end"}, {"at": 3.75, "value": 0}]}),
    );
    ok(&mut s, json!({"cmd": "project.save", "path": output}));
}
