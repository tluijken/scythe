//! The linear and natural phase paths: the EQ's response sampled on an FFT grid, turned into
//! FIR kernels and run as a uniformly partitioned overlap-save convolution.
//!
//! The stereo processing is a 2×2 kernel matrix (left/right to left/right) so mid/side and
//! single-channel bands fit in; with only stereo bands one kernel serves both channels.
//! Dynamic bands add a correction kernel each whose output is scaled by the band's current
//! dynamic amount, so the response stays linear (or natural) phase while it moves.
//!
//! Latency is `latency()` frames: the kernel's centre (`delay`) plus one partition of input
//! buffering. Everything is allocated in `new` and `ensure_dynamic`; `process` and `rebuild`
//! only use what is there.

use std::sync::Arc;

use realfft::num_complex::{Complex32, Complex64};
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

use super::filters::C64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// One kernel for both channels.
    Scalar,
    /// Left to left and right to right.
    Diagonal,
    Full,
}

impl Layout {
    fn kernels(self) -> &'static [usize] {
        match self {
            Self::Scalar => &[0],
            Self::Diagonal => &[0, 3],
            Self::Full => &[0, 1, 2, 3],
        }
    }
}

/// Partition spectra of up to four kernels (LL, LR, RL, RR).
struct KernelSet {
    layout: Layout,
    parts: [Vec<Vec<Complex32>>; 4],
    silent: bool,
}

impl KernelSet {
    fn new(parts: usize, bins: usize) -> Self {
        let make = || vec![vec![Complex32::default(); bins]; parts];
        Self { layout: Layout::Scalar, parts: [make(), make(), make(), make()], silent: true }
    }
}

/// How the kernel is shaped in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timing {
    /// Symmetric around the centre.
    Linear,
    /// Causal around a short pre-delay that catches the leading ripples of sampling.
    Natural,
}

pub struct FirEngine {
    /// Partition size.
    p: usize,
    /// Size of the design grid.
    n: usize,
    /// Kernel taps and their centre.
    taps: usize,
    delay: usize,
    timing: Timing,
    fft: Arc<dyn RealToComplex<f32>>,
    ifft: Arc<dyn ComplexToReal<f32>>,
    design_ifft: Arc<dyn ComplexToReal<f64>>,
    window: Vec<f64>,
    /// Input spectra per channel, newest at `head`.
    fdl: [Vec<Vec<Complex32>>; 2],
    head: usize,
    /// The last 2P input frames per channel.
    input: [Vec<f32>; 2],
    fill: usize,
    main: [KernelSet; 2],
    active: usize,
    fade_pending: bool,
    dynamic: Vec<Option<KernelSet>>,
    /// Output of the last block: main, then per dynamic band.
    out: [Vec<f32>; 2],
    fade: Vec<f32>,
    dyn_out: Vec<[Vec<f32>; 2]>,
    acc: Vec<Complex32>,
    time: Vec<f32>,
    spec: Vec<Complex32>,
    design_spec: Vec<Complex64>,
    design_time: Vec<f64>,
}

impl FirEngine {
    /// An engine for a design grid of `n` points (a power of two) and partitions of `p`.
    pub fn new(n: usize, p: usize, timing: Timing, bands: usize) -> Self {
        let delay = match timing {
            Timing::Linear => n / 2 - p,
            Timing::Natural => n / 8 - p,
        };
        let taps = match timing {
            Timing::Linear => 2 * delay,
            Timing::Natural => n - 2 * p,
        };
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(2 * p);
        let ifft = planner.plan_fft_inverse(2 * p);
        let design_ifft = RealFftPlanner::<f64>::new().plan_fft_inverse(n);
        let parts = taps.div_ceil(p);
        let bins = p + 1;
        let hann = |x: f64| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * x).cos();
        let window = (0..taps)
            .map(|i| match timing {
                Timing::Linear => hann((i as f64 + 0.5) / taps as f64),
                Timing::Natural => {
                    let tail = taps / 4;
                    if i < delay {
                        hann(0.5 * (i as f64 + 0.5) / delay as f64)
                    } else if i + tail > taps {
                        hann(0.5 + 0.5 * (i + tail - taps) as f64 / tail as f64)
                    } else {
                        1.0
                    }
                }
            })
            .collect();
        Self {
            p,
            n,
            taps,
            delay,
            timing,
            window,
            fdl: [vec![vec![Complex32::default(); bins]; parts], vec![vec![Complex32::default(); bins]; parts]],
            head: 0,
            input: [vec![0.0; 2 * p], vec![0.0; 2 * p]],
            fill: 0,
            main: [KernelSet::new(parts, bins), KernelSet::new(parts, bins)],
            active: 0,
            fade_pending: false,
            dynamic: (0..bands).map(|_| None).collect(),
            out: [vec![0.0; p], vec![0.0; p]],
            fade: vec![0.0; p],
            dyn_out: (0..bands).map(|_| [Vec::new(), Vec::new()]).collect(),
            acc: vec![Complex32::default(); bins],
            time: vec![0.0; 2 * p],
            spec: vec![Complex32::default(); bins],
            design_spec: vec![Complex64::default(); n / 2 + 1],
            design_time: vec![0.0; n],
            fft,
            ifft,
            design_ifft,
        }
    }

    pub fn grid(&self) -> usize {
        self.n
    }

    pub fn timing(&self) -> Timing {
        self.timing
    }

    pub fn latency(&self) -> usize {
        self.delay + self.p
    }

    /// Makes room for band `b`'s correction kernel (or drops it).
    pub fn ensure_dynamic(&mut self, b: usize, on: bool) {
        let parts = self.taps.div_ceil(self.p);
        match (on, self.dynamic[b].is_some()) {
            (true, false) => {
                self.dynamic[b] = Some(KernelSet::new(parts, self.p + 1));
                self.dyn_out[b] = [vec![0.0; self.p], vec![0.0; self.p]];
            }
            (false, true) => {
                self.dynamic[b] = None;
                self.dyn_out[b] = [Vec::new(), Vec::new()];
            }
            _ => {}
        }
    }

    pub fn has_dynamic(&self, b: usize) -> bool {
        self.dynamic.get(b).is_some_and(|d| d.is_some())
    }

    /// Sets the main kernels from the response matrix on the design grid (`n / 2 + 1` bins
    /// each of LL, LR, RL, RR); the change crossfades over the next partition.
    pub fn set_main(&mut self, layout: Layout, resp: &[Vec<C64>; 4]) {
        let target = self.active ^ 1;
        let mut set = std::mem::replace(&mut self.main[target], KernelSet::new(0, 0));
        self.design(&mut set, layout, resp);
        self.main[target] = set;
        self.fade_pending = true;
    }

    /// Sets band `b`'s correction kernels; the band must have room (`ensure_dynamic`).
    pub fn set_dynamic(&mut self, b: usize, layout: Layout, resp: &[Vec<C64>; 4]) {
        let Some(mut set) = self.dynamic[b].take() else { return };
        self.design(&mut set, layout, resp);
        self.dynamic[b] = Some(set);
    }

    fn design(&mut self, set: &mut KernelSet, layout: Layout, resp: &[Vec<C64>; 4]) {
        set.layout = layout;
        set.silent = true;
        let n = self.n;
        let scale = 1.0 / n as f64;
        for &k in layout.kernels() {
            let r = &resp[k];
            if r.iter().all(|c| c.re == 0.0 && c.im == 0.0) {
                for part in &mut set.parts[k] {
                    part.fill(Complex32::default());
                }
                continue;
            }
            set.silent = false;
            for (i, (dst, c)) in self.design_spec.iter_mut().zip(r).enumerate() {
                let ph = -2.0 * std::f64::consts::PI * (i * self.delay) as f64 / n as f64;
                let (s, co) = ph.sin_cos();
                *dst = Complex64::new(c.re * co - c.im * s, c.re * s + c.im * co);
            }
            self.design_spec[0].im = 0.0;
            self.design_spec[n / 2].im = 0.0;
            let _ = self.design_ifft.process(&mut self.design_spec, &mut self.design_time);
            for (pi, part) in set.parts[k].iter_mut().enumerate() {
                self.time.fill(0.0);
                for j in 0..self.p {
                    let t = pi * self.p + j;
                    if t < self.taps {
                        self.time[j] = (self.design_time[t] * scale * self.window[t]) as f32;
                    }
                }
                let _ = self.fft.process(&mut self.time, part);
            }
        }
    }

    pub fn reset(&mut self) {
        for ch in &mut self.fdl {
            for s in ch.iter_mut() {
                s.fill(Complex32::default());
            }
        }
        for b in &mut self.input {
            b.fill(0.0);
        }
        for b in &mut self.out {
            b.fill(0.0);
        }
        for d in &mut self.dyn_out {
            for b in d.iter_mut() {
                b.fill(0.0);
            }
        }
        self.fill = 0;
    }

    /// Feeds one frame and returns the output frame `latency` frames late: the main kernels'
    /// output plus each dynamic band's correction times its amount in `amounts`.
    #[inline]
    pub fn push(&mut self, l: f32, r: f32, amounts: &[f32]) -> (f32, f32) {
        let p = self.p;
        let i = self.fill;
        self.input[0][p + i] = l;
        self.input[1][p + i] = r;
        let (mut ol, mut or) = (self.out[0][i], self.out[1][i]);
        for (b, k) in amounts.iter().enumerate() {
            if *k != 0.0
                && let [dl, dr] = &self.dyn_out[b]
                && !dl.is_empty()
            {
                ol += k * dl[i];
                or += k * dr[i];
            }
        }
        self.fill += 1;
        if self.fill == p {
            self.block();
            self.fill = 0;
        }
        (ol, or)
    }

    fn block(&mut self) {
        let p = self.p;
        let parts = self.fdl[0].len();
        self.head = (self.head + parts - 1) % parts;
        for ch in 0..2 {
            self.time.copy_from_slice(&self.input[ch]);
            let head = self.head;
            let _ = self.fft.process(&mut self.time, &mut self.fdl[ch][head]);
            self.input[ch].copy_within(p.., 0);
        }
        let fade = self.fade_pending;
        let old = self.active;
        let new = if fade { old ^ 1 } else { old };
        for ch in 0..2 {
            let mut out = std::mem::take(&mut self.out[ch]);
            self.convolve_into(Which::Main(new), ch, &mut out);
            if fade {
                let mut prev = std::mem::take(&mut self.fade);
                self.convolve_into(Which::Main(old), ch, &mut prev);
                for (j, (o, q)) in out.iter_mut().zip(&prev).enumerate() {
                    let t = (j as f32 + 0.5) / p as f32;
                    *o = *o * t + *q * (1.0 - t);
                }
                self.fade = prev;
            }
            self.out[ch] = out;
        }
        if fade {
            self.active = new;
            self.fade_pending = false;
        }
        for b in 0..self.dynamic.len() {
            if self.dynamic[b].is_none() {
                continue;
            }
            for ch in 0..2 {
                let mut out = std::mem::take(&mut self.dyn_out[b][ch]);
                self.convolve_into(Which::Dynamic(b), ch, &mut out);
                self.dyn_out[b][ch] = out;
            }
        }
    }

    fn convolve_into(&mut self, which: Which, ch: usize, out: &mut [f32]) {
        let p = self.p;
        let set = match which {
            Which::Main(i) => &self.main[i],
            Which::Dynamic(b) => match &self.dynamic[b] {
                Some(s) => s,
                None => return,
            },
        };
        if set.silent {
            out.fill(0.0);
            return;
        }
        self.acc.fill(Complex32::default());
        let parts = self.fdl[0].len();
        // Output channel `ch` takes kernel (ch, in) for each input channel.
        let inputs: &[(usize, usize)] = match (set.layout, ch) {
            (Layout::Scalar, 0) => &[(0, 0)],
            (Layout::Scalar, _) => &[(0, 1)],
            (Layout::Diagonal, 0) => &[(0, 0)],
            (Layout::Diagonal, _) => &[(3, 1)],
            (Layout::Full, 0) => &[(0, 0), (1, 1)],
            (Layout::Full, _) => &[(2, 0), (3, 1)],
        };
        for &(k, input) in inputs {
            for (pi, kernel) in set.parts[k].iter().enumerate() {
                let x = &self.fdl[input][(self.head + pi) % parts];
                for ((a, h), v) in self.acc.iter_mut().zip(kernel).zip(x) {
                    *a += h * v;
                }
            }
        }
        self.spec.copy_from_slice(&self.acc);
        self.spec[0].im = 0.0;
        self.spec[p].im = 0.0;
        let mut time = std::mem::take(&mut self.time);
        time.resize(2 * p, 0.0);
        let _ = self.ifft.process(&mut self.spec, &mut time);
        let norm = 1.0 / (2 * p) as f32;
        for (o, t) in out.iter_mut().zip(&time[p..]) {
            *o = t * norm;
        }
        self.time = time;
    }
}

enum Which {
    Main(usize),
    Dynamic(usize),
}
