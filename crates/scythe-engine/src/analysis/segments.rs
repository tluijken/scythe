//! Finding the songs in a long backing-track or click file: stretches of sound between
//! silences, with where each one starts and stops, how loud it is and, for a click, its tempo.

use serde::{Deserialize, Serialize};

use super::{FLOOR_DB, Onsets, r2};
use crate::loudness::LoudnessScan;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SegmentOptions {
    /// Below this peak level (dBFS) counts as silence.
    pub threshold_db: f64,
    /// A silence at least this long (seconds) separates two songs.
    pub min_silence: f64,
    /// Sound shorter than this (seconds) is not a song (a stray click, a cough).
    pub min_length: f64,
}

impl Default for SegmentOptions {
    fn default() -> Self {
        Self { threshold_db: -50.0, min_silence: 2.0, min_length: 5.0 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    /// The first sample above the threshold, in seconds from the start of the file.
    pub start: f64,
    /// Just after the last sample above the threshold.
    pub end: f64,
    pub length: f64,
    pub peak_db: f64,
    pub loudness_lufs: f64,
    /// Attacks found in the segment.
    pub onsets: usize,
    /// The tempo, when the attacks come at a steady rate as a click's do.
    pub bpm: Option<f64>,
}

/// Where the songs of a file are.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segments {
    pub duration: f64,
    pub segments: Vec<Segment>,
    /// Sound too short to be a song, as `[start, end]` pairs.
    pub ignored: Vec<[f64; 2]>,
}

const WINDOW_SECONDS: f64 = 0.01;

pub fn find(channels: &[Vec<f32>], rate: u32, options: SegmentOptions) -> Segments {
    let frames = channels.first().map_or(0, Vec::len);
    let window = ((rate as f64 * WINDOW_SECONDS) as usize).max(1);
    let threshold = 10f64.powf(options.threshold_db / 20.0) as f32;
    let peak_at = |i: usize| channels.iter().map(|c| c[i].abs()).fold(0.0f32, f32::max);
    let loud: Vec<bool> = (0..frames.div_ceil(window))
        .map(|w| (w * window..((w + 1) * window).min(frames)).any(|i| peak_at(i) > threshold))
        .collect();
    let gap = (options.min_silence / WINDOW_SECONDS).ceil() as usize;
    // Runs of loud windows, joined across silences shorter than `gap`.
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut w = 0;
    while w < loud.len() {
        if !loud[w] {
            w += 1;
            continue;
        }
        let start = w;
        let mut end = w + 1;
        let mut quiet = 0;
        let mut i = w + 1;
        while i < loud.len() {
            if loud[i] {
                end = i + 1;
                quiet = 0;
            } else {
                quiet += 1;
                if quiet >= gap {
                    break;
                }
            }
            i += 1;
        }
        spans.push((start, end));
        w = end + quiet.max(1);
    }

    let seconds = |frame: usize| frame as f64 / rate as f64;
    let mut segments = Vec::new();
    let mut ignored = Vec::new();
    for (a, b) in spans {
        let first = (a * window..(b * window).min(frames)).find(|&i| peak_at(i) > threshold).unwrap_or(a * window);
        let last = (a * window..(b * window).min(frames)).rev().find(|&i| peak_at(i) > threshold).unwrap_or(first);
        let (start, end) = (seconds(first), seconds(last + 1));
        if end - start < options.min_length {
            ignored.push([r2(start), r2(end)]);
            continue;
        }
        segments.push(measure(channels, rate, first, last + 1, start, end));
    }
    Segments { duration: r2(seconds(frames)), segments, ignored }
}

fn measure(channels: &[Vec<f32>], rate: u32, from: usize, to: usize, start: f64, end: f64) -> Segment {
    let left = &channels[0][from..to];
    let right = channels.get(1).map_or(left, |c| &c[from..to]);
    let mut scan = LoudnessScan::new(rate);
    scan.push(left, right);
    let mut onsets = Onsets::new(rate);
    let mut peak = 0.0f32;
    for (&l, &r) in left.iter().zip(right) {
        onsets.push(l, r);
        peak = peak.max(l.abs()).max(r.abs());
    }
    let integrated = scan.integrated();
    Segment {
        start: (start * 1000.0).round() / 1000.0,
        end: (end * 1000.0).round() / 1000.0,
        length: r2(end - start),
        peak_db: r2(if peak > 0.0 { 20.0 * (peak as f64).log10() } else { FLOOR_DB }),
        loudness_lufs: r2(if integrated.is_finite() { integrated } else { FLOOR_DB }),
        onsets: onsets.times.len(),
        bpm: steady_tempo(&onsets.times),
    }
}

/// 60 over the typical time between attacks, when nearly all of them keep to it.
fn steady_tempo(times: &[f64]) -> Option<f64> {
    if times.len() < 8 {
        return None;
    }
    let mut gaps: Vec<f64> = times.windows(2).map(|w| w[1] - w[0]).collect();
    gaps.sort_by(f64::total_cmp);
    let median = gaps[gaps.len() / 2];
    let steady = gaps.iter().filter(|g| ((*g - median) / median).abs() < 0.05).count();
    if steady * 10 < gaps.len() * 8 || median <= 0.0 {
        return None;
    }
    let mean = gaps.iter().filter(|g| ((*g - median) / median).abs() < 0.05).sum::<f64>() / steady as f64;
    let bpm = 60.0 / mean;
    (30.0..=300.0).contains(&bpm).then(|| (bpm * 10.0).round() / 10.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three "songs" of clicks at different tempos with silences between, as a click file for a
    /// set would have.
    #[test]
    fn finds_songs_and_their_tempo_in_a_click_file() {
        let rate = 48_000;
        let mut signal = vec![0.0f32; rate as usize];
        let mut expected = Vec::new();
        for (bpm, beats) in [(120.0, 40), (90.0, 30), (150.0, 50)] {
            let start = signal.len();
            let period = (60.0 / bpm * rate as f64) as usize;
            for beat in 0..beats {
                let mut block = vec![0.0f32; period];
                for (i, s) in block.iter_mut().take(480).enumerate() {
                    *s = 0.5
                        * (std::f32::consts::TAU * 1500.0 * i as f32 / rate as f32).sin()
                        * (1.0 - i as f32 / 480.0);
                }
                if beat == beats - 1 {
                    block.truncate(480);
                }
                signal.extend(block);
            }
            expected.push((start as f64 / rate as f64, bpm));
            signal.extend(vec![0.0f32; 4 * rate as usize]);
        }
        signal.extend(vec![0.0f32; 100]);
        let stray = signal.len() - 50;
        signal[stray] = 0.3;
        let found = find(&[signal.clone(), signal], rate, SegmentOptions::default());
        assert_eq!(found.segments.len(), 3, "{found:?}");
        for (segment, (start, bpm)) in found.segments.iter().zip(expected) {
            assert!((segment.start - start).abs() < 0.002, "{segment:?} against {start}");
            let got = segment.bpm.unwrap_or_else(|| panic!("no tempo in {segment:?}"));
            assert!((got - bpm).abs() < 0.5, "{got} against {bpm}");
        }
        assert_eq!(found.ignored.len(), 1);
        assert!((found.segments[0].length - (39.0 * 0.5 + 0.01)).abs() < 0.02, "{:?}", found.segments[0]);
    }
}
