//! Hosts the workspace's own `lv2-test-plugins`. Skipped when they have not been built
//! (`cargo build -p lv2-test-plugins`).

use std::path::{Path, PathBuf};

use scythe_core::rpp::{project_from_rpp, project_to_rpp};
use scythe_engine::MediaPool;
use scythe_engine::fx::FxRegistry;
use scythe_engine::lv2::{self, PortKind};
use scythe_engine::render::{MeterRegistry, RenderGraph, RenderScratch};

const PLUGINS_TTL: &str = r#"
@prefix lv2:  <http://lv2plug.in/ns/lv2core#> .
@prefix atom: <http://lv2plug.in/ns/ext/atom#> .
@prefix doap: <http://usefulinc.com/ns/doap#> .
@prefix midi: <http://lv2plug.in/ns/ext/midi#> .
@prefix ui:   <http://lv2plug.in/ns/extensions/ui#> .

<urn:scythe:test-amp>
    a lv2:Plugin ;
    ui:ui <urn:scythe:test-amp#ui> ;
    doap:name "Test Amp" ;
    lv2:port [ a lv2:AudioPort , lv2:InputPort ; lv2:index 0 ; lv2:symbol "in_l" ; lv2:name "In L" ] ,
             [ a lv2:AudioPort , lv2:InputPort ; lv2:index 1 ; lv2:symbol "in_r" ; lv2:name "In R" ] ,
             [ a lv2:AudioPort , lv2:OutputPort ; lv2:index 2 ; lv2:symbol "out_l" ; lv2:name "Out L" ] ,
             [ a lv2:AudioPort , lv2:OutputPort ; lv2:index 3 ; lv2:symbol "out_r" ; lv2:name "Out R" ] ,
             [ a lv2:ControlPort , lv2:InputPort ; lv2:index 4 ; lv2:symbol "gain" ; lv2:name "Gain" ;
               lv2:default 0.5 ; lv2:minimum 0.0 ; lv2:maximum 1.0 ] ,
             [ a lv2:ControlPort , lv2:OutputPort ; lv2:index 5 ; lv2:symbol "label_len" ; lv2:name "Label length" ] .

<urn:scythe:test-amp#ui> a ui:X11UI ; ui:binary <test%20plugins.so> .

<urn:scythe:test-synth>
    a lv2:Plugin , lv2:InstrumentPlugin ;
    doap:name "Test Synth" ;
    lv2:port [ a atom:AtomPort , lv2:InputPort ; atom:bufferType atom:Sequence ; atom:supports midi:MidiEvent ;
               lv2:index 0 ; lv2:symbol "midi" ; lv2:name "MIDI" ] ,
             [ a lv2:AudioPort , lv2:OutputPort ; lv2:index 1 ; lv2:symbol "out_l" ; lv2:name "Out L" ] ,
             [ a lv2:AudioPort , lv2:OutputPort ; lv2:index 2 ; lv2:symbol "out_r" ; lv2:name "Out R" ] .
"#;

/// Writes an LV2 bundle around the built test library, or `None` when it is not built.
fn bundle(name: &str) -> Option<PathBuf> {
    let target = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
    let lib =
        ["debug", "release"].iter().map(|p| target.join(p).join("liblv2_test_plugins.so")).find(|p| p.exists())?;
    let root = std::env::temp_dir().join(format!("scythe-lv2-{name}-{}", std::process::id()));
    let dir = root.join("scythe-test.lv2");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(lib, dir.join("test plugins.so")).unwrap();
    let manifest = r#"
@prefix lv2:  <http://lv2plug.in/ns/lv2core#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<urn:scythe:test-amp> a lv2:Plugin ; lv2:binary <test%20plugins.so> ; rdfs:seeAlso <plugins.ttl> .
<urn:scythe:test-synth> a lv2:Plugin ; lv2:binary <test%20plugins.so> ; rdfs:seeAlso <plugins.ttl> .
"#;
    std::fs::write(dir.join("manifest.ttl"), manifest).unwrap();
    std::fs::write(dir.join("plugins.ttl"), PLUGINS_TTL).unwrap();
    Some(root)
}

fn registry(root: &Path, rate: u32) -> FxRegistry {
    let mut fx = FxRegistry::new(rate);
    fx.set_lv2_plugins(lv2::scan(&[root.to_owned()]));
    fx
}

#[test]
fn scans_bundles() {
    let Some(root) = bundle("scan") else {
        eprintln!("lv2-test-plugins not built; skipping");
        return;
    };
    let plugins = lv2::scan(std::slice::from_ref(&root));
    assert_eq!(plugins.len(), 2);
    let amp = plugins.iter().find(|p| p.uri == "urn:scythe:test-amp").unwrap();
    assert_eq!(amp.name, "Test Amp");
    assert!(amp.binary.ends_with("scythe-test.lv2/test plugins.so"));
    assert_eq!(amp.ports.len(), 6);
    let gain = &amp.ports[4];
    assert_eq!((gain.kind, gain.input, gain.symbol.as_str(), gain.default), (PortKind::Control, true, "gain", 0.5));
    assert!(!amp.instrument);
    let synth = plugins.iter().find(|p| p.uri == "urn:scythe:test-synth").unwrap();
    assert!(synth.instrument && synth.ports[0].midi);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn plays_controls_and_state_from_a_reaper_chunk() {
    let Some(root) = bundle("state") else {
        eprintln!("lv2-test-plugins not built; skipping");
        return;
    };
    // The state string is "hello\nworld", split over rows the way REAPER does.
    let rpp = r#"<REAPER_PROJECT 0.1 "7.73" 0
  <TRACK
    <FXCHAIN
      BYPASS 0 0 0
      <LV2 "LV2: Test Amp" urn:scythe:test-amp ""
        CFG 0 0 0 ""
        P gain 0.25
        <STATE
          <S urn:scythe:test-amp#label 3
             |hel
             |lo
            n|world
          >
        >
      >
      FXID {11111111-2222-3333-4444-555555555555}
      WAK 0 0
    >
  >
>
"#;
    let mut project = project_from_rpp(rpp).unwrap();
    let mut fx = registry(&root, 1000);
    let id = project.tracks[0].id;
    let chain = fx.chain(Some(id), &project.tracks[0].fx);
    assert_eq!(chain.len(), 1, "{:?}", fx.status(Some(id), &project.tracks[0].fx[0], 0));
    let (mut l, mut r) = (vec![1.0f32; 16], vec![-1.0f32; 16]);
    let ctx = scythe_engine::fx::ProcessContext {
        sample_rate: 1000,
        position: 0,
        playing: true,
        notes: None,
        live: &[],
        live_channel: None,
        bpm: 120.0,
    };
    chain[0].process(&mut l, &mut r, &ctx);
    assert_eq!((l[0], r[15]), (0.25, -0.25));

    chain[0].with(|e| e.set_param(4, 0.75));
    fx.store_states(&mut project);
    let text = project_to_rpp(&project);
    assert!(text.contains("P gain 0.75"), "{text}");
    assert!(text.contains("n|world"), "{text}");

    let reloaded = project_from_rpp(&text).unwrap();
    let mut fresh = registry(&root, 1000);
    let chain = fresh.chain(Some(reloaded.tracks[0].id), &reloaded.tracks[0].fx);
    let (mut l, mut r) = (vec![1.0f32; 4], vec![1.0f32; 4]);
    chain[0].process(&mut l, &mut r, &ctx);
    assert_eq!(l[0], 0.75);
    let saved = chain[0].with(|e| e.save_nodes()).flatten().unwrap();
    let state = saved.iter().find_map(|n| match n {
        scythe_core::rpp::Node::Chunk(c) => Some(lv2::parse_reaper_state(c)),
        _ => None,
    });
    assert_eq!(state.unwrap()[0].value, b"hello\nworld\0");
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn midi_items_play_through_an_lv2_instrument() {
    let Some(root) = bundle("midi") else {
        eprintln!("lv2-test-plugins not built; skipping");
        return;
    };
    // At 120 bpm and 960 ticks per quarter: key 64 from 0.5 s to 1 s.
    let project = project_from_rpp(
        "<REAPER_PROJECT 0.1 \"7.73\" 0\n  TEMPO 120 4 4\n  <TRACK\n    <FXCHAIN\n      BYPASS 0 0 0\n      <LV2 \"LV2i: Test Synth\" urn:scythe:test-synth \"\"\n      >\n      FXID {22222222-2222-3333-4444-555555555555}\n      WAK 0 0\n    >\n    <ITEM\n      POSITION 0\n      LENGTH 2\n      <SOURCE MIDI\n        HASDATA 1 960 QN\n        E 960 90 40 64\n        E 960 80 40 00\n        E 1920 b0 7b 00\n      >\n    >\n  >\n>\n",
    )
    .unwrap();
    let mut fx = registry(&root, 1000);
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
fn opens_the_x11_ui_and_takes_its_control_changes() {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{ConnectionExt as _, CreateWindowAux, WindowClass};
    let Some(root) = bundle("ui") else {
        eprintln!("lv2-test-plugins not built; skipping");
        return;
    };
    let Ok((conn, screen)) = x11rb::connect(None) else {
        eprintln!("no X11 display; skipping");
        return;
    };
    let parent = conn.generate_id().unwrap();
    let screen_root = conn.setup().roots[screen].root;
    conn.create_window(
        0,
        parent,
        screen_root,
        0,
        0,
        640,
        480,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new(),
    )
    .unwrap();
    conn.flush().unwrap();

    let plugins = lv2::scan(std::slice::from_ref(&root));
    let amp = plugins.iter().find(|p| p.uri == "urn:scythe:test-amp").unwrap();
    assert_eq!(amp.ui.as_ref().map(|u| u.0.as_str()), Some("urn:scythe:test-amp#ui"));

    let mut project = scythe_core::Project::default();
    let mut track = scythe_core::Track::new("t");
    track.fx.push(scythe_engine::fx::lv2_slot(amp));
    let id = track.id;
    project.tracks.push(track);
    let mut fx = registry(&root, 1000);
    let chain = fx.chain(Some(id), &project.tracks[0].fx);
    let mut editor = chain[0].with(|e| e.editor()).flatten().expect("the amp has a UI");
    assert_eq!(editor.open(parent).unwrap(), (320, 200));
    assert_eq!(editor.idle(), None);

    let ctx = scythe_engine::fx::ProcessContext {
        sample_rate: 1000,
        position: 0,
        playing: true,
        notes: None,
        live: &[],
        live_channel: None,
        bpm: 120.0,
    };
    let (mut l, mut r) = (vec![1.0f32; 4], vec![1.0f32; 4]);
    chain[0].process(&mut l, &mut r, &ctx);
    assert_eq!(l[0], 0.8, "the UI's control change reached the plugin");
    editor.close();
    conn.destroy_window(parent).unwrap();
    conn.flush().unwrap();
    std::fs::remove_dir_all(root).ok();
}
