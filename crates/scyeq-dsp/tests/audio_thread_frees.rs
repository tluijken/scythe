//! What the audio thread allocates and frees: a spectral band turned on or off while playing must
//! not free the processing an earlier phase mode switch left behind, and a phase mode or
//! resolution set between blocks is built where it is set, not in the block.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use scyeq_dsp::ScytheEq;
use scyeq_dsp::params::{GLOBAL, band, band_param, global};

struct Counting;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static FREED: Cell<usize> = const { Cell::new(0) };
    static ALLOCATED: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.try_with(Cell::get).unwrap_or(false) {
            ALLOCATED.with(|c| c.set(c.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if COUNTING.try_with(Cell::get).unwrap_or(false) {
            FREED.with(|c| c.set(c.get() + 1));
        }
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn play(eq: &mut ScytheEq, frames: usize) {
    let (mut l, mut r) = ([0.1f32; 256], [-0.1f32; 256]);
    for _ in 0..frames.div_ceil(256) {
        eq.process_block(&mut l, &mut r, None, None);
    }
}

#[test]
fn a_spectral_band_switched_while_playing_frees_nothing() {
    let mut eq = ScytheEq::new(48_000);
    eq.set(band_param(0, band::USED), 1.0);
    play(&mut eq, 4096);
    eq.set(GLOBAL + global::PHASE, 2.0);
    play(&mut eq, 48_000);
    eq.set(GLOBAL + global::PHASE, 1.0);
    play(&mut eq, 48_000);

    COUNTING.with(|c| c.set(true));
    eq.set(band_param(0, band::DYNAMIC), 2.0);
    play(&mut eq, 48_000);
    eq.set(band_param(0, band::DYNAMIC), 0.0);
    play(&mut eq, 48_000);
    COUNTING.with(|c| c.set(false));
    assert_eq!(FREED.with(Cell::get), 0);
}

#[test]
fn a_phase_mode_set_between_blocks_is_built_where_it_is_set() {
    let mut eq = ScytheEq::new(48_000);
    eq.set(band_param(0, band::USED), 1.0);
    play(&mut eq, 4096);
    let switches = [(global::PHASE, 2.0), (global::RESOLUTION, 3.0), (global::PHASE, 1.0), (global::PHASE, 0.0)];
    for (param, value) in switches.into_iter().chain(switches) {
        eq.set(GLOBAL + param, value);
        COUNTING.with(|c| c.set(true));
        play(&mut eq, 48_000);
        COUNTING.with(|c| c.set(false));
        let counted = (ALLOCATED.with(Cell::get), FREED.with(Cell::get));
        assert_eq!(counted, (0, 0), "allocations and frees in the blocks after setting {param} to {value}");
    }
}
