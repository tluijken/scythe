//! Fitting a few ReaEQ bands to the tonal difference between a mix and a reference: a
//! smoothed curve with limited gain, so matching moves a mix towards the reference's balance
//! without copying its every resonance.

use serde::{Deserialize, Serialize};

use super::{Analysis, FLOOR_DB, band_centres, r2, smooth};
use crate::reaeq::{self, Band, BandKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EqKind {
    LowShelf,
    HighShelf,
    Peak,
}

impl EqKind {
    /// ReaEQ's band type code.
    pub fn code(self) -> u32 {
        match self {
            Self::LowShelf => 0,
            Self::HighShelf => 1,
            Self::Peak => 8,
        }
    }

    fn kind(self) -> BandKind {
        match self {
            Self::LowShelf => BandKind::LowShelf,
            Self::HighShelf => BandKind::HighShelf,
            Self::Peak => BandKind::Peak,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EqBand {
    pub kind: EqKind,
    pub freq: f64,
    pub gain_db: f64,
    /// Bandwidth in octaves (ReaEQ's; for shelves it sets the slope).
    pub bandwidth: f64,
}

impl EqBand {
    pub fn reaeq(&self) -> Band {
        Band {
            kind: self.kind.kind(),
            enabled: true,
            freq: self.freq,
            gain: 10f64.powf(self.gain_db / 20.0),
            bandwidth: self.bandwidth,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MatchOptions {
    pub max_bands: usize,
    /// No band boosts or cuts more than this.
    pub max_gain_db: f64,
    pub min_hz: f64,
    pub max_hz: f64,
}

impl Default for MatchOptions {
    fn default() -> Self {
        Self { max_bands: 6, max_gain_db: 6.0, min_hz: 30.0, max_hz: 16_000.0 }
    }
}

/// The hard limit on any band's gain, whatever is asked.
pub const SAFE_GAIN_DB: f64 = 12.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CurvePoint {
    pub hz: f64,
    /// The tonal difference to correct (the reference less the mix), without its average.
    pub target_db: f64,
    /// What the proposed bands do there.
    pub eq_db: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EqProposal {
    pub bands: Vec<EqBand>,
    /// The largest difference left to correct in the fitted range, before and after.
    pub max_error_before_db: f64,
    pub max_error_after_db: f64,
    pub rms_error_before_db: f64,
    pub rms_error_after_db: f64,
    pub curve: Vec<CurvePoint>,
}

/// Each band's gain at the third-octave centres, in dB, before smoothing.
fn response(band: &EqBand, rate: f64, centres: &[f64]) -> Vec<f64> {
    let b = band.reaeq();
    centres.iter().map(|&f| if f < rate * 0.49 { reaeq::response_db(&b, f, rate) } else { 0.0 }).collect()
}

/// The bands' combined effect as the analysis would see it: summed in dB and smoothed like the
/// tonal curve.
fn effect(bands: &[EqBand], rate: f64, centres: &[f64]) -> Vec<f64> {
    let mut total = vec![0.0; centres.len()];
    for band in bands {
        for (t, r) in total.iter_mut().zip(response(band, rate, centres)) {
            *t += r;
        }
    }
    smooth(&total)
}

struct Fit<'a> {
    target: &'a [f64],
    weight: &'a [f64],
    rate: f64,
    centres: &'a [f64],
}

impl Fit<'_> {
    /// The squared error of `bands` and what is left to correct, with the best constant offset
    /// taken out (EQ is matched by shape; the level is the fader's job).
    fn error(&self, bands: &[EqBand]) -> (f64, Vec<f64>) {
        let (sq, residual, _) = self.error_and_offset(bands);
        (sq, residual)
    }

    /// [`Self::error`] and the offset.
    fn error_and_offset(&self, bands: &[EqBand]) -> (f64, Vec<f64>, f64) {
        let e = effect(bands, self.rate, self.centres);
        let wsum: f64 = self.weight.iter().sum();
        let offset =
            self.target.iter().zip(&e).zip(self.weight).map(|((t, e), w)| w * (t - e)).sum::<f64>() / wsum.max(1e-12);
        let residual: Vec<f64> = self.target.iter().zip(&e).map(|(t, e)| t - e - offset).collect();
        let sq = residual.iter().zip(self.weight).map(|(r, w)| w * r * r).sum::<f64>();
        (sq, residual, offset)
    }

    /// Gauss-Newton on the gains, keeping frequencies and widths.
    fn refine(&self, bands: &mut [EqBand], max_gain: f64) {
        for _ in 0..8 {
            let (base, residual) = self.error(bands);
            let n = bands.len();
            let step = 0.5;
            let columns: Vec<Vec<f64>> = (0..n)
                .map(|i| {
                    let mut probe = bands.to_vec();
                    probe[i].gain_db += step;
                    let (_, r) = self.error(&probe);
                    residual.iter().zip(&r).map(|(a, b)| (a - b) / step).collect()
                })
                .collect();
            let mut jtj = vec![vec![0.0; n]; n];
            let mut jtr = vec![0.0; n];
            for i in 0..n {
                for k in 0..self.weight.len() {
                    jtr[i] += self.weight[k] * columns[i][k] * residual[k];
                    for j in 0..n {
                        jtj[i][j] += self.weight[k] * columns[i][k] * columns[j][k];
                    }
                }
                jtj[i][i] += 1e-6;
            }
            let Some(delta) = solve(jtj, jtr) else { break };
            let before: Vec<f64> = bands.iter().map(|b| b.gain_db).collect();
            for (b, d) in bands.iter_mut().zip(&delta) {
                b.gain_db = (b.gain_db + d).clamp(-max_gain, max_gain);
            }
            let (after, _) = self.error(bands);
            if after >= base {
                for (b, g) in bands.iter_mut().zip(before) {
                    b.gain_db = g;
                }
                break;
            }
            if base - after < 1e-6 {
                break;
            }
        }
    }
}

/// Solves `a x = b` by Gaussian elimination with partial pivoting.
fn solve(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for col in 0..n {
        let pivot = (col..n).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[pivot][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);
        for row in col + 1..n {
            let f = a[row][col] / a[col][col];
            let pivot_row = a[col].clone();
            for (x, p) in a[row][col..].iter_mut().zip(&pivot_row[col..]) {
                *x -= f * p;
            }
            b[row] -= f * b[col];
        }
    }
    let mut x = vec![0.0; n];
    for row in (0..n).rev() {
        let s: f64 = (row + 1..n).map(|k| a[row][k] * x[k]).sum();
        x[row] = (b[row] - s) / a[row][row];
    }
    Some(x)
}

fn candidates(min_hz: f64, max_hz: f64, rate: f64) -> Vec<EqBand> {
    let band = |kind, freq, bandwidth| EqBand { kind, freq, gain_db: 0.0, bandwidth };
    let mut out = Vec::new();
    for f in [50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0] {
        out.push(band(EqKind::LowShelf, f, 1.0));
    }
    for f in [2500.0, 3150.0, 4000.0, 5000.0, 6300.0, 8000.0, 10_000.0, 12_500.0] {
        if f < rate * 0.4 {
            out.push(band(EqKind::HighShelf, f, 1.0));
        }
    }
    for f in band_centres() {
        if f >= min_hz.max(31.0) && f <= max_hz.min(rate * 0.4) {
            for bw in [0.7, 1.4, 2.4] {
                out.push(band(EqKind::Peak, f, bw));
            }
        }
    }
    out
}

/// Bands that move `mix`'s tonal balance towards `reference`'s, at most `max_bands` of them
/// and none beyond `max_gain_db` (itself at most [`SAFE_GAIN_DB`]).
pub fn match_eq(reference: &Analysis, mix: &Analysis, options: MatchOptions) -> EqProposal {
    let centres = band_centres();
    let rate = mix.sample_rate as f64;
    let max_gain = options.max_gain_db.clamp(0.0, SAFE_GAIN_DB);
    let usable = |a: f64, b: f64| a > FLOOR_DB && b > FLOOR_DB;
    let weight: Vec<f64> = centres
        .iter()
        .zip(reference.bands.iter().zip(&mix.bands))
        .map(|(&f, (r, m))| {
            let inside = f >= options.min_hz && f <= options.max_hz && f < rate * 0.45;
            if inside && usable(r.tonal_db, m.tonal_db) { 1.0 } else { 0.0 }
        })
        .collect();
    let target: Vec<f64> = reference
        .bands
        .iter()
        .zip(&mix.bands)
        .map(|(r, m)| if usable(r.tonal_db, m.tonal_db) { r.tonal_db - m.tonal_db } else { 0.0 })
        .collect();
    let fit = Fit { target: &target, weight: &weight, rate, centres: &centres };
    let mut bands: Vec<EqBand> = Vec::new();
    let (start, start_residual) = fit.error(&bands);
    let mut current = start;
    let pool = candidates(options.min_hz, options.max_hz, rate);
    if weight.iter().sum::<f64>() > 0.0 {
        while bands.len() < options.max_bands.min(12) {
            let (_, residual) = fit.error(&bands);
            if max_abs(&residual, &weight) < 0.25 {
                break;
            }
            // The candidate whose best gain on its own removes the most error.
            let best = pool
                .iter()
                .filter(|c| !bands.iter().any(|b| b.kind == c.kind && (b.freq / c.freq).log2().abs() < 0.3))
                .filter_map(|c| {
                    let shape = response(&EqBand { gain_db: 3.0, ..*c }, rate, &centres);
                    let shape: Vec<f64> = smooth(&shape).iter().map(|s| s / 3.0).collect();
                    let num: f64 = shape.iter().zip(&residual).zip(&weight).map(|((s, r), w)| w * s * r).sum();
                    let den: f64 = shape.iter().zip(&weight).map(|(s, w)| w * s * s).sum();
                    (den > 1e-9).then(|| (num / den, num * num / den, c))
                })
                .max_by(|a, b| a.1.total_cmp(&b.1));
            let Some((gain, _, c)) = best else { break };
            let mut next = bands.clone();
            next.push(EqBand { gain_db: gain.clamp(-max_gain, max_gain), ..*c });
            fit.refine(&mut next, max_gain);
            let (err, _) = fit.error(&next);
            if err > current * 0.97 {
                break;
            }
            bands = next;
            current = err;
        }
    }
    bands.retain(|b| b.gain_db.abs() >= 0.1);
    for b in &mut bands {
        b.gain_db = r2(b.gain_db);
    }
    let (_, residual, offset) = fit.error_and_offset(&bands);
    let e = effect(&bands, rate, &centres);
    let curve = centres
        .iter()
        .zip(&target)
        .zip(&e)
        .zip(&weight)
        .filter(|(_, w)| **w > 0.0)
        .map(|(((&hz, t), e), _)| CurvePoint { hz: r2(hz), target_db: r2(t - offset), eq_db: r2(*e) })
        .collect();
    bands.sort_by(|a, b| a.freq.total_cmp(&b.freq));
    EqProposal {
        bands,
        max_error_before_db: r2(max_abs(&start_residual, &weight)),
        max_error_after_db: r2(max_abs(&residual, &weight)),
        rms_error_before_db: r2(rms(&start_residual, &weight)),
        rms_error_after_db: r2(rms(&residual, &weight)),
        curve,
    }
}

fn max_abs(values: &[f64], weight: &[f64]) -> f64 {
    values.iter().zip(weight).filter(|(_, w)| **w > 0.0).map(|(v, _)| v.abs()).fold(0.0, f64::max)
}

fn rms(values: &[f64], weight: &[f64]) -> f64 {
    let wsum: f64 = weight.iter().sum();
    if wsum <= 0.0 {
        return 0.0;
    }
    (values.iter().zip(weight).map(|(v, w)| w * v * v).sum::<f64>() / wsum).sqrt()
}

/// What a compressor and limiter could do to bring the mix's loudness and peaks towards the
/// reference's, kept within safe steps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DynamicsAdvice {
    /// Loudness to gain, in LU (the reference less the mix).
    pub loudness_gap_lu: f64,
    /// Gain into a limiter for this step, at most 6 dB.
    pub limiter_drive_db: f64,
    /// Where the limiter should hold true peaks.
    pub limiter_ceiling_db: f64,
    /// Bus compression for a mix much more dynamic than the reference, if any.
    pub compressor: Option<CompressorAdvice>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompressorAdvice {
    pub ratio: f64,
    pub attack_ms: f64,
    pub release_ms: f64,
    /// Gain reduction to aim for on the loud parts.
    pub target_reduction_db: f64,
}

/// The largest single step of limiter drive suggested.
pub const MAX_DRIVE_DB: f64 = 6.0;

pub fn dynamics_advice(reference: &Analysis, mix: &Analysis) -> DynamicsAdvice {
    let gap = reference.integrated_lufs - mix.integrated_lufs;
    let ceiling = reference.true_peak_dbtp.clamp(-2.0, -1.0);
    let headroom = ceiling - mix.true_peak_dbtp;
    // Raising the mix by `gap` pushes its peaks over the ceiling by `gap - headroom`, which the
    // limiter takes off; the loudness is what it adds.
    let drive = if gap > 0.5 { gap.min(MAX_DRIVE_DB) } else { 0.0 };
    let mut notes = Vec::new();
    if gap > MAX_DRIVE_DB {
        notes.push(format!(
            "The mix is {gap:.1} LU quieter than the reference; drive the limiter {MAX_DRIVE_DB:.0} dB now and measure again rather than in one go."
        ));
    } else if gap < -0.5 {
        notes.push(format!(
            "The mix is {:.1} LU louder than the reference: lower the master or the limiter's drive.",
            -gap
        ));
    }
    let crest_gap = mix.plr_db - reference.plr_db;
    let compressor = (crest_gap > 3.0).then(|| CompressorAdvice {
        ratio: if crest_gap > 6.0 { 3.0 } else { 2.0 },
        attack_ms: 30.0,
        release_ms: 150.0,
        target_reduction_db: r2((crest_gap / 2.0).clamp(1.0, 4.0)),
    });
    if compressor.is_some() {
        notes.push(format!(
            "The mix's peak-to-loudness ratio is {crest_gap:.1} dB above the reference's: gentle bus compression before the limiter keeps the limiter from working too hard."
        ));
    }
    if drive > 0.0 && drive - headroom > 4.0 {
        notes.push(format!(
            "About {:.1} dB of limiting would be needed; listen for pumping and dulled transients.",
            drive - headroom
        ));
    }
    DynamicsAdvice {
        loudness_gap_lu: r2(gap),
        limiter_drive_db: r2(drive),
        limiter_ceiling_db: r2(ceiling),
        compressor,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::analyse;
    use crate::analysis::signals::pink;
    use crate::fx::Effect;
    use crate::reaeq::ReaEqCompat;

    const RATE: u32 = 48_000;

    fn equalise(signal: &[f32], bands: &[EqBand]) -> Vec<f32> {
        let mut eq = ReaEqCompat::new(bands.iter().map(EqBand::reaeq).collect(), RATE);
        let (mut l, mut r) = (signal.to_vec(), signal.to_vec());
        let ctx = crate::fx::ProcessContext {
            sample_rate: RATE,
            position: 0,
            playing: true,
            notes: None,
            live: &[],
            live_channel: None,
            aux: None,
            bpm: 120.0,
            midi: None,
            midi_from: 0,
        };
        for (a, b) in l.chunks_mut(1024).zip(r.chunks_mut(1024)) {
            eq.process(a, b, &ctx);
        }
        l
    }

    /// A reference made by equalising pink noise with ReaEQ: matching the plain noise to it
    /// finds bands that bring the tonal balance within 0.5 dB, and applying them gets there.
    #[test]
    fn matching_converges_on_an_equalised_reference() {
        let noise = pink(30.0, RATE, 11);
        let applied = [
            EqBand { kind: EqKind::LowShelf, freq: 120.0, gain_db: 4.0, bandwidth: 1.0 },
            EqBand { kind: EqKind::Peak, freq: 2500.0, gain_db: -3.0, bandwidth: 1.0 },
            EqBand { kind: EqKind::HighShelf, freq: 8000.0, gain_db: 2.5, bandwidth: 1.0 },
        ];
        let shaped = equalise(&noise, &applied);
        let reference = analyse(&[shaped.clone(), shaped], RATE, None, None);
        let mix = analyse(&[noise.clone(), noise.clone()], RATE, None, None);
        let proposal = match_eq(&reference, &mix, MatchOptions::default());
        assert!(proposal.max_error_before_db > 3.0, "{proposal:?}");
        assert!(proposal.max_error_after_db <= 0.5, "{proposal:?}");
        assert!(proposal.bands.len() <= 6 && proposal.bands.iter().all(|b| b.gain_db.abs() <= 6.0));

        let matched = equalise(&noise, &proposal.bands);
        let after = analyse(&[matched.clone(), matched], RATE, None, None);
        let c = crate::analysis::compare(&reference, &after);
        let inside: Vec<f64> =
            c.bands.iter().filter(|b| (30.0..=16_000.0).contains(&b.hz)).map(|b| b.diff_db).collect();
        let mean = inside.iter().sum::<f64>() / inside.len() as f64;
        let worst = inside.iter().map(|d| (d - mean).abs()).fold(0.0, f64::max);
        assert!(worst <= 0.5, "{worst} dB off: {inside:?} with {:?}", proposal.bands);
    }

    #[test]
    fn gains_stay_within_the_limit() {
        let noise = pink(10.0, RATE, 12);
        let shaped = equalise(&noise, &[EqBand { kind: EqKind::Peak, freq: 1000.0, gain_db: 15.0, bandwidth: 1.0 }]);
        let reference = analyse(&[shaped.clone(), shaped], RATE, None, None);
        let mix = analyse(&[noise.clone(), noise], RATE, None, None);
        let proposal = match_eq(&reference, &mix, MatchOptions { max_gain_db: 40.0, ..Default::default() });
        assert!(proposal.bands.iter().all(|b| b.gain_db.abs() <= SAFE_GAIN_DB), "{proposal:?}");
        let advice = dynamics_advice(&reference, &mix);
        assert!(advice.limiter_drive_db <= MAX_DRIVE_DB);
    }
}
