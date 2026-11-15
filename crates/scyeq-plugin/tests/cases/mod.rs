//! The states and signals both host tests run the plugin with.

use base64::Engine as _;
use scyeq_dsp::filters::Shape;
use scyeq_dsp::params::{self, band};

/// The first ScyEQ-Pro of Song 2, as Scythe stored it.
pub const SONG2: &[u8] = include_bytes!("../../../scyeq-dsp/tests/fixtures/scyeq-song2.state");

/// The state in a slot an older Scythe titled `CLAP: Scythe EQ (Scythe)`, read from the slot the
/// way REAPER hands it to the plugin.
pub fn old_titled_slot_state() -> Vec<u8> {
    let slot = include_str!("../fixtures/old-titled-slot.txt");
    assert!(slot.starts_with("<CLAP \"CLAP: Scythe EQ (Scythe)\" scythe.eq"), "{slot}");
    let text: String =
        slot.lines().map(str::trim).skip_while(|l| *l != "<STATE").skip(1).take_while(|l| *l != ">").collect();
    base64::engine::general_purpose::STANDARD.decode(text).unwrap()
}

/// One bell band at 1 kHz, +6 dB, Q 1; the rest at the defaults.
pub fn peak_at_1k() -> Vec<u8> {
    let mut values = params::defaults();
    for (offset, v) in [
        (band::USED, 1.0),
        (band::SHAPE, Shape::Bell.index() as f64),
        (band::FREQ, 1000.0),
        (band::GAIN, 6.0),
        (band::Q, 1.0),
    ] {
        values[params::band_param(0, offset) as usize] = v;
    }
    params::encode(&values, &[], params::VERSION)
}

pub fn sine(rate: f64, hz: f64, frames: usize) -> Vec<f32> {
    (0..frames).map(|i| (0.25 * (std::f64::consts::TAU * hz * i as f64 / rate).sin()) as f32).collect()
}

pub fn noise(frames: usize, seed: u64) -> Vec<f32> {
    let mut state = seed;
    (0..frames)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((state >> 33) as f32 / (1u64 << 31) as f32 - 0.5) * 0.8
        })
        .collect()
}

/// The level of `output` over `input` in dB.
pub fn gain_db(input: &[f32], output: &[f32]) -> f64 {
    let rms = |s: &[f32]| (s.iter().map(|&x| x as f64 * x as f64).sum::<f64>() / s.len() as f64).sqrt();
    20.0 * (rms(output) / rms(input)).log10()
}

/// Every band's frequency parameter.
pub fn frequencies() -> impl Iterator<Item = u32> {
    (0..params::BANDS).map(|b| params::band_param(b, band::FREQ))
}
