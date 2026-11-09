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

/// Loudness of a whole render after BS.1770-4: mean squares of 100 ms blocks, from which the
/// 400 ms gating blocks (75 % overlap) and 3 s windows are formed.
pub struct LoudnessScan {
    filters: [[Biquad; 2]; 2],
    acc: f64,
    count: usize,
    block_len: usize,
    blocks: Vec<f64>,
}

impl LoudnessScan {
    pub fn new(sample_rate: u32) -> Self {
        let k = k_weighting(sample_rate as f64);
        Self { filters: [k, k], acc: 0.0, count: 0, block_len: (sample_rate / 10).max(1) as usize, blocks: Vec::new() }
    }

    pub fn push(&mut self, left: &[f32], right: &[f32]) {
        for (&l, &r) in left.iter().zip(right) {
            let [fl, fr] = &mut self.filters;
            let kl = fl[0].run(l as f64);
            let kl = fl[1].run(kl);
            let kr = fr[0].run(r as f64);
            let kr = fr[1].run(kr);
            self.acc += kl * kl + kr * kr;
            self.count += 1;
            if self.count == self.block_len {
                self.blocks.push(self.acc / self.count as f64);
                (self.acc, self.count) = (0.0, 0);
            }
        }
    }

    /// Mean squares of the windows of `n` 100 ms blocks ending at every block. As in REAPER's
    /// measurement, the first windows reach back before the start, where it is silent.
    fn windows(&self, n: usize) -> impl Iterator<Item = f64> + '_ {
        (1..=self.blocks.len()).map(move |end| self.blocks[end.saturating_sub(n)..end].iter().sum::<f64>() / n as f64)
    }

    /// Integrated loudness in LUFS: 400 ms blocks above -70 LUFS, then above the mean of
    /// those less 10 LU.
    pub fn integrated(&self) -> f64 {
        let gated: Vec<f64> = self.windows(4).filter(|&m| lufs(m) as f64 > -70.0).collect();
        if gated.is_empty() {
            return f64::NEG_INFINITY;
        }
        let relative = lufs(gated.iter().sum::<f64>() / gated.len() as f64) as f64 - 10.0;
        let kept: Vec<f64> = gated.into_iter().filter(|&m| lufs(m) as f64 > relative).collect();
        if kept.is_empty() {
            return f64::NEG_INFINITY;
        }
        lufs(kept.iter().sum::<f64>() / kept.len() as f64) as f64
    }

    pub fn max_momentary(&self) -> f64 {
        self.windows(4).map(|m| lufs(m) as f64).fold(f64::NEG_INFINITY, f64::max)
    }

    pub fn max_short_term(&self) -> f64 {
        self.windows(30).map(|m| lufs(m) as f64).fold(f64::NEG_INFINITY, f64::max)
    }

    /// Loudness range in LU after EBU Tech 3342: the short-term values above -70 LUFS and above
    /// their mean less 20 LU, from the 10th to the 95th percentile. Only whole 3 s windows count.
    pub fn loudness_range(&self) -> f64 {
        let gated: Vec<f64> = self.windows(30).skip(29).filter(|&m| lufs(m) as f64 > -70.0).collect();
        if gated.is_empty() {
            return 0.0;
        }
        let relative = lufs(gated.iter().sum::<f64>() / gated.len() as f64) as f64 - 20.0;
        let mut kept: Vec<f64> = gated.into_iter().map(|m| lufs(m) as f64).filter(|&l| l > relative).collect();
        if kept.len() < 2 {
            return 0.0;
        }
        kept.sort_by(f64::total_cmp);
        let at = |p: f64| kept[((kept.len() - 1) as f64 * p).round() as usize];
        at(0.95) - at(0.10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(amp: f32, seconds: f32, sr: u32) -> Vec<f32> {
        (0..(seconds * sr as f32) as usize)
            .map(|i| amp * (std::f32::consts::TAU * 1000.0 * i as f32 / sr as f32).sin())
            .collect()
    }

    #[test]
    fn integrated_loudness_with_gating() {
        // A 1 kHz sine at -20 dBFS reads -23 LUFS per channel (EBU Tech 3341); two add 3 dB.
        // The three lead-in windows, partly before the start, pull 10 s of it down by 0.06 LU.
        let s = sine(0.1, 10.0, 48000);
        let mut scan = LoudnessScan::new(48000);
        scan.push(&s, &s);
        assert!((scan.integrated() + 20.06).abs() < 0.01, "{}", scan.integrated());
        // A much quieter second half falls under the relative gate; only the windows across the
        // step still count, by another 0.06 LU.
        let quiet = sine(0.001, 10.0, 48000);
        scan.push(&quiet, &quiet);
        assert!((scan.integrated() + 20.12).abs() < 0.02, "{}", scan.integrated());
        assert!((scan.max_momentary() + 20.0).abs() < 0.2);
    }

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

    /// EBU Tech 3342 test signal 1: 20 s of a 1 kHz sine at -20 dBFS then 20 s at -30 dBFS
    /// (stereo, so -23 and -33 LUFS) has a loudness range of 10 LU.
    #[test]
    fn loudness_range_of_two_levels() {
        let mut scan = LoudnessScan::new(48000);
        let loud = sine(0.1, 20.0, 48000);
        let quiet = sine(0.1 / 10f32.sqrt(), 20.0, 48000);
        scan.push(&loud, &loud);
        scan.push(&quiet, &quiet);
        assert!((scan.loudness_range() - 10.0).abs() < 0.1, "{}", scan.loudness_range());
        let mut steady = LoudnessScan::new(48000);
        steady.push(&loud, &loud);
        assert!(steady.loudness_range() < 0.1);
    }

    #[test]
    fn silence_is_minus_infinity() {
        let m = Loudness::new(48000);
        m.record(&[0.0; 48000], &[0.0; 48000]);
        assert_eq!(m.momentary(), -f32::INFINITY);
    }
}
