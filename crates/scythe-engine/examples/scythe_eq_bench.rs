//! CPU cost of one ScyEQ-Pro instance at 48 kHz in blocks of 256 frames, per processing mode:
//! eight static bands, then the same with two dynamic bands and one spectral band.
//!
//! cargo run --release -p scythe-engine --example scythe_eq_bench

use std::time::Instant;

use scythe_engine::fx::{Effect, ProcessContext};
use scythe_engine::scythe_eq::ScytheEq;
use scythe_engine::scythe_eq::filters::Shape;
use scythe_engine::scythe_eq::params::{GLOBAL, band, band_param, global};

const SR: u32 = 48000;
const BLOCK: usize = 256;
const SECONDS: usize = 20;

fn eq(mode: f64, dynamics: bool) -> ScytheEq {
    let mut eq = ScytheEq::new(SR);
    let bands = [
        (Shape::LowCut, 40.0, 0.0, 0.7),
        (Shape::LowShelf, 120.0, 2.0, 0.7),
        (Shape::Bell, 300.0, -3.0, 1.5),
        (Shape::Bell, 900.0, 2.0, 1.0),
        (Shape::Bell, 2500.0, -2.0, 3.0),
        (Shape::Bell, 5000.0, 3.0, 1.0),
        (Shape::HighShelf, 9000.0, 2.0, 0.7),
        (Shape::HighCut, 18000.0, 0.0, 0.7),
    ];
    for (b, (shape, f, g, q)) in bands.into_iter().enumerate() {
        eq.set(band_param(b, band::USED), 1.0);
        eq.set(band_param(b, band::SHAPE), shape.index() as f64);
        eq.set(band_param(b, band::FREQ), f);
        eq.set(band_param(b, band::GAIN), g);
        eq.set(band_param(b, band::Q), q);
    }
    if dynamics {
        for (b, m) in [(2, 1.0), (4, 1.0), (5, 2.0)] {
            eq.set(band_param(b, band::DYNAMIC), m);
            eq.set(band_param(b, band::RANGE), -6.0);
            eq.set(band_param(b, band::THRESHOLD), -30.0);
        }
    }
    eq.set(GLOBAL + global::PHASE, mode);
    eq
}

fn main() {
    let mut seed = 7u64;
    let noise: Vec<f32> = (0..SR as usize * SECONDS)
        .map(|_| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) as f32 / (1u64 << 31) as f32 - 0.5) * 0.5
        })
        .collect();
    let ctx = ProcessContext {
        sample_rate: SR,
        position: 0,
        playing: true,
        notes: None,
        live: &[],
        live_channel: None,
        aux: None,
        bpm: 120.0,
        midi: None,
        midi_from: 0,
    };
    for (name, mode) in [("zero latency", 0.0), ("natural phase", 1.0), ("linear phase", 2.0)] {
        for dynamics in [false, true] {
            let mut fx = eq(mode, dynamics);
            let (mut l, mut r) = (noise.clone(), noise.clone());
            let start = Instant::now();
            for (a, b) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)) {
                fx.process(a, b, &ctx);
            }
            let took = start.elapsed().as_secs_f64();
            let blocks = noise.len() / BLOCK;
            println!(
                "{name:14} {:22} latency {:5}  {:6.1} µs per block  {:5.2} % of one core",
                if dynamics { "8 bands, 2 dyn + 1 spec" } else { "8 static bands" },
                fx.latency(),
                took * 1e6 / blocks as f64,
                took / SECONDS as f64 * 100.0
            );
        }
    }
}
