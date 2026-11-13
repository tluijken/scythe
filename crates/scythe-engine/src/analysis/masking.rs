//! Masking between instrument groups: which group buries which, in what part of the spectrum
//! and for how much of the time they play together. Each group is rendered on its own and cut
//! into short frames of third-octave levels from 40 Hz to 10 kHz ([`Spectrogram`]). In a frame,
//! a group's important bands are those no more than `importance_db` under its own loudest band;
//! it is buried in a region by another group when, in at least half of its important bands
//! there, the other is more than `margin_db` louder.

use std::sync::Arc;

use realfft::num_complex::Complex32;
use realfft::{RealFftPlanner, RealToComplex};
use serde::{Deserialize, Serialize};

use super::{FLOOR_DB, band_centres, band_edges, r2};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MaskingOptions {
    /// How much louder the other group must be in a band to bury it, in dB.
    pub margin_db: f64,
    /// Bands no more than this under the group's loudest band in a frame are important.
    pub importance_db: f64,
    /// Frame length in milliseconds.
    pub frame_ms: f64,
}

impl Default for MaskingOptions {
    fn default() -> Self {
        Self { margin_db: 3.0, importance_db: 12.0, frame_ms: 100.0 }
    }
}

/// A group plays in a frame when its loudest band is above this level, in dBFS.
pub const PLAYING_DB: f64 = -60.0;

/// Nor when it is more than this many dB under its own loud frames.
pub const PLAYING_RANGE_DB: f64 = 24.0;

/// Pairs that play together for less than this many seconds are left out.
const MIN_TOGETHER: f64 = 1.0;

/// Pairs buried for less than this share of their time together are left out.
const MIN_SHARE: f64 = 0.05;

/// From this share of the time together, a burying makes the summary.
const NOTABLE_SHARE: f64 = 0.25;

/// At most this many pairs are reported, the most significant.
pub const MAX_PAIRS: usize = 50;

/// The regions results are reported in, by the centres of their first and last band.
const REGIONS: [(&str, f64, f64); 5] = [
    ("low (40-125 Hz)", 40.0, 125.0),
    ("low mids (160-400 Hz)", 160.0, 400.0),
    ("mids (500 Hz-2 kHz)", 500.0, 2000.0),
    ("presence (2.5-5 kHz)", 2500.0, 5000.0),
    ("highs (6.3-10 kHz)", 6300.0, 10_000.0),
];

/// The third-octave centres analysed: 40 Hz to 10 kHz.
pub fn centres() -> Vec<f64> {
    band_centres().into_iter().filter(|c| (39.0..=10_100.0).contains(c)).collect()
}

/// Indices into [`centres`] of each region's bands.
fn region_bands(centres: &[f64], lo: f64, hi: f64) -> Vec<usize> {
    (0..centres.len()).filter(|&k| centres[k] >= lo * 0.95 && centres[k] <= hi * 1.05).collect()
}

/// Third-octave levels per frame, one group's render cut up as [`Spectrogram`] does it.
#[derive(Debug, Clone, PartialEq)]
pub struct Levels {
    bands: usize,
    /// Frame after frame, `bands` levels in dBFS each.
    db: Vec<f64>,
}

impl Levels {
    pub fn frames(&self) -> usize {
        self.db.len() / self.bands.max(1)
    }

    pub fn frame(&self, i: usize) -> &[f64] {
        &self.db[i * self.bands..(i + 1) * self.bands]
    }

    /// Per frame whether the group plays: its loudest band above [`PLAYING_DB`] and no more
    /// than [`PLAYING_RANGE_DB`] under its loud frames (the 95th percentile), so bleed through
    /// a closed gate or a reverb tail does not count as playing.
    fn plays(&self) -> Vec<bool> {
        let loudest: Vec<f64> =
            (0..self.frames()).map(|i| self.frame(i).iter().copied().fold(FLOOR_DB, f64::max)).collect();
        let mut sorted = loudest.clone();
        sorted.sort_by(f64::total_cmp);
        let loud = sorted.get(sorted.len() * 95 / 100).copied().unwrap_or(FLOOR_DB);
        let threshold = PLAYING_DB.max(loud - PLAYING_RANGE_DB);
        loudest.into_iter().map(|l| l > threshold).collect()
    }
}

/// Cuts audio pushed block by block into frames of `frame_ms` and measures the third-octave
/// levels of each (the mean of both channels' power), from a Hann window of at least the
/// frame's length centred on it.
pub struct Spectrogram {
    hop: usize,
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    /// Turns summed bin power into the signal's mean square.
    scale: f64,
    /// Per band, the bins in it and how much of each.
    weights: Vec<Vec<(usize, f64)>>,
    fill: [Vec<f32>; 2],
    /// Where the next frame's window starts in `fill`.
    next: usize,
    samples: u64,
    input: Vec<f32>,
    spectrum: Vec<Complex32>,
    power: Vec<f64>,
    db: Vec<f64>,
}

impl Spectrogram {
    pub fn new(rate: u32, frame_ms: f64) -> Self {
        let hop = ((frame_ms / 1000.0 * rate as f64).round() as usize).max(16);
        let len = hop.next_power_of_two().max(4096);
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(len);
        let window: Vec<f32> =
            (0..len).map(|i| (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / len as f64).cos()) as f32).collect();
        let window_power: f64 = window.iter().map(|w| (*w as f64).powi(2)).sum();
        let bin_hz = rate as f64 / len as f64;
        let weights = centres()
            .into_iter()
            .map(|c| {
                let (lo, hi) = band_edges(c);
                (1..len / 2)
                    .filter_map(|j| {
                        let (a, b) = ((j as f64 - 0.5) * bin_hz, (j as f64 + 0.5) * bin_hz);
                        let overlap = (b.min(hi) - a.max(lo)) / bin_hz;
                        (overlap > 0.0).then_some((j, overlap))
                    })
                    .collect()
            })
            .collect();
        // Centres the first window on the first frame.
        let lead = (len / 2).saturating_sub(hop / 2);
        Self {
            hop,
            input: fft.make_input_vec(),
            spectrum: fft.make_output_vec(),
            fft,
            window,
            scale: 2.0 / (len as f64 * window_power),
            weights,
            fill: [vec![0.0; lead], vec![0.0; lead]],
            next: 0,
            samples: 0,
            power: Vec::new(),
            db: Vec::new(),
        }
    }

    fn len(&self) -> usize {
        self.window.len()
    }

    pub fn push(&mut self, left: &[f32], right: &[f32]) {
        let n = left.len().min(right.len());
        self.fill[0].extend_from_slice(&left[..n]);
        self.fill[1].extend_from_slice(&right[..n]);
        self.samples += n as u64;
        self.drain();
    }

    fn drain(&mut self) {
        while self.fill[0].len() >= self.next + self.len() {
            self.transform();
            self.next += self.hop;
        }
        if self.next > 4 * self.len() {
            self.fill[0].drain(..self.next);
            self.fill[1].drain(..self.next);
            self.next = 0;
        }
    }

    fn transform(&mut self) {
        self.power.clear();
        self.power.resize(self.weights.len(), 0.0);
        for c in 0..2 {
            let frame = &self.fill[c][self.next..self.next + self.window.len()];
            for ((x, s), w) in self.input.iter_mut().zip(frame).zip(&self.window) {
                *x = s * w;
            }
            let _ = self.fft.process(&mut self.input, &mut self.spectrum);
            for (p, band) in self.power.iter_mut().zip(&self.weights) {
                *p += band.iter().map(|&(j, w)| w * self.spectrum[j].norm_sqr() as f64).sum::<f64>();
            }
        }
        let scale = self.scale / 2.0;
        self.db.extend(self.power.iter().map(|p| {
            let ms = p * scale;
            if ms > 1e-15 { 10.0 * ms.log10() } else { FLOOR_DB }
        }));
    }

    /// The levels of every whole frame pushed.
    pub fn finish(mut self) -> Levels {
        let bands = self.weights.len();
        let frames = (self.samples / self.hop as u64) as usize;
        while self.db.len() / bands < frames {
            let pad = self.len();
            self.fill[0].resize(self.fill[0].len() + pad, 0.0);
            self.fill[1].resize(self.fill[1].len() + pad, 0.0);
            self.drain();
        }
        self.db.truncate(frames * bands);
        Levels { bands, db: self.db }
    }
}

/// A stretch of project time.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Span {
    pub start: f64,
    pub end: f64,
}

/// One group buried by another in one region.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Burying {
    pub buried: String,
    pub by: String,
    pub region: String,
    pub from_hz: f64,
    pub to_hz: f64,
    /// Of the frames where both play and `buried` has important bands in the region, the
    /// share where it is buried, in percent.
    pub share_pct: f64,
    /// Seconds of those frames.
    pub together_seconds: f64,
    pub buried_seconds: f64,
    /// The longest unbroken stretch where it is buried.
    pub longest: Span,
}

/// A group's average third-octave levels over the frames where it plays.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupSpectrum {
    pub name: String,
    pub plays_seconds: f64,
    /// Per band in [`Masking::bands_hz`], dBFS (mean power).
    pub level_db: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Masking {
    pub frame_ms: f64,
    pub margin_db: f64,
    pub importance_db: f64,
    /// Seconds analysed.
    pub duration: f64,
    pub bands_hz: Vec<f64>,
    /// Most significant first: by share, weighted by the square root of the time together.
    pub pairs: Vec<Burying>,
    /// Less significant pairs beyond the first [`MAX_PAIRS`], left out.
    pub pairs_left_out: usize,
    pub groups: Vec<GroupSpectrum>,
    /// What stands out, in plain words.
    pub summary: Vec<String>,
}

/// Who buries whom among `groups`, each measured over the same range starting at `start`
/// seconds of project time.
pub fn analyse(groups: &[(String, Levels)], start: f64, options: MaskingOptions) -> Masking {
    let hop = options.frame_ms / 1000.0;
    let frames = groups.iter().map(|(_, l)| l.frames()).min().unwrap_or(0);
    let centres = centres();
    let plays: Vec<Vec<bool>> = groups.iter().map(|(_, l)| l.plays().into_iter().take(frames).collect()).collect();
    let at = |frame: usize| r2(start + frame as f64 * hop);

    let mut pairs = Vec::new();
    for (a, (name_a, la)) in groups.iter().enumerate() {
        for (b, (name_b, lb)) in groups.iter().enumerate() {
            if a == b {
                continue;
            }
            for &(region, lo, hi) in &REGIONS {
                let bands = region_bands(&centres, lo, hi);
                let (mut together, mut buried) = (0usize, 0usize);
                let (mut run, mut longest) = (None::<usize>, (0usize, 0usize));
                for (i, (&plays_a, &plays_b)) in plays[a].iter().zip(&plays[b]).enumerate() {
                    let mut hit = false;
                    if plays_a && plays_b {
                        let (fa, fb) = (la.frame(i), lb.frame(i));
                        let own = fa.iter().copied().fold(FLOOR_DB, f64::max);
                        let important: Vec<usize> =
                            bands.iter().copied().filter(|&k| fa[k] > own - options.importance_db).collect();
                        if !important.is_empty() {
                            together += 1;
                            let louder = important.iter().filter(|&&k| fb[k] > fa[k] + options.margin_db).count();
                            hit = 2 * louder >= important.len();
                        }
                    }
                    if hit {
                        buried += 1;
                        let from = *run.get_or_insert(i);
                        if i + 1 - from > longest.1 - longest.0 {
                            longest = (from, i + 1);
                        }
                    } else {
                        run = None;
                    }
                }
                let share = if together > 0 { buried as f64 / together as f64 } else { 0.0 };
                if together as f64 * hop < MIN_TOGETHER || share < MIN_SHARE {
                    continue;
                }
                pairs.push(Burying {
                    buried: name_a.clone(),
                    by: name_b.clone(),
                    region: region.into(),
                    from_hz: lo,
                    to_hz: hi,
                    share_pct: r2(100.0 * share),
                    together_seconds: r2(together as f64 * hop),
                    buried_seconds: r2(buried as f64 * hop),
                    longest: Span { start: at(longest.0), end: at(longest.1) },
                });
            }
        }
    }
    let weight = |p: &Burying| p.share_pct * p.together_seconds.sqrt();
    pairs.sort_by(|p, q| weight(q).total_cmp(&weight(p)));
    let pairs_left_out = pairs.len().saturating_sub(MAX_PAIRS);
    pairs.truncate(MAX_PAIRS);

    let spectra: Vec<GroupSpectrum> = groups
        .iter()
        .zip(&plays)
        .map(|((name, l), plays)| {
            let on: Vec<usize> = (0..frames).filter(|&i| plays[i]).collect();
            let level_db = (0..centres.len())
                .map(|k| {
                    let mean =
                        on.iter().map(|&i| 10f64.powf(l.frame(i)[k] / 10.0)).sum::<f64>() / on.len().max(1) as f64;
                    r2(if mean > 1e-15 { 10.0 * mean.log10() } else { FLOOR_DB })
                })
                .collect();
            GroupSpectrum { name: name.clone(), plays_seconds: r2(on.len() as f64 * hop), level_db }
        })
        .collect();

    let mut summary: Vec<String> = pairs
        .iter()
        .filter(|p| p.share_pct >= 100.0 * NOTABLE_SHARE)
        .take(6)
        .map(|p| {
            format!(
                "{} buries {} in the {} {:.0}% of the {:.0} s they play together (longest {:.1}-{:.1} s)",
                p.by, p.buried, p.region, p.share_pct, p.together_seconds, p.longest.start, p.longest.end
            )
        })
        .collect();
    if summary.is_empty() {
        summary.push(format!(
            "No group is buried by another for more than {:.0}% of the time they play together.",
            100.0 * NOTABLE_SHARE
        ));
    }
    for g in spectra.iter().filter(|g| g.plays_seconds == 0.0) {
        summary.push(format!("{} is silent over the range.", g.name));
    }

    Masking {
        frame_ms: options.frame_ms,
        margin_db: options.margin_db,
        importance_db: options.importance_db,
        duration: r2(frames as f64 * hop),
        bands_hz: centres.iter().map(|&c| r2(c)).collect(),
        pairs,
        pairs_left_out,
        groups: spectra,
        summary,
    }
}

#[cfg(test)]
mod tests {
    use super::super::signals::{band_noise, harmonic, sine};
    use super::*;

    const RATE: u32 = 48_000;

    fn levels(signal: &[f32]) -> Levels {
        let mut s = Spectrogram::new(RATE, 100.0);
        for block in signal.chunks(512) {
            s.push(block, block);
        }
        s.finish()
    }

    #[test]
    fn a_sine_reads_its_level_in_its_band() {
        let l = levels(&sine(250.0, 0.5, 2.0, RATE));
        assert_eq!(l.frames(), 20);
        let k = centres().iter().position(|c| (c - 251.2).abs() < 1.0).unwrap();
        for i in 2..l.frames() - 2 {
            assert!((l.frame(i)[k] - (20.0 * 0.5f64.log10() - 3.01)).abs() < 0.3, "{:?}", l.frame(i));
        }
        assert!(l.frame(10)[k + 3] < l.frame(10)[k] - 40.0);
    }

    #[test]
    fn a_loud_low_tone_buries_band_noise_in_the_low_mids_only() {
        let bass = harmonic(80.0, 6, 0.5, 10.0, RATE);
        let guitar = band_noise(150.0, 1500.0, 0.3, 10.0, RATE, 7);
        let mut hats = band_noise(8000.0, 16_000.0, 0.3, 10.0, RATE, 8);
        hats[RATE as usize * 5..].fill(0.0);
        let groups = vec![
            ("bass".to_owned(), levels(&bass)),
            ("guitar".to_owned(), levels(&guitar)),
            ("hats".into(), levels(&hats)),
        ];
        let m = analyse(&groups, 30.0, MaskingOptions::default());
        let top = &m.pairs[0];
        assert_eq!(
            (top.buried.as_str(), top.by.as_str(), top.region.as_str()),
            ("guitar", "bass", "low mids (160-400 Hz)")
        );
        assert!(top.share_pct > 90.0 && (top.together_seconds - 10.0).abs() < 0.2, "{top:?}");
        assert!(top.longest.start <= 30.1 && top.longest.end >= 39.9, "{top:?}");
        assert!(m.pairs.iter().all(|p| p.buried != "hats" && p.by != "hats"), "{:?}", m.pairs);
        assert!(!m.pairs.iter().any(|p| p.buried == "bass" && p.by == "guitar"), "{:?}", m.pairs);
        assert!(m.summary[0].starts_with("bass buries guitar in the low mids"), "{:?}", m.summary);
        let hats = m.groups.iter().find(|g| g.name == "hats").unwrap();
        assert!((hats.plays_seconds - 5.0).abs() < 0.2, "{hats:?}");
        assert_eq!(m.bands_hz.len(), 25);
    }
}
