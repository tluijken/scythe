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

/// Adds `bands` into free band slots of `eq`; returns how many found one.
pub fn apply_bands(eq: &mut super::ScytheEq, bands: &[Design]) -> usize {
    use super::params::{self as p, BANDS, band};
    let mut free = (0..BANDS).filter(|b| eq.value(p::band_param(*b, band::USED)) < 0.5).collect::<Vec<_>>().into_iter();
    let mut added = 0;
    for d in bands {
        let Some(b) = free.next() else { break };
        let id = |o: u32| p::band_param(b, o);
        for o in 0..band::COUNT {
            eq.set(id(o), p::default(id(o)));
        }
        eq.set(id(band::SHAPE), d.shape.index() as f64);
        eq.set(id(band::FREQ), d.freq);
        eq.set(id(band::GAIN), d.gain_db);
        eq.set(id(band::Q), d.q);
        eq.set(id(band::SLOPE), d.slope as f64);
        eq.set(id(band::USED), 1.0);
        added += 1;
    }
    added
}

/// `n` frequencies log spaced from 20 Hz to 20 kHz.
pub fn log_grid(n: usize) -> Vec<f64> {
    (0..n).map(|i| 20.0 * 1000f64.powf(i as f64 / (n - 1).max(1) as f64)).collect()
}

/// The power of `samples` in an `octaves` wide band around each of `freqs`, in dB (0 dB for
/// a full-scale sine): a Hann-windowed FFT's bins summed over the band, or interpolated
/// between bins where the band is narrower than two of them.
pub fn levels(samples: &[f32], sample_rate: f64, freqs: &[f64], octaves: f64) -> Vec<f64> {
    use realfft::RealFftPlanner;
    let n = samples.len();
    if n < 64 {
        return vec![-150.0; freqs.len()];
    }
    let window: Vec<f32> = (0..n).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos()).collect();
    let mut buf: Vec<f32> = samples.iter().zip(&window).map(|(s, w)| s * w).collect();
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(n);
    let mut spec = fft.make_output_vec();
    if fft.process(&mut buf, &mut spec).is_err() {
        return vec![-150.0; freqs.len()];
    }
    let norm = 2.0 / window.iter().map(|w| *w as f64).sum::<f64>();
    let power: Vec<f64> = spec.iter().map(|c| (c.norm() as f64 * norm).powi(2)).collect();
    // A Hann window spreads a sine's power over 1.5 bins' worth.
    const ENBW: f64 = 1.5;
    let hz_per_bin = sample_rate / n as f64;
    let half = 2f64.powf(octaves / 2.0);
    let last = power.len() - 1;
    freqs
        .iter()
        .map(|f| {
            let (a, b) = (f / half / hz_per_bin, f * half / hz_per_bin);
            let p = if b - a < 2.0 {
                let x = (f / hz_per_bin).min(last as f64);
                let i = (x.floor() as usize).min(last - 1);
                let t = x - i as f64;
                power[i] * (1.0 - t) + power[i + 1] * t
            } else {
                let (lo, hi) = ((a.round() as usize).min(last), (b.round() as usize).min(last));
                power[lo..=hi].iter().sum::<f64>() / ENBW
            };
            10.0 * p.max(1e-15).log10()
        })
        .collect()
}

/// Ranges of points where both spectra are above `floor` dB and within 6 dB of each other,
/// with a strength from 0 to 1 (how loud and how close they are).
pub fn collisions(ours: &[f32], theirs: &[f32], floor: f32) -> Vec<(usize, usize, f32)> {
    let mut out = Vec::new();
    let mut start: Option<(usize, f32)> = None;
    for i in 0..ours.len().min(theirs.len()) {
        let (a, b) = (ours[i], theirs[i]);
        let hit = a > floor && b > floor && (a - b).abs() < 6.0;
        let strength =
            if hit { (((a.min(b) - floor) / 30.0).clamp(0.0, 1.0)) * (1.0 - (a - b).abs() / 6.0) } else { 0.0 };
        match (&mut start, hit) {
            (None, true) => start = Some((i, strength)),
            (Some((_, s)), true) => *s = s.max(strength),
            (Some((from, s)), false) => {
                if i - *from >= 3 {
                    out.push((*from, i - 1, *s));
                }
                start = None;
            }
            (None, false) => {}
        }
    }
    if let Some((from, s)) = start {
        out.push((from, ours.len() - 1, s));
    }
    out
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

    fn noise(n: usize, mut seed: u64) -> Vec<f32> {
        (0..n)
            .map(|_| {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                ((seed >> 33) as f32 / (1u64 << 31) as f32) - 0.5
            })
            .collect()
    }

    #[test]
    fn levels_read_a_sine_at_full_scale() {
        let sr = 48000.0;
        let f = log_grid(200);
        for hz in [100.0, 1000.0, 8000.0] {
            let sine: Vec<f32> =
                (0..16384).map(|i| (std::f64::consts::TAU * hz * i as f64 / sr).sin() as f32).collect();
            let l = levels(&sine, sr, &f, 1.0 / 3.0);
            let at = f.iter().position(|x| *x >= hz).unwrap();
            assert!(l[at].abs() < 1.0, "{hz} Hz reads {}", l[at]);
        }
    }

    /// Noise against the same noise through a bell and a shelf: the fit finds both from the
    /// two signals' spectra.
    #[test]
    fn fits_one_signal_to_another_filtered_one() {
        use super::super::filters::{Cascade, CascadeState};
        let sr = 48000.0;
        let dry = noise(1 << 17, 7);
        let truth = [
            Design { shape: Shape::Bell, freq: 1000.0, gain_db: 6.0, q: 1.0, slope: 1, gain_q: false },
            Design { shape: Shape::HighShelf, freq: 5000.0, gain_db: -5.0, q: 0.7, slope: 1, gain_q: false },
        ];
        let mut wet: Vec<f64> = dry.iter().map(|x| *x as f64).collect();
        for d in &truth {
            let c = Cascade::matched(&Prototype::design(d), sr);
            let mut st = CascadeState::default();
            wet.iter_mut().for_each(|x| *x = st.run(&c, *x));
        }
        let wet: Vec<f32> = wet.iter().map(|x| *x as f32).collect();
        let f = log_grid(240);
        let o = MatchOptions { smoothing: 0.5, ..Default::default() };
        let t = target(&f, &levels(&dry, sr, &f, 1.0 / 6.0), &levels(&wet, sr, &f, 1.0 / 6.0), &o);
        let bands = fit(&f, &t, &o);
        let shift: Vec<f64> = (0..f.len()).map(|i| truth.iter().map(|d| curve(d, &f)[i]).sum()).collect();
        let fitted: Vec<f64> = (0..f.len()).map(|i| bands.iter().map(|d| curve(d, &f)[i]).sum()).collect();
        let off: Vec<f64> = (0..f.len()).map(|i| fitted[i] - shift[i]).collect();
        let mid: Vec<f64> =
            f.iter().zip(&off).filter(|(x, _)| (100.0..=10000.0).contains(*x)).map(|(_, d)| *d).collect();
        let mean = mid.iter().sum::<f64>() / mid.len() as f64;
        let worst = f
            .iter()
            .zip(&off)
            .filter(|(x, _)| (100.0..=12000.0).contains(*x))
            .fold(0.0f64, |m, (_, d)| m.max((d - mean).abs()));
        assert!(worst < 1.5, "{worst} dB off with {bands:?}");
    }
}
