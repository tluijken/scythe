//! The track selection travels with the project in the tracks' `SEL` lines, as REAPER saves it.

use std::sync::Arc;

use scythe_core::render_settings::ResampleMode;
use scythe_engine::{Engine, MediaPool};
use scythe_session::Session;
use serde_json::json;

#[test]
fn the_selection_is_read_from_and_written_to_sel_lines() {
    let dir = std::env::temp_dir().join(format!("scythe-sel-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sel.RPP");
    std::fs::write(
        &path,
        "<REAPER_PROJECT 0.1 \"7.73\"\n  <TRACK\n    NAME a\n    SEL 0\n  >\n  <TRACK\n    NAME b\n    SEL 1\n  >\n  <TRACK\n    NAME c\n    SEL 1\n  >\n>\n",
    )
    .unwrap();
    let pool = Arc::new(MediaPool::with_resample(ResampleMode::PLAYBACK));
    let mut s = Session::new(Engine::start_offline(pool, 48_000));
    s.open_path(&path).unwrap();
    let ids: Vec<_> = s.project.tracks.iter().map(|t| t.id).collect();
    assert_eq!(s.selection(), [ids[1], ids[2]], "the tracks REAPER saved as selected");
    s.select_tracks(vec![ids[0]]);
    s.save_to(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.matches("SEL 1").count(), 1, "{text}");
    let first = text.find("NAME a").unwrap();
    assert!(text[first..].find("SEL 1").unwrap() < text[first..].find("NAME b").unwrap());
    let _ = scythe_session::api::execute_json(&mut s, json!({"cmd": "track.move", "track": ids[0].0, "index": 2, "also": [ids[1].0]}));
    let names: Vec<&str> = s.project.tracks.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["c", "a", "b"], "both tracks moved, keeping their order");
    let _ = std::fs::remove_dir_all(dir);
}
