//! Time-stretching and pitch-shifting for takes that change speed without changing pitch, or
//! the other way round. A phase vocoder with identity phase locking (each bin keeps its phase
//! relation to the nearest spectral peak) stretches the time axis; a pitch shift is a stretch
//! followed by resampling back to the wanted length. Strong onsets reset the phases so drums
//! stay crisp. Takes set to SoundTouch or Rubber Band use those libraries when they are there
//! (`crate::shifters`); élastique, REAPER's default, is proprietary and stays on the vocoder,
//! as do REAPER's own shifters (simple windowed, Rrreeeaaa, ReaReaRea).

use std::f32::consts::PI;

use realfft::RealFftPlanner;
use realfft::num_complex::Complex32;
use scythe_core::Processing;

use crate::media::{AudioData, resample_by_step};

const FFT: usize = 2048;
const SYNTH_HOP: usize = FFT / 4;

/// `data` stretched and pitch-shifted as `processing` says (its resampling by `rate` aside).
pub fn apply(data: &AudioData, processing: &Processing) -> AudioData {
    crate::shifters::process(data, processing.time, processing.pitch, processing.mode)
        .unwrap_or_else(|| stretch(data, processing.time, processing.pitch))
}

/// Returns `data` lasting `time_ratio` times as long, with its pitch scaled by `pitch_ratio`.
pub fn stretch(data: &AudioData, time_ratio: f64, pitch_ratio: f64) -> AudioData {
    let frames = (data.frames() as f64 * time_ratio).round() as usize;
    let channels = data
        .channels
        .iter()
        .map(|ch| {
            if (pitch_ratio - 1.0).abs() < 1e-9 {
                if (time_ratio - 1.0).abs() < 1e-9 { ch.clone() } else { vocode(ch, time_ratio, frames) }
            } else {
                // Stretch further by the pitch ratio, then read faster to land on `frames`.
                let long = (ch.len() as f64 * time_ratio * pitch_ratio).round() as usize;
                let stretched = vocode(ch, time_ratio * pitch_ratio, long);
                resample_by_step(&stretched, pitch_ratio, frames)
            }
        })
        .collect();
    AudioData { sample_rate: data.sample_rate, channels }
}

fn vocode(input: &[f32], ratio: f64, out_frames: usize) -> Vec<f32> {
    let mut planner = RealFftPlanner::<f32>::new();
    let forward = planner.plan_fft_forward(FFT);
    let inverse = planner.plan_fft_inverse(FFT);
    let bins = FFT / 2 + 1;
    let window: Vec<f32> = (0..FFT).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / FFT as f32).cos()).collect();
    let analysis_hop = SYNTH_HOP as f64 / ratio;

    let mut out = vec![0.0f32; out_frames + FFT];
    let mut norm = vec![0.0f32; out_frames + FFT];
    let mut frame = vec![0.0f32; FFT];
    let mut spectrum = vec![Complex32::default(); bins];
    let mut prev_phase = vec![0.0f32; bins];
    let mut synth_phase = vec![0.0f32; bins];
    let mut magnitude = vec![0.0f32; bins];
    let mut phase = vec![0.0f32; bins];
    let mut peaks = Vec::with_capacity(bins / 2);
    let mut prev_energy = 0.0f32;
    let mut prev_at: Option<i64> = None;

    // Frames are centred on their position, so the first one starts half a window early.
    let half = (FFT / 2) as i64;
    let mut m = 0usize;
    loop {
        let synth_at = (m * SYNTH_HOP) as i64 - half;
        if synth_at >= out_frames as i64 {
            break;
        }
        let at = (m as f64 * analysis_hop).round() as i64 - half;
        for (i, (f, w)) in frame.iter_mut().zip(&window).enumerate() {
            let idx = at + i as i64;
            *f = if idx >= 0 && (idx as usize) < input.len() { input[idx as usize] * w } else { 0.0 };
        }
        forward.process(&mut frame, &mut spectrum).expect("fft size");
        let mut energy = 0.0f32;
        for k in 0..bins {
            magnitude[k] = spectrum[k].norm();
            phase[k] = spectrum[k].arg();
            // High bins weigh more, so onsets stand out over sustained bass.
            energy += magnitude[k] * magnitude[k] * k as f32;
        }
        let onset = prev_energy > 0.0 && energy > 4.0 * prev_energy;
        prev_energy = energy;

        match prev_at {
            Some(prev) if !onset => {
                let hop = (at - prev) as f32;
                peaks.clear();
                for k in 1..bins - 1 {
                    let m = magnitude[k];
                    if m > magnitude[k - 1] && m >= magnitude[k + 1] && (k < 2 || m > magnitude[k - 2]) {
                        peaks.push(k);
                    }
                }
                // Each peak advances by its measured frequency over the synthesis hop.
                for &k in &peaks {
                    let omega = 2.0 * PI * k as f32 / FFT as f32;
                    let deviation = wrap(phase[k] - prev_phase[k] - omega * hop);
                    let freq = omega + deviation / hop.max(1.0);
                    synth_phase[k] += freq * SYNTH_HOP as f32;
                }
                // Other bins follow the nearest peak, keeping their phase offset to it.
                if !peaks.is_empty() {
                    let mut p = 0;
                    for k in 0..bins {
                        while p + 1 < peaks.len() && peaks[p + 1].abs_diff(k) < peaks[p].abs_diff(k) {
                            p += 1;
                        }
                        let peak = peaks[p];
                        if k != peak {
                            synth_phase[k] = synth_phase[peak] + phase[k] - phase[peak];
                        }
                    }
                }
            }
            _ => synth_phase.copy_from_slice(&phase),
        }
        prev_phase.copy_from_slice(&phase);
        prev_at = Some(at);

        for k in 0..bins {
            spectrum[k] = Complex32::from_polar(magnitude[k], synth_phase[k]);
        }
        spectrum[0].im = 0.0;
        spectrum[bins - 1].im = 0.0;
        inverse.process(&mut spectrum, &mut frame).expect("fft size");
        for (i, (s, w)) in frame.iter().zip(&window).enumerate() {
            let idx = synth_at + i as i64;
            if idx >= 0 && (idx as usize) < out.len() {
                out[idx as usize] += s * w / FFT as f32;
                norm[idx as usize] += w * w;
            }
        }
        m += 1;
    }
    out.truncate(out_frames);
    for (o, n) in out.iter_mut().zip(&norm) {
        if *n > 1e-3 {
            *o /= n;
        }
    }
    out
}

fn wrap(x: f32) -> f32 {
    x - 2.0 * PI * ((x + PI) / (2.0 * PI)).floor()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, frames: usize) -> AudioData {
        let ch = (0..frames).map(|i| 0.5 * (2.0 * PI * freq * i as f32 / 48000.0).sin()).collect();
        AudioData { sample_rate: 48000, channels: vec![ch] }
    }

    /// Frequency from upward zero crossings in the middle of the signal.
    fn frequency(x: &[f32]) -> f32 {
        let mid = &x[x.len() / 4..3 * x.len() / 4];
        let crossings = mid.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        crossings as f32 * 48000.0 / mid.len() as f32
    }

    fn rms(x: &[f32]) -> f32 {
        let mid = &x[x.len() / 4..3 * x.len() / 4];
        (mid.iter().map(|v| v * v).sum::<f32>() / mid.len() as f32).sqrt()
    }

    #[test]
    fn stretching_keeps_pitch_and_level() {
        let input = sine(440.0, 48000);
        for ratio in [0.5, 0.8, 1.25, 2.0] {
            let out = stretch(&input, ratio, 1.0);
            assert_eq!(out.frames(), (48000.0 * ratio) as usize);
            let f = frequency(&out.channels[0]);
            assert!((f - 440.0).abs() < 3.0, "ratio {ratio}: {f} Hz");
            let level = rms(&out.channels[0]) / rms(&input.channels[0]);
            assert!((level - 1.0).abs() < 0.05, "ratio {ratio}: level {level}");
        }
    }

    #[test]
    fn pitch_shifting_keeps_length() {
        let input = sine(440.0, 48000);
        let up = stretch(&input, 1.0, 2f64.powf(7.0 / 12.0));
        assert_eq!(up.frames(), 48000);
        let f = frequency(&up.channels[0]);
        assert!((f - 659.3).abs() < 4.0, "{f} Hz");
        let both = stretch(&input, 1.5, 0.5);
        assert_eq!(both.frames(), 72000);
        assert!((frequency(&both.channels[0]) - 220.0).abs() < 3.0);
    }
}
