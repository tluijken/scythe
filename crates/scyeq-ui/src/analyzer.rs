//! The ScyEQ-Pro analyzer: spectra of the audio before and after the EQ, its sidechain and
//! another instance's signal, made on a thread of its own from the probes' rings and handed
//! to the editor as levels on a fixed log-frequency grid.
//!
//! Each update windows the newest samples (4-term Blackman-Harris, zero padded twice at the
//! highest resolution), turns the bins into power and resamples them onto the display grid:
//! where a grid point's fractional-octave window spans less than two bins the power is
//! interpolated between bins (so the bass is a smooth curve, not steps), elsewhere it is the
//! window's mean power, pulled towards the window's peak by how tonal the window is (its
//! spectral flatness), so noise is averaged while a sine keeps its level. Levels then rise and
//! fall with separate ballistics, and an optional peak hold decays after a moment.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use scyeq_dsp::probe::{FxProbe, RING, Side};
use scyeq_dsp::realfft::RealFftPlanner;
use scyeq_dsp::realfft::num_complex::Complex32;

/// Points of the display grid, log spaced from `LO` to `HI` Hz.
pub const POINTS: usize = 560;
pub const LO: f64 = 10.0;
pub const HI: f64 = 30000.0;
pub const FLOOR: f32 = -150.0;

pub fn grid_hz(i: usize) -> f64 {
    LO * (HI / LO).powf(i as f64 / (POINTS - 1) as f64)
}

/// Where on the grid (fractional index) `hz` sits.
pub fn grid_pos(hz: f64) -> f64 {
    (hz.max(1.0) / LO).ln() / (HI / LO).ln() * (POINTS - 1) as f64
}

/// What the analyzer listens to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Pre,
    Post,
    Sidechain,
    /// Another instance's output, for collisions and matching.
    Other,
}

const SOURCES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// 0 low .. 3 maximum.
    pub resolution: usize,
    /// 0 very slow .. 4 very fast.
    pub speed: usize,
    pub peak_hold: bool,
    pub frozen: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { resolution: 2, speed: 2, peak_hold: false, frozen: false }
    }
}

impl Settings {
    fn fft_size(&self) -> (usize, usize) {
        match self.resolution {
            0 => (2048, 2048),
            1 => (4096, 4096),
            2 => (8192, 8192),
            _ => (RING, RING * 2),
        }
    }

    /// Smoothing width in octaves.
    fn smoothing(&self) -> f64 {
        [1.0 / 6.0, 1.0 / 12.0, 1.0 / 24.0, 1.0 / 48.0][self.resolution.min(3)]
    }

    /// Rise time constant (s) and fall rate (dB/s).
    fn ballistics(&self) -> (f32, f32) {
        [(0.25, 8.0), (0.12, 18.0), (0.05, 36.0), (0.025, 72.0), (0.008, 160.0)][self.speed.min(4)]
    }
}

/// One source's levels on the grid.
#[derive(Clone)]
pub struct Curve {
    pub db: Vec<f32>,
    pub peak: Vec<f32>,
    pub live: bool,
}

impl Default for Curve {
    fn default() -> Self {
        Self { db: vec![FLOOR; POINTS], peak: vec![FLOOR; POINTS], live: false }
    }
}

#[derive(Default)]
struct Shared {
    probes: [Option<(Arc<FxProbe>, Side)>; SOURCES],
    settings: Settings,
    curves: [Curve; SOURCES],
    sample_rate: u32,
    /// Microseconds the last update took.
    cost_us: u64,
}

/// A running analyzer; the thread ends when it is dropped.
pub struct Analyzer {
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
}

impl Drop for Analyzer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Analyzer {
    pub fn start() -> Self {
        let shared = Arc::new(Mutex::new(Shared { sample_rate: 48000, ..Default::default() }));
        let stop = Arc::new(AtomicBool::new(false));
        let (s, st) = (shared.clone(), stop.clone());
        let _ = std::thread::Builder::new().name("eq-analyzer".into()).spawn(move || run(s, st));
        Self { shared, stop }
    }

    pub fn set_source(&self, source: Source, probe: Option<(Arc<FxProbe>, Side)>) {
        if let Ok(mut s) = self.shared.lock() {
            let slot = &mut s.probes[source as usize];
            let same = match (slot.as_ref(), probe.as_ref()) {
                (Some((a, sa)), Some((b, sb))) => Arc::ptr_eq(a, b) && sa == sb,
                (None, None) => true,
                _ => false,
            };
            if !same {
                *slot = probe;
                s.curves[source as usize] = Curve::default();
            }
        }
    }

    pub fn set_settings(&self, settings: Settings) {
        if let Ok(mut s) = self.shared.lock() {
            s.settings = settings;
        }
    }

    /// Copies a source's curve into `out` (kept when frozen).
    pub fn read(&self, source: Source, out: &mut Curve) {
        if let Ok(s) = self.shared.lock() {
            out.clone_from(&s.curves[source as usize]);
        }
    }

    pub fn cost(&self) -> Duration {
        Duration::from_micros(self.shared.lock().map_or(0, |s| s.cost_us))
    }
}

struct Work {
    size: usize,
    padded: usize,
    window: Vec<f32>,
    norm: f32,
    input: Vec<f32>,
    buf: Vec<f32>,
    spec: Vec<Complex32>,
    power: Vec<f64>,
    sum: Vec<f64>,
    logsum: Vec<f64>,
    level: Vec<f32>,
    fft: Arc<dyn scyeq_dsp::realfft::RealToComplex<f32>>,
}

impl Work {
    fn new(size: usize, padded: usize) -> Self {
        let n = size as f32;
        let window: Vec<f32> = (0..size)
            .map(|i| {
                let x = std::f32::consts::TAU * i as f32 / n;
                0.35875 - 0.48829 * x.cos() + 0.14128 * (2.0 * x).cos() - 0.01168 * (3.0 * x).cos()
            })
            .collect();
        let norm = 2.0 / window.iter().sum::<f32>();
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(padded);
        let bins = padded / 2 + 1;
        Self {
            size,
            padded,
            window,
            norm,
            input: vec![0.0; size],
            buf: vec![0.0; padded],
            spec: vec![Complex32::default(); bins],
            power: vec![0.0; bins],
            sum: vec![0.0; bins + 1],
            logsum: vec![0.0; bins + 1],
            level: vec![FLOOR; POINTS],
            fft,
        }
    }

    /// Analyses the newest samples into `level` (dB per grid point).
    fn analyse(&mut self, probe: &FxProbe, side: Side, sr: f64, octaves: f64) -> usize {
        let head = probe.latest(side, &mut self.input);
        self.buf.fill(0.0);
        for ((b, x), w) in self.buf.iter_mut().zip(&self.input).zip(&self.window) {
            *b = x * w;
        }
        if self.fft.process(&mut self.buf, &mut self.spec).is_err() {
            return head;
        }
        let norm = self.norm as f64;
        for (p, c) in self.power.iter_mut().zip(&self.spec) {
            let a = c.norm() as f64 * norm;
            *p = a * a;
        }
        self.sum[0] = 0.0;
        self.logsum[0] = 0.0;
        for (k, p) in self.power.iter().enumerate() {
            self.sum[k + 1] = self.sum[k] + p;
            self.logsum[k + 1] = self.logsum[k] + p.max(1e-30).ln();
        }
        let hz_per_bin = sr / self.padded as f64;
        let last = (self.power.len() - 1) as f64;
        let half = 2f64.powf(octaves / 2.0);
        let pad = (self.padded / self.size) as f64;
        for (i, out) in self.level.iter_mut().enumerate() {
            let hz = grid_hz(i);
            let centre = hz / hz_per_bin;
            if centre >= last {
                *out = FLOOR;
                continue;
            }
            let (a, b) = ((hz / half) / hz_per_bin, (hz * half) / hz_per_bin);
            let width = b - a;
            let p = if width < 2.0 * pad {
                interpolate(&self.power, centre)
            } else {
                let (a, b) = (a.max(0.0), b.min(last));
                let lo = a.floor() as usize;
                let hi = (b.ceil() as usize).min(self.power.len() - 1);
                let n = (hi - lo + 1) as f64;
                let mean = (self.sum[hi + 1] - self.sum[lo]) / n;
                let geo = ((self.logsum[hi + 1] - self.logsum[lo]) / n).exp();
                let peak = self.power[lo..=hi].iter().copied().fold(0.0, f64::max);
                let flatness = if mean > 0.0 { geo / mean } else { 1.0 };
                // Exponentially distributed noise power has a flatness near 0.56; a lone
                // sine is far below.
                let tonal = ((0.5 - flatness) / 0.4).clamp(0.0, 1.0);
                mean + (peak - mean) * tonal
            };
            *out = (10.0 * p.max(1e-20).log10()) as f32;
        }
        head
    }
}

/// The power at fractional bin `x`, a Catmull-Rom curve through the neighbouring bins in dB.
fn interpolate(power: &[f64], x: f64) -> f64 {
    let n = power.len() as isize;
    let i = x.floor() as isize;
    let t = x - x.floor();
    let at = |k: isize| 10.0 * power[k.clamp(0, n - 1) as usize].max(1e-20).log10();
    let (p0, p1, p2, p3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
    let db = p1 + 0.5 * t * (p2 - p0 + t * (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3 + t * (3.0 * (p1 - p2) + p3 - p0)));
    10f64.powf(db.min(p1.max(p2) + 3.0) / 10.0)
}

const PEAK_HOLD: f32 = 1.5;
const PEAK_FALL: f32 = 12.0;

fn run(shared: Arc<Mutex<Shared>>, stop: Arc<AtomicBool>) {
    let mut work: Option<Work> = None;
    let mut heads = [0usize; SOURCES];
    let mut held = vec![[0.0f32; POINTS]; SOURCES];
    let mut last = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(16));
        let started = Instant::now();
        let dt = started.duration_since(last).as_secs_f32().min(0.25);
        last = started;
        let (probes, settings) = match shared.lock() {
            Ok(s) => (s.probes.clone(), s.settings),
            Err(_) => return,
        };
        if settings.frozen {
            continue;
        }
        let (size, padded) = settings.fft_size();
        if work.as_ref().is_none_or(|w| w.size != size || w.padded != padded) {
            work = Some(Work::new(size, padded));
        }
        let w = work.as_mut().unwrap();
        let (rise, fall) = settings.ballistics();
        let k_rise = 1.0 - (-dt / rise).exp();
        let mut sr = 48000;
        let mut fresh: [Option<Vec<f32>>; SOURCES] = Default::default();
        for (s, probe) in probes.iter().enumerate() {
            let Some((p, side)) = probe else { continue };
            if p.sample_rate() > 0 {
                sr = p.sample_rate();
            }
            let head = w.analyse(p, *side, sr as f64, settings.smoothing());
            if head != heads[s] {
                heads[s] = head;
                fresh[s] = Some(w.level.clone());
            }
        }
        let cost = started.elapsed();
        let Ok(mut sh) = shared.lock() else { return };
        sh.sample_rate = sr;
        sh.cost_us = cost.as_micros() as u64;
        for s in 0..SOURCES {
            let live = fresh[s].is_some();
            let curve = &mut sh.curves[s];
            curve.live = probes[s].is_some() && (live || curve.live && curve.db.iter().any(|d| *d > FLOOR + 1.0));
            for i in 0..POINTS {
                let target = fresh[s].as_ref().map_or(FLOOR, |l| l[i]);
                let cur = &mut curve.db[i];
                if target > *cur {
                    *cur += (target - *cur) * k_rise;
                } else {
                    *cur = (*cur - fall * dt).max(target);
                }
                let peak = &mut curve.peak[i];
                if *cur >= *peak {
                    *peak = *cur;
                    held[s][i] = 0.0;
                } else {
                    held[s][i] += dt;
                    if held[s][i] > PEAK_HOLD {
                        *peak = (*peak - PEAK_FALL * dt).max(*cur);
                    }
                }
            }
        }
    }
}

const NOTES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/// The note nearest `hz` (A4 = 440 Hz) and how many cents `hz` is off it: `A4 +12 ct`.
pub fn note_name(hz: f64) -> String {
    let midi = 69.0 + 12.0 * (hz / 440.0).log2();
    let n = midi.round();
    let cents = ((midi - n) * 100.0).round() as i32;
    let name = NOTES[(n as i64).rem_euclid(12) as usize];
    let octave = (n as i64).div_euclid(12) - 1;
    format!("{name}{octave} {cents:+} ct")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_are_named_with_cents() {
        assert_eq!(note_name(440.0), "A4 +0 ct");
        assert_eq!(note_name(261.63), "C4 +0 ct");
        assert_eq!(note_name(452.0), "A4 +47 ct");
        assert_eq!(note_name(27.5), "A0 +0 ct");
    }

    /// A full-scale sine reads 0 dB at every resolution, and white noise reads an even level
    /// without the grid's steps.
    #[test]
    fn sines_keep_their_level_and_noise_is_smooth() {
        let probe = Arc::new(FxProbe::default());
        let _w = probe.watch();
        let sr = 48000.0;
        for hz in [55.0, 1000.0, 12000.0] {
            let sine: Vec<f32> = (0..RING).map(|i| (std::f64::consts::TAU * hz * i as f64 / sr).sin() as f32).collect();
            probe.tap(Side::Pre, &sine, &sine, 48000);
            for res in 0..4 {
                let s = Settings { resolution: res, ..Default::default() };
                let (size, padded) = s.fft_size();
                let mut w = Work::new(size, padded);
                w.analyse(&probe, Side::Pre, sr, s.smoothing());
                let at = grid_pos(hz).round() as usize;
                let peak = w.level[at.saturating_sub(2)..at + 3].iter().copied().fold(FLOOR, f32::max);
                assert!(peak.abs() < 1.5, "{hz} Hz at resolution {res}: {peak}");
            }
        }
        let mut seed = 1u64;
        let noise: Vec<f32> = (0..RING)
            .map(|_| {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                ((seed >> 33) as f32 / (1u64 << 31) as f32) - 0.5
            })
            .collect();
        probe.tap(Side::Pre, &noise, &noise, 48000);
        let s = Settings { resolution: 3, ..Default::default() };
        let (size, padded) = s.fft_size();
        let mut w = Work::new(size, padded);
        w.analyse(&probe, Side::Pre, sr, s.smoothing());
        let lo = grid_pos(2000.0) as usize;
        let hi = grid_pos(16000.0) as usize;
        let steps: Vec<f32> = w.level[lo..hi].windows(2).map(|p| (p[1] - p[0]).abs()).collect();
        let mean_step = steps.iter().sum::<f32>() / steps.len() as f32;
        // One frame of noise at the finest smoothing, before any time averaging.
        assert!(mean_step < 2.0, "jagged noise: {mean_step} dB per point");
    }
}
