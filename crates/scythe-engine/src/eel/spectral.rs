//! EEL2's FFT and MDCT builtins with the bin orders, scales and size limits REAPER's JSFX
//! have (measured by rendering probe effects in REAPER).

use rustfft::num_complex::Complex64;
use rustfft::{Fft, FftPlanner};
use std::collections::HashMap;
use std::f64::consts::PI;
use std::sync::Arc;

pub(super) struct Spectral {
    planner: FftPlanner<f64>,
    plans: HashMap<(usize, bool), Arc<dyn Fft<f64>>>,
    orders: HashMap<usize, Vec<usize>>,
    work: Vec<Complex64>,
}

impl Default for Spectral {
    fn default() -> Self {
        Self { planner: FftPlanner::new(), plans: HashMap::new(), orders: HashMap::new(), work: Vec::new() }
    }
}

/// The largest power of two not above `x`, or 0.
pub(super) fn pow2_floor(x: f64) -> usize {
    if x < 1.0 { 0 } else { 1 << (x.min(1e9) as u64).ilog2() }
}

/// Which frequency each slot of a transformed buffer holds: the split-radix order, in which
/// slot `j` of an `n`-point transform holds bin `order[j]`.
fn split_radix_order(n: usize) -> Vec<usize> {
    fn plus(n: usize) -> Vec<usize> {
        match n {
            0 | 1 => vec![0],
            2 => vec![0, 1],
            _ => {
                let half = plus(n / 2);
                let quarter = plus(n / 4);
                let mut out: Vec<usize> = half.iter().map(|v| 2 * v).collect();
                out.extend(quarter.iter().map(|v| 4 * v + 1));
                out.extend(quarter.iter().map(|v| (4 * v + n - 1) % n));
                out
            }
        }
    }
    plus(n).into_iter().map(|v| (n - v) % n).collect()
}

impl Spectral {
    /// Unnormalised transform of `work[..n]` in place: e^-i forward, e^+i inverse.
    fn transform(&mut self, n: usize, inverse: bool) {
        let planner = &mut self.planner;
        let plan = self
            .plans
            .entry((n, inverse))
            .or_insert_with(|| if inverse { planner.plan_fft_inverse(n) } else { planner.plan_fft_forward(n) });
        plan.process(&mut self.work[..n]);
    }

    fn load_pairs(&mut self, data: &[f64]) {
        self.work.clear();
        self.work.extend(data.as_chunks::<2>().0.iter().map(|&[re, im]| Complex64::new(re, im)));
    }

    /// `fft` (forward, output in split-radix order) or `ifft` (input in that order) on
    /// `data.len() / 2` interleaved complex values.
    pub(super) fn fft(&mut self, data: &mut [f64], inverse: bool) {
        let n = data.len() / 2;
        if inverse {
            self.permute(data, false);
        }
        self.load_pairs(data);
        self.transform(n, inverse);
        for (p, z) in data.as_chunks_mut::<2>().0.iter_mut().zip(&self.work) {
            *p = [z.re, z.im];
        }
        if !inverse {
            self.permute(data, true);
        }
    }

    /// `fft_permute` (split-radix order to natural, `to_split` false) or `fft_ipermute`.
    pub(super) fn permute(&mut self, data: &mut [f64], to_split: bool) {
        let n = data.len() / 2;
        self.load_pairs(data);
        let order = std::mem::take(self.orders.entry(n).or_insert_with(|| split_radix_order(n)));
        for (j, &bin) in order.iter().enumerate() {
            let (from, to) = if to_split { (bin, j) } else { (j, bin) };
            data[2 * to] = self.work[from].re;
            data[2 * to + 1] = self.work[from].im;
        }
        self.orders.insert(n, order);
    }

    /// `fft_real`: `n` samples to `n / 2` complex bins at twice their size, DC and Nyquist
    /// sharing the first pair, in split-radix order. `ifft_real` undoes it up to a factor `2n`.
    pub(super) fn fft_real(&mut self, data: &mut [f64], inverse: bool) {
        let n = data.len();
        let half = n / 2;
        if inverse {
            self.permute(data, false);
            self.work.clear();
            self.work.resize(n, Complex64::default());
            self.work[0] = Complex64::new(data[0], 0.0);
            self.work[half] = Complex64::new(data[1], 0.0);
            for k in 1..half {
                let z = Complex64::new(data[2 * k], data[2 * k + 1]);
                self.work[k] = z;
                self.work[n - k] = z.conj();
            }
            self.transform(n, true);
            for (d, z) in data.iter_mut().zip(&self.work) {
                *d = z.re;
            }
        } else {
            self.work.clear();
            self.work.extend(data.iter().map(|&v| Complex64::new(v, 0.0)));
            self.transform(n, false);
            data[0] = 2.0 * self.work[0].re;
            data[1] = 2.0 * self.work[half].re;
            for k in 1..half {
                data[2 * k] = 2.0 * self.work[k].re;
                data[2 * k + 1] = 2.0 * self.work[k].im;
            }
            self.permute(data, true);
        }
    }

    /// `mdct`: `n` samples under a sine window to `n / 2` coefficients in `data[..n / 2]`.
    /// `imdct`: `n / 2` coefficients to `n` windowed samples.
    pub(super) fn mdct(&mut self, data: &mut [f64], inverse: bool) {
        let n = data.len();
        let m = n / 2;
        let n0 = 0.5 + m as f64 / 2.0;
        let window = |i: usize| (PI * (i as f64 + 0.5) / n as f64).sin();
        // REAPER's 32-point transform is scaled apart from the rest.
        let (forward_scale, inverse_scale) = if n == 32 { (1.0, 0.125) } else { (4.0 / n as f64, 1.0) };
        self.work.clear();
        if inverse {
            self.work.extend((0..n).map(|k| {
                let x = if k < m { data[k] } else { 0.0 };
                Complex64::from_polar(x, PI * n0 * k as f64 / m as f64)
            }));
            self.transform(n, true);
            for (i, d) in data.iter_mut().enumerate() {
                let turn = Complex64::from_polar(1.0, PI * (i as f64 + n0) / n as f64);
                *d = (turn * self.work[i]).re * window(i) * inverse_scale;
            }
        } else {
            self.work.extend(
                data.iter().enumerate().map(|(i, &x)| Complex64::from_polar(x * window(i), -PI * i as f64 / n as f64)),
            );
            self.transform(n, false);
            for (k, d) in data[..m].iter_mut().enumerate() {
                let turn = Complex64::from_polar(1.0, -PI * n0 * (k as f64 + 0.5) / m as f64);
                *d = (turn * self.work[k]).re * forward_scale;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(n: usize) -> Vec<f64> {
        (0..n).map(|i| ((i * 7) % 13) as f64 - 6.0 + i as f64 * 0.01).collect()
    }

    fn direct_dft(z: &[Complex64]) -> Vec<Complex64> {
        let n = z.len();
        (0..n)
            .map(|k| {
                z.iter()
                    .enumerate()
                    .map(|(t, v)| v * Complex64::from_polar(1.0, -2.0 * PI * (k * t) as f64 / n as f64))
                    .sum()
            })
            .collect()
    }

    #[test]
    fn split_radix_order_matches_reaper() {
        // As REAPER's fft(buf, 16) leaves the bins.
        assert_eq!(split_radix_order(16), [0, 8, 12, 4, 14, 6, 2, 10, 15, 7, 11, 3, 1, 9, 13, 5]);
        assert_eq!(&split_radix_order(64)[..8], [0, 32, 48, 16, 56, 24, 8, 40]);
    }

    #[test]
    fn fft_round_trips_through_permute_and_ifft() {
        let mut s = Spectral::default();
        let x = input(64);
        let mut data = x.clone();
        s.fft(&mut data, false);
        let natural: Vec<Complex64> = x.as_chunks::<2>().0.iter().map(|&[re, im]| Complex64::new(re, im)).collect();
        let expect = direct_dft(&natural);
        let order = split_radix_order(32);
        for (j, &bin) in order.iter().enumerate() {
            assert!((Complex64::new(data[2 * j], data[2 * j + 1]) - expect[bin]).norm() < 1e-9);
        }
        s.fft(&mut data, true);
        for (a, b) in data.iter().zip(&x) {
            assert!((a / 32.0 - b).abs() < 1e-12);
        }
    }

    #[test]
    fn mdct_matches_the_direct_sums() {
        let mut s = Spectral::default();
        for n in [32usize, 64, 256] {
            let m = n / 2;
            let n0 = 0.5 + m as f64 / 2.0;
            let w = |i: usize| (PI * (i as f64 + 0.5) / n as f64).sin();
            let basis = |i: usize, k: usize| (PI / m as f64 * (i as f64 + n0) * (k as f64 + 0.5)).cos();
            let (fs, is) = if n == 32 { (1.0, 0.125) } else { (4.0 / n as f64, 1.0) };
            let x = input(n);
            let mut data = x.clone();
            s.mdct(&mut data, false);
            for (k, got) in data[..m].iter().enumerate() {
                let d: f64 = (0..n).map(|i| x[i] * w(i) * basis(i, k)).sum::<f64>() * fs;
                assert!((got - d).abs() < 1e-9, "mdct {n} bin {k}");
            }
            let mut data = x.clone();
            s.mdct(&mut data, true);
            for (i, got) in data.iter().enumerate() {
                let d: f64 = (0..m).map(|k| x[k] * basis(i, k)).sum::<f64>() * w(i) * is;
                assert!((got - d).abs() < 1e-9, "imdct {n} sample {i}");
            }
        }
    }
}
