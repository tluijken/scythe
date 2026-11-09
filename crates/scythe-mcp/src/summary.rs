//! A project snapshot as a few readable lines per track, small enough for a model's context
//! where the full snapshot of a large project is not.

use std::collections::HashMap;
use std::fmt::Write;

use serde_json::Value;

/// Whether `value` is a project snapshot (what `snapshot`, `project.open` and `project.new`
/// answer with).
pub fn is_snapshot(value: &Value) -> bool {
    value.get("tracks").is_some() && value.get("transport").is_some() && value.get("master").is_some()
}

/// Seconds as the API takes them, without trailing zeros: `83.5s`.
fn secs(value: &Value) -> String {
    let text = format!("{:.3}", value.as_f64().unwrap_or(0.0));
    let text = text.trim_end_matches('0').trim_end_matches('.');
    format!("{text}s")
}

fn level(gain: &Value) -> String {
    let gain = gain.as_f64().unwrap_or(1.0);
    if gain <= 0.0 {
        return "-inf dB".into();
    }
    let db = 20.0 * gain.log10();
    if db.abs() < 0.05 { "0.0 dB".into() } else { format!("{db:+.1} dB") }
}

fn pan(value: &Value) -> String {
    let pan = value.as_f64().unwrap_or(0.0);
    let percent = (pan.abs() * 100.0).round();
    match percent {
        0.0 => "C".into(),
        _ if pan < 0.0 => format!("{percent}%L"),
        _ => format!("{percent}%R"),
    }
}

/// REAPER's `I_RECINPUT` in words.
fn input(code: i64) -> String {
    match code {
        c if c < 0 => "no input".into(),
        c if c >= 4096 => "MIDI".into(),
        c if c >= 2048 => format!("input code {c}"),
        c if c >= 1024 => format!("inputs {}/{}", c - 1024 + 1, c - 1024 + 2),
        c => format!("input {}", c + 1),
    }
}

fn quoted(value: &Value) -> String {
    format!("\"{}\"", value.as_str().unwrap_or(""))
}

fn track_details(track: &Value, names: &HashMap<u64, String>) -> String {
    let mut out = format!("{}, pan {}", level(&track["volume"]), pan(&track["pan"]));
    for (flag, word) in [("muted", "muted"), ("solo", "solo")] {
        if track[flag] == true {
            write!(out, ", {word}").ok();
        }
    }
    if track["armed"] == true {
        let monitored = if track["monitor"] == true { ", monitored" } else { "" };
        write!(out, ", armed ({}{monitored})", input(track["input"].as_i64().unwrap_or(-1))).ok();
    }
    if track["folder_depth"].as_i64() == Some(1) {
        out.push_str(", folder");
    }
    let fx: Vec<String> = track["fx"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|f| {
            let name = f["name"].as_str().unwrap_or("?");
            if f["enabled"] == false {
                format!("{name} (bypassed)")
            } else if f["running"] == false {
                format!("{name} (not running)")
            } else {
                name.to_owned()
            }
        })
        .collect();
    if !fx.is_empty() {
        write!(out, "; FX: {}", fx.join(", ")).ok();
    }
    let sends: Vec<String> = track["sends"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| {
            let dest = s["dest"].as_u64().unwrap_or(0);
            let name = names.get(&dest).map_or_else(String::new, |n| format!(" \"{n}\""));
            let muted = if s["muted"] == true { ", muted" } else { "" };
            format!("to #{dest}{name} at {}{muted}", level(&s["volume"]))
        })
        .collect();
    if !sends.is_empty() {
        write!(out, "; sends: {}", sends.join(", ")).ok();
    }
    let items = track["items"].as_array().map_or(&[][..], Vec::as_slice);
    if !items.is_empty() {
        let start = items.iter().filter_map(|i| i["position"].as_f64()).fold(f64::INFINITY, f64::min);
        let end =
            items.iter().filter_map(|i| Some(i["position"].as_f64()? + i["length"].as_f64()?)).fold(0.0, f64::max);
        let plural = if items.len() == 1 { "" } else { "s" };
        write!(out, "; {} item{plural} from {} to {}", items.len(), secs(&start.into()), secs(&end.into())).ok();
    }
    out
}

fn item_line(item: &Value) -> String {
    let start = item["position"].as_f64().unwrap_or(0.0);
    let end = start + item["length"].as_f64().unwrap_or(0.0);
    let mut out =
        format!("item #{} {} {} to {}", item["id"], quoted(&item["name"]), secs(&start.into()), secs(&end.into()));
    if item["volume"].as_f64().is_some_and(|v| (v - 1.0).abs() > 1e-6) {
        write!(out, ", {}", level(&item["volume"])).ok();
    }
    if item["muted"] == true {
        out.push_str(", muted");
    }
    if item["takes"].as_u64().is_some_and(|n| n > 1) {
        write!(out, ", {} takes (active {})", item["takes"], item["active_take"]).ok();
    }
    out
}

/// The overview `get_project` answers with; `items` lists every item under its track.
pub fn project(snapshot: &Value, items: bool) -> String {
    let mut out = String::new();
    let s = snapshot;
    write!(out, "Project {}", quoted(&s["name"])).ok();
    match s["path"].as_str() {
        Some(path) => write!(out, " ({path})").ok(),
        None => write!(out, " (not saved to a file yet)").ok(),
    };
    if s["dirty"] == true {
        out.push_str(", unsaved changes");
    }
    let signature = &s["time_signature"];
    writeln!(
        out,
        "\n{} Hz, {} BPM, {}/{}, length {}",
        s["sample_rate"],
        s["bpm"],
        signature[0],
        signature[1],
        secs(&s["length"])
    )
    .ok();

    let t = &s["transport"];
    let state = if t["recording"] == true {
        "recording"
    } else if t["playing"] == true {
        "playing"
    } else {
        "stopped"
    };
    write!(out, "Transport: {state} at {}, edit cursor {}", secs(&t["position"]), secs(&t["cursor"])).ok();
    match (t["loop_enabled"] == true, t["loop_range"].as_array()) {
        (true, Some(range)) if range.len() == 2 => {
            write!(out, ", repeat {} to {}", secs(&range[0]), secs(&range[1])).ok()
        }
        (true, _) => write!(out, ", repeat on").ok(),
        (false, _) => write!(out, ", repeat off").ok(),
    };
    writeln!(out, ", metronome {}", if t["metronome"] == true { "on" } else { "off" }).ok();
    let step = |v: &Value| v.as_str().map_or_else(|| "nothing".to_owned(), |l| format!("\"{l}\""));
    writeln!(out, "Undo: {}; redo: {}", step(&s["undo"]), step(&s["redo"])).ok();

    let tracks = s["tracks"].as_array().map_or(&[][..], Vec::as_slice);
    let names: HashMap<u64, String> =
        tracks.iter().filter_map(|t| Some((t["id"].as_u64()?, t["name"].as_str()?.to_owned()))).collect();
    writeln!(out, "Master: {}", track_details(&s["master"], &names)).ok();
    writeln!(out, "Tracks ({}), as #id \"name\":", tracks.len()).ok();
    for track in tracks {
        let indent = "  ".repeat(track["level"].as_u64().unwrap_or(0) as usize + 1);
        writeln!(out, "{indent}#{} {}: {}", track["id"], quoted(&track["name"]), track_details(track, &names)).ok();
        if items {
            for item in track["items"].as_array().into_iter().flatten() {
                writeln!(out, "{indent}    {}", item_line(item)).ok();
            }
        }
    }

    let markers = s["markers"].as_array().map_or(&[][..], Vec::as_slice);
    if markers.is_empty() {
        out.push_str("No markers or regions.\n");
    } else {
        writeln!(out, "Markers and regions ({}):", markers.len()).ok();
        for m in markers {
            let name = m["name"].as_str().filter(|n| !n.is_empty()).map(|n| format!(" \"{n}\"")).unwrap_or_default();
            if m["region"] == true {
                writeln!(out, "  region {}{name} {} to {}", m["number"], secs(&m["position"]), secs(&m["end"])).ok();
            } else {
                writeln!(out, "  marker {}{name} at {}", m["number"], secs(&m["position"])).ok();
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn summarises_tracks_routing_and_markers() {
        let snapshot = json!({
            "revision": 3, "name": "monitors", "path": "/live/monitors.RPP", "dirty": true,
            "sample_rate": 48000, "length": 213.5, "bpm": 120.0, "time_signature": [4, 4],
            "undo": "Track volume", "redo": null,
            "transport": {"playing": false, "recording": false, "position": 0.0, "cursor": 12.25,
                          "loop_enabled": true, "loop_range": [10.0, 20.0], "metronome": false},
            "master": {"id": 0, "volume": 1.0, "pan": 0.0, "fx": [], "sends": [], "items": []},
            "tracks": [
                {"id": 12, "name": "Vox", "level": 0, "volume": 0.5, "pan": -0.3, "muted": false, "solo": true,
                 "armed": true, "monitor": true, "input": 1024, "folder_depth": 0,
                 "fx": [{"index": 0, "name": "Compressor", "enabled": false, "running": true}],
                 "sends": [{"index": 0, "dest": 15, "volume": 1.0, "pan": 0.0, "muted": false}],
                 "items": [{"id": 31, "name": "take.wav", "position": 1.0, "length": 4.0, "volume": 1.0,
                            "muted": true, "takes": 1, "active_take": 0}]},
                {"id": 15, "name": "Verb", "level": 0, "volume": 0.0, "pan": 1.0, "fx": [], "sends": [], "items": []}
            ],
            "markers": [{"number": 1, "region": true, "name": "Song 1", "position": 10.0, "end": 200.0}]
        });
        let text = project(&snapshot, true);
        assert!(text.starts_with("Project \"monitors\" (/live/monitors.RPP), unsaved changes\n"), "{text}");
        assert!(text.contains("edit cursor 12.25s, repeat 10s to 20s"), "{text}");
        assert!(
            text.contains(
                "#12 \"Vox\": -6.0 dB, pan 30%L, solo, armed (inputs 1/2, monitored); \
                 FX: Compressor (bypassed); sends: to #15 \"Verb\" at 0.0 dB; 1 item from 1s to 5s"
            ),
            "{text}"
        );
        assert!(text.contains("item #31 \"take.wav\" 1s to 5s, muted"), "{text}");
        assert!(text.contains("#15 \"Verb\": -inf dB, pan 100%R\n"), "{text}");
        assert!(text.contains("region 1 \"Song 1\" 10s to 200s"), "{text}");
        assert!(!project(&snapshot, false).contains("item #31"));
    }
}
