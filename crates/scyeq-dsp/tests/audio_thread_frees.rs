//! What the audio thread frees: a spectral band turned on or off while playing must not free the
//! processing an earlier phase mode switch left behind.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use scyeq_dsp::ScytheEq;
use scyeq_dsp::params::{GLOBAL, band, band_param, global};

struct Counting;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static FREED: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
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
