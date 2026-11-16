//! Spectral dynamics: bands that compress or expand each frequency bin inside their region on
//! its own, so a resonance that pokes out is pulled down while its neighbours are left alone.
//!
//! An STFT (Hann analysis and synthesis, 75 % overlap) runs after the EQ. Each spectral band
//! weighs the bins by its own shape (1 at the centre, falling to 0 where the band's curve at
//! full range has no effect), follows every bin's level with its attack and release, and adds
//! the gain change of the shared gain computer, scaled by the weight, to the bins of its
//! stereo placement. The stage adds `latency()` frames while any band uses it.

use std::sync::Arc;

use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

use super::dynamics::gain_change;
use super::filters::{Design, Prototype};
use super::params::Placement;

/// What a spectral band needs from the EQ.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpectralBand {
    pub design: Design,
    pub placement: Placement,
    pub range: f64,
    pub threshold: f64,
    pub auto: bool,
    pub attack_ms: f64,
    pub release_ms: f64,
}

#[derive(Default)]
struct BandState {
    config: Option<SpectralBand>,
    /// Bins the band touches and their weights.
    bins: Vec<(usize, f32)>,
    env: Vec<f32>,
    average: Vec<f32>,
    /// Mean gain change over the band's bins in the last frame, for the editor.
    mean_db: f32,
}

pub struct Spectral {
    n: usize,
    hop: usize,
    fs: f64,
    window: Vec<f32>,
    fft: Arc<dyn RealToComplex<f32>>,
    ifft: Arc<dyn ComplexToReal<f32>>,
    input: [Vec<f32>; 2],
    side: [Vec<f32>; 2],
    acc: [Vec<f32>; 2],
    ready: [Vec<f32>; 2],
    fill: usize,
    bands: Vec<BandState>,
    time: Vec<f32>,
    spec: [Vec<Complex32>; 2],
    det: [Vec<Complex32>; 2],
    /// Gain change in dB per placement (stereo, left, right, mid, side) and bin.
    gains: [Vec<f32>; 5],
    fft_scratch: Vec<Complex32>,
    ifft_scratch: Vec<Complex32>,
}

impl Spectral {
    /// The latency of a stage at `fs`: its frame length.
    pub fn latency_at(fs: f64) -> usize {
        ((2048.0 * fs / 48000.0).round().max(256.0) as usize).next_power_of_two()
    }

    pub fn new(fs: f64, bands: usize) -> Self {
        let n = Self::latency_at(fs);
        let hop = n / 4;
        let mut planner = RealFftPlanner::<f32>::new();
        let bins = n / 2 + 1;
        let window =
            (0..n).map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()) as f32).collect();
        let (fft, ifft) = (planner.plan_fft_forward(n), planner.plan_fft_inverse(n));
        Self {
            n,
            hop,
            fs,
            window,
            fft_scratch: vec![Complex32::default(); fft.get_scratch_len()],
            ifft_scratch: vec![Complex32::default(); ifft.get_scratch_len()],
            fft,
            ifft,
            input: [vec![0.0; n], vec![0.0; n]],
            side: [vec![0.0; n], vec![0.0; n]],
            acc: [vec![0.0; n], vec![0.0; n]],
            ready: [vec![0.0; hop], vec![0.0; hop]],
            fill: 0,
            bands: (0..bands)
                .map(|_| BandState {
                    bins: Vec::with_capacity(n / 2),
                    env: vec![-120.0; bins],
                    average: vec![-120.0; bins],
                    ..Default::default()
                })
                .collect(),
            time: vec![0.0; n],
            spec: [vec![Complex32::default(); bins], vec![Complex32::default(); bins]],
            det: [vec![Complex32::default(); bins], vec![Complex32::default(); bins]],
            gains: std::array::from_fn(|_| vec![0.0; bins]),
        }
    }

    pub fn latency(&self) -> usize {
        self.n
    }

    pub fn configure(&mut self, b: usize, config: Option<SpectralBand>) {
        let state = &mut self.bands[b];
        if state.config == config {
            return;
        }
        let reshape = match (&state.config, &config) {
            (Some(old), Some(new)) => old.design != new.design || old.range != new.range,
            _ => true,
        };
        state.config = config;
        if !reshape {
            return;
        }
        state.bins.clear();
        let Some(c) = config else { return };
        if c.range == 0.0 {
            return;
        }
        let full = Prototype::design(&Design { gain_db: c.range, ..c.design });
        for k in 1..self.n / 2 {
            let f = k as f64 * self.fs / self.n as f64;
            let db = 20.0 * full.magnitude(f).max(1e-12).log10();
            let w = (db / c.range).clamp(0.0, 1.0);
            if w > 0.02 {
                state.bins.push((k, w as f32));
            }
        }
    }

    pub fn active(&self) -> bool {
        self.bands.iter().any(|b| b.config.is_some())
    }

    /// The band's mean gain change over its bins, in dB.
    pub fn band_gain(&self, b: usize) -> f64 {
        self.bands[b].mean_db as f64
    }

    pub fn reset(&mut self) {
        for v in self.input.iter_mut().chain(&mut self.side).chain(&mut self.acc).chain(&mut self.ready) {
            v.fill(0.0);
        }
        for b in &mut self.bands {
            b.env.fill(-120.0);
            b.mean_db = 0.0;
        }
        self.fill = 0;
    }

    /// Back to the state `new` makes, without allocating.
    pub fn clear(&mut self) {
        self.reset();
        for b in &mut self.bands {
            b.config = None;
            b.bins.clear();
            b.average.fill(-120.0);
        }
    }

    /// Takes over `other`'s state, without allocating; both have the same sample rate.
    pub fn copy_from(&mut self, other: &Spectral) {
        let dst = self.input.iter_mut().chain(&mut self.side).chain(&mut self.acc).chain(&mut self.ready);
        let src = other.input.iter().chain(&other.side).chain(&other.acc).chain(&other.ready);
        for (dst, src) in dst.zip(src) {
            dst.copy_from_slice(src);
        }
        self.fill = other.fill;
        for (dst, src) in self.bands.iter_mut().zip(&other.bands) {
            dst.config = src.config;
            dst.bins.clear();
            dst.bins.extend_from_slice(&src.bins);
            dst.env.copy_from_slice(&src.env);
            dst.average.copy_from_slice(&src.average);
            dst.mean_db = src.mean_db;
        }
    }

    /// One frame through the stage: `x` the signal, `sc` the detector's (the signal itself,
    /// or the external sidechain aligned with it).
    #[inline]
    pub fn push(&mut self, x: (f32, f32), sc: (f32, f32)) -> (f32, f32) {
        let at = self.n - self.hop + self.fill;
        self.input[0][at] = x.0;
        self.input[1][at] = x.1;
        self.side[0][at] = sc.0;
        self.side[1][at] = sc.1;
        let out = (self.ready[0][self.fill], self.ready[1][self.fill]);
        self.fill += 1;
        if self.fill == self.hop {
            self.frame();
            self.fill = 0;
        }
        out
    }

    fn frame(&mut self) {
        let (n, hop) = (self.n, self.hop);
        for ch in 0..2 {
            for ((t, x), w) in self.time.iter_mut().zip(&self.input[ch]).zip(&self.window) {
                *t = x * w;
            }
            let _ = self.fft.process_with_scratch(&mut self.time, &mut self.spec[ch], &mut self.fft_scratch);
            for ((t, x), w) in self.time.iter_mut().zip(&self.side[ch]).zip(&self.window) {
                *t = x * w;
            }
            let _ = self.fft.process_with_scratch(&mut self.time, &mut self.det[ch], &mut self.fft_scratch);
        }
        for g in &mut self.gains {
            g.fill(0.0);
        }
        let to_amp = 4.0 / n as f32;
        let frame_rate = self.fs / hop as f64;
        for band in &mut self.bands {
            let Some(c) = band.config else { continue };
            let coef = |ms: f64| (-1.0 / (ms.max(0.1) * 0.001 * frame_rate)).exp() as f32;
            let (att, rel, avg) = (coef(c.attack_ms), coef(c.release_ms), coef(1500.0));
            let role = c.placement as usize;
            let mut sum = 0.0f32;
            for &(k, w) in &band.bins {
                let (l, r) = (self.det[0][k], self.det[1][k]);
                let mag = match c.placement {
                    Placement::Stereo => l.norm().max(r.norm()),
                    Placement::Left => l.norm(),
                    Placement::Right => r.norm(),
                    Placement::Mid => ((l + r) * 0.5).norm(),
                    Placement::Side => ((l - r) * 0.5).norm(),
                };
                let lvl = 20.0 * (mag * to_amp).max(1e-6).log10();
                let env = &mut band.env[k];
                let a = if lvl > *env { att } else { rel };
                *env = a * *env + (1.0 - a) * lvl;
                let mean = &mut band.average[k];
                if lvl > -100.0 {
                    *mean = avg * *mean + (1.0 - avg) * lvl;
                }
                let thr = if c.auto { mean.max(-80.0) } else { c.threshold as f32 };
                let g = w * gain_change((*env - thr) as f64, c.range) as f32;
                self.gains[role][k] += g;
                sum += g;
            }
            band.mean_db = if band.bins.is_empty() { 0.0 } else { sum / band.bins.len() as f32 };
        }
        let amp = |db: f32| if db == 0.0 { 1.0 } else { 10f32.powf(db / 20.0) };
        for k in 0..self.spec[0].len() {
            let (mut l, mut r) = (self.spec[0][k], self.spec[1][k]);
            let (gm, gs) = (amp(self.gains[3][k]), amp(self.gains[4][k]));
            if gm != 1.0 || gs != 1.0 {
                let (m, s) = ((l + r) * (0.5 * gm), (l - r) * (0.5 * gs));
                l = m + s;
                r = m - s;
            }
            let st = amp(self.gains[0][k]);
            self.spec[0][k] = l * (st * amp(self.gains[1][k]));
            self.spec[1][k] = r * (st * amp(self.gains[2][k]));
        }
        // Hann² at 75 % overlap sums to 1.5.
        let norm = 1.0 / (n as f32 * 1.5);
        for ch in 0..2 {
            let last = self.spec[ch].len() - 1;
            self.spec[ch][0].im = 0.0;
            self.spec[ch][last].im = 0.0;
            let _ = self.ifft.process_with_scratch(&mut self.spec[ch], &mut self.time, &mut self.ifft_scratch);
            for ((a, t), w) in self.acc[ch].iter_mut().zip(&self.time).zip(&self.window) {
                *a += t * w * norm;
            }
            self.ready[ch].copy_from_slice(&self.acc[ch][..hop]);
            self.acc[ch].copy_within(hop.., 0);
            self.acc[ch][n - hop..].fill(0.0);
            self.input[ch].copy_within(hop.., 0);
            self.side[ch].copy_within(hop.., 0);
        }
    }
}
