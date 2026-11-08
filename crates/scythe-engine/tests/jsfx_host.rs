//! Runs a stock JSFX from the installed REAPER. Skipped when no REAPER `Effects` folder is
//! found.

use scythe_core::rpp::{project_from_rpp, project_to_rpp};
use scythe_engine::fx::{FxRegistry, FxStatus, ProcessContext};

#[test]
fn soft_clipper_runs_and_keeps_its_sliders() {
    if scythe_engine::jsfx::find("schwa/soft_clipper", &scythe_engine::jsfx::search_paths()).is_none() {
        eprintln!("no REAPER Effects folder; skipping");
        return;
    }
    let mut project = project_from_rpp(
        "<REAPER_PROJECT 0.1 \"7.73\" 0\n  <TRACK\n    <FXCHAIN\n      BYPASS 0 0 0\n      <JS schwa/soft_clipper \"\"\n        1 -1 - - -\n      >\n      FXID {00000000-0000-0000-0000-000000000002}\n      WAK 0 0\n    >\n  >\n>\n",
    )
    .unwrap();
    let id = project.tracks[0].id;
    let mut fx = FxRegistry::new(48000);
    let chain = fx.chain(Some(id), &project.tracks[0].fx);
    assert!(matches!(fx.status(Some(id), &project.tracks[0].fx[0], 0), Some(FxStatus::Native(_))));
    let params = chain[0].with(|e| e.params()).unwrap();
    assert_eq!(params[0].name, "Boost (dB)");
    assert_eq!(chain[0].with(|e| e.param(1)).flatten().map(|p| p.0), Some(-1.0));

    // A loud sine comes out under the -1 dB brickwall.
    let ctx = ProcessContext {
        midi: None,
        midi_from: 0,
        sample_rate: 48000,
        position: 0,
        playing: true,
        notes: None,
        live: &[],
        live_channel: None,
        aux: None,
        bpm: 120.0,
    };
    let (mut l, mut r): (Vec<f32>, Vec<f32>) = (0..4800).map(|i| ((i as f32 * 0.05).sin(), 0.0)).unzip();
    chain[0].process(&mut l, &mut r, &ctx);
    let peak = l.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(peak <= 10f32.powf(-1.0 / 20.0) + 1e-4 && peak > 0.5, "{peak}");

    chain[0].with(|e| e.set_param(0, 3.0));
    fx.store_states(&mut project);
    let text = project_to_rpp(&project);
    assert!(text.contains("<JS schwa/soft_clipper \"\"\n        3 -1 - -"), "{text}");
}

/// Renders key 64 (0.5 s to 1 s at 1000 Hz) through JSFX `chain`, written from `scripts`.
fn render_midi_chain(scripts: &[(&str, &str)], chain: &[&str]) -> Vec<f32> {
    use scythe_core::rpp::project_from_rpp;
    use scythe_engine::MediaPool;
    use scythe_engine::render::{MeterRegistry, RenderGraph, RenderScratch};

    let dir = std::env::temp_dir().join(format!("scythe-jsfx-midi-{}-{}", std::process::id(), chain.join("-")));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, src) in scripts {
        std::fs::write(dir.join(name), src).unwrap();
    }
    let slots: String =
        chain.iter().map(|n| format!("      BYPASS 0 0 0\n      <JS {n} \"\"\n      >\n      WAK 0 0\n")).collect();
    let text = format!(
        "<REAPER_PROJECT 0.1 \"7.73\" 0\n  TEMPO 120 4 4\n  <TRACK\n    <FXCHAIN\n{slots}    >\n    <ITEM\n      POSITION 0\n      LENGTH 2\n      <SOURCE MIDI\n        HASDATA 1 960 QN\n        E 960 90 40 64\n        E 960 80 40 00\n        E 1920 b0 7b 00\n      >\n    >\n  >\n>\n"
    );
    let project = project_from_rpp(&text).unwrap();
    let mut fx = FxRegistry::new(1000);
    fx.set_jsfx_paths(vec![dir.clone()]);
    let graph = RenderGraph::build(&project, &MediaPool::default(), &mut MeterRegistry::default(), &mut fx, 1000);
    let (mut l, mut r) = (vec![0.0f32; 1500], vec![0.0f32; 1500]);
    graph.render(0, &mut l, &mut r, &mut RenderScratch::new(4));
    std::fs::remove_dir_all(&dir).unwrap();
    l
}

const TRANSPOSE: &str = "desc:Octave up\n@block\nwhile (midirecv(offset, msg1, msg2, msg3)) (\n  s = msg1 & 0xF0;\n  s == 0x90 || s == 0x80 ? msg2 += 12;\n  midisend(offset, msg1, msg2, msg3);\n);\n";
const LEVEL: &str = "desc:Note level\n@init\nfrom = 0;\nlvl = 1000;\n@block\nn = 0;\nwhile (midirecv(offset, msg1, msg2, msg3)) (\n  (msg1 & 0xF0) == 0x90 || (msg1 & 0xF0) == 0x80 ? (\n    from[n] = offset;\n    lvl[n] = (msg1 & 0xF0) == 0x90 && msg3 > 0 ? msg2 / 128 : 0;\n    n += 1;\n  );\n);\nnext = 0;\npos = 0;\n@sample\nwhile (next < n && from[next] <= pos) (\n  level = lvl[next];\n  next += 1;\n);\npos += 1;\nspl0 = level;\nspl1 = level;\n";
const GAIN: &str = "desc:Half\n@sample\nspl0 *= 0.5;\nspl1 *= 0.5;\n";

#[test]
fn jsfx_play_the_track_midi() {
    let l = render_midi_chain(&[("level", LEVEL)], &["level"]);
    assert_eq!((l[499], l[500], l[999], l[1000]), (0.0, 0.5, 0.5, 0.0), "key 64 from frame 500 to 1000");
}

#[test]
fn jsfx_midi_reaches_the_next_effect() {
    let scripts = [("transpose", TRANSPOSE), ("level", LEVEL), ("half", GAIN)];
    let l = render_midi_chain(&scripts, &["transpose", "half", "level"]);
    assert_eq!((l[499], l[500], l[999], l[1000]), (0.0, 76.0 / 128.0, 76.0 / 128.0, 0.0), "transposed to key 76");
    // An effect that does not read MIDI lets it through unread.
    let l = render_midi_chain(&scripts, &["half", "level"]);
    assert_eq!(l[700], 0.5);
}

#[test]
fn serialized_state_is_saved_and_restored_like_reaper() {
    use scythe_core::rpp::{project_from_rpp, project_to_rpp};
    let dir = std::env::temp_dir().join(format!("scythe-jsfx-ser-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("ser"),
        "desc:ser\n@init\next_noinit = 1;\nx = 1.5; y = -2; buf = 100; buf[0] = 7; buf[1] = 8.25; buf[2] = 1/3;\n\
         @serialize\nfile_var(0, x);\nfile_var(0, y);\nfile_mem(0, buf, 3);\n@sample\nspl0 = x; spl1 = buf[1];\n",
    )
    .unwrap();
    let text = "<REAPER_PROJECT 0.1 \"7.73\" 0\n  <TRACK\n    <FXCHAIN\n      BYPASS 0 0 0\n      <JS ser \"\"\n      >\n      WAK 0 0\n    >\n  >\n>\n";
    let mut project = project_from_rpp(text).unwrap();
    let mut fx = FxRegistry::new(48000);
    fx.set_jsfx_paths(vec![dir.clone()]);
    let id = project.tracks[0].id;
    fx.chain(Some(id), &project.tracks[0].fx);
    fx.store_states(&mut project);
    let saved = project_to_rpp(&project);
    // What REAPER 7.73 wrote for the same effect.
    assert!(saved.contains("<JS_SER\n        AADAPwAAAMAAAOBAAAAEQauqqj4=\n      >"), "{saved}");

    // A stored state is read back after @init and @slider.
    let changed = saved.replace("AADAPwAAAMAAAOBAAAAEQauqqj4=", "AACAQAAAAMAAAOBAAAAoQauqqj4=");
    let project = project_from_rpp(&changed).unwrap();
    let mut fx = FxRegistry::new(48000);
    fx.set_jsfx_paths(vec![dir.clone()]);
    let chain = fx.chain(Some(id), &project.tracks[0].fx);
    let ctx = ProcessContext {
        midi: None,
        midi_from: 0,
        sample_rate: 48000,
        position: 0,
        playing: true,
        notes: None,
        live: &[],
        live_channel: None,
        aux: None,
        bpm: 120.0,
    };
    let (mut l, mut r) = (vec![0.0f32; 4], vec![0.0f32; 4]);
    chain[0].process(&mut l, &mut r, &ctx);
    assert_eq!((l[0], r[0]), (4.0, 10.5));
    std::fs::remove_dir_all(&dir).unwrap();
}
