//! EQ Match: bands that move one spectrum towards another. The difference between the two
//! long-term spectra is levelled (its mean over 100 Hz-10 kHz removed, so only the shape
//! counts), smoothed over `smoothing` octaves and scaled by `amount`; then bands are placed
//! greedily where what is left is largest (a shelf when that region reaches either end of the
//! spectrum, a bell otherwise, its Q from the region's width) and their gains refined so the
//! sum of their curves follows the target.

use super::filters::{Design, Prototype, Shape};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatchOptions {
    /// 0..1 of the difference to correct.
    pub amount: f64,
    /// Smoothing width in octaves.
    pub smoothing: f64,
    pub max_bands: usize,
    /// No band moves further than this, in dB.
    pub max_gain: f64,
}

impl Default for MatchOptions {
    fn default() -> Self {
        Self { amount: 1.0, smoothing: 1.0 / 3.0, max_bands: 8, max_gain: 12.0 }
    }
}

/// The correction curve: `reference - current` in dB at `freqs`, levelled, smoothed and scaled.
pub fn target(freqs: &[f64], current: &[f64], reference: &[f64], o: &MatchOptions) -> Vec<f64> {
    let diff: Vec<f64> = current.iter().zip(reference).map(|(c, r)| r - c).collect();
    let mid: Vec<f64> =
        freqs.iter().zip(&diff).filter(|(f, _)| (100.0..=10000.0).contains(*f)).map(|(_, d)| *d).collect();
    let mean = if mid.is_empty() { 0.0 } else { mid.iter().sum::<f64>() / mid.len() as f64 };
    let half = o.smoothing.max(0.01) / 2.0;
    freqs
        .iter()
        .map(|f| {
            let (lo, hi) = (f * 2f64.powf(-half), f * 2f64.powf(half));
            let (sum, n) = freqs
                .iter()
                .zip(&diff)
                .filter(|(g, _)| (lo..=hi).contains(*g))
                .fold((0.0, 0usize), |(s, n), (_, d)| (s + d - mean, n + 1));
            let v = if n > 0 { sum / n as f64 } else { 0.0 };
            (v * o.amount).clamp(-o.max_gain, o.max_gain)
        })
        .collect()
}

fn curve(d: &Design, freqs: &[f64]) -> Vec<f64> {
    let p = Prototype::design(d);
    freqs.iter().map(|f| 20.0 * p.magnitude(*f).max(1e-9).log10()).collect()
}

/// Bands whose summed curve follows `target` at `freqs` (log spaced, ascending).
pub fn fit(freqs: &[f64], target: &[f64], o: &MatchOptions) -> Vec<Design> {
    let usable = |f: f64| (20.0..=20000.0).contains(&f);
    let mut bands: Vec<Design> = Vec::new();
    let mut residual = target.to_vec();
    for _ in 0..o.max_bands {
        let Some((peak, value)) = residual
            .iter()
            .enumerate()
            .filter(|(i, _)| usable(freqs[*i]))
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .map(|(i, v)| (i, *v))
        else {
            break;
        };
        if value.abs() < 0.5 {
            break;
        }
        let half = value / 2.0;
        let inside = |i: usize| residual[i].signum() == value.signum() && residual[i].abs() >= half.abs();
        let mut lo = peak;
        while lo > 0 && inside(lo - 1) {
            lo -= 1;
        }
        let mut hi = peak;
        while hi + 1 < freqs.len() && inside(hi + 1) {
            hi += 1;
        }
        let edge_lo = freqs[lo] <= 40.0;
        let edge_hi = freqs[hi] >= 12000.0;
        let design = if edge_lo && !edge_hi {
            Design {
                shape: Shape::LowShelf,
                freq: freqs[hi].clamp(30.0, 2000.0),
                gain_db: value,
                q: 0.7,
                slope: 1,
                gain_q: false,
            }
        } else if edge_hi && !edge_lo {
            Design {
                shape: Shape::HighShelf,
                freq: freqs[lo].clamp(1000.0, 16000.0),
                gain_db: value,
                q: 0.7,
                slope: 1,
                gain_q: false,
            }
        } else {
            let octaves = (freqs[hi] / freqs[lo]).log2().max(0.1);
            let r = 2f64.powf(octaves);
            let q = (r.sqrt() / (r - 1.0)).clamp(0.3, 8.0);
            Design { shape: Shape::Bell, freq: freqs[peak], gain_db: value, q, slope: 1, gain_q: false }
        };
        for (r, c) in residual.iter_mut().zip(curve(&design, freqs)) {
            *r -= c;
        }
        bands.push(design);
    }
    // Gauss-Seidel on the gains: each band takes what the others leave of the target, its
    // curve per dB of gain taken from a 6 dB version of it.
    let mut curves: Vec<Vec<f64>> = bands.iter().map(|b| curve(b, freqs)).collect();
    for _ in 0..6 {
        for k in 0..bands.len() {
            let unit: Vec<f64> =
                curve(&Design { gain_db: 6.0, ..bands[k] }, freqs).into_iter().map(|c| c / 6.0).collect();
            let (mut num, mut den) = (0.0, 0.0);
            for i in 0..freqs.len() {
                let others: f64 = curves.iter().enumerate().filter(|(j, _)| *j != k).map(|(_, c)| c[i]).sum();
                num += unit[i] * (target[i] - others);
                den += unit[i] * unit[i];
            }
            if den > 1e-9 {
                bands[k].gain_db = (num / den).clamp(-o.max_gain, o.max_gain);
                curves[k] = curve(&bands[k], freqs);
            }
        }
    }
    bands.retain(|b| b.gain_db.abs() >= 0.25);
    bands
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Vec<f64> {
        (0..200).map(|i| 20.0 * 1000f64.powf(i as f64 / 199.0)).collect()
    }

    /// A reference that is a mix through a known EQ is matched by bands whose sum comes back
    /// close to that EQ.
    #[test]
    fn matching_recovers_a_known_curve() {
        let f = grid();
        let truth = [
            Design { shape: Shape::Bell, freq: 400.0, gain_db: -5.0, q: 1.2, slope: 1, gain_q: false },
            Design { shape: Shape::HighShelf, freq: 6000.0, gain_db: 4.0, q: 0.7, slope: 1, gain_q: false },
        ];
        let current: Vec<f64> = f.iter().map(|x| -20.0 - 3.0 * (x / 1000.0).log2()).collect();
        let shift: Vec<f64> = (0..f.len()).map(|i| truth.iter().map(|d| curve(d, &f)[i]).sum::<f64>()).collect();
        let reference: Vec<f64> = current.iter().zip(&shift).map(|(c, s)| c + s + 7.0).collect();
        let o = MatchOptions { smoothing: 0.1, ..Default::default() };
        let t = target(&f, &current, &reference, &o);
        let bands = fit(&f, &t, &o);
        assert!(!bands.is_empty() && bands.len() <= 8);
        let mean_shift = {
            let mid: Vec<f64> =
                f.iter().zip(&shift).filter(|(x, _)| (100.0..=10000.0).contains(*x)).map(|(_, s)| *s).collect();
            mid.iter().sum::<f64>() / mid.len() as f64
        };
        let mut worst: f64 = 0.0;
        for i in 0..f.len() {
            if !(40.0..=16000.0).contains(&f[i]) {
                continue;
            }
            let got: f64 = bands.iter().map(|d| curve(d, &f)[i]).sum();
            worst = worst.max((got - (shift[i] - mean_shift)).abs());
        }
        assert!(worst < 1.5, "{worst} dB off with {bands:?}");
    }

    #[test]
    fn amount_scales_and_flat_needs_nothing() {
        let f = grid();
        let flat = vec![-30.0; f.len()];
        assert!(fit(&f, &target(&f, &flat, &flat, &MatchOptions::default()), &MatchOptions::default()).is_empty());
        let bump: Vec<f64> = f.iter().map(|x| -30.0 + 6.0 * (-(x / 1000.0).log2().powi(2) * 4.0).exp()).collect();
        let half = target(&f, &flat, &bump, &MatchOptions { amount: 0.5, smoothing: 0.1, ..Default::default() });
        let full = target(&f, &flat, &bump, &MatchOptions { smoothing: 0.1, ..Default::default() });
        let peak = |v: &[f64]| v.iter().copied().fold(f64::MIN, f64::max);
        assert!((peak(&half) * 2.0 - peak(&full)).abs() < 1e-9);
    }
}
