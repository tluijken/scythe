//! Publishing to an open editor's probe must not allocate on the audio thread: the stock
//! effects that measure run block after block with their probe watched while a counting
//! allocator looks on.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use scythe_engine::fx::{FxInstance, ProcessContext};
use scythe_engine::probe::{self, Side, Spectrum};
use scythe_engine::stock::{StockContext, by_name};

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

fn marked(params: &[f32]) -> Vec<u8> {
    let mut d = Vec::new();
    d.extend(0xdead_beef_u32.to_le_bytes());
    d.extend(0xdead_f00d_u32.to_le_bytes());
    for p in params {
        d.extend(p.to_le_bytes());
    }
    d
}

fn reaxcomp() -> Vec<u8> {
    let mut c = Vec::new();
    c.extend(56u32.to_le_bytes());
    c.extend(3u32.to_le_bytes());
    for top in [300.0f64, 3000.0, 24000.0] {
        for v in [top, 1.0, 0.05, 4.0, 6.0] {
            c.extend(v.to_le_bytes());
        }
        for v in [5i32, 100, 5, 1] {
            c.extend(v.to_le_bytes());
        }
    }
    c.extend(1u32.to_le_bytes());
    c.extend(1u32.to_le_bytes());
    for v in [1.0f64, 0.0, 0.0] {
        c.extend(v.to_le_bytes());
    }
    c.extend(2u32.to_le_bytes());
    c
}

fn realimit() -> Vec<u8> {
    let mut c = vec![0u8; 48];
    c[0..4].copy_from_slice(&3u32.to_le_bytes());
    c[8..16].copy_from_slice(&(-12.0f64).to_le_bytes());
    c[16..24].copy_from_slice(&(-1.0f64).to_le_bytes());
    c[40..48].copy_from_slice(&10f64.powf(-15.0 / 20.0).to_le_bytes());
    c
}

fn reaeq() -> Vec<u8> {
    let mut c = Vec::new();
    c.extend(10u32.to_le_bytes());
    c.extend(1u32.to_le_bytes());
    c.extend(8u32.to_le_bytes());
    c.extend(1u32.to_le_bytes());
    for v in [1000.0f64, 2.0, 1.0] {
        c.extend(v.to_le_bytes());
    }
    c.push(1);
    c.extend([0u8; 16]);
    c
}

#[test]
fn watched_effects_publish_without_allocating() {
    let mut comp = vec![0.0f32; 21];
    (comp[0], comp[1], comp[2], comp[3], comp[11], comp[13]) = (0.1, 0.0303, 0.02, 0.02, 1.0, 0.05);
    let mut gate = vec![0.0f32; 21];
    (gate[0], gate[1], gate[2], gate[10], gate[12], gate[14]) = (0.1, 0.02, 0.02, 1.0, 1.0, 0.05);
    let cases = [
        ("ReaComp", marked(&comp)),
        ("ReaGate", marked(&gate)),
        ("ReaXcomp", reaxcomp()),
        ("ReaLimit", realimit()),
        ("ReaEQ", reaeq()),
    ];
    let cx = StockContext { sample_rate: 48000, project_dir: None, hardware: None };
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
    for (name, chunk) in cases {
        let fx = (by_name(name).unwrap().create)(&chunk, &cx).unwrap();
        let inst = FxInstance::new(fx);
        let watch = inst.probe.watch();
        let block: Vec<f32> = (0..512).map(|i| 0.8 * (i as f32 * 0.07).sin()).collect();
        let (mut l, mut r) = (block.clone(), block.clone());
        // A first block lets effects grow what they keep between blocks.
        inst.process_automated(&mut l, &mut r, &ctx, &[], 0.0, None);
        COUNT.with(|c| c.set(0));
        COUNTING.with(|c| c.set(true));
        for _ in 0..50 {
            l.copy_from_slice(&block);
            r.copy_from_slice(&block);
            inst.process_automated(&mut l, &mut r, &ctx, &[], 0.0, None);
        }
        COUNTING.with(|c| c.set(false));
        assert_eq!(COUNT.with(Cell::get), 0, "{name} allocated while publishing");
        let p = watch.probe();
        assert!(p.take(probe::INPUT) > 0.7, "{name} input level");
        if matches!(name, "ReaComp" | "ReaLimit") {
            assert!(p.take(probe::REDUCTION) > 1.0, "{name} gain reduction");
        }
        if name == "ReaGate" {
            assert!(p.take(probe::GATE) > 0.5);
        }
        if name == "ReaXcomp" {
            assert!(p.take(probe::BAND_REDUCTION) > 0.5);
        }
        let mut spectrum = Spectrum::new(2048);
        assert!(spectrum.update(p, Side::Post, 0.02));
    }
}

#[test]
fn unwatched_effects_publish_nothing() {
    let cx = StockContext { sample_rate: 48000, project_dir: None, hardware: None };
    let mut comp = vec![0.0f32; 21];
    (comp[0], comp[1], comp[11]) = (0.1, 0.0303, 1.0);
    let inst = FxInstance::new((by_name("ReaComp").unwrap().create)(&marked(&comp), &cx).unwrap());
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
    let (mut l, mut r) = (vec![0.9f32; 512], vec![0.9f32; 512]);
    inst.process_automated(&mut l, &mut r, &ctx, &[], 0.0, None);
    assert_eq!(inst.probe.peek(probe::INPUT), 0.0);
    assert_eq!(inst.probe.peek(probe::REDUCTION), 0.0);
    let mut out = [0.0f32; 8];
    assert_eq!(inst.probe.latest(Side::Pre, &mut out), 0);
}
