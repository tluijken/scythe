//! Loudness metering after ITU-R BS.1770: K-weighted mean square over 400 ms (momentary) and
//! 3 s (short-term) windows, updated every 100 ms. Written by the audio thread, read by the UI
//! through atomics.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Debug, Clone, Copy, Default)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// The two K-weighting stages (high shelf, then high-pass) for a sample rate, from the
/// filter definitions of BS.1770.
fn k_weighting(sample_rate: f64) -> [Biquad; 2] {
    let (f0, g, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
    let k = (std::f64::consts::PI * f0 / sample_rate).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b: [(vh + vb * k / q + k * k) / a0, 2.0 * (k * k - vh) / a0, (vh - vb * k / q + k * k) / a0],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        z: [0.0; 2],
    };
    let (f0, q) = (38.13547087602444, 0.5003270373238773);
    let k = (std::f64::consts::PI * f0 / sample_rate).tan();
    let a0 = 1.0 + k / q + k * k;
    let highpass =
        Biquad { b: [1.0, -2.0, 1.0], a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0], z: [0.0; 2] };
    [shelf, highpass]
}

const BLOCKS: usize = 30;

struct State {
    filters: [[Biquad; 2]; 2],
    /// Sum of squares of the 100 ms block being filled, and its length.
    acc: f64,
    count: usize,
    block_len: usize,
    /// Mean squares of the last 30 blocks (3 s), newest last.
    blocks: [f64; BLOCKS],
    filled: usize,
}

pub struct Loudness {
    state: Mutex<State>,
    momentary: AtomicU32,
    short_term: AtomicU32,
}

impl std::fmt::Debug for Loudness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loudness").field("momentary", &self.momentary()).finish()
    }
}

/// LUFS of a mean square (summed over channels).
fn lufs(mean_square: f64) -> f32 {
    if mean_square <= 1e-12 { -f32::INFINITY } else { (-0.691 + 10.0 * mean_square.log10()) as f32 }
}

impl Loudness {
    pub fn new(sample_rate: u32) -> Self {
        let k = k_weighting(sample_rate as f64);
        Self {
            state: Mutex::new(State {
                filters: [k, k],
                acc: 0.0,
                count: 0,
                block_len: (sample_rate / 10).max(1) as usize,
                blocks: [0.0; BLOCKS],
                filled: 0,
            }),
            momentary: AtomicU32::new((-f32::INFINITY).to_bits()),
            short_term: AtomicU32::new((-f32::INFINITY).to_bits()),
        }
    }

    pub fn record(&self, left: &[f32], right: &[f32]) {
        let Ok(mut s) = self.state.try_lock() else { return };
        let s = &mut *s;
        for (&l, &r) in left.iter().zip(right) {
            let [fl, fr] = &mut s.filters;
            let kl = fl[0].run(l as f64);
            let kl = fl[1].run(kl);
            let kr = fr[0].run(r as f64);
            let kr = fr[1].run(kr);
            s.acc += kl * kl + kr * kr;
            s.count += 1;
            if s.count == s.block_len {
                s.blocks.rotate_left(1);
                s.blocks[BLOCKS - 1] = s.acc / s.count as f64;
                s.filled = (s.filled + 1).min(BLOCKS);
                s.acc = 0.0;
                s.count = 0;
                let mean = |n: usize| s.blocks[BLOCKS - n..].iter().sum::<f64>() / n as f64;
                self.momentary.store(lufs(mean(4.min(s.filled))).to_bits(), Ordering::Relaxed);
                self.short_term.store(lufs(mean(s.filled)).to_bits(), Ordering::Relaxed);
            }
        }
    }

    /// Loudness over the last 400 ms, in LUFS.
    pub fn momentary(&self) -> f32 {
        f32::from_bits(self.momentary.load(Ordering::Relaxed))
    }

    /// Loudness over the last 3 s, in LUFS.
    pub fn short_term(&self) -> f32 {
        f32::from_bits(self.short_term.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// BS.1770 calibration: a full-scale 997 Hz sine on one channel reads -3.01 LUFS, so the
    /// same sine at -20 dBFS on both channels reads -20.0 LUFS.
    #[test]
    fn sine_calibration() {
        let sr = 48000;
        let m = Loudness::new(sr);
        let amp = 10f32.powf(-20.0 / 20.0);
        let sig: Vec<f32> =
            (0..sr as usize * 4).map(|i| amp * (std::f32::consts::TAU * 997.0 * i as f32 / sr as f32).sin()).collect();
        m.record(&sig, &sig);
        assert!((m.momentary() - -20.0).abs() < 0.1, "{}", m.momentary());
        assert!((m.short_term() - -20.0).abs() < 0.1, "{}", m.short_term());
    }

    #[test]
    fn silence_is_minus_infinity() {
        let m = Loudness::new(48000);
        m.record(&[0.0; 48000], &[0.0; 48000]);
        assert_eq!(m.momentary(), -f32::INFINITY);
    }
}
