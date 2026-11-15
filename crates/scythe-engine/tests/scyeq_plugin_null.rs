//! ScyEQ-Pro built into Scythe and the ScyEQ-Pro plugin hosted by Scythe sound the same to the
//! bit, and the plugin's `process` does not allocate.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::{Path, PathBuf};

use scythe_core::rpp::Node;
use scythe_core::{FxSlot, Item, Project, Send, Take};
use scythe_engine::MediaPool;
use scythe_engine::clap_host::ClapLibrary;
use scythe_engine::fx::{FxRegistry, vst3_slot};
use scythe_engine::render::{MeterRegistry, RenderGraph, RenderScratch};
use scythe_engine::scythe_eq::filters::Shape;
use scythe_engine::scythe_eq::params::{self, BAND_STRIDE, GLOBAL, band, global};
use scythe_engine::{scythe_eq, vst3_host};

#[path = "../../scyeq-plugin/tests/built/library.rs"]
mod library;

struct Counting;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.try_with(Cell::get).unwrap_or(false) {
            COUNT.with(|c| c.set(c.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.try_with(Cell::get).unwrap_or(false) {
            COUNT.with(|c| c.set(c.get() + 1));
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const RATE: u32 = 48_000;
/// Renders start part way into a 32-frame grid cell, so a plugin that does not learn the
/// position from its host places the cells elsewhere.
const START: i64 = 1013;
const FRAMES: usize = 24_000;

/// Every shape on a band of its own, a dynamic band, a spectral band and a band keyed by the
/// sidechain, in phase mode `phase`.
fn state(phase: usize) -> Vec<u8> {
    let mut v = params::defaults();
    let mut set = |b: usize, offset: u32, value: f64| v[b * BAND_STRIDE as usize + offset as usize] = value;
    let shapes = [
        (Shape::Bell, 1000.0, 4.0, 0),
        (Shape::LowShelf, 120.0, -3.0, 0),
        (Shape::HighShelf, 8000.0, 2.5, 0),
        (Shape::LowCut, 40.0, 0.0, 0),
        (Shape::HighCut, 16000.0, 0.0, 0),
        (Shape::Notch, 3000.0, 0.0, 0),
        (Shape::BandPass, 700.0, 0.0, 4),
        (Shape::TiltShelf, 2000.0, 1.5, 0),
        (Shape::FlatTilt, 500.0, -1.0, 0),
        (Shape::AllPass, 1500.0, 0.0, 1),
    ];
    for (b, (shape, freq, gain, placement)) in shapes.into_iter().enumerate() {
        set(b, band::USED, 1.0);
        set(b, band::SHAPE, shape.index() as f64);
        set(b, band::FREQ, freq);
        set(b, band::GAIN, gain);
        set(b, band::Q, 0.9);
        set(b, band::SLOPE, 3.0);
        set(b, band::STEREO, placement as f64);
    }
    for (b, freq, mode, sidechain) in [(10, 2000.0, 1.0, 0.0), (11, 5000.0, 2.0, 0.0), (12, 300.0, 1.0, 1.0)] {
        set(b, band::USED, 1.0);
        set(b, band::SHAPE, Shape::Bell.index() as f64);
        set(b, band::FREQ, freq);
        set(b, band::Q, 2.0);
        set(b, band::DYNAMIC, mode);
        set(b, band::RANGE, -9.0);
        set(b, band::THRESHOLD, -36.0);
        set(b, band::ATTACK, 2.0);
        set(b, band::RELEASE, 50.0);
        set(b, band::SIDECHAIN, sidechain);
    }
    v[(GLOBAL + global::PHASE) as usize] = phase as f64;
    params::encode(&v, &[], params::VERSION)
}

/// A VST 3 slot for `info` holding `state` as REAPER stores it.
fn vst3_slot_with(info: &vst3_host::Vst3Info, state: &[u8]) -> FxSlot {
    let mut slot = vst3_slot(info);
    let cid = info.cid;
    let id = (u32::from_le_bytes([cid[0], cid[1], cid[2], cid[3]])
        ^ u32::from_le_bytes([cid[12], cid[13], cid[14], cid[15]]))
        & 0x7fff_ffff;
    let Some(Node::Chunk(chunk)) = slot.raw.get_mut(1) else { panic!("no plugin chunk") };
    chunk.children.retain(|n| !matches!(n, Node::Line(_)));
    for line in scythe_engine::builtin::vst_lines_with(id, &vst3_host::reaper_state(state, &[]), 0xffff) {
        chunk.line([line]);
    }
    slot
}

fn write_wav(path: &Path, seed: u64, bursts: bool, level: f32) {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    let mut state = seed;
    for i in 0..RATE as usize * 2 {
        let gate = if bursts && (i / 4000) % 2 == 1 { 0.05 } else { 1.0 };
        for _ in 0..2 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            w.write_sample(((state >> 33) as f32 / (1u64 << 31) as f32 - 0.5) * 0.8 * gate * level).unwrap();
        }
    }
    w.finalize().unwrap();
}

struct Media {
    dir: PathBuf,
    main: PathBuf,
    key: PathBuf,
    pool: MediaPool,
}

impl Media {
    fn new() -> Self {
        Self::at("loud", 1.0)
    }

    fn at(name: &str, level: f32) -> Self {
        let dir = std::env::temp_dir().join(format!("scythe-scyeq-null-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (main, key) = (dir.join("main.wav"), dir.join("key.wav"));
        write_wav(&main, 7, false, level);
        write_wav(&key, 11, true, level);
        let pool = MediaPool::default();
        pool.load(&main, RATE).unwrap();
        pool.load(&key, RATE).unwrap();
        Self { dir, main, key, pool }
    }
}

impl Drop for Media {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

/// A 4-channel track playing `main` through `slot`, keyed on its channels 3/4 by a second track.
fn project(media: &Media, slot: Option<FxSlot>, keyed: bool) -> Project {
    let mut p = Project::default();
    let (main, key) = (p.add_track("main"), p.add_track("key"));
    let t = p.track_mut(main).unwrap();
    t.channels = 4;
    t.items.push(Item::new(0.0, 2.0, Take::from_file(media.main.clone())));
    t.fx.extend(slot);
    let k = p.track_mut(key).unwrap();
    k.items.push(Item::new(0.0, 2.0, Take::from_file(media.key.clone())));
    k.main_send = false;
    k.muted = !keyed;
    k.sends.push(Send { dst_channels: 2, ..Send::new(main) });
    p
}

#[derive(Clone, Copy, Debug)]
enum Host {
    BuiltIn,
    Clap,
    Vst3,
}

fn registry(host: Host, library: &Path) -> FxRegistry {
    let mut fx = FxRegistry::new(RATE);
    match host {
        Host::BuiltIn => {}
        Host::Clap => {
            fx.set_clap_plugins(ClapLibrary::load(library).unwrap().descriptors());
            fx.host_scyeq_plugin = true;
        }
        Host::Vst3 => {
            let module = vst3_host::Vst3Module::load(library).unwrap();
            fx.set_vst3_plugins(module.classes("ScyEQ-Pro.vst3"));
            fx.host_scyeq_plugin = true;
        }
    }
    fx
}

fn render(media: &Media, host: Host, state: Option<&[u8]>, block: usize, keyed: bool) -> (Vec<f32>, Vec<f32>) {
    let library = library::library("scyeq_plugin");
    let mut fx = registry(host, &library);
    let slot = state.map(|state| match host {
        Host::BuiltIn | Host::Clap => scythe_eq::slot(Some(state)),
        Host::Vst3 => vst3_slot_with(&fx.vst3_plugins()[0], state),
    });
    let project = project(media, slot, keyed);
    let graph = RenderGraph::build(&project, &media.pool, &mut MeterRegistry::default(), &mut fx, RATE);
    if state.is_some() {
        let status = fx.status(Some(project.tracks[0].id), &project.tracks[0].fx[0], 0);
        let hosted = matches!(status, Some(scythe_engine::fx::FxStatus::Hosted(_)));
        assert_eq!(hosted, !matches!(host, Host::BuiltIn), "{host:?}: {status:?}");
    }
    let (mut l, mut r) = (vec![0.0f32; FRAMES], vec![0.0f32; FRAMES]);
    let mut scratch = RenderScratch::new(4);
    for at in (0..FRAMES).step_by(block) {
        let n = block.min(FRAMES - at);
        graph.render(START + at as i64, &mut l[at..at + n], &mut r[at..at + n], &mut scratch);
    }
    (l, r)
}

/// The plugin hosts to compare with the built-in EQ.
fn hosts() -> &'static [Host] {
    if cfg!(target_os = "macos") {
        // A VST 3 module on macOS loads only from a bundle.
        &[Host::Clap]
    } else {
        &[Host::Clap, Host::Vst3]
    }
}

#[test]
fn built_in_and_plugin_null() {
    let media = Media::new();
    let dry = render(&media, Host::BuiltIn, None, 512, true);
    let hosts = hosts();
    for phase in 0..3 {
        let state = state(phase);
        for block in [64, 512] {
            let built_in = render(&media, Host::BuiltIn, Some(&state), block, true);
            assert!(built_in != dry, "phase {phase}: the EQ changes the sound");
            if block == 512 {
                let unkeyed = render(&media, Host::BuiltIn, Some(&state), block, false);
                assert!(unkeyed != built_in, "phase {phase}: the sidechain keys a band");
            }
            for &host in hosts {
                let plugin = render(&media, host, Some(&state), block, true);
                let worst = built_in
                    .0
                    .iter()
                    .chain(&built_in.1)
                    .zip(plugin.0.iter().chain(&plugin.1))
                    .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
                assert!(plugin == built_in, "phase {phase}, blocks of {block}, {host:?}: differs by up to {worst}");
            }
        }
    }
}

/// A signal so quiet that the EQ's results fall below the smallest normal f32: the plugin
/// flushes them to zero, so the built-in EQ has to as well.
#[test]
fn built_in_and_plugin_null_on_subnormal_results() {
    let media = Media::at("quiet", 1e-36);
    let state = state(0);
    let built_in = render(&media, Host::BuiltIn, Some(&state), 512, true);
    assert!(built_in.0.iter().any(|s| *s != 0.0), "the quiet signal comes through");
    for &host in hosts() {
        let plugin = render(&media, host, Some(&state), 512, true);
        assert!(plugin == built_in, "{host:?}");
    }
}

/// Both hosts give the plugin a stereo main input (CLAP through its port configurations) and
/// its sidechain on the track's channels 3/4.
#[test]
fn hosted_plugin_takes_stereo_and_the_sidechain() {
    let library = library::library("scyeq_plugin");
    let hosts: &[Host] = if cfg!(target_os = "macos") { &[Host::Clap] } else { &[Host::Clap, Host::Vst3] };
    for &host in hosts {
        let mut fx = registry(host, &library);
        let slot = match host {
            Host::Vst3 => vst3_slot_with(&fx.vst3_plugins()[0], &state(0)),
            _ => scythe_eq::slot(Some(&state(0))),
        };
        let chain = fx.chain(None, &[slot]);
        assert_eq!(chain[0].pins, (4, 2), "{host:?}");
    }
}

#[test]
fn process_does_not_allocate() {
    use scyeq_plugin::ScyEqPlugin;
    use scyeq_plugin::plugin_format::{Block, Plugin, Shared};

    let block = 512;
    let main = [noise(block * 60, 3), noise(block * 60, 4)];
    let key = [noise(block * 60, 5), noise(block * 60, 6)];
    for phase in 0..3 {
        let (mut plugin, shared) = ScyEqPlugin::new();
        assert!(shared.load(&state(phase)));
        let _watch = shared.probe().watch();
        plugin.activate(RATE as f64, block, 2);
        let mut run = |at: usize| {
            let (mut l, mut r) = (main[0][at..at + block].to_vec(), main[1][at..at + block].to_vec());
            let mut channels: [&mut [f32]; 2] = [&mut l, &mut r];
            let sidechain = Some([&key[0][at..at + block], &key[1][at..at + block]]);
            COUNTING.with(|c| c.set(true));
            plugin.process(Block { channels: &mut channels, sidechain, position: Some(at as i64), playing: true });
            COUNTING.with(|c| c.set(false));
        };
        // A first block lets the EQ grow what it keeps between blocks.
        run(0);
        COUNT.with(|c| c.set(0));
        for at in (block..block * 60).step_by(block) {
            run(at);
        }
        assert_eq!(COUNT.with(Cell::get), 0, "phase {phase}: allocated while processing");
        assert!(shared.probe().take(scythe_engine::probe::OUTPUT) > 0.0, "the probe was fed");
    }
}

fn noise(frames: usize, seed: u64) -> Vec<f32> {
    let mut state = seed;
    (0..frames)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((state >> 33) as f32 / (1u64 << 31) as f32 - 0.5) * 0.8
        })
        .collect()
}

/// Every value each automatable parameter (and solo and audition) can take in turn, from the
/// rich state with all bands in use, then back: each band shape, slope, stereo placement and
/// dynamic mode once. Phase mode and, in linear phase, its resolution change the FIR length:
/// they build a new FIR path.
fn automation(phase: usize) -> (Vec<f64>, Vec<(u32, f64)>) {
    use scythe_engine::scythe_eq::params::Unit;
    let mut values = params::decode(&state(phase)).unwrap().values;
    for b in 0..params::BANDS {
        values[params::band_param(b, band::USED) as usize] = 1.0;
    }
    let solo = (0..params::BANDS).flat_map(|b| [band::SOLO, band::AUDITION].map(|o| params::band_param(b, o)));
    let mut changes = Vec::new();
    for id in params::automatable().chain(solo) {
        let structural = id == GLOBAL + global::PHASE || (phase == 2 && id == GLOBAL + global::RESOLUTION);
        if structural {
            continue;
        }
        let spec = params::spec_of(id).unwrap();
        let v = values[id as usize];
        match spec.unit {
            Unit::Switch | Unit::Choice(_) | Unit::Shape | Unit::Slope => {
                for k in spec.min as usize..=spec.max as usize {
                    if k as f64 != v {
                        changes.push((id, k as f64));
                    }
                }
            }
            _ => {
                let n = params::to_normalized(id, v);
                changes.push((id, params::from_normalized(id, if n < 0.5 { n + 0.37 } else { n - 0.37 })));
            }
        }
        changes.push((id, v));
    }
    (values, changes)
}

#[test]
fn automation_does_not_allocate() {
    use scyeq_plugin::ScyEqPlugin;
    use scyeq_plugin::plugin_format::{Block, Plugin, Shared};
    use scythe_engine::scythe_eq::ScytheEq;

    let block = 256;
    let blocks = 64;
    let main = [noise(block * blocks, 3), noise(block * blocks, 4)];
    let key = [noise(block * blocks, 5), noise(block * blocks, 6)];
    let counted = |f: &mut dyn FnMut()| {
        COUNT.with(|c| c.set(0));
        COUNTING.with(|c| c.set(true));
        f();
        COUNTING.with(|c| c.set(false));
        COUNT.with(Cell::get)
    };
    for phase in 0..3 {
        let (values, changes) = automation(phase);
        let mut eq = ScytheEq::with_values(values.clone(), RATE);
        let (mut plugin, shared) = ScyEqPlugin::new();
        assert!(shared.load(&params::encode(&values, &[], params::VERSION)));
        plugin.activate(RATE as f64, block, 2);
        let mut failed = Vec::new();
        for (i, change) in std::iter::once(None).chain(changes.iter().copied().map(Some)).enumerate() {
            let at = i % blocks * block;
            let input = |c: usize| main[c][at..at + block].to_vec();
            let sidechain = [&key[0][at..at + block], &key[1][at..at + block]];
            let position = Some((i * block) as i64);
            let (mut l, mut r) = (input(0), input(1));
            let built_in = counted(&mut || {
                if let Some((id, v)) = change {
                    eq.set(id, v);
                }
                eq.process_block(&mut l, &mut r, Some((sidechain[0], sidechain[1])), position);
            });
            if let Some((id, v)) = change {
                shared.set_param(id, v);
            }
            let (mut l, mut r) = (input(0), input(1));
            let hosted = counted(&mut || {
                let mut channels: [&mut [f32]; 2] = [&mut l, &mut r];
                plugin.process(Block { channels: &mut channels, sidechain: Some(sidechain), position, playing: true });
            });
            // The first block lets the EQ grow what it keeps between blocks.
            if let Some((id, v)) = change
                && built_in + hosted > 0
            {
                failed.push(format!("{} = {v}: {built_in} built in, {hosted} hosted", params::name(id)));
            }
        }
        assert!(failed.is_empty(), "phase {phase}: allocated on\n{}", failed.join("\n"));
    }
}
