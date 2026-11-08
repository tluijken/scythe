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
    let ctx = ProcessContext { sample_rate: 1000, position: 0, playing: true, notes: None, bpm: 120.0 };
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
