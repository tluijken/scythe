//! Hosts the workspace's own `vst2-test-plugin`. Skipped when it has not been built
//! (`cargo build -p vst2-test-plugin`).

use std::path::PathBuf;

use scythe_core::rpp::project_from_rpp;
use scythe_engine::MediaPool;
use scythe_engine::fx::FxRegistry;
use scythe_engine::render::{MeterRegistry, RenderGraph, RenderScratch};

fn library() -> Option<PathBuf> {
    let target = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
    ["debug", "release"].iter().map(|p| target.join(p).join("libvst2_test_plugin.so")).find(|p| p.exists())
}

#[test]
fn midi_items_play_through_a_vst2_instrument() {
    let Some(lib) = library() else {
        eprintln!("vst2-test-plugin not built; skipping");
        return;
    };
    let lib = lib.canonicalize().unwrap();
    // At 120 bpm and 960 ticks per quarter: key 64 from 0.5 s to 1 s.
    let text = format!(
        "<REAPER_PROJECT 0.1 \"7.73\" 0\n  TEMPO 120 4 4\n  <TRACK\n    <FXCHAIN\n      BYPASS 0 0 0\n      <VST \"VSTi: Test Synth (Scythe)\" \"{}\" 0 \"\" 1399027028<56535453637954746573742073796e74> \"\"\n      >\n      FXID {{00000000-0000-0000-0000-000000000001}}\n      WAK 0 0\n    >\n    <ITEM\n      POSITION 0\n      LENGTH 2\n      <SOURCE MIDI\n        HASDATA 1 960 QN\n        E 960 90 40 64\n        E 960 80 40 00\n        E 1920 b0 7b 00\n      >\n    >\n  >\n>\n",
        lib.display()
    );
    let project = project_from_rpp(&text).unwrap();
    let mut fx = FxRegistry::new(1000);
    let status = fx.status(Some(project.tracks[0].id), &project.tracks[0].fx[0], 0).cloned();
    let graph = RenderGraph::build(&project, &MediaPool::default(), &mut MeterRegistry::default(), &mut fx, 1000);
    let (mut l, mut r) = (vec![0.0f32; 1500], vec![0.0f32; 1500]);
    graph.render(0, &mut l, &mut r, &mut RenderScratch::new(4));
    let level = 64.0 / 128.0;
    assert_eq!(l[499], 0.0, "{status:?}");
    assert!((l[500] - level).abs() < 1e-6 && (l[999] - level).abs() < 1e-6, "{} {} {status:?}", l[500], l[999]);
    assert_eq!(l[1000], 0.0);
}
