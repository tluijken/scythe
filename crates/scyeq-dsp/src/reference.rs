//! The long-term average spectrum of a recording in third octaves: what EQ Match fits a file
//! reference with, and what Scythe's mix analysis builds on.

use std::ops::Range;
use std::sync::Arc;

use realfft::num_complex::Complex32;
use realfft::{RealFftPlanner, RealToComplex};

/// What silence and other immeasurably low levels read, in dB, so every value stays a number.
pub const FLOOR_DB: f64 = -150.0;

/// Third-octave band centres from 20 Hz to 20 kHz (base-10 series, IEC 61260).
pub fn band_centres() -> Vec<f64> {
    (-17..=13).map(|k| 1000.0 * 10f64.powf(k as f64 / 10.0)).collect()
}

/// The lower and upper edge of the third-octave band around `centre`.
pub fn band_edges(centre: f64) -> (f64, f64) {
    let half = 10f64.powf(1.0 / 20.0);
    (centre / half, centre * half)
}

pub fn db(power: f64) -> f64 {
    if power > 1e-15 { 10.0 * power.log10() } else { FLOOR_DB }
}

/// Power spectra of left, right and their cross product, summed over half-overlapping
/// Hann-windowed frames of about 3 Hz per bin.
pub struct Ltas {
    rate: u32,
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    window_power: f64,
    fill: [Vec<f32>; 2],
    input: Vec<f32>,
    spectrum: [Vec<Complex32>; 2],
    /// Summed over frames, per bin: |L|², |R|² and Re(L·R*).
    power: [Vec<f64>; 3],
    frames: u64,
}

impl Ltas {
    pub fn new(rate: u32) -> Self {
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
            input: fft.make_input_vec(),
            spectrum: [fft.make_output_vec(), fft.make_output_vec()],
            fft,
            window,
            window_power,
            fill: [Vec::with_capacity(len), Vec::with_capacity(len)],
            power: [vec![0.0; bins], vec![0.0; bins], vec![0.0; bins]],
            frames: 0,
        }
    }

    fn len(&self) -> usize {
        self.window.len()
    }

    pub fn push(&mut self, left: &[f32], right: &[f32]) {
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

    /// Ends the input. A source shorter than one window is measured over the part it fills,
    /// padded with silence.
    pub fn finish(&mut self) {
        if self.frames == 0 && !self.fill[0].is_empty() {
            let len = self.len();
            self.fill[0].resize(len, 0.0);
            self.fill[1].resize(len, 0.0);
            self.transform();
        }
    }

    /// |L|², |R|² and Re(L·R*) summed per bin.
    pub fn power(&self) -> &[Vec<f64>; 3] {
        &self.power
    }

    pub fn bin_hz(&self) -> f64 {
        self.rate as f64 / self.len() as f64
    }

    /// Mean power per bin of the mid and the side signal, in the units of the signal's mean
    /// square.
    pub fn mid_side(&self) -> (Vec<f64>, Vec<f64>) {
        let scale = 2.0 / (self.len() as f64 * self.window_power * self.frames.max(1) as f64);
        self.power[0]
            .iter()
            .zip(&self.power[1])
            .zip(&self.power[2])
            .map(|((ll, rr), lr)| ((ll + rr + 2.0 * lr) / 4.0 * scale, ((ll + rr - 2.0 * lr) / 4.0 * scale).max(0.0)))
            .unzip()
    }

    /// The bins from `lo` to `hi` Hz.
    pub fn bins(&self, lo: f64, hi: f64) -> Range<usize> {
        let bin_hz = self.bin_hz();
        let a = ((lo / bin_hz).ceil() as usize).max(1);
        let b = ((hi / bin_hz).ceil() as usize).min(self.power[0].len());
        a..b.max(a)
    }
}

/// The mid signal's level (dB) in each third-octave band of planar audio (one or two
/// channels), with the band centres.
pub fn third_octaves(channels: &[Vec<f32>], rate: u32) -> Vec<(f64, f64)> {
    let left = channels.first().map_or(&[][..], Vec::as_slice);
    let right = channels.get(1).map_or(left, Vec::as_slice);
    let mut ltas = Ltas::new(rate);
    for (l, r) in left.chunks(8192).zip(right.chunks(8192)) {
        ltas.push(l, r);
    }
    ltas.finish();
    let (mid, _) = ltas.mid_side();
    band_centres()
        .into_iter()
        .map(|hz| {
            let (lo, hi) = band_edges(hz);
            (hz, db(mid[ltas.bins(lo, hi)].iter().sum()))
        })
        .collect()
}

/// Band levels interpolated at `freqs`, on a log frequency axis; held flat beyond the ends.
pub fn levels_at(bands: &[(f64, f64)], freqs: &[f64]) -> Vec<f64> {
    let hz: Vec<f64> = bands.iter().map(|b| b.0.ln()).collect();
    let db: Vec<f64> = bands.iter().map(|b| b.1).collect();
    freqs
        .iter()
        .map(|f| {
            let x = f.ln();
            match hz.iter().position(|h| *h >= x) {
                Some(0) => db[0],
                Some(k) => db[k - 1] + (x - hz[k - 1]) / (hz[k] - hz[k - 1]) * (db[k] - db[k - 1]),
                None => *db.last().unwrap_or(&FLOOR_DB),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sine_reads_its_level_in_its_band() {
        let rate = 48_000;
        let sine: Vec<f32> = (0..rate as usize)
            .map(|i| (std::f64::consts::TAU * 1000.0 * i as f64 / rate as f64).sin() as f32)
            .collect();
        let bands = third_octaves(&[sine.clone(), sine], rate);
        let (hz, level) = bands.iter().copied().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
        assert!((hz - 1000.0).abs() < 1.0, "{hz}");
        // A full-scale sine's mean square is 0.5: -3 dB.
        assert!((level + 3.01).abs() < 0.2, "{level}");
    }

    #[test]
    fn levels_interpolate_between_bands_and_hold_beyond_them() {
        let bands = [(100.0, -10.0), (1000.0, -20.0)];
        let at = levels_at(&bands, &[10.0, 100.0, (100.0f64 * 1000.0).sqrt(), 1000.0, 20_000.0]);
        assert_eq!(at[0], -10.0);
        assert_eq!(at[1], -10.0);
        assert!((at[2] + 15.0).abs() < 1e-9);
        assert_eq!(at[3], -20.0);
        assert_eq!(at[4], -20.0);
    }
}
