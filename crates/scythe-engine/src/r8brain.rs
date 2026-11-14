//! REAPER's "r8brain free" resample mode: a Rust implementation of the converter design of
//! r8brain-free-src 6.x by Aleksey Vaneev (MIT licence, github.com/avaneev/r8brain-free-src).
//! The filter design formulas, correction tables and half-band filters are that library's; the
//! conversion runs over whole buffers instead of in blocks, which gives the same samples.
//!
//! Copyright (c) 2013-2026 Aleksey Vaneev
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy of this software
//! and associated documentation files (the "Software"), to deal in the Software without
//! restriction, including without limitation the rights to use, copy, modify, merge, publish,
//! distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the
//! Software is furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in all copies or
//! substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING
//! BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
//! NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
//! DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

use std::f64::consts::PI;

use realfft::RealFftPlanner;

/// REAPER runs `CDSPResampler` with its defaults: a 2% transition band, 206.91 dB stop-band
/// attenuation and linear phase. Its renders null against these to about 1e-8.
const TRANS_BAND: f64 = 2.0;
const ATTEN: f64 = 206.91;

/// Converts `input` from `from` to `to` Hz, returning `frames` frames.
pub(crate) fn convert(input: &[f32], from: f64, to: f64, frames: usize) -> Vec<f32> {
    let stages = plan(from, to);
    let mut needed = vec![frames; stages.len() + 1];
    for (i, stage) in stages.iter().enumerate().rev() {
        needed[i] = stage.input_needed(needed[i + 1]);
    }
    let mut x: Vec<f64> = input.iter().take(needed[0]).map(|&v| v as f64).collect();
    for (stage, &n) in stages.iter().zip(&needed[1..]) {
        x = stage.run(&x, n);
    }
    x.into_iter().map(|v| v as f32).collect()
}

enum Stage {
    /// Zero-stuffing by `up`, the zero-phase low-pass `kernel` (centred on `kernel.len() / 2`),
    /// then keeping every `down`th sample.
    Convolve {
        kernel: Vec<f64>,
        up: usize,
        down: usize,
    },
    Interpolate(Interpolator),
    /// 2x upsampling: input samples pass through, the ones between come from the half-band
    /// filter's taps (one side of it).
    HalfBandUp(&'static [f64]),
    /// 2x downsampling with a half-band filter, at a gain of 2.
    HalfBandDown(&'static [f64]),
}

impl Stage {
    fn input_needed(&self, out: usize) -> usize {
        if out == 0 {
            return 0;
        }
        match self {
            Self::Convolve { kernel, up, down } => (down * (out - 1) + kernel.len() / 2) / up + 1,
            Self::Interpolate(i) => i.input_needed(out),
            Self::HalfBandUp(taps) => (out - 1) / 2 + taps.len() + 1,
            Self::HalfBandDown(taps) => 2 * (out - 1) + 2 * taps.len(),
        }
    }

    fn run(&self, x: &[f64], out: usize) -> Vec<f64> {
        let at = |i: i64| if i < 0 { 0.0 } else { x.get(i as usize).copied().unwrap_or(0.0) };
        match self {
            Self::Convolve { kernel, up, down } => convolve(x, kernel, *up, *down, out),
            Self::Interpolate(i) => i.run(x, out),
            Self::HalfBandUp(taps) => (0..out as i64)
                .map(|n| {
                    let i = n / 2;
                    if n % 2 == 0 {
                        return at(i);
                    }
                    taps.iter().enumerate().map(|(k, t)| t * (at(i + 1 + k as i64) + at(i - k as i64))).sum()
                })
                .collect(),
            Self::HalfBandDown(taps) => (0..out as i64)
                .map(|m| {
                    let c = 2 * m;
                    at(c)
                        + taps
                            .iter()
                            .enumerate()
                            .map(|(k, t)| t * (at(c + 2 * k as i64 + 1) + at(c - 2 * k as i64 - 1)))
                            .sum::<f64>()
                })
                .collect(),
        }
    }
}

/// The processing steps `CDSPResampler`'s constructor picks for a conversion.
fn plan(from: f64, to: f64) -> Vec<Stage> {
    if from == to {
        return Vec::new();
    }
    let lp = |freq: f64, tb: f64, gain: f64| lowpass_kernel(freq, tb, ATTEN, gain);
    for (num, den) in [(1, 2), (1, 3), (2, 3), (3, 2), (3, 4)] {
        if from * num as f64 == to * den as f64 {
            let kernel = lp(1.0 / num.max(den) as f64, TRANS_BAND, num as f64);
            return vec![Stage::Convolve { kernel, up: num, down: den }];
        }
    }
    for factor in [2usize, 3] {
        let mut c = 0;
        loop {
            let rate = from * (factor << c) as f64;
            if rate == to {
                let kernel = lp(1.0 / factor as f64, TRANS_BAND, factor as f64);
                let mut stages = vec![Stage::Convolve { kernel, up: factor, down: 1 }];
                stages.extend((0..c).map(|i| Stage::HalfBandUp(half_band(i, factor == 3))));
                return stages;
            }
            if rate > to {
                break;
            }
            c += 1;
        }
    }
    if to * 2.0 > from {
        let norm_freq = if to > from { 0.5 } else { 0.5 * to / from };
        let first = Stage::Convolve { kernel: lp(norm_freq, TRANS_BAND, 2.0), up: 2, down: 1 };
        // Interpolating at a lower rate and upsampling the rest of the way, when the
        // intermediate filter's transition band is no steeper than the requested one.
        let tbw = 0.0175;
        let thresh = from / (1.0 - tbw * TRANS_BAND);
        let mut c = 0;
        let mut div = 1;
        while to >= thresh * (div * 2) as f64 {
            div *= 2;
            c += 1;
        }
        let third_div = (to >= thresh * (div * 3) as f64).then_some(div * 3);
        if c == 1 && whole_stepping(from * 2.0, to).is_some() {
            c = 0;
        }
        if c == 0 {
            return vec![first, Stage::Interpolate(Interpolator::new(from * 2.0, to, false))];
        }
        let (div, c, num) = match third_div {
            Some(d) if d > div => (d, 1, 3),
            _ => (div, c, 2),
        };
        let interp = Interpolator::new(from * 2.0 * div as f64, to, false);
        let tb = ((1.0 - from * div as f64 / to) / tbw).min(MAX_TRANS_BAND);
        let mut stages = vec![
            first,
            Stage::Interpolate(interp),
            Stage::Convolve { kernel: lp(1.0 / num as f64, tb, num as f64), up: num, down: 1 },
        ];
        stages.extend((1..c).map(|i| Stage::HalfBandUp(half_band(i - 1, num == 3))));
        return stages;
    }
    let mut check = to * 4.0;
    let mut c = 0;
    let mut gain = 1.0;
    while check <= from {
        c += 1;
        check *= 2.0;
        gain *= 0.5;
    }
    let to_div = to * (1u64 << c) as f64;
    let (down, norm_freq, interp, third) = if to_div * 2.0 == from {
        (2, 0.5, false, false)
    } else if to_div * 3.0 == from {
        (3, 1.0 / 3.0, false, true)
    } else {
        let norm_freq = to_div / from;
        (1, norm_freq, true, norm_freq * 3.0 <= 1.0)
    };
    let mut stages: Vec<Stage> = (0..c).map(|i| Stage::HalfBandDown(half_band(c - 1 - i, third))).collect();
    stages.push(Stage::Convolve { kernel: lp(norm_freq, TRANS_BAND, gain), up: 1, down });
    if interp {
        stages.push(Stage::Interpolate(Interpolator::new(from, to_div, third)));
    }
    stages
}

const MAX_TRANS_BAND: f64 = 45.0;

/// The one-sided taps of the half-band filter r8brain picks for 206.91 dB at `steep` (0 for
/// the stage nearest the final rate), for 1/2 or 1/3 band-limited signals.
fn half_band(steep: usize, third: bool) -> &'static [f64] {
    const HALF: [&[f64]; 7] = [
        &[
            0.6281641625236732,
            -0.18809076955230414,
            0.09091853986735303,
            -0.04676550268359931,
            0.023287520498995663,
            -0.010760627245014184,
            0.004485392294842568,
            -0.00164387754269108,
            0.0005144131235476498,
            -0.0001321172568576505,
            2.6191319837779187e-05,
            -3.5802430606313093e-06,
            2.54912782706286e-07,
        ],
        &[
            0.6161037226347895,
            -0.15767891882524138,
            0.05508969117029469,
            -0.01689575565636606,
            0.003941664343821398,
            -0.0006060362379160467,
            4.5632602433393365e-05,
        ],
        &[0.6062680828523072, -0.13588224032740795, 0.035544305238309, -0.006512702237728965, 0.0005825544956595077],
        &[0.5983502865716359, -0.11999986086623511, 0.024132530854004228, -0.0024829565686819706],
        &[0.59819599352772, -0.11972157555011861, 0.023977305567947922, -0.002451723545585399],
        &[0.5859551474467424, -0.09768272515679195, 0.01172757771111723],
        &[0.5859419120118738, -0.0976628682669912, 0.011720956255134043],
    ];
    const THIRD: [&[f64]; 7] = [
        &[
            0.6216318895189931,
            -0.1710811532381094,
            0.06958837009560026,
            -0.027339625080613838,
            0.009295446918379177,
            -0.002553717995955543,
            0.0005257229089795102,
            -7.181335613515492e-05,
            4.8802382808892154e-06,
        ],
        &[
            0.611614563419044,
            -0.14743901958274458,
            0.045344160157313275,
            -0.01120737178092453,
            0.0018328497112594935,
            -0.00014518193006359589,
        ],
        &[0.605909228820302, -0.13515953438018685, 0.035020857107815606, -0.006325619699046705, 0.0005550681514759879],
        &[0.5982360124316252, -0.11979368994739022, 0.024017458606412575, -0.0024597810910081913],
        &[0.5859688723420108, -0.0977033211130803, 0.011734448777069783],
        &[0.5859453433674705, -0.09766801583863982, 0.011722672471262996],
        &[0.5859394578696376, -0.09765918685349961, 0.011719728983863425],
    ];
    let table = if third { &THIRD } else { &HALF };
    table[steep.min(6)]
}

/// Zero-stuffs `x` by `up`, filters it with the centred `kernel` and keeps every `down`th
/// sample, `out` of them. FFT overlap-save; samples past the end of `x` are silence.
fn convolve(x: &[f64], kernel: &[f64], up: usize, down: usize, out: usize) -> Vec<f64> {
    let len = kernel.len();
    let half = len / 2;
    let size = (len * 4).next_power_of_two().max(4096);
    let step = size - len + 1;
    let mut planner = RealFftPlanner::<f64>::new();
    let forward = planner.plan_fft_forward(size);
    let inverse = planner.plan_fft_inverse(size);
    let mut buf = vec![0.0; size];
    buf[..len].copy_from_slice(kernel);
    let mut spectrum = forward.make_output_vec();
    forward.process(&mut buf, &mut spectrum).unwrap();
    let kernel_spectrum = spectrum.clone();
    let scale = 1.0 / size as f64;
    let stuffed = |n: i64| -> f64 {
        if n < 0 || n % up as i64 != 0 {
            return 0.0;
        }
        x.get(n as usize / up).copied().unwrap_or(0.0)
    };
    let total = down * out;
    let mut result = Vec::with_capacity(out);
    let mut start = 0;
    while start < total {
        // `buf[i]` holds stuffed sample `start - half + i`; filtered sample `start + s` lands
        // at `s + 2 * half`.
        for (i, v) in buf.iter_mut().enumerate() {
            *v = stuffed(start as i64 - half as i64 + i as i64);
        }
        forward.process(&mut buf, &mut spectrum).unwrap();
        for (s, k) in spectrum.iter_mut().zip(&kernel_spectrum) {
            *s *= k * scale;
        }
        inverse.process(&mut spectrum, &mut buf).unwrap();
        let first = start.div_ceil(down) * down;
        let mut n = first;
        while n < (start + step).min(total) {
            result.push(buf[n - start + 2 * half]);
            n += down;
        }
        start += step;
    }
    result
}

/// `CDSPFIRFilter::buildLPFilter`: a Kaiser-windowed sinc low-pass at `norm_freq` (1 is the
/// Nyquist frequency) whose taps sum to `gain`.
fn lowpass_kernel(norm_freq: f64, trans_band: f64, req_atten: f64, gain: f64) -> Vec<f64> {
    let tb = trans_band * 0.01;
    let band = if tb >= 0.25 {
        0
    } else if tb >= 0.10 {
        1
    } else {
        2
    };
    let class = if req_atten >= 117.0 {
        0
    } else if req_atten >= 60.0 {
        1
    } else {
        2
    };
    const OFFSETS: [[f64; 3]; 3] = [[1.60, 1.91, 2.25], [0.69, 0.73, 1.13], [0.21, 0.25, 0.36]];
    let mut atten = -req_atten - OFFSETS[band][class];
    let corr = ((-atten - 49.0) * 264.0 / 176.25 + 0.5).floor() as i64;
    let corr = corr.clamp(0, 264) as usize;
    let (table, scale): (&[i8; 265], f64) = match band {
        0 => (&ATTEN_CORRS_WIDE, 101.0),
        1 => (&ATTEN_CORRS_MID, 210.0),
        _ => (&ATTEN_CORRS_NARROW, 196.0),
    };
    atten -= table[corr] as f64 / scale;

    let pwr = 7.43932822146293e-8 * atten * atten
        + 0.000102747434588003
            * (0.00785021930010397 * atten).cos()
            * (0.633854318781239 + 0.103208573657699 * atten).cos()
        - 0.00798132247867036
        - 0.000903555213543865 * atten
        - 0.0969365532127236 * (0.0779275237937911 * atten).exp()
        - 1.37304948662012e-5 * atten * (0.00785021930010397 * atten).cos();
    let (hl, fo1);
    if pwr <= 0.067665322581 {
        if tb >= 0.25 {
            hl = 2.6778150875894 / tb
                + 300.547590563091 * (2.68959772209918 * pwr).atan().atan()
                    / (5.5099277187035 * tb - tb * atten.asinh().cos().tanh());
            fo1 = 0.987205355829873 * tb
                + 1.00011788929851
                    * (-0.321432067051302 - 6.19131357321578 * pwr.sqrt()).atan2(
                        hl + -1.14861472207245 / (hl - 14.1821147585957)
                            + 0.9521145021664f64
                                .powf(1.12018764830637f64.atan2(tb).powf(2.10988901686912 * hl - 20.9691278378345)),
                    );
        } else if tb >= 0.10 {
            hl = (1.56688617018066 + 142.064321294568 * pwr + 0.00419441117131136 * (243.633511747297 * pwr).cos()
                - 0.022953443903576 * atten
                - 0.026629568860284 * (127.715550622571 * pwr).cos())
                / tb;
            fo1 = 0.982299356642411 * tb
                + 0.999441744774215 * ((-0.361783054039583 - 5.80540593623676 * pwr.sqrt()) / hl).asinh();
        } else {
            hl = (2.45739657014937
                + 269.183679500541
                    * pwr
                    * (5.73225668178813
                        + (0.988861169868941 - 17.2201556280744 * pwr).cosh().atan2(1.08340138240431 * pwr))
                    .cos())
                / tb;
            fo1 = 2.291956939 * tb + 0.01942450693 * tb * tb * hl
                - 4.67538973161837 * pwr * tb
                - 1.668433124 * tb * pwr.powf(pwr);
        }
    } else if tb >= 0.25 {
        hl = (1.50258368698213 + 158.556968859477 * pwr.asinh() * (57.9466246871383 * pwr.tanh()).tanh()
            - 0.0105440479814834 * atten)
            / tb;
        fo1 = 0.994024401639321 * tb + (-0.236282717577215 - 6.8724924545387 * pwr.sin().sqrt()) / hl;
    } else if tb >= 0.10 {
        hl = (1.50277377248945 + 158.222625721046 * pwr.asinh() * (1.02875299001715 + 42.072277322604 * pwr).tanh()
            - 0.0108380943845632 * atten)
            / tb;
        fo1 = 0.992539376734551 * tb + (-0.251747813037178 - 6.74159892452584 * pwr.tan().tanh().tanh().sqrt()) / hl;
    } else {
        hl = (1.15990238966306 * pwr
            - 5.02124037125213 * pwr * pwr
            - 0.158676856669827 * atten * (1.1609073390614 * pwr - 6.33932586197475 * pwr * pwr * pwr).cos())
            / tb;
        fo1 = 0.867344453126885 * tb
            + 0.052693817907757 * tb * pwr.ln()
            + 0.0895511178735932 * tb * (59.7538527741309 * pwr).atan()
            - 0.0745653568081453 * pwr * tb;
    }

    let len2 = 0.25 * hl / norm_freq;
    let freq = PI * (1.0 - fo1) * norm_freq;
    let half = len2.floor() as usize;
    let mut window = Kaiser::new(125.0, pwr.abs(), len2, 0.0, 0);
    let mut sine = SineGen::new(freq, 0.0, 1.0 / PI);
    sine.next();
    let mut kernel = vec![0.0; 2 * half + 1];
    kernel[half] = freq * window.next() / PI;
    for t in 1..=half {
        let v = sine.next() * window.next() / t as f64;
        kernel[half + t] = v;
        kernel[half - t] = v;
    }
    let sum: f64 = kernel.iter().sum();
    let s = gain / sum;
    kernel.iter_mut().for_each(|v| *v *= s);
    kernel
}

/// `CSineGen`: sine by recurrence.
struct SineGen {
    v1: f64,
    v2: f64,
    incr: f64,
}

impl SineGen {
    fn new(step: f64, phase: f64, gain: f64) -> Self {
        Self { v1: phase.sin() * gain, v2: (phase - step).sin() * gain, incr: 2.0 * step.cos() }
    }

    fn next(&mut self) -> f64 {
        let res = self.v1;
        self.v1 = self.incr * res - self.v2;
        self.v2 = res;
        res
    }
}

/// r8brain's Kaiser window raised to `power`, stepped one tap at a time from position `wn`.
struct Kaiser {
    beta: f64,
    power: f64,
    mul: f64,
    len2_inv: f64,
    frac: f64,
    wn: i64,
}

impl Kaiser {
    fn new(beta: f64, power: f64, len2: f64, frac_delay: f64, wn: i64) -> Self {
        let beta = beta.clamp(1.0, 350.0);
        let len2_inv = 1.0 / len2;
        Self { beta, power, mul: 1.0 / bessel_i0(beta), len2_inv, frac: frac_delay * len2_inv, wn }
    }

    fn next(&mut self) -> f64 {
        let u = self.wn as f64 * self.len2_inv + self.frac;
        let n = 1.0 - u * u;
        self.wn += 1;
        let w = if n <= 0.0 { 0.0 } else { bessel_i0(self.beta * n.sqrt()) * self.mul };
        (self.power * (w.abs() + 1e-300).ln()).exp()
    }
}

/// The polynomial approximation of the modified Bessel function I0 r8brain uses.
fn bessel_i0(x: f64) -> f64 {
    let ax = x.abs();
    if ax < 3.75 {
        let y = (x / 3.75) * (x / 3.75);
        return 1.0
            + y * (3.5156229
                + y * (3.0899424 + y * (1.2067492 + y * (0.2659732 + y * (0.360768e-1 + y * 0.45813e-2)))));
    }
    let y = 3.75 / ax;
    ax.exp() / ax.sqrt()
        * (0.39894228
            + y * (0.1328592e-1
                + y * (0.225319e-2
                    + y * (-0.157565e-2
                        + y * (0.916281e-2
                            + y * (-0.2057706e-1 + y * (0.2635537e-1 + y * (-0.1647633e-1 + y * 0.392377e-2))))))))
}

/// `CDSPFracInterpolator`: fractional-delay filters picked from a bank (whole-number steps
/// between the rates) or spline-interpolated between 2nd-order coefficient sets.
struct Interpolator {
    from: f64,
    to: f64,
    len: usize,
    kind: InterpKind,
}

enum InterpKind {
    /// `out_step` filters of `len` taps; filter `k` reads at fraction `k / out_step`.
    Whole { in_step: u64, out_step: u64, bank: Vec<f64> },
    /// `fracs + 1` filters of `len` taps, three spline coefficients per tap.
    Spline { fracs: usize, bank: Vec<f64> },
}

impl Interpolator {
    fn new(from: f64, to: f64, third: bool) -> Self {
        let (beta, power, atten, len) = frac_window(ATTEN, third);
        let kind = match whole_stepping(from, to) {
            Some((in_step, out_step)) => {
                let fracs = out_step as usize;
                let bank =
                    (0..fracs).flat_map(|i| frac_filter(len, (fracs - i) as f64 / fracs as f64, beta, power)).collect();
                InterpKind::Whole { in_step, out_step, bank }
            }
            None => {
                let fracs = 6.4f64.powf(atten / 50.0).ceil() as usize;
                let raw: Vec<Vec<f64>> = (-3..=fracs as i64 + 4)
                    .map(|i| frac_filter(len, (fracs as i64 - i) as f64 / fracs as f64, beta, power))
                    .collect();
                // Second-order spline coefficients over 8 neighbouring filters, per tap.
                let bank = (0..=fracs)
                    .flat_map(|e| (0..len).map(move |j| (e, j)))
                    .flat_map(|(e, j)| {
                        let p = |k: usize| raw[e + k][j];
                        let (xm3, xm2, xm1, x0, x1, x2, x3, x4) = (p(0), p(1), p(2), p(3), p(4), p(5), p(6), p(7));
                        let scale = 1.315_789_473_684_210_5e-2;
                        [
                            x0,
                            (61.0 * (x1 - xm1) + 16.0 * (xm2 - x2) + 3.0 * (x3 - xm3)) * scale,
                            (106.0 * (xm1 + x1) + 10.0 * x3 + 6.0 * xm3 - 3.0 * x4 - 29.0 * (xm2 + x2) - 167.0 * x0)
                                * scale,
                        ]
                    })
                    .collect();
                InterpKind::Spline { fracs, bank }
            }
        };
        Self { from, to, len, kind }
    }

    /// The input frame and fraction output frame `n` reads at.
    fn position(&self, n: usize) -> (i64, f64, usize) {
        match &self.kind {
            InterpKind::Whole { in_step, out_step, .. } => {
                let p = n as u64 * in_step;
                ((p / out_step) as i64, 0.0, (p % out_step) as usize)
            }
            InterpKind::Spline { .. } => {
                let pos = n as f64 * self.from / self.to;
                let int = pos as i64;
                (int, pos - int as f64, 0)
            }
        }
    }

    fn input_needed(&self, out: usize) -> usize {
        (self.position(out - 1).0 + self.len as i64 / 2 + 1) as usize
    }

    fn run(&self, x: &[f64], out: usize) -> Vec<f64> {
        let half = self.len as i64 / 2;
        let at = |i: i64| if i < 0 { 0.0 } else { x.get(i as usize).copied().unwrap_or(0.0) };
        (0..out)
            .map(|n| {
                let (int, frac, k) = self.position(n);
                let first = int - half + 1;
                match &self.kind {
                    InterpKind::Whole { bank, .. } => {
                        let taps = &bank[k * self.len..][..self.len];
                        taps.iter().enumerate().map(|(j, t)| t * at(first + j as i64)).sum()
                    }
                    InterpKind::Spline { fracs, bank } => {
                        let xf = frac * *fracs as f64;
                        let fti = xf as usize;
                        let x1 = xf - fti as f64;
                        let x2 = x1 * x1;
                        let taps = &bank[fti * self.len * 3..][..self.len * 3];
                        taps.chunks(3)
                            .enumerate()
                            .map(|(j, c)| (c[0] + c[1] * x1 + c[2] * x2) * at(first + j as i64))
                            .sum()
                    }
                }
            })
            .collect()
    }
}

/// Kaiser parameters, rounded attenuation and filter length of the fractional-delay filters.
#[allow(clippy::excessive_precision)]
fn frac_window(att: f64, third: bool) -> (f64, f64, f64, usize) {
    const HALF: [[f64; 3]; 12] = [
        [4.1308468534586913, 1.1752580009977263, 55.5446],
        [4.4241520324148826, 1.8004881791443044, 81.4191],
        [5.2615232289173663, 1.8133318236025469, 96.3392],
        [5.9433751227216174, 1.8730186391986436, 111.1315],
        [6.8308658290513815, 1.8549555110340281, 125.4653],
        [7.6648458290312904, 1.8565766090828464, 139.7379],
        [8.2038728664307605, 1.9269521820570166, 154.0532],
        [8.7865150946655142, 1.9775307667441668, 168.2101],
        [9.5945017884101773, 1.9718456992078597, 182.1076],
        [10.5163141145985240, 1.9504067820201083, 195.5668],
        [10.2382465206362470, 2.1608923446870087, 209.0610],
        [10.9976060250714000, 2.1536533525688935, 222.5010],
    ];
    const THIRD: [[f64; 3]; 10] = [
        [3.9888564562781847, 1.5869927184268915, 66.5701],
        [4.6986694038145007, 1.8086068597928262, 86.4715],
        [5.5995071329337822, 1.8930163360942349, 106.1195],
        [6.3627287800257228, 1.9945748322093975, 125.2307],
        [7.4299550711428308, 1.9893400572347544, 144.3469],
        [8.0667715944075642, 2.0928201458699909, 163.4099],
        [8.7469970226288822, 2.1640279784268355, 181.0694],
        [10.0823430069835230, 2.0896678025321922, 199.2880],
        [10.9222206090489510, 2.1221681162186004, 216.6865],
        [21.2017743894772010, 1.1856768080118900, 233.9188],
    ];
    let (table, base): (&[[f64; 3]], usize) = if third { (&THIRD, 6) } else { (&HALF, 8) };
    let i = table.iter().position(|p| p[2] >= att).unwrap_or(table.len() - 1);
    let [beta, power, att] = table[i];
    (beta, power, att, base + i * 2)
}

/// `CDSPSincFilterGen::generateFrac`, normalised to unity DC gain: `len` taps reading at a
/// fraction `1 - fd` past the centre.
fn frac_filter(len: usize, fd: f64, beta: f64, power: f64) -> Vec<f64> {
    let len2 = (len / 2) as f64;
    let fl2 = (len / 2) as i64;
    let mut window = Kaiser::new(beta, power, len2, fd, -fl2);
    let mut out = vec![0.0; len];
    let mut idx = 0;
    let mut t = -fl2;
    if t as f64 + fd < -len2 {
        window.next();
        idx += 1;
        t += 1;
    }
    let mut f = (fd * PI).sin() / PI;
    if t & 1 != 0 {
        f = -f;
    }
    const EPS: f64 = 1.0 / (1u64 << 42) as f64;
    let at_one = (fd - 1.0).abs() < EPS;
    let mut mt = -(at_one as i64);
    let zero_x = at_one || fd.abs() < EPS;
    while t < mt {
        out[idx] = f * window.next() / (t as f64 + fd);
        idx += 1;
        t += 1;
        f = -f;
    }
    out[idx] = if zero_x { window.next() } else { f * window.next() / fd };
    mt = fl2 - 2;
    while t < mt {
        idx += 1;
        t += 1;
        f = -f;
        out[idx] = f * window.next() / (t as f64 + fd);
    }
    idx += 1;
    t += 1;
    f = -f;
    let ut = t as f64 + fd;
    out[idx] = if ut > len2 { 0.0 } else { f * window.next() / ut };
    let s = 1.0 / out.iter().sum::<f64>();
    out.iter_mut().for_each(|v| *v *= s);
    out
}

/// Input and output steps when the rates have a common divisor small enough for a filter per
/// output phase (`getWholeStepping`).
fn whole_stepping(from: f64, to: f64) -> Option<(u64, u64)> {
    let (mut l, mut s) = (from, to);
    let mut gcd = None;
    for _ in 0..149 {
        let r = l - s;
        if r == 0.0 {
            gcd = Some(s);
            break;
        }
        l = s;
        s = r.abs();
    }
    let gcd = gcd.filter(|&g| g >= 1.0)?;
    let (in_step, out_step) = (from / gcd, to / gcd);
    if in_step.fract() != 0.0 || out_step.fract() != 0.0 || out_step > 1500.0 {
        return None;
    }
    Some((in_step as u64, out_step as u64))
}

#[rustfmt::skip]
const ATTEN_CORRS_WIDE: [i8; 265] = [
    -127, -127, -125, -125, -122, -119, -115, -110, -104, -97, -91, -82, -75, -24, -16, -6, 4, 14, 24, 29,
    30, 32, 37, 44, 51, 57, 63, 67, 65, 50, 53, 56, 58, 60, 63, 64, 66, 68, 74, 77,
    78, 78, 78, 79, 79, 60, 60, 60, 61, 59, 52, 47, 41, 36, 30, 24, 17, 9, 0, -8,
    -10, -11, -14, -13, -18, -25, -31, -38, -44, -50, -57, -63, -68, -74, -81, -89, -96, -101, -104, -107,
    -109, -110, -86, -84, -85, -82, -80, -77, -73, -67, -62, -55, -48, -42, -35, -30, -20, -11, -2, 5,
    6, 6, 7, 11, 16, 21, 26, 34, 41, 46, 49, 52, 55, 56, 48, 49, 51, 51, 52, 52,
    52, 52, 52, 51, 51, 50, 47, 47, 50, 48, 46, 42, 38, 35, 31, 27, 24, 20, 16, 12,
    11, 12, 10, 8, 4, -1, -6, -11, -16, -19, -17, -21, -24, -27, -32, -34, -37, -38, -40, -41,
    -40, -40, -42, -41, -44, -45, -43, -41, -34, -31, -28, -24, -21, -18, -14, -10, -5, -1, 2, 5,
    8, 7, 4, 3, 2, 2, 4, 6, 8, 9, 9, 10, 10, 10, 10, 9, 8, 9, 11, 14,
    13, 12, 11, 10, 8, 7, 6, 5, 3, 2, 2, -1, -1, -3, -3, -4, -4, -5, -4, -6,
    -7, -9, -5, -1, -1, 0, 1, 0, -2, -3, -4, -5, -5, -8, -13, -13, -13, -12, -13, -12,
    -11, -11, -9, -8, -7, -5, -3, -1, 2, 4, 6, 9, 10, 11, 14, 18, 21, 24, 27, 30,
    34, 37, 37, 39, 40,
];

#[rustfmt::skip]
const ATTEN_CORRS_MID: [i8; 265] = [
    -113, -118, -122, -125, -126, -97, -95, -92, -92, -89, -82, -75, -69, -48, -42, -36, -30, -22, -14, -5,
    -2, 1, 6, 13, 22, 28, 35, 41, 48, 55, 56, 56, 61, 65, 71, 77, 81, 83, 85, 85,
    74, 74, 73, 72, 71, 70, 68, 64, 59, 56, 49, 52, 46, 42, 36, 32, 26, 20, 13, 7,
    -2, -6, -10, -15, -20, -27, -33, -38, -44, -43, -48, -53, -57, -63, -69, -73, -75, -79, -81, -74,
    -76, -77, -77, -78, -81, -80, -80, -78, -76, -65, -62, -59, -56, -51, -48, -44, -38, -33, -25, -19,
    -13, -5, -1, 2, 7, 13, 17, 21, 25, 30, 35, 40, 45, 50, 53, 56, 57, 55, 58, 59,
    62, 64, 67, 67, 68, 68, 62, 61, 61, 59, 59, 57, 57, 55, 52, 48, 42, 38, 35, 31,
    26, 20, 15, 13, 10, 7, 3, -2, -8, -13, -17, -23, -28, -34, -37, -40, -41, -45, -48, -50,
    -53, -57, -59, -62, -63, -63, -57, -57, -56, -56, -54, -54, -53, -49, -48, -41, -38, -33, -31, -26,
    -23, -18, -12, -9, -7, -7, -3, 0, 5, 9, 14, 16, 20, 22, 21, 23, 25, 27, 28, 29,
    34, 33, 35, 33, 31, 30, 29, 29, 26, 26, 25, 24, 20, 19, 15, 10, 8, 4, 1, -2,
    -6, -10, -16, -19, -23, -26, -27, -30, -34, -39, -43, -47, -51, -52, -54, -56, -58, -59, -62, -63,
    -66, -65, -65, -64, -59, -57, -54, -52, -48, -44, -42, -37, -32, -22, -17, -10, -3, 5, 13, 22,
    30, 40, 50, 60, 72,
];

#[rustfmt::skip]
const ATTEN_CORRS_NARROW: [i8; 265] = [
    -15, -17, -20, -20, -20, -21, -20, -16, -17, -18, -17, -13, -12, -11, -9, -7, -5, -4, -1, 1,
    3, 4, 5, 6, 7, 9, 9, 10, 10, 10, 11, 11, 11, 12, 12, 12, 10, 11, 10, 10,
    8, 10, 11, 10, 11, 11, 13, 14, 15, 19, 27, 26, 23, 18, 14, 8, 4, -2, -6, -12,
    -17, -23, -28, -33, -37, -42, -46, -49, -53, -57, -60, -61, -64, -65, -67, -66, -66, -66, -65, -64,
    -61, -59, -56, -52, -48, -42, -38, -31, -27, -19, -13, -7, -1, 8, 14, 22, 29, 37, 45, 52,
    59, 66, 73, 80, 86, 91, 96, 100, 104, 108, 111, 114, 115, 117, 118, 120, 120, 118, 117, 114,
    113, 111, 107, 103, 99, 95, 89, 84, 78, 72, 66, 60, 52, 44, 37, 30, 21, 14, 6, -3,
    -11, -18, -26, -34, -43, -51, -58, -65, -73, -78, -85, -90, -97, -102, -107, -113, -115, -118, -121, -125,
    -125, -126, -126, -126, -125, -124, -121, -119, -115, -111, -109, -101, -102, -95, -88, -81, -73, -67, -63, -54,
    -47, -40, -33, -26, -18, -11, -5, 2, 8, 14, 19, 25, 31, 36, 37, 43, 47, 49, 51, 52,
    57, 57, 56, 57, 58, 58, 58, 57, 56, 52, 52, 50, 48, 44, 41, 39, 37, 33, 31, 26,
    24, 21, 18, 14, 11, 8, 4, 2, -2, -5, -7, -9, -11, -13, -15, -16, -18, -19, -20, -23,
    -24, -24, -25, -27, -26, -27, -29, -30, -31, -32, -35, -36, -39, -40, -44, -46, -51, -54, -59, -63,
    -69, -76, -83, -91, -98,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn impulse_stays_centred_and_symmetric() {
        let mut x = vec![0.0f32; 4410];
        x[1000] = 1.0;
        let y = convert(&x, 44100.0, 48000.0, 4800);
        let peak = (0..y.len()).max_by(|&a, &b| y[a].abs().total_cmp(&y[b].abs())).unwrap();
        assert_eq!(peak, 1088);
        assert!((y.iter().map(|&v| v as f64).sum::<f64>() - 48000.0 / 44100.0).abs() < 1e-4);
    }

    #[test]
    fn picks_r8brains_stages() {
        let kinds = |from, to| {
            plan(from, to)
                .iter()
                .map(|s| match s {
                    Stage::Convolve { up, down, .. } => format!("c{up}/{down}"),
                    Stage::Interpolate(_) => "i".into(),
                    Stage::HalfBandUp(_) => "u".into(),
                    Stage::HalfBandDown(_) => "d".into(),
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert_eq!(kinds(44100.0, 48000.0), "c2/1 i");
        assert_eq!(kinds(96000.0, 48000.0), "c1/2");
        assert_eq!(kinds(32000.0, 48000.0), "c3/2");
        assert_eq!(kinds(12000.0, 48000.0), "c2/1 u");
        assert_eq!(kinds(11025.0, 48000.0), "c2/1 i c2/1 u");
        assert_eq!(kinds(192000.0, 48000.0), "d c1/2");
        assert_eq!(kinds(96000.0, 44100.0), "c1/1 i");
    }

    #[test]
    fn lowpass_taps_sum_to_the_gain() {
        let k = lowpass_kernel(0.5, TRANS_BAND, ATTEN, 2.0);
        assert_eq!(k.len() % 2, 1);
        assert!((k.iter().sum::<f64>() - 2.0).abs() < 1e-12);
        assert_eq!(k[0], k[k.len() - 1]);
    }
}
