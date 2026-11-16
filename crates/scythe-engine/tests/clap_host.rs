//! Hosts the workspace's own `clap-test-gain` plugin. Skipped when it has not been built
//! (`cargo build -p clap-test-gain`).

use std::env::consts::{DLL_PREFIX, DLL_SUFFIX};
use std::path::PathBuf;

use scythe_engine::clap_host::{ClapLibrary, ClapPlugin};

fn test_plugin() -> Option<PathBuf> {
    let target = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
    ["debug", "release"]
        .iter()
        .map(|p| target.join(p).join(format!("{DLL_PREFIX}clap_test_gain{DLL_SUFFIX}")))
        .find(|p| p.exists())
}

#[test]
fn hosts_scans_processes_and_restores_state() {
    let Some(path) = test_plugin() else {
        eprintln!("clap-test-gain not built; skipping");
        return;
    };
    let lib = ClapLibrary::load(&path).unwrap();
    let descs = lib.descriptors();
    assert_eq!(descs.len(), 4);
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
    assert_eq!(params.len(), 2);
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
fn keeps_one_library_per_file_for_the_whole_process() {
    let Some(path) = test_plugin() else {
        eprintln!("clap-test-gain not built; skipping");
        return;
    };
    let first = ClapLibrary::load(&path).unwrap();
    let again = ClapLibrary::load(&path.parent().unwrap().join(".").join(path.file_name().unwrap())).unwrap();
    assert!(std::sync::Arc::ptr_eq(&first, &again));

    drop(ClapPlugin::create(first, "org.scythe.test-gain", 48000.0, 512).unwrap());
    drop(again);
    let reloaded = ClapLibrary::load(&path).unwrap();
    assert!(ClapPlugin::create(reloaded, "org.scythe.test-gain", 48000.0, 512).is_ok());
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

#[test]
fn a_parameter_set_while_idle_reads_back_and_saves_at_once() {
    let Some(path) = test_plugin() else {
        eprintln!("clap-test-gain not built; skipping");
        return;
    };
    let lib = ClapLibrary::load(&path).unwrap();
    let mut plugin = ClapPlugin::create(lib, "org.scythe.test-gain", 48000.0, 512).unwrap();
    plugin.set_param(7, 0.75);
    assert_eq!(plugin.param_value(7), Some(0.75));
    assert_eq!(plugin.save_state().unwrap(), 0.75f32.to_le_bytes());
    let (mut l, mut r) = (vec![1.0f32; 4], vec![1.0f32; 4]);
    plugin.process(&mut l, &mut r);
    assert_eq!(l[0], 0.75);

    plugin.set_param(7, 0.25);
    assert_eq!(plugin.param_value(7), Some(0.25), "a running instance between blocks takes it at once too");
}

#[test]
fn a_render_after_a_parameter_change_plays_the_new_value_without_a_save() {
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

    let mut live = FxRegistry::new(48000);
    live.set_clap_plugins(descs.clone());
    let chain = live.chain(Some(id), &project.tracks[0].fx);
    chain[0].with(|e| e.set_param(7, 0.3));
    live.store_states(&mut project);

    let mut render = FxRegistry::new(48000);
    render.set_clap_plugins(descs);
    let chain = render.chain(Some(id), &project.tracks[0].fx);
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
    assert!((l[0] - 0.3).abs() < 1e-6, "{}", l[0]);
}

/// LSP's plugins, when installed: one write to an idle instance reads back.
#[test]
fn lsp_parameters_take_the_first_write() {
    let Some(path) = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".clap/lsp-plugins.clap")) else { return };
    if !path.exists() {
        eprintln!("LSP plugins not installed; skipping");
        return;
    }
    let lib = ClapLibrary::load(&path).unwrap();
    let ids = ["in.lsp-plug.limiter_stereo", "in.lsp-plug.clipper_stereo", "in.lsp-plug.filter_stereo"];
    let descs: Vec<_> = lib.descriptors().into_iter().filter(|d| ids.contains(&d.id.as_str())).collect();
    assert_eq!(descs.len(), ids.len());
    for desc in descs {
        let mut plugin = ClapPlugin::create(lib.clone(), &desc.id, 48000.0, 512).unwrap();
        let p = plugin.params().into_iter().find(|p| p.max > p.min && p.name.contains("put gain")).unwrap();
        let wanted = p.min + (p.max - p.min) * 0.25;
        for _ in 0..3 {
            let (mut l, mut r) = (vec![0.0f32; 64], vec![0.0f32; 64]);
            plugin.process(&mut l, &mut r);
        }
        plugin.set_param(p.id, wanted);
        let got = plugin.param_value(p.id).unwrap();
        assert!((got - wanted).abs() < 1e-4, "{} {}: {got}, wanted {wanted}", desc.id, p.name);
        let saved = plugin.save_state().unwrap();
        let mut fresh = ClapPlugin::create(lib.clone(), &desc.id, 48000.0, 512).unwrap();
        assert!(fresh.load_state(&saved));
        let restored = fresh.param_value(p.id).unwrap();
        assert!((restored - wanted).abs() < 1e-4, "{} {} restored: {restored}", desc.id, p.name);
    }
}

#[test]
fn port_configurations_end_on_stereo_with_the_sidechain() {
    let Some(path) = test_plugin() else {
        eprintln!("clap-test-gain not built; skipping");
        return;
    };
    let lib = ClapLibrary::load(&path).unwrap();
    let stereo = ClapPlugin::create(lib.clone(), "org.scythe.test-gain", 48000.0, 512).unwrap();
    assert_eq!(
        stereo.pins(),
        (4, 2),
        "a plugin starting on stereo keeps its sidechain, not the plain stereo listed first"
    );
    let mono = ClapPlugin::create(lib, "org.scythe.test-gain-mono", 48000.0, 512).unwrap();
    assert_eq!(mono.pins(), (4, 2), "a mono input moves to stereo, keeping the sidechain");
}

/// Renders one second of ones at 1 kHz through the test gain slot `fx` (the lines after its
/// `FXID`) of a project REAPER wrote.
fn render_ones_through_gain(path: &std::path::Path, fx: &str) -> Vec<f32> {
    use scythe_core::rpp::project_from_rpp;
    use scythe_engine::MediaPool;
    use scythe_engine::fx::FxRegistry;
    use scythe_engine::render::{MeterRegistry, RenderGraph, RenderScratch};

    let descs = ClapLibrary::load(path).unwrap().descriptors();
    let dir = std::env::temp_dir().join(format!("scythe-clap-units-{}-{}", std::process::id(), fx.len()));
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
    let rpp = format!(
        "<REAPER_PROJECT 0.1 \"7.73/linux-x86_64\" 0 0\n  <TRACK\n    <FXCHAIN\n      BYPASS 0 0 0\n      <CLAP \"CLAP: Test Gain (Scythe)\" org.scythe.test-gain \"\"\n        CFG 4 0 0 \"\"\n      >\n      FXID {{44444444-2222-3333-4444-555555555555}}\n{fx}      WAK 0 0\n    >\n    <ITEM\n      POSITION 0\n      LENGTH 1\n      FADEIN 1 0 0 1 0 0 0\n      FADEOUT 1 0 0 1 0 0 0\n      <SOURCE WAVE\n        FILE \"{}\"\n      >\n    >\n  >\n>\n",
        ones.display()
    );
    let project = project_from_rpp(&rpp).unwrap();
    let pool = MediaPool::default();
    pool.load(&ones, 1000).unwrap();
    let mut registry = FxRegistry::new(1000);
    registry.set_clap_plugins(descs);
    let graph = RenderGraph::build(&project, &pool, &mut MeterRegistry::default(), &mut registry, 1000);
    let (mut l, mut r) = (vec![0.0f32; 1000], vec![0.0f32; 1000]);
    graph.render(0, &mut l, &mut r, &mut RenderScratch::new(4));
    std::fs::remove_dir_all(&dir).ok();
    l
}

#[test]
fn a_clap_parameter_counts_in_its_own_units_as_reaper_automates_it() {
    let Some(path) = test_plugin() else {
        eprintln!("clap-test-gain not built; skipping");
        return;
    };
    let lib = ClapLibrary::load(&path).unwrap();
    let mut registry = scythe_engine::fx::FxRegistry::new(48_000);
    registry.set_clap_plugins(lib.descriptors());
    let slot = scythe_engine::fx::clap_slot(&lib.descriptors()[0]);
    let chain = registry.chain(None, std::slice::from_ref(&slot));
    let (range, base) = chain[0]
        .with(|e| {
            e.set_param(9, 2.0);
            (e.param_range(1), scythe_engine::modulation::normalized(e, 1))
        })
        .unwrap();
    assert_eq!(range, Some((0.0, 4.0)), "the boost's own range");
    assert_eq!(base, Some(0.5));
    chain[0].with(|e| assert!(e.set_normalized(1, 0.75))).unwrap();
    assert_eq!(chain[0].with(|e| e.param(9)).flatten().map(|(v, _)| v), Some(3.0), "linear over the range");
    assert_eq!(
        scythe_engine::fx::envelope_spec(&slot, chain[0].with(|e| e.params()).as_deref(), 1).as_deref(),
        Some("1:9"),
        "REAPER names a CLAP parameter by index and id"
    );

    // As REAPER 7.73 wrote it: the boost from 0 to 4 over the second, read at each 512-frame
    // block's last frame.
    let l = render_ones_through_gain(
        &path,
        "      <PARMENV 1:9 0 4 2 \"Boost / Test Gain\"\n        ACT 1 -1\n        PT 0 0 0\n        PT 1 4 0\n      >\n",
    );
    for (t, block_last) in [(100, 511), (600, 999)] {
        let want = 0.5 * 4.0 * block_last as f32 / 1000.0;
        assert!((l[t] - want).abs() < 1e-3, "frame {t}: {} for {want}", l[t]);
    }

    // A modulation baseline in the boost's units: 2 plays 2.
    let l = render_ones_through_gain(
        &path,
        "      <PROGRAMENV 1:9 0 \"Boost / Test Gain\"\n        PARAMBASE 2\n        LFO 1\n        LFOWT 0 1\n        AUDIOCTL 0\n        AUDIOCTLWT 1 1\n        LFOSHAPE 0\n        LFOSYNC 0 0 0\n        LFOSPEED 0.124573 0\n      >\n",
    );
    assert!((l[900] - 1.0).abs() < 1e-3, "baseline 2 plays {}", l[900] / 0.5);
}

/// A project rendered straight from a file converts as opening it does: an old Scythe file's
/// baseline on the boost, in an offline slot whose plugin runs nowhere, moves to the boost's units.
#[test]
fn a_project_read_to_render_converts_old_clap_baselines() {
    let Some(path) = test_plugin() else {
        eprintln!("clap-test-gain not built; skipping");
        return;
    };
    let text = "<REAPER_PROJECT 0.1 scythe-0.1.0 0\n  <TRACK\n    <FXCHAIN\n      BYPASS 0 1 0\n      <CLAP \"CLAP: Test Gain (Scythe)\" org.scythe.test-gain \"\"\n        CFG 4 0 0 \"\"\n        <STATE\n          AACAPg==\n        >\n      >\n      FXID {11111111-2222-3333-4444-555555555555}\n      <PROGRAMENV 1 0 \"Boost\"\n        PARAMBASE 0.5\n        LFO 1\n        LFOWT 0.1 1\n      >\n      WAK 0 0\n    >\n  >\n>\n";
    let mut project = scythe_core::rpp::project_from_rpp(text).unwrap();
    let mut fx = scythe_engine::fx::FxRegistry::new(48_000);
    fx.set_clap_plugins(ClapLibrary::load(&path).unwrap().descriptors());
    assert!(scythe_engine::upgrade::upgrade_read(&mut project, &fx));
    let m = scythe_core::modulation::modulation(&project.tracks[0].fx[0], 1).unwrap();
    assert_eq!(m.base, 2.0);
}

/// CLAP wants a parameter's value and text asked on the plugin's main thread: for Scythe the
/// plugin thread hosted plugins are made on. Scythe's window, its control API and scripts ask
/// from their own threads, modulation from the audio thread.
#[test]
fn parameters_are_asked_on_the_plugin_thread_only() {
    let Some(path) = test_plugin() else {
        eprintln!("clap-test-gain not built; skipping");
        return;
    };
    let lib = ClapLibrary::load(&path).unwrap();
    let off_main = || {
        // SAFETY: the test plugin's own counter, a plain function without arguments.
        unsafe {
            let lib = libloading::Library::new(&path).unwrap();
            lib.get::<unsafe extern "C" fn() -> u32>(b"clap_test_gain_off_main_param_calls").unwrap()()
        }
    };
    let mut registry = scythe_engine::fx::FxRegistry::new(48_000);
    registry.set_clap_plugins(lib.descriptors());
    let slot = scythe_engine::fx::clap_slot(&lib.descriptors()[0]);
    let chain = registry.chain(None, std::slice::from_ref(&slot));
    let before = off_main();
    let read = chain[0]
        .with(|e| {
            e.set_param(9, 3.0);
            (e.param(7), e.normalized(1), scythe_engine::modulation::normalized(e, 0))
        })
        .unwrap();
    let listed = chain[0].param_values(&[9, 99]);
    let inst = chain[0].clone();
    let elsewhere = std::thread::spawn(move || inst.param_values(&[7])).join().unwrap();
    assert_eq!(off_main() - before, 0, "asked off the plugin thread");
    assert_eq!(read, (Some((0.5, "0.50".into())), Some(0.75), Some(0.5)));
    assert_eq!(listed, [Some((3.0, "3.00".into())), None], "a value set and not yet played");
    assert_eq!(elsewhere, [Some((0.5, "0.50".into()))]);
}

/// A parameter read waiting on a busy plugin thread does not keep the audio thread from the
/// plugin: a block processed meanwhile is played, not passed through.
#[test]
fn a_block_plays_while_a_parameter_read_waits_for_the_plugin_thread() {
    let Some(path) = test_plugin() else {
        eprintln!("clap-test-gain not built; skipping");
        return;
    };
    let lib = ClapLibrary::load(&path).unwrap();
    let mut registry = scythe_engine::fx::FxRegistry::new(48_000);
    registry.set_clap_plugins(lib.descriptors());
    let slot = scythe_engine::fx::clap_slot(&lib.descriptors()[0]);
    let chain = registry.chain(None, std::slice::from_ref(&slot));
    let (release, held) = std::sync::mpsc::channel::<()>();
    let (busy, is_busy) = std::sync::mpsc::channel();
    scythe_engine::plugin_thread::spawn(move || {
        busy.send(()).unwrap();
        let _ = held.recv();
    });
    is_busy.recv().unwrap();
    let inst = chain[0].clone();
    let reading = std::thread::spawn(move || inst.with(|e| e.param_values(&[7])).unwrap());
    let inst = chain[0].clone();
    let asking = std::thread::spawn(move || inst.param_values(&[7]));
    std::thread::sleep(std::time::Duration::from_millis(200));
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
    let (mut l, mut r) = (vec![1.0f32; 64], vec![1.0f32; 64]);
    let played = chain[0].process(&mut l, &mut r, &ctx);
    release.send(()).unwrap();
    let (read, asked) = (reading.join().unwrap(), asking.join().unwrap());
    assert!(played, "the block passed through while a read waited");
    assert_eq!(l[0], 0.5);
    assert_eq!(read[0].as_ref().map(|(v, _)| *v), Some(0.5));
    assert_eq!(asked, [Some((0.5, "0.50".into()))]);
}

/// A value the plugin reports while it processes (as a change in its own editor would) is the
/// one Scythe keeps: modulation reading it on the audio thread follows it.
#[test]
fn a_value_the_plugin_reports_is_kept() {
    let Some(path) = test_plugin() else {
        eprintln!("clap-test-gain not built; skipping");
        return;
    };
    let lib = ClapLibrary::load(&path).unwrap();
    let mut registry = scythe_engine::fx::FxRegistry::new(48_000);
    registry.set_clap_plugins(lib.descriptors());
    let slot = scythe_engine::fx::clap_slot(&lib.descriptors()[0]);
    let chain = registry.chain(None, std::slice::from_ref(&slot));
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
    let (mut l, mut r) = (vec![1.0f32; 64], vec![1.0f32; 64]);
    chain[0].with(|e| e.set_param(9, 3.25));
    assert!(chain[0].process(&mut l, &mut r, &ctx));
    // SAFETY: the test plugin's own hook, a plain function taking a number.
    unsafe {
        let lib = libloading::Library::new(&path).unwrap();
        lib.get::<unsafe extern "C" fn(f64)>(b"clap_test_gain_report_gain").unwrap()(0.25);
    }
    assert!(chain[0].process(&mut l, &mut r, &ctx));
    let kept = chain[0].with(|e| (e.normalized(0), e.param(7).map(|(v, _)| v))).unwrap();
    assert_eq!(kept, (Some(0.25), Some(0.25)));
}

/// A read that ends after a newer value was set and played does not bring the older value back.
#[test]
fn a_read_finished_after_a_newer_value_keeps_the_newer_one() {
    let Some(path) = test_plugin() else {
        eprintln!("clap-test-gain not built; skipping");
        return;
    };
    let lib = ClapLibrary::load(&path).unwrap();
    let mut registry = scythe_engine::fx::FxRegistry::new(48_000);
    registry.set_clap_plugins(lib.descriptors());
    let slot = scythe_engine::fx::clap_slot(&lib.descriptors()[0]);
    let chain = registry.chain(None, std::slice::from_ref(&slot));
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
    let reader = chain[0].with(|e| e.param_reader(&[9])).flatten().unwrap();
    let read = scythe_engine::plugin_thread::run(reader.read);
    assert_eq!(read[0].as_ref().map(|(v, _)| *v), Some(1.0));
    chain[0].with(|e| e.set_param(9, 2.0));
    let (mut l, mut r) = (vec![1.0f32; 64], vec![1.0f32; 64]);
    assert!(chain[0].process(&mut l, &mut r, &ctx));
    chain[0].with(|e| e.keep_values(&[9], &read, reader.stamp));
    assert_eq!(chain[0].with(|e| e.normalized(1)).flatten(), Some(0.5), "the value set and played");
}
