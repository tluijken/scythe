//! Hosts the workspace's own `clap-test-gain` plugin. Skipped when it has not been built
//! (`cargo build -p clap-test-gain`).

use std::path::PathBuf;

use scythe_engine::clap_host::{ClapLibrary, ClapPlugin};

fn test_plugin() -> Option<PathBuf> {
    let target = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
    ["debug", "release"].iter().map(|p| target.join(p).join("libclap_test_gain.so")).find(|p| p.exists())
}

#[test]
fn hosts_scans_processes_and_restores_state() {
    let Some(path) = test_plugin() else {
        eprintln!("clap-test-gain not built; skipping");
        return;
    };
    let lib = ClapLibrary::load(&path).unwrap();
    let descs = lib.descriptors();
    assert_eq!(descs.len(), 3);
    assert_eq!(descs[0].id, "org.scythe.test-gain");
    assert!(descs[1].is_instrument() && !descs[0].is_instrument());
    assert_eq!(descs[0].features, ["audio-effect"]);

    let mut plugin = ClapPlugin::create(lib.clone(), "org.scythe.test-gain", 48000.0, 512).unwrap();
    let (mut l, mut r) = (vec![1.0f32; 64], vec![-0.5f32; 64]);
    plugin.process(&mut l, &mut r);
    assert!(l.iter().all(|&s| s == 0.5) && r.iter().all(|&s| s == -0.25));

    let state = plugin.save_state().unwrap();
    assert_eq!(state, 0.5f32.to_le_bytes());
    assert!(plugin.load_state(&0.25f32.to_le_bytes()));
    let (mut l, mut r) = (vec![1.0f32; 8], vec![1.0f32; 8]);
    plugin.process(&mut l, &mut r);
    assert_eq!(l[0], 0.25);

    let params = plugin.params();
    assert_eq!(params.len(), 1);
    assert_eq!((params[0].id, params[0].name.as_str(), params[0].default), (7, "Gain", 0.5));
    assert_eq!(plugin.param_text(7, 0.25).as_deref(), Some("0.25"));
    plugin.set_param(7, 0.75);
    let (mut l, mut r) = (vec![1.0f32; 4], vec![1.0f32; 4]);
    plugin.process(&mut l, &mut r);
    assert_eq!((l[0], plugin.param_value(7)), (0.75, Some(0.75)));
    assert_eq!(plugin.save_state().unwrap(), 0.75f32.to_le_bytes());

    assert!(ClapPlugin::create(lib, "org.other", 48000.0, 512).is_err());
}

#[test]
fn discovers_and_loads_factory_presets() {
    let Some(path) = test_plugin() else {
        eprintln!("clap-test-gain not built; skipping");
        return;
    };
    let lib = ClapLibrary::load(&path).unwrap();
    let mut plugin = ClapPlugin::create(lib.clone(), "org.scythe.test-gain", 48000.0, 512).unwrap();
    let presets = plugin.presets();
    let names: Vec<&str> = presets.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Quiet", "Loud"], "only the provider with the plugin's id is asked");
    assert_eq!(presets[1].load_key.as_deref(), Some("0.75"));
    assert!(plugin.load_preset(&presets[1]));
    assert_eq!(plugin.save_state().unwrap(), 0.75f32.to_le_bytes());
    assert!(lib.presets("org.scythe.test-synth").is_empty(), "presets name the plugin they are for");
}

#[test]
fn midi_items_play_through_a_clap_instrument() {
    use scythe_core::rpp::project_from_rpp;
    use scythe_engine::MediaPool;
    use scythe_engine::fx::{FxRegistry, clap_slot};
    use scythe_engine::render::{MeterRegistry, RenderGraph, RenderScratch};

    let Some(path) = test_plugin() else {
        eprintln!("clap-test-gain not built; skipping");
        return;
    };
    let descs = ClapLibrary::load(&path).unwrap().descriptors();
    // At 120 bpm and 960 ticks per quarter: key 64 from 0.5 s to 1 s.
    let mut project = project_from_rpp(
        "<REAPER_PROJECT 0.1 \"7.73\" 0\n  TEMPO 120 4 4\n  <TRACK\n    <ITEM\n      POSITION 0\n      LENGTH 2\n      <SOURCE MIDI\n        HASDATA 1 960 QN\n        E 960 90 40 64\n        E 960 80 40 00\n        E 1920 b0 7b 00\n      >\n    >\n  >\n>\n",
    )
    .unwrap();
    project.tracks[0].fx.push(clap_slot(&descs[1]));
    let mut fx = FxRegistry::new(1000);
    fx.set_clap_plugins(descs);
    let graph = RenderGraph::build(&project, &MediaPool::default(), &mut MeterRegistry::default(), &mut fx, 1000);
    let (mut l, mut r) = (vec![0.0f32; 1500], vec![0.0f32; 1500]);
    graph.render(0, &mut l, &mut r, &mut RenderScratch::new(4));
    let level = 64.0 / 128.0;
    assert_eq!(l[499], 0.0, "the preview synth must stay quiet when an instrument is present");
    assert!((l[500] - level).abs() < 1e-6 && (l[999] - level).abs() < 1e-6);
    assert_eq!(l[1000], 0.0);
}

#[test]
fn plugin_state_survives_save_and_load() {
    use scythe_core::rpp::{project_from_rpp, project_to_rpp};
    use scythe_core::{Project, Track};
    use scythe_engine::fx::{FxRegistry, clap_slot};

    let Some(path) = test_plugin() else {
        eprintln!("clap-test-gain not built; skipping");
        return;
    };
    let descs = ClapLibrary::load(&path).unwrap().descriptors();
    let mut project = Project::default();
    let mut track = Track::new("t");
    track.fx.push(clap_slot(&descs[0]));
    let id = track.id;
    project.tracks.push(track);

    let mut fx = FxRegistry::new(48000);
    fx.set_clap_plugins(descs.clone());
    let chain = fx.chain(Some(id), &project.tracks[0].fx);
    assert_eq!(chain.len(), 1);
    chain[0].with(|e| e.set_param(7, 0.3));
    let (mut l, mut r) = (vec![1.0f32; 8], vec![1.0f32; 8]);
    let ctx = scythe_engine::fx::ProcessContext {
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
    chain[0].process(&mut l, &mut r, &ctx);
    fx.store_states(&mut project);
    // Storing must not make the registry think the slot changed.
    assert!(std::sync::Arc::ptr_eq(&fx.chain(Some(id), &project.tracks[0].fx)[0], &chain[0]));

    let reloaded = project_from_rpp(&project_to_rpp(&project)).unwrap();
    let mut fresh = FxRegistry::new(48000);
    fresh.set_clap_plugins(descs);
    let chain = fresh.chain(Some(reloaded.tracks[0].id), &reloaded.tracks[0].fx);
    let value = chain[0].with(|e| e.param(7)).flatten().map(|(v, _)| v);
    assert!((value.unwrap() - 0.3).abs() < 1e-6, "{value:?}");
}

#[test]
fn plugin_latency_is_compensated() {
    use scythe_core::{Item, Project, Send, Take};
    use scythe_engine::MediaPool;
    use scythe_engine::fx::{FxRegistry, clap_slot};
    use scythe_engine::render::{MeterRegistry, RenderGraph, RenderScratch};

    let Some(path) = test_plugin() else {
        eprintln!("clap-test-gain not built; skipping");
        return;
    };
    let descs = ClapLibrary::load(&path).unwrap().descriptors();
    let delay = descs.iter().find(|d| d.id == "org.scythe.test-delay").unwrap();

    let dir = std::env::temp_dir().join(format!("scythe-pdc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let impulse = dir.join("impulse.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 1000,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(&impulse, spec).unwrap();
    for i in 0..300 {
        w.write_sample(if i == 100 { 1.0f32 } else { 0.0 }).unwrap();
    }
    w.finalize().unwrap();

    // A plays through the 64-frame delay; B plays dry and also sends to A; C plays dry.
    let mut project = Project::default();
    let ids: Vec<_> = ["a", "b", "c"].into_iter().map(|n| project.add_track(n)).collect();
    for &id in &ids {
        let item = Item::new(0.4, 0.3, Take::from_file(impulse.clone()));
        let t = project.track_mut(id).unwrap();
        t.items.push(Item { fade_in: scythe_core::Fade::default(), fade_out: scythe_core::Fade::default(), ..item });
    }
    project.track_mut(ids[0]).unwrap().fx.push(clap_slot(delay));
    project.track_mut(ids[1]).unwrap().sends.push(Send { volume: 1.0, ..Send::new(ids[0]) });

    let pool = MediaPool::default();
    pool.load(&impulse, 1000).unwrap();
    let mut fx = FxRegistry::new(1000);
    fx.set_clap_plugins(descs.clone());
    let graph = RenderGraph::build(&project, &pool, &mut MeterRegistry::default(), &mut fx, 1000);
    let (mut l, mut r) = (vec![0.0f32; 1000], vec![0.0f32; 1000]);
    graph.render(0, &mut l, &mut r, &mut RenderScratch::new(4));
    let hits: Vec<(usize, f32)> = l.iter().copied().enumerate().filter(|(_, v)| v.abs() > 1e-6).collect();
    // A, B, C and B's send all land on the impulse's own frame.
    assert_eq!(hits, [(500, 4.0)]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn parameter_envelopes_drive_plugins_by_index() {
    use scythe_core::rpp::project_from_rpp;
    use scythe_engine::MediaPool;
    use scythe_engine::fx::FxRegistry;
    use scythe_engine::render::{MeterRegistry, RenderGraph, RenderScratch};

    let Some(path) = test_plugin() else {
        eprintln!("clap-test-gain not built; skipping");
        return;
    };
    let descs = ClapLibrary::load(&path).unwrap().descriptors();
    let dir = std::env::temp_dir().join(format!("scythe-parmenv-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ones = dir.join("ones.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 1000,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(&ones, spec).unwrap();
    for _ in 0..1000 {
        w.write_sample(1.0f32).unwrap();
    }
    w.finalize().unwrap();
    // The gain parameter (index 0, plain range 0-1) ramps from 0 to 1 over the second.
    let rpp = format!(
        "<REAPER_PROJECT 0.1 \"7.73\" 0\n  <TRACK\n    <FXCHAIN\n      BYPASS 0 0 0\n      <CLAP \"CLAP: Test Gain (Scythe)\" org.scythe.test-gain \"\"\n        CFG 4 0 0 \"\"\n      >\n      FXID {{44444444-2222-3333-4444-555555555555}}\n      <PARMENV 0 0 1 0.5 \"Gain\"\n        ACT 1 -1\n        PT 0 0 0\n        PT 1 1 0\n      >\n      WAK 0 0\n    >\n    <ITEM\n      POSITION 0\n      LENGTH 1\n      FADEIN 1 0 0 1 0 0 0\n      FADEOUT 1 0 0 1 0 0 0\n      <SOURCE WAVE\n        FILE \"{}\"\n      >\n    >\n  >\n>\n",
        ones.display()
    );
    let project = project_from_rpp(&rpp).unwrap();
    let pool = MediaPool::default();
    pool.load(&ones, 1000).unwrap();
    let mut fx = FxRegistry::new(1000);
    fx.set_clap_plugins(descs);
    let graph = RenderGraph::build(&project, &pool, &mut MeterRegistry::default(), &mut fx, 1000);
    let (mut l, mut r) = (vec![0.0f32; 1000], vec![0.0f32; 1000]);
    graph.render(0, &mut l, &mut r, &mut RenderScratch::new(4));
    // As REAPER automates hosted plugins: the envelope value at each 512-frame block's last
    // frame (the second block ends with the render at frame 999).
    for (t, block_last) in [(100, 511), (400, 511), (600, 999), (900, 999)] {
        assert!((l[t] - block_last as f32 / 1000.0).abs() < 1e-3, "frame {t}: {}", l[t]);
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn sidechain_port_takes_channels_three_and_four() {
    let Some(path) = test_plugin() else {
        eprintln!("clap-test-gain not built; skipping");
        return;
    };
    let lib = ClapLibrary::load(&path).unwrap();
    let mut plugin = ClapPlugin::create(lib, "org.scythe.test-gain", 48000.0, 512).unwrap();
    assert_eq!(plugin.pins(), (4, 2), "main and sidechain ports in, main port out");
    let mut bufs = vec![vec![1.0f32; 16], vec![-1.0; 16], vec![0.25; 16], vec![0.5; 16]];
    plugin.process_channels(&mut bufs, 4..12);
    assert_eq!((bufs[0][4], bufs[1][11]), (0.75, 0.0), "main at the default gain of 0.5 plus the sidechain");
    assert_eq!(bufs[0][0], 1.0, "frames outside the range stay");
    // The stereo path feeds the sidechain silence.
    let (mut l, mut r) = (vec![1.0f32; 8], vec![1.0f32; 8]);
    plugin.process(&mut l, &mut r);
    assert_eq!(l[0], 0.5);
}
