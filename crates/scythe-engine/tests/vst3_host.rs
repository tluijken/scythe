//! Hosts the workspace's own `vst3-test-plugin`. Skipped when it has not been built
//! (`cargo build -p vst3-test-plugin`).

use std::path::PathBuf;

use scythe_core::rpp::{project_from_rpp, project_to_rpp};
use scythe_core::{Project, Track};
use scythe_engine::MediaPool;
use scythe_engine::fx::{FxRegistry, ProcessContext, vst3_slot};
use scythe_engine::render::{MeterRegistry, RenderGraph, RenderScratch};
use scythe_engine::vst3_host;

/// Lays the built library out as a `.vst3` bundle, or `None` when it is not built.
fn bundle(name: &str) -> Option<PathBuf> {
    let target = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
    let lib =
        ["debug", "release"].iter().map(|p| target.join(p).join("libvst3_test_plugin.so")).find(|p| p.exists())?;
    let root = std::env::temp_dir().join(format!("scythe-vst3-{name}-{}", std::process::id()));
    let dir = root.join("Scythe Test.vst3/Contents").join(format!("{}-linux", std::env::consts::ARCH));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(lib, dir.join("Scythe Test.so")).unwrap();
    Some(root)
}

fn registry(root: &PathBuf, rate: u32) -> FxRegistry {
    let mut fx = FxRegistry::new(rate);
    fx.set_vst3_plugins(vst3_host::scan(std::slice::from_ref(root)));
    fx
}

#[test]
fn scans_processes_and_keeps_state() {
    let Some(root) = bundle("state") else {
        eprintln!("vst3-test-plugin not built; skipping");
        return;
    };
    let plugins = vst3_host::scan(std::slice::from_ref(&root));
    assert_eq!(plugins.len(), 2, "{plugins:?}");
    let gain = plugins.iter().find(|p| p.name == "Test Gain").unwrap();
    assert_eq!((gain.vendor.as_str(), gain.file_name.as_str(), gain.instrument), ("Scythe", "Scythe Test.vst3", false));
    assert!(plugins.iter().any(|p| p.name == "Test Synth" && p.instrument));

    let mut project = Project::default();
    let mut track = Track::new("t");
    track.fx.push(vst3_slot(gain));
    let id = track.id;
    project.tracks.push(track);
    let mut fx = registry(&root, 1000);
    let chain = fx.chain(Some(id), &project.tracks[0].fx);
    assert_eq!(chain.len(), 1, "{:?}", fx.status(Some(id), &project.tracks[0].fx[0], 0));
    let ctx = ProcessContext {
        midi: None,
        midi_from: 0,
        sample_rate: 1000,
        position: 0,
        playing: true,
        notes: None,
        live: &[],
        live_channel: None,
        aux: None,
        bpm: 120.0,
    };
    let (mut l, mut r) = (vec![1.0f32; 8], vec![-1.0f32; 8]);
    chain[0].process(&mut l, &mut r, &ctx);
    assert_eq!((l[0], r[7]), (0.5, -0.5));

    let params = chain[0].with(|e| e.params()).unwrap();
    assert_eq!((params.len(), params[0].name.as_str()), (1, "Gain"));
    chain[0].with(|e| e.set_param(0, 0.25));
    assert_eq!(chain[0].with(|e| e.param(0)).flatten(), Some((0.25, "0.25".into())));
    let (mut l, mut r) = (vec![1.0f32; 8], vec![1.0f32; 8]);
    chain[0].process(&mut l, &mut r, &ctx);
    assert_eq!(l[0], 0.25);

    fx.store_states(&mut project);
    let text = project_to_rpp(&project);
    assert!(text.contains("<VST \"VST3: Test Gain (Scythe)\" \"Scythe Test.vst3\""), "{text}");
    let reloaded = project_from_rpp(&text).unwrap();
    let mut fresh = registry(&root, 1000);
    let chain = fresh.chain(Some(reloaded.tracks[0].id), &reloaded.tracks[0].fx);
    let (mut l, mut r) = (vec![1.0f32; 4], vec![1.0f32; 4]);
    chain[0].process(&mut l, &mut r, &ctx);
    assert_eq!(l[0], 0.25, "state restored after reload");
    assert_eq!(chain[0].with(|e| e.param(0)).flatten().map(|(v, _)| v), Some(0.25), "controller follows the state");
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn midi_items_play_through_a_vst3_instrument() {
    let Some(root) = bundle("midi") else {
        eprintln!("vst3-test-plugin not built; skipping");
        return;
    };
    let mut fx = registry(&root, 1000);
    let synth = fx.vst3_plugins().iter().find(|p| p.name == "Test Synth").unwrap().clone();
    // At 120 bpm and 960 ticks per quarter: key 64 from 0.5 s to 1 s.
    let mut project = project_from_rpp(
        "<REAPER_PROJECT 0.1 \"7.73\" 0\n  TEMPO 120 4 4\n  <TRACK\n    <ITEM\n      POSITION 0\n      LENGTH 2\n      <SOURCE MIDI\n        HASDATA 1 960 QN\n        E 960 90 40 64\n        E 960 80 40 00\n        E 1920 b0 7b 00\n      >\n    >\n  >\n>\n",
    )
    .unwrap();
    project.tracks[0].fx.push(vst3_slot(&synth));
    let graph = RenderGraph::build(&project, &MediaPool::default(), &mut MeterRegistry::default(), &mut fx, 1000);
    let (mut l, mut r) = (vec![0.0f32; 1500], vec![0.0f32; 1500]);
    graph.render(0, &mut l, &mut r, &mut RenderScratch::new(4));
    let level = 64.0 / 128.0;
    assert_eq!(l[499], 0.0);
    assert!((l[500] - level).abs() < 1e-6 && (l[999] - level).abs() < 1e-6, "{} {}", l[500], l[999]);
    assert_eq!(l[1000], 0.0);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn midi_controllers_reach_mapped_vst3_parameters() {
    let Some(root) = bundle("cc") else {
        eprintln!("vst3-test-plugin not built; skipping");
        return;
    };
    let mut fx = registry(&root, 1000);
    let synth = fx.vst3_plugins().iter().find(|p| p.name == "Test Synth").unwrap().clone();
    // Key 64 from 0 to 1 s; controller 7 drops to 32 at 0.25 s.
    let mut project = project_from_rpp(
        "<REAPER_PROJECT 0.1 \"7.73\" 0\n  TEMPO 120 4 4\n  <TRACK\n    <ITEM\n      POSITION 0\n      LENGTH 2\n      <SOURCE MIDI\n        HASDATA 1 960 QN\n        E 0 90 40 64\n        E 480 b0 07 20\n        E 1440 80 40 00\n        E 1920 b0 7b 00\n      >\n    >\n  >\n>\n",
    )
    .unwrap();
    project.tracks[0].fx.push(vst3_slot(&synth));
    let graph = RenderGraph::build(&project, &MediaPool::default(), &mut MeterRegistry::default(), &mut fx, 1000);
    let (mut l, mut r) = (vec![0.0f32; 600], vec![0.0f32; 600]);
    graph.render(0, &mut l, &mut r, &mut RenderScratch::new(4));
    let level = 64.0 / 128.0;
    assert!((l[249] - level).abs() < 1e-6, "{}", l[249]);
    assert!((l[250] - level * 32.0 / 127.0).abs() < 1e-6, "{}", l[250]);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn live_midi_plays_an_armed_vst3_instrument() {
    use scythe_engine::midi_io::{ALL_MIDI_INPUTS, LiveMidi};
    use scythe_engine::render::LiveInput;
    let Some(root) = bundle("live") else {
        eprintln!("vst3-test-plugin not built; skipping");
        return;
    };
    let mut fx = registry(&root, 1000);
    let synth = fx.vst3_plugins().iter().find(|p| p.name == "Test Synth").unwrap().clone();
    let mut project = Project::default();
    let mut track = Track::new("keys");
    track.fx.push(vst3_slot(&synth));
    (track.armed, track.monitor, track.rec_input) = (true, true, ALL_MIDI_INPUTS + 2);
    project.tracks.push(track);
    let graph = RenderGraph::build(&project, &MediaPool::default(), &mut MeterRegistry::default(), &mut fx, 1000);
    assert!(graph.monitoring());
    let mut scratch = RenderScratch::new(4);
    let render = |midi: &[LiveMidi], scratch: &mut RenderScratch| {
        let (mut l, mut r) = (vec![0.0f32; 64], vec![0.0f32; 64]);
        let input = LiveInput { midi, play_items: false, ..LiveInput::default() };
        graph.render_with(0, &mut l, &mut r, scratch, &input);
        l[63]
    };
    let on = |ch: u8| LiveMidi { bytes: [0x90 | ch, 64, 100], len: 3 };
    assert_eq!(render(&[on(0)], &mut scratch), 0.0, "channel 1 is not the track's input channel 2");
    assert_eq!(render(&[on(1)], &mut scratch), 0.5, "note held from the live input");
    assert_eq!(render(&[], &mut scratch), 0.5, "and still held in the next block");
    assert_eq!(render(&[LiveMidi { bytes: [0x81, 64, 0], len: 3 }], &mut scratch), 0.0);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn sidechain_bus_takes_pins_three_and_four() {
    let Some(root) = bundle("sidechain") else {
        eprintln!("vst3-test-plugin not built; skipping");
        return;
    };
    let mut fx = registry(&root, 1000);
    let gain = fx.vst3_plugins().iter().find(|p| p.name == "Test Gain").unwrap().clone();
    let mut track = Track::new("t");
    track.fx.push(vst3_slot(&gain));
    let chain = fx.chain(Some(track.id), &track.fx);
    assert_eq!(chain[0].pins, (4, 2), "main and sidechain bus in, main bus out");
    let ctx = ProcessContext {
        midi: None,
        midi_from: 0,
        sample_rate: 1000,
        position: 0,
        playing: true,
        notes: None,
        live: &[],
        live_channel: None,
        aux: None,
        bpm: 120.0,
    };
    let mut bufs = vec![vec![1.0f32; 8], vec![1.0; 8], vec![0.25; 8], vec![0.25; 8]];
    chain[0].process_pins(&mut bufs, 0..8, &ctx);
    assert_eq!((bufs[0][3], bufs[1][3]), (0.75, 0.75), "main at the default gain of 0.5 plus the sidechain");
    std::fs::remove_dir_all(root).ok();
}

/// What REAPER 7.73 asks of the test gain on tracks of 2 to 16 channels: every main and
/// sidechain bus in the widest layout it takes (up to eight channels) that fits the track.
#[test]
fn buses_follow_the_track_width_like_reaper() {
    let Some(root) = bundle("buses") else {
        eprintln!("vst3-test-plugin not built; skipping");
        return;
    };
    let mut fx = registry(&root, 1000);
    let gain = fx.vst3_plugins().iter().find(|p| p.name == "Test Gain").unwrap().clone();
    let synth = fx.vst3_plugins().iter().find(|p| p.name == "Test Synth").unwrap().clone();
    let mut project = Project::default();
    for (channels, pins) in [(2, (4, 2)), (4, (8, 4)), (6, (12, 6)), (8, (16, 8)), (16, (16, 8))] {
        let mut track = Track::new("t");
        track.channels = channels as u32;
        track.fx.push(vst3_slot(&gain));
        track.fx.push(vst3_slot(&synth));
        let (chain, _) = fx.chain_with_automation(Some(track.id), &track.fx, channels);
        assert_eq!(chain[0].pins, pins, "gain on {channels} channels");
        assert_eq!(chain[1].pins, (0, pins.1), "synth on {channels} channels");
        project.tracks.push(track);
    }

    // A 6-channel bus gains every channel; the sidechain (pins 6 and up) adds to the first.
    let id = project.tracks[2].id;
    let chain = fx.chain_with_automation(Some(id), &project.tracks[2].fx, 6).0;
    let ctx = ProcessContext {
        midi: None,
        midi_from: 0,
        sample_rate: 1000,
        position: 0,
        playing: true,
        notes: None,
        live: &[],
        live_channel: None,
        aux: None,
        bpm: 120.0,
    };
    let mut bufs: Vec<Vec<f32>> = (0..12).map(|k| vec![if k < 6 { k as f32 + 1.0 } else { 0.0 }; 8]).collect();
    bufs[6] = vec![0.125; 8];
    chain[0].process_pins(&mut bufs, 0..8, &ctx);
    let out: Vec<f32> = bufs[..6].iter().map(|b| b[7]).collect();
    assert_eq!(out, [0.625, 1.0, 1.5, 2.0, 2.5, 3.0]);

    // REAPER's header after adding the gain to a 6-channel track: the input pins of the
    // plugin's own stereo buses, an output pin per track channel.
    fx.store_states(&mut project);
    let map = scythe_engine::fx::pin_map(&project.tracks[2].fx[0]).unwrap();
    assert_eq!(map.inputs, [1, 2, 4, 8]);
    assert_eq!(map.outputs, [1, 2, 4, 8, 16, 32]);
    assert_eq!(scythe_engine::fx::pin_map(&project.tracks[0].fx[0]), None, "plain stereo on 2 channels");

    // Narrowing the track sets the instance up anew, as REAPER does.
    let chain = fx.chain_with_automation(Some(id), &project.tracks[2].fx, 2).0;
    assert_eq!(chain[0].pins, (4, 2));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn arrangements_are_offered_in_reapers_order() {
    let counts: Vec<u32> = vst3_host::layouts_up_to(16).map(|a| a.count_ones()).collect();
    assert_eq!(counts, [1, 2, 3, 4, 4, 5, 6, 7, 8, 9, 9, 10, 11, 12, 12, 13, 14, 14, 14, 15, 16, 16, 16]);
    let six: Vec<u64> = vst3_host::layouts_up_to(6).collect();
    assert_eq!(six, [0x80000, 0x3, 0x103, 0x33, 0xf00000, 0x37, 0x3f]);
}
