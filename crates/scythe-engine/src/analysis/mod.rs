//! Measuring a mix to compare it with a reference: loudness after BS.1770 and EBU R128, true
//! peak, crest factor, a long-term average spectrum in third octaves with a tonal balance curve
//! normalised to the loudness, stereo width and correlation per band, a low-end mono check,
//! brightness and transient density. [`Analyzer`] takes the audio block by block, so a render
//! is measured as it is made without being kept.

pub mod eq;
pub mod masking;
pub mod probe;
pub mod segments;

use std::sync::Arc;

use realfft::num_complex::Complex32;
use realfft::{RealFftPlanner, RealToComplex};
use serde::{Deserialize, Serialize};

use crate::loudness::LoudnessScan;

/// What silence and other immeasurably low levels read, in dB, so every value stays a number.
pub const FLOOR_DB: f64 = -150.0;

/// Where the low end ends for the mono check.
pub const LOW_END_HZ: f64 = 120.0;

/// Third-octave band centres from 20 Hz to 20 kHz (base-10 series, IEC 61260).
pub fn band_centres() -> Vec<f64> {
    (-17..=13).map(|k| 1000.0 * 10f64.powf(k as f64 / 10.0)).collect()
}

/// The lower and upper edge of the third-octave band around `centre`.
pub fn band_edges(centre: f64) -> (f64, f64) {
    let half = 10f64.powf(1.0 / 20.0);
    (centre / half, centre * half)
}

fn db(power: f64) -> f64 {
    if power > 1e-15 { 10.0 * power.log10() } else { FLOOR_DB }
}

fn amp_db(amplitude: f64) -> f64 {
    if amplitude > 1e-7 { 20.0 * amplitude.log10() } else { FLOOR_DB }
}

/// Two decimals: what the analysis is good for, and compact for an agent to read.
pub(crate) fn r2(x: f64) -> f64 {
    if x.is_finite() { (x * 100.0).round() / 100.0 } else { FLOOR_DB }
}

/// One third-octave band of the long-term average spectrum.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BandStats {
    /// Centre frequency.
    pub hz: f64,
    /// Mean power of the mid signal in the band, in dB relative to full scale.
    pub level_db: f64,
    /// The tonal balance: the level smoothed over about an octave, less the integrated
    /// loudness, so mixes of different loudness compare by their shape.
    pub tonal_db: f64,
    /// The side signal against the mid signal: -150 for mono, 0 for as much side as mid.
    pub side_db: f64,
    /// Correlation of left and right in the band: 1 mono, 0 unrelated, -1 out of phase.
    pub correlation: f64,
}

/// Whether the low end stays mono, as it must for vinyl, clubs and mono playback.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LowEnd {
    pub below_hz: f64,
    pub correlation: f64,
    pub side_db: f64,
    /// Correlation above 0.7 and side at least 12 dB under the mid.
    pub mono_safe: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Analysis {
    pub sample_rate: u32,
    pub channels: usize,
    /// Seconds measured.
    pub duration: f64,
    /// Integrated loudness (LUFS), -150 for silence.
    pub integrated_lufs: f64,
    pub short_term_max_lufs: f64,
    pub momentary_max_lufs: f64,
    /// Loudness range (LU, EBU Tech 3342).
    pub loudness_range_lu: f64,
    /// Highest level between samples, 4x oversampled (dBTP).
    pub true_peak_dbtp: f64,
    pub sample_peak_dbfs: f64,
    /// RMS of both channels (a full-scale sine reads -3 dBFS).
    pub rms_dbfs: f64,
    /// Sample peak against RMS, in dB.
    pub crest_db: f64,
    /// True peak against integrated loudness (peak-to-loudness ratio), in dB.
    pub plr_db: f64,
    /// Correlation of left and right over the whole spectrum.
    pub correlation: f64,
    /// Side against mid over the whole spectrum, in dB.
    pub side_db: f64,
    /// Power-weighted mean frequency of the mid signal.
    pub centroid_hz: f64,
    /// Share of the mid signal's power above 4 kHz, in percent.
    pub brightness_pct: f64,
    /// Slope of the band levels from 100 Hz to 10 kHz in dB per octave: 0 for pink noise,
    /// +3 for white noise; commercial mixes sit around -3 to -5.
    pub tilt_db_per_octave: f64,
    /// Attacks (sudden rises in high-passed energy) per second.
    pub transients_per_second: f64,
    pub low_end: LowEnd,
    pub bands: Vec<BandStats>,
}

/// The 4x oversampling interpolator for true peak (BS.1770-4 Annex 2): a windowed sinc of 16
/// taps per phase.
struct TruePeak {
    phases: [[f32; 16]; 3],
    history: [[f32; 16]; 2],
    peak: f32,
}

impl TruePeak {
    fn new() -> Self {
        let mut phases = [[0.0f32; 16]; 3];
        for (p, phase) in phases.iter_mut().enumerate() {
            let frac = (p + 1) as f64 / 4.0;
            for (j, c) in phase.iter_mut().enumerate() {
                let d = j as f64 - 7.0 - frac;
                let sinc =
                    if d.abs() < 1e-12 { 1.0 } else { (std::f64::consts::PI * d).sin() / (std::f64::consts::PI * d) };
                let w = 0.5 + 0.5 * (std::f64::consts::PI * d / 8.5).cos();
                *c = (sinc * w) as f32;
            }
        }
        Self { phases, history: [[0.0; 16]; 2], peak: 0.0 }
    }

    fn push(&mut self, channel: usize, s: f32) {
        let h = &mut self.history[channel];
        h.copy_within(1.., 0);
        h[15] = s;
        self.peak = self.peak.max(s.abs());
        // Between two samples the band-limited signal rarely rises above twice the larger of
        // them, so quiet stretches need no interpolation.
        if h[7].abs().max(h[8].abs()) * 2.0 < self.peak {
            return;
        }
        for phase in &self.phases {
            let v: f32 = phase.iter().zip(h.iter()).map(|(c, x)| c * x).sum();
            self.peak = self.peak.max(v.abs());
        }
    }
}

/// Attacks: the energy of the first difference (which stresses highs) in 10 ms frames, rising
/// 8 dB over the frames of the last 100 ms.
struct Onsets {
    frame_len: usize,
    acc: f64,
    count: usize,
    last: [f32; 2],
    recent: Vec<f64>,
    frame: u64,
    since: u32,
    times: Vec<f64>,
    rate: f64,
}

impl Onsets {
    fn new(rate: u32) -> Self {
        Self {
            frame_len: (rate / 100).max(1) as usize,
            acc: 0.0,
            count: 0,
            last: [0.0; 2],
            recent: Vec::with_capacity(10),
            frame: 0,
            since: u32::MAX,
            times: Vec::new(),
            rate: rate as f64,
        }
    }

    fn push(&mut self, l: f32, r: f32) {
        let dl = (l - self.last[0]) as f64;
        let dr = (r - self.last[1]) as f64;
        self.last = [l, r];
        self.acc += dl * dl + dr * dr;
        self.count += 1;
        if self.count < self.frame_len {
            return;
        }
        let energy = db(self.acc / self.count as f64);
        (self.acc, self.count) = (0.0, 0);
        let before = if self.recent.is_empty() {
            FLOOR_DB
        } else {
            db(self.recent.iter().map(|e| 10f64.powf(e / 10.0)).sum::<f64>() / self.recent.len() as f64)
        };
        self.since = self.since.saturating_add(1);
        if energy > -60.0 && energy - before > 8.0 && self.since >= 5 {
            self.times.push(self.frame as f64 * self.frame_len as f64 / self.rate);
            self.since = 0;
        }
        if self.recent.len() == 10 {
            self.recent.remove(0);
        }
        self.recent.push(energy);
        self.frame += 1;
    }
}

/// Measures audio pushed to it block by block.
pub struct Analyzer {
    rate: u32,
    channels: usize,
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    window_power: f64,
    fill: [Vec<f32>; 2],
    input: Vec<f32>,
    spectrum: [Vec<Complex32>; 2],
    /// Summed over frames, per bin: |L|², |R|² and Re(L·R*).
    power: [Vec<f64>; 3],
    frames: u64,
    loudness: LoudnessScan,
    true_peak: TruePeak,
    sample_peak: f32,
    sum_squares: f64,
    samples: u64,
    onsets: Onsets,
}

impl Analyzer {
    /// `channels` is what the source has (1 or 2); a mono source is pushed as two equal channels.
    pub fn new(rate: u32, channels: usize) -> Self {
        // About 3 Hz per bin, so the lowest third-octave bands hold a few bins.
        let len = (rate as usize / 3).next_power_of_two().max(1024);
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(len);
        let window: Vec<f32> =
            (0..len).map(|i| (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / len as f64).cos()) as f32).collect();
        let window_power = window.iter().map(|w| (*w as f64).powi(2)).sum();
        let bins = len / 2 + 1;
        Self {
            rate,
            channels: channels.max(1),
            input: fft.make_input_vec(),
            spectrum: [fft.make_output_vec(), fft.make_output_vec()],
            fft,
            window,
            window_power,
            fill: [Vec::with_capacity(len), Vec::with_capacity(len)],
            power: [vec![0.0; bins], vec![0.0; bins], vec![0.0; bins]],
            frames: 0,
            loudness: LoudnessScan::new(rate),
            true_peak: TruePeak::new(),
            sample_peak: 0.0,
            sum_squares: 0.0,
            samples: 0,
            onsets: Onsets::new(rate),
        }
    }

    fn len(&self) -> usize {
        self.window.len()
    }

    pub fn push(&mut self, left: &[f32], right: &[f32]) {
        self.loudness.push(left, right);
        for (&l, &r) in left.iter().zip(right) {
            self.true_peak.push(0, l);
            self.true_peak.push(1, r);
            self.sample_peak = self.sample_peak.max(l.abs()).max(r.abs());
            self.sum_squares += (l as f64).powi(2) + (r as f64).powi(2);
            self.samples += 1;
            self.onsets.push(l, r);
        }
        let mut at = 0;
        while at < left.len().min(right.len()) {
            let room = self.len() - self.fill[0].len();
            let n = room.min(left.len() - at);
            self.fill[0].extend_from_slice(&left[at..at + n]);
            self.fill[1].extend_from_slice(&right[at..at + n]);
            at += n;
            if self.fill[0].len() == self.len() {
                self.transform();
                let hop = self.len() / 2;
                self.fill[0].drain(..hop);
                self.fill[1].drain(..hop);
            }
        }
    }

    fn transform(&mut self) {
        for c in 0..2 {
            for ((x, s), w) in self.input.iter_mut().zip(&self.fill[c]).zip(&self.window) {
                *x = s * w;
            }
            let _ = self.fft.process(&mut self.input, &mut self.spectrum[c]);
        }
        let [l, r] = &self.spectrum;
        for (k, (a, b)) in l.iter().zip(r).enumerate() {
            self.power[0][k] += a.norm_sqr() as f64;
            self.power[1][k] += b.norm_sqr() as f64;
            self.power[2][k] += (a.re * b.re + a.im * b.im) as f64;
        }
        self.frames += 1;
    }

    /// The measurements. A source shorter than one analysis window is measured over the part
    /// it fills, padded with silence.
    pub fn finish(mut self) -> Analysis {
        if self.frames == 0 && !self.fill[0].is_empty() {
            let len = self.len();
            self.fill[0].resize(len, 0.0);
            self.fill[1].resize(len, 0.0);
            self.transform();
        }
        let len = self.len();
        let bin_hz = self.rate as f64 / len as f64;
        // Mean power per bin in the units of the signal's mean square (see `band_power`).
        let scale = 2.0 / (len as f64 * self.window_power * self.frames.max(1) as f64);
        let bins = self.power[0].len();
        let mut mid = vec![0.0; bins];
        let mut side = vec![0.0; bins];
        for k in 0..bins {
            let (ll, rr, lr) = (self.power[0][k], self.power[1][k], self.power[2][k]);
            mid[k] = (ll + rr + 2.0 * lr) / 4.0 * scale;
            side[k] = ((ll + rr - 2.0 * lr) / 4.0 * scale).max(0.0);
        }
        let range = |lo: f64, hi: f64| {
            let a = ((lo / bin_hz).ceil() as usize).max(1);
            let b = ((hi / bin_hz).ceil() as usize).min(bins);
            a..b.max(a)
        };
        let sum = |v: &[f64], r: std::ops::Range<usize>| v[r].iter().sum::<f64>();
        let correlation_in = |r: std::ops::Range<usize>| {
            let ll = sum(&self.power[0], r.clone());
            let rr = sum(&self.power[1], r.clone());
            let lr = sum(&self.power[2], r);
            if ll * rr > 1e-30 { (lr / (ll * rr).sqrt()).clamp(-1.0, 1.0) } else { 1.0 }
        };

        let integrated = self.loudness.integrated();
        let integrated_db = if integrated.is_finite() { integrated } else { FLOOR_DB };
        let centres = band_centres();
        let mut bands: Vec<BandStats> = centres
            .iter()
            .map(|&hz| {
                let (lo, hi) = band_edges(hz);
                let r = range(lo, hi);
                let m = sum(&mid, r.clone());
                let s = sum(&side, r.clone());
                BandStats {
                    hz: r2(hz),
                    level_db: r2(db(m)),
                    tonal_db: 0.0,
                    side_db: r2(if m > 1e-15 { db(s / m) } else { FLOOR_DB }),
                    correlation: r2(correlation_in(r)),
                }
            })
            .collect();
        let tonal = tonal_curve(&bands.iter().map(|b| b.level_db).collect::<Vec<_>>(), integrated_db);
        for (b, t) in bands.iter_mut().zip(tonal) {
            b.tonal_db = r2(t);
        }

        let audible = range(20.0, 20_000.0);
        let total_mid = sum(&mid, audible.clone());
        let centroid = if total_mid > 1e-15 {
            audible.clone().map(|k| k as f64 * bin_hz * mid[k]).sum::<f64>() / total_mid
        } else {
            0.0
        };
        let bright = sum(&mid, range(4000.0, 20_000.0));
        let low = range(20.0, LOW_END_HZ);
        let (low_mid, low_side) = (sum(&mid, low.clone()), sum(&side, low.clone()));
        let low_correlation = correlation_in(low);
        let low_side_db = if low_mid > 1e-15 { db(low_side / low_mid) } else { FLOOR_DB };
        let total_side = sum(&side, audible.clone());

        let (xs, ys): (Vec<f64>, Vec<f64>) = bands
            .iter()
            .filter(|b| (100.0..=10_500.0).contains(&b.hz) && b.level_db > FLOOR_DB)
            .map(|b| (b.hz.log2(), b.level_db))
            .unzip();
        let tilt = slope(&xs, &ys);

        let duration = self.samples as f64 / self.rate as f64;
        let sample_peak = self.sample_peak as f64;
        let rms = if self.samples > 0 { (self.sum_squares / (2 * self.samples) as f64).sqrt() } else { 0.0 };
        let true_peak = amp_db(self.true_peak.peak as f64);
        Analysis {
            sample_rate: self.rate,
            channels: self.channels,
            duration: r2(duration),
            integrated_lufs: r2(integrated_db),
            short_term_max_lufs: r2(finite(self.loudness.max_short_term())),
            momentary_max_lufs: r2(finite(self.loudness.max_momentary())),
            loudness_range_lu: r2(self.loudness.loudness_range()),
            true_peak_dbtp: r2(true_peak),
            sample_peak_dbfs: r2(amp_db(sample_peak)),
            rms_dbfs: r2(amp_db(rms)),
            crest_db: r2(if rms > 1e-7 { amp_db(sample_peak) - amp_db(rms) } else { 0.0 }),
            plr_db: r2(if integrated.is_finite() { true_peak - integrated } else { 0.0 }),
            correlation: r2(correlation_in(audible)),
            side_db: r2(if total_mid > 1e-15 { db(total_side / total_mid) } else { FLOOR_DB }),
            centroid_hz: r2(centroid),
            brightness_pct: r2(if total_mid > 1e-15 { 100.0 * bright / total_mid } else { 0.0 }),
            tilt_db_per_octave: r2(tilt),
            transients_per_second: r2(if duration > 0.0 { self.onsets.times.len() as f64 / duration } else { 0.0 }),
            low_end: LowEnd {
                below_hz: LOW_END_HZ,
                correlation: r2(low_correlation),
                side_db: r2(low_side_db),
                mono_safe: low_mid <= 1e-15 || (low_correlation > 0.7 && low_side_db < -12.0),
            },
            bands,
        }
    }
}

fn finite(x: f64) -> f64 {
    if x.is_finite() { x } else { FLOOR_DB }
}

/// Least-squares slope of `ys` over `xs`.
fn slope(xs: &[f64], ys: &[f64]) -> f64 {
    let n = xs.len() as f64;
    if n < 2.0 {
        return 0.0;
    }
    let (mx, my) = (xs.iter().sum::<f64>() / n, ys.iter().sum::<f64>() / n);
    let sxx: f64 = xs.iter().map(|x| (x - mx).powi(2)).sum();
    let sxy: f64 = xs.iter().zip(ys).map(|(x, y)| (x - mx) * (y - my)).sum();
    if sxx > 0.0 { sxy / sxx } else { 0.0 }
}

/// Weights of the tonal smoothing over neighbouring third octaves: about an octave wide.
const SMOOTHING: [f64; 5] = [1.0, 2.0, 3.0, 2.0, 1.0];

/// Smooths band levels (dB) in the power domain over about an octave.
pub fn smooth(levels: &[f64]) -> Vec<f64> {
    (0..levels.len())
        .map(|i| {
            let (mut sum, mut weight) = (0.0, 0.0);
            for (j, w) in SMOOTHING.iter().enumerate() {
                let Some(k) = (i + j).checked_sub(2) else { continue };
                if let Some(&l) = levels.get(k) {
                    sum += w * 10f64.powf(l / 10.0);
                    weight += w;
                }
            }
            db(sum / weight)
        })
        .collect()
}

/// The tonal balance curve: band levels smoothed over about an octave, less the loudness.
pub fn tonal_curve(levels: &[f64], loudness: f64) -> Vec<f64> {
    smooth(levels).into_iter().map(|l| if l > FLOOR_DB { l - loudness } else { FLOOR_DB }).collect()
}

/// Measures planar audio (one or two channels), or the part from `start` to `end` seconds.
pub fn analyse(channels: &[Vec<f32>], rate: u32, start: Option<f64>, end: Option<f64>) -> Analysis {
    let frames = channels.first().map_or(0, Vec::len);
    let at = |t: Option<f64>, default: usize| t.map_or(default, |t| ((t.max(0.0) * rate as f64) as usize).min(frames));
    let (a, b) = (at(start, 0), at(end, frames));
    let b = b.max(a);
    let left = channels.first().map_or(&[][..], |c| &c[a..b]);
    let right = channels.get(1).map_or(left, |c| &c[a..b]);
    let mut analyzer = Analyzer::new(rate, channels.len());
    for (l, r) in left.chunks(8192).zip(right.chunks(8192)) {
        analyzer.push(l, r);
    }
    analyzer.finish()
}

/// One band of a comparison.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BandDiff {
    pub hz: f64,
    /// The mix's tonal balance less the reference's: positive where the mix has more.
    pub diff_db: f64,
    /// The same for the side signal (stereo width) in the band.
    pub side_diff_db: f64,
}

/// A region of the spectrum, for the summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegionDiff {
    pub name: String,
    pub from_hz: f64,
    pub to_hz: f64,
    pub diff_db: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comparison {
    /// The mix less the reference, for each measure.
    pub loudness_diff_lu: f64,
    pub true_peak_diff_db: f64,
    pub crest_diff_db: f64,
    pub plr_diff_db: f64,
    pub loudness_range_diff_lu: f64,
    pub side_diff_db: f64,
    pub tilt_diff_db_per_octave: f64,
    pub centroid_ratio: f64,
    pub transients_diff_per_second: f64,
    pub regions: Vec<RegionDiff>,
    pub bands: Vec<BandDiff>,
    /// The largest tonal difference between 40 Hz and 16 kHz, in dB.
    pub max_tonal_diff_db: f64,
    /// What stands out, in plain words.
    pub summary: Vec<String>,
}

const REGIONS: [(&str, f64, f64); 6] = [
    ("sub (below 60 Hz)", 20.0, 60.0),
    ("low end (60-250 Hz)", 60.0, 250.0),
    ("low mids (250-800 Hz)", 250.0, 800.0),
    ("mids (800 Hz-3 kHz)", 800.0, 3000.0),
    ("presence (3-8 kHz)", 3000.0, 8000.0),
    ("air (above 8 kHz)", 8000.0, 20_000.0),
];

/// How `mix` differs from `reference`.
pub fn compare(reference: &Analysis, mix: &Analysis) -> Comparison {
    let bands: Vec<BandDiff> = reference
        .bands
        .iter()
        .zip(&mix.bands)
        .map(|(a, b)| BandDiff {
            hz: a.hz,
            diff_db: r2(if a.tonal_db > FLOOR_DB && b.tonal_db > FLOOR_DB { b.tonal_db - a.tonal_db } else { 0.0 }),
            side_diff_db: r2(if a.side_db > FLOOR_DB && b.side_db > FLOOR_DB { b.side_db - a.side_db } else { 0.0 }),
        })
        .collect();
    let regions: Vec<RegionDiff> = REGIONS
        .iter()
        .map(|&(name, lo, hi)| {
            let inside: Vec<f64> = bands.iter().filter(|b| b.hz >= lo && b.hz < hi).map(|b| b.diff_db).collect();
            let diff = if inside.is_empty() { 0.0 } else { inside.iter().sum::<f64>() / inside.len() as f64 };
            RegionDiff { name: name.into(), from_hz: lo, to_hz: hi, diff_db: r2(diff) }
        })
        .collect();
    let max_tonal =
        bands.iter().filter(|b| (40.0..=16_500.0).contains(&b.hz)).map(|b| b.diff_db.abs()).fold(0.0, f64::max);
    let c = Comparison {
        loudness_diff_lu: r2(mix.integrated_lufs - reference.integrated_lufs),
        true_peak_diff_db: r2(mix.true_peak_dbtp - reference.true_peak_dbtp),
        crest_diff_db: r2(mix.crest_db - reference.crest_db),
        plr_diff_db: r2(mix.plr_db - reference.plr_db),
        loudness_range_diff_lu: r2(mix.loudness_range_lu - reference.loudness_range_lu),
        side_diff_db: r2(mix.side_db - reference.side_db),
        tilt_diff_db_per_octave: r2(mix.tilt_db_per_octave - reference.tilt_db_per_octave),
        centroid_ratio: r2(if reference.centroid_hz > 0.0 { mix.centroid_hz / reference.centroid_hz } else { 1.0 }),
        transients_diff_per_second: r2(mix.transients_per_second - reference.transients_per_second),
        regions,
        bands,
        max_tonal_diff_db: r2(max_tonal),
        summary: Vec::new(),
    };
    let summary = summarise(reference, mix, &c);
    Comparison { summary, ..c }
}

fn more_or_less(diff: f64, more: &str, less: &str) -> String {
    format!("{:.1} {}", diff.abs(), if diff > 0.0 { more } else { less })
}

fn summarise(reference: &Analysis, mix: &Analysis, c: &Comparison) -> Vec<String> {
    let mut out = Vec::new();
    if c.loudness_diff_lu.abs() >= 0.5 {
        out.push(format!(
            "The mix is {} than the reference ({:.1} against {:.1} LUFS integrated).",
            more_or_less(c.loudness_diff_lu, "LU louder", "LU quieter"),
            mix.integrated_lufs,
            reference.integrated_lufs
        ));
    } else {
        out.push(format!("Loudness matches within 0.5 LU ({:.1} LUFS).", mix.integrated_lufs));
    }
    if c.plr_diff_db.abs() >= 1.0 {
        let what = if c.plr_diff_db > 0.0 {
            "more dynamic peaks (less limited)"
        } else {
            "flatter peaks (more limited or compressed)"
        };
        out.push(format!(
            "Peak-to-loudness ratio is {:.1} dB {} than the reference's {:.1} dB: {what}.",
            c.plr_diff_db.abs(),
            if c.plr_diff_db > 0.0 { "higher" } else { "lower" },
            reference.plr_db
        ));
    }
    if mix.true_peak_dbtp > -1.0 {
        out.push(format!(
            "True peak reaches {:.1} dBTP: keep it at or below -1 dBTP to avoid clipping after encoding.",
            mix.true_peak_dbtp
        ));
    }
    if c.loudness_range_diff_lu.abs() >= 2.0 {
        out.push(format!(
            "Loudness range is {} ({:.1} against {:.1} LU).",
            more_or_less(c.loudness_range_diff_lu, "LU wider", "LU narrower"),
            mix.loudness_range_lu,
            reference.loudness_range_lu
        ));
    }
    let mut regions: Vec<&RegionDiff> = c.regions.iter().filter(|r| r.diff_db.abs() >= 1.0).collect();
    regions.sort_by(|a, b| b.diff_db.abs().total_cmp(&a.diff_db.abs()));
    for r in regions.iter().take(4) {
        out.push(format!("Tonal balance: {} in the {}.", more_or_less(r.diff_db, "dB more", "dB less"), r.name));
    }
    if regions.is_empty() {
        out.push(format!(
            "Tonal balance matches within 1 dB per region (largest band difference {:.1} dB).",
            c.max_tonal_diff_db
        ));
    }
    if c.side_diff_db.abs() >= 2.0 {
        out.push(format!(
            "The stereo image is {} than the reference.",
            if c.side_diff_db > 0.0 { "wider" } else { "narrower" }
        ));
    }
    if !mix.low_end.mono_safe {
        out.push(format!(
            "The low end below {:.0} Hz is not mono-safe (correlation {:.2}, side {:.1} dB under mid).",
            LOW_END_HZ, mix.low_end.correlation, -mix.low_end.side_db
        ));
    }
    if reference.transients_per_second > 0.5
        && c.transients_diff_per_second.abs() > reference.transients_per_second * 0.3
    {
        out.push(format!(
            "Transients: {:.1} per second against {:.1}; {}.",
            mix.transients_per_second,
            reference.transients_per_second,
            if c.transients_diff_per_second > 0.0 {
                "the mix is punchier or busier"
            } else {
                "the mix is softer or more compressed"
            }
        ));
    }
    out
}

/// Test signals with known answers, shared by the tests here and in the session's API tests.
pub mod signals {
    /// A small deterministic generator, so tests measure the same noise every run.
    pub struct Noise(u64);

    impl Noise {
        pub fn new(seed: u64) -> Self {
            Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
        }

        /// Uniform in -1..1.
        pub fn white(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
        }
    }

    /// Pink noise (equal power per octave) from Paul Kellet's filter of white noise, accurate
    /// to about 0.05 dB above 10 Hz at 44.1-48 kHz.
    pub fn pink(seconds: f64, rate: u32, seed: u64) -> Vec<f32> {
        let mut noise = Noise::new(seed);
        let mut b = [0.0f32; 7];
        (0..(seconds * rate as f64) as usize)
            .map(|_| {
                let w = noise.white();
                b[0] = 0.99886 * b[0] + w * 0.0555179;
                b[1] = 0.99332 * b[1] + w * 0.0750759;
                b[2] = 0.96900 * b[2] + w * 0.153852;
                b[3] = 0.86650 * b[3] + w * 0.3104856;
                b[4] = 0.55000 * b[4] + w * 0.5329522;
                b[5] = -0.7616 * b[5] - w * 0.0168980;
                let out = b.iter().sum::<f32>() + w * 0.5362;
                b[6] = w * 0.115926;
                out * 0.05
            })
            .collect()
    }

    pub fn sine(freq: f64, amplitude: f32, seconds: f64, rate: u32) -> Vec<f32> {
        (0..(seconds * rate as f64) as usize)
            .map(|i| amplitude * (std::f64::consts::TAU * freq * i as f64 / rate as f64).sin() as f32)
            .collect()
    }

    /// A tone of `freq` with its first `harmonics` partials, the n-th at `amplitude / n`.
    pub fn harmonic(freq: f64, harmonics: usize, amplitude: f32, seconds: f64, rate: u32) -> Vec<f32> {
        let mut out = vec![0.0f32; (seconds * rate as f64) as usize];
        for n in 1..=harmonics {
            for (o, s) in out.iter_mut().zip(sine(freq * n as f64, amplitude / n as f32, seconds, rate)) {
                *o += s;
            }
        }
        out
    }

    /// White noise through four one-pole high-passes at `lo` and four one-pole low-passes at
    /// `hi`.
    pub fn band_noise(lo: f64, hi: f64, amplitude: f32, seconds: f64, rate: u32, seed: u64) -> Vec<f32> {
        let mut n = Noise::new(seed);
        let a_hi = (-std::f64::consts::TAU * lo / rate as f64).exp() as f32;
        let a_lo = (-std::f64::consts::TAU * hi / rate as f64).exp() as f32;
        let (mut hp, mut lp) = ([(0.0f32, 0.0f32); 4], [0.0f32; 4]);
        (0..(seconds * rate as f64) as usize)
            .map(|_| {
                let mut x = n.white();
                for (prev_in, prev_out) in &mut hp {
                    let y = a_hi * (*prev_out + x - *prev_in);
                    (*prev_in, *prev_out) = (x, y);
                    x = y;
                }
                for s in &mut lp {
                    *s = (1.0 - a_lo) * x + a_lo * *s;
                    x = *s;
                }
                x * amplitude
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::signals::{Noise, pink, sine};
    use super::*;

    const RATE: u32 = 48_000;

    #[test]
    fn pink_noise_is_flat_in_third_octaves_and_white_rises_3_db_per_octave() {
        let p = pink(20.0, RATE, 1);
        let a = analyse(&[p.clone(), p], RATE, None, None);
        assert!(a.tilt_db_per_octave.abs() < 0.3, "pink tilt {}", a.tilt_db_per_octave);
        let mid: Vec<f64> = a.bands.iter().filter(|b| (50.0..=16_000.0).contains(&b.hz)).map(|b| b.level_db).collect();
        let mean = mid.iter().sum::<f64>() / mid.len() as f64;
        assert!(mid.iter().all(|l| (l - mean).abs() < 1.0), "{mid:?}");
        assert_eq!(a.correlation, 1.0);
        assert!(a.low_end.mono_safe);

        let mut n = Noise::new(2);
        let w: Vec<f32> = (0..RATE as usize * 10).map(|_| n.white() * 0.25).collect();
        let a = analyse(&[w.clone(), w], RATE, None, None);
        assert!((a.tilt_db_per_octave - 3.0).abs() < 0.3, "white tilt {}", a.tilt_db_per_octave);
    }

    #[test]
    fn sine_loudness_peak_and_crest() {
        // EBU Tech 3341 test 1: a 1 kHz sine at -23 dBFS (RMS of each channel at -26 dB) on
        // both channels reads -23 LUFS; here amplitude 10^(-20/20) on both reads -20.
        let amp = 10f32.powf(-20.0 / 20.0);
        let s = sine(997.0, amp, 20.0, RATE);
        let a = analyse(&[s.clone(), s], RATE, None, None);
        assert!((a.integrated_lufs + 20.0).abs() < 0.1, "{}", a.integrated_lufs);
        assert!((a.momentary_max_lufs + 20.0).abs() < 0.1);
        assert!((a.sample_peak_dbfs + 20.0).abs() < 0.05);
        assert!((a.true_peak_dbtp + 20.0).abs() < 0.1);
        // A sine's crest factor is 3.01 dB.
        assert!((a.crest_db - 3.01).abs() < 0.05, "{}", a.crest_db);
        assert!(a.loudness_range_lu < 0.5);
        let band = a.bands.iter().max_by(|x, y| x.level_db.total_cmp(&y.level_db)).unwrap();
        assert_eq!(band.hz, 1000.0);
        assert!((a.centroid_hz - 997.0).abs() < 20.0, "{}", a.centroid_hz);
    }

    /// A sine at a quarter of the sample rate, sampled 45 degrees off its peaks, reads 3 dB
    /// low in samples; the true peak finds the real one.
    #[test]
    fn true_peak_finds_peaks_between_samples() {
        let s: Vec<f32> = (0..RATE as usize)
            .map(|i| 0.5 * (std::f64::consts::FRAC_PI_2 * i as f64 + std::f64::consts::FRAC_PI_4).sin() as f32)
            .collect();
        let a = analyse(&[s.clone(), s], RATE, None, None);
        assert!((a.sample_peak_dbfs - (-6.02 - 3.01)).abs() < 0.05, "{}", a.sample_peak_dbfs);
        assert!((a.true_peak_dbtp + 6.02).abs() < 0.2, "{}", a.true_peak_dbtp);
    }

    #[test]
    fn width_and_correlation() {
        let l = pink(10.0, RATE, 3);
        let r = pink(10.0, RATE, 4);
        let wide = analyse(&[l.clone(), r], RATE, None, None);
        assert!(wide.correlation.abs() < 0.05, "{}", wide.correlation);
        assert!(wide.side_db.abs() < 0.3, "{}", wide.side_db);
        assert!(!wide.low_end.mono_safe);
        let inverted: Vec<f32> = l.iter().map(|s| -s).collect();
        let out = analyse(&[l.clone(), inverted], RATE, None, None);
        assert!(out.correlation < -0.99);
        let mono = analyse(&[l.clone(), l], RATE, None, None);
        assert_eq!(mono.side_db, FLOOR_DB);
        let silent = analyse(&[vec![0.0; 1000], vec![0.0; 1000]], RATE, None, None);
        assert_eq!(silent.integrated_lufs, FLOOR_DB);
        assert!(serde_json::to_string(&silent).is_ok());
    }

    #[test]
    fn transients_are_counted() {
        let mut s = vec![0.0f32; RATE as usize * 10];
        let mut n = Noise::new(5);
        for hit in 0..40 {
            let at = hit * RATE as usize / 4;
            for i in 0..2400 {
                s[at + i] = n.white() * 0.5 * (1.0 - i as f32 / 2400.0);
            }
        }
        let a = analyse(&[s.clone(), s], RATE, None, None);
        assert!((a.transients_per_second - 4.0).abs() < 0.3, "{}", a.transients_per_second);
    }

    #[test]
    fn comparison_names_what_differs() {
        let p = pink(10.0, RATE, 6);
        let reference = analyse(&[p.clone(), p.clone()], RATE, None, None);
        let quiet: Vec<f32> = p.iter().map(|s| s * 0.5).collect();
        let mix = analyse(&[quiet.clone(), quiet], RATE, None, None);
        let c = compare(&reference, &mix);
        assert!((c.loudness_diff_lu + 6.02).abs() < 0.05, "{}", c.loudness_diff_lu);
        assert!(c.max_tonal_diff_db < 0.05, "{}", c.max_tonal_diff_db);
        assert!(c.summary[0].contains("6.0 LU quieter"), "{:?}", c.summary);
        assert!(c.summary.iter().any(|s| s.starts_with("Tonal balance matches")), "{:?}", c.summary);
    }
}
