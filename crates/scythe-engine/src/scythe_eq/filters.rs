//! Band shapes as analog prototypes and their digital counterparts.
//!
//! Every band is a cascade of analog first- and second-order sections. The zero-latency path
//! turns each section into a biquad whose poles are the analog poles mapped through
//! `z = e^(sT)` (impulse invariance) and whose numerator makes the digital magnitude equal
//! the analog one at DC, at Nyquist and at the section's own frequency (M. Vicanek, "Matched
//! Second Order Digital Filters", 2016, applied to every section). Unlike the bilinear transform this keeps the analog
//! shape up to Nyquist instead of cramping it there; the linear and natural phase paths
//! sample the analog response directly.

use std::f64::consts::PI;

/// Sections a band can need: a 144 dB/oct brickwall is 12, a 96 dB/oct band pass 8 and a
/// flat tilt 10.
pub const MAX_SECTIONS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Shape {
    #[default]
    Bell,
    LowShelf,
    HighShelf,
    LowCut,
    HighCut,
    Notch,
    BandPass,
    TiltShelf,
    FlatTilt,
    AllPass,
}

impl Shape {
    pub const ALL: [Shape; 10] = [
        Self::Bell,
        Self::LowShelf,
        Self::HighShelf,
        Self::LowCut,
        Self::HighCut,
        Self::Notch,
        Self::BandPass,
        Self::TiltShelf,
        Self::FlatTilt,
        Self::AllPass,
    ];

    pub fn from_index(i: usize) -> Self {
        Self::ALL.get(i).copied().unwrap_or_default()
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Bell => "Bell",
            Self::LowShelf => "Low shelf",
            Self::HighShelf => "High shelf",
            Self::LowCut => "Low cut",
            Self::HighCut => "High cut",
            Self::Notch => "Notch",
            Self::BandPass => "Band pass",
            Self::TiltShelf => "Tilt shelf",
            Self::FlatTilt => "Flat tilt",
            Self::AllPass => "All pass",
        }
    }

    /// Whether the gain parameter means anything for the shape (and so whether it can be
    /// dynamic).
    pub fn has_gain(self) -> bool {
        matches!(self, Self::Bell | Self::LowShelf | Self::HighShelf | Self::TiltShelf | Self::FlatTilt)
    }

    pub fn has_slope(self) -> bool {
        matches!(
            self,
            Self::LowShelf | Self::HighShelf | Self::LowCut | Self::HighCut | Self::BandPass | Self::TiltShelf
        )
    }
}

/// Slopes in dB per octave; the last entry is the brickwall.
pub const SLOPES: [u32; 10] = [6, 12, 18, 24, 30, 36, 48, 72, 96, 0];
pub const BRICKWALL: usize = 9;

/// The filter order (in 6 dB/oct steps) of slope index `i` when run as an IIR cascade; the
/// brickwall becomes a 144 dB/oct Butterworth there.
pub fn slope_order(i: usize) -> usize {
    match SLOPES.get(i) {
        Some(0) | None => 24,
        Some(s) => (*s / 6) as usize,
    }
}

pub fn slope_label(i: usize) -> String {
    match SLOPES.get(i) {
        Some(0) | None => "Brickwall".into(),
        Some(s) => format!("{s} dB/oct"),
    }
}

/// How a section's numerator is made digital.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    /// Magnitude matched (minimum phase).
    #[default]
    Matched,
    /// Zeros on the unit circle at the analog zero frequency.
    Notch,
    /// Numerator mirrors the denominator.
    AllPass,
}

/// `(b0 s² + b1 s + b2) / (a0 s² + a1 s + a2)` with `s` normalised to `w0` rad/s; a first-order
/// section has `b0 = a0 = 0`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Section {
    pub b: [f64; 3],
    pub a: [f64; 3],
    pub w0: f64,
    pub kind: Kind,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct C64 {
    pub re: f64,
    pub im: f64,
}

impl C64 {
    pub const ONE: C64 = C64 { re: 1.0, im: 0.0 };

    pub fn new(re: f64, im: f64) -> Self {
        Self { re, im }
    }

    pub fn times(self, o: C64) -> C64 {
        C64::new(self.re * o.re - self.im * o.im, self.re * o.im + self.im * o.re)
    }

    pub fn over(self, o: C64) -> C64 {
        let d = (o.re * o.re + o.im * o.im).max(1e-300);
        C64::new((self.re * o.re + self.im * o.im) / d, (self.im * o.re - self.re * o.im) / d)
    }

    pub fn scale(self, k: f64) -> C64 {
        C64::new(self.re * k, self.im * k)
    }

    pub fn plus(self, o: C64) -> C64 {
        C64::new(self.re + o.re, self.im + o.im)
    }

    pub fn minus(self, o: C64) -> C64 {
        C64::new(self.re - o.re, self.im - o.im)
    }

    pub fn norm_sqr(self) -> f64 {
        self.re * self.re + self.im * self.im
    }

    pub fn abs(self) -> f64 {
        self.norm_sqr().sqrt()
    }
}

impl Section {
    fn new(b: [f64; 3], a: [f64; 3], w0: f64) -> Self {
        Self { b, a, w0, kind: Kind::Matched }
    }

    /// The analog response at `w` rad/s.
    pub fn response(&self, w: f64) -> C64 {
        let x = w / self.w0;
        let num = C64::new(self.b[2] - self.b[0] * x * x, self.b[1] * x);
        let den = C64::new(self.a[2] - self.a[0] * x * x, self.a[1] * x);
        num.over(den)
    }
}

/// A band's analog prototype: sections and a constant gain.
#[derive(Debug, Clone, Copy)]
pub struct Prototype {
    pub sections: [Section; MAX_SECTIONS],
    pub len: usize,
    pub gain: f64,
}

impl Default for Prototype {
    fn default() -> Self {
        Self { sections: [Section::default(); MAX_SECTIONS], len: 0, gain: 1.0 }
    }
}

/// What shapes a band.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Design {
    pub shape: Shape,
    pub freq: f64,
    pub gain_db: f64,
    pub q: f64,
    pub slope: usize,
    /// Narrow bells as they get louder (Q times the square root of the linear gain).
    pub gain_q: bool,
}

impl Default for Design {
    fn default() -> Self {
        Self { shape: Shape::Bell, freq: 1000.0, gain_db: 0.0, q: 1.0, slope: 1, gain_q: false }
    }
}

const SQRT_HALF: f64 = std::f64::consts::FRAC_1_SQRT_2;

impl Prototype {
    fn push(&mut self, s: Section) {
        if self.len < MAX_SECTIONS {
            self.sections[self.len] = s;
            self.len += 1;
        }
    }

    pub fn sections(&self) -> &[Section] {
        &self.sections[..self.len]
    }

    /// The analog response at `hz`.
    pub fn response(&self, hz: f64) -> C64 {
        let w = 2.0 * PI * hz;
        self.sections().iter().fold(C64::new(self.gain, 0.0), |acc, s| acc.times(s.response(w)))
    }

    pub fn magnitude(&self, hz: f64) -> f64 {
        self.response(hz).abs()
    }

    pub fn design(d: &Design) -> Self {
        let mut p = Prototype::default();
        let w0 = 2.0 * PI * d.freq.max(1.0);
        let q = d.q.clamp(0.01, 100.0);
        match d.shape {
            Shape::Bell => {
                let a = 10f64.powf(d.gain_db / 40.0);
                let q = if d.gain_q { q * 10f64.powf(d.gain_db.abs() / 40.0) } else { q };
                p.push(Section::new([1.0, a / q, 1.0], [1.0, 1.0 / (a * q), 1.0], w0));
            }
            Shape::LowShelf | Shape::HighShelf | Shape::TiltShelf => {
                let order = slope_order(d.slope).clamp(1, 8);
                let g = 10f64.powf(d.gain_db / 20.0);
                let high = d.shape != Shape::LowShelf;
                shelf(&mut p, if high { g } else { 1.0 / g }, order, q, w0);
                p.gain = match d.shape {
                    Shape::LowShelf => g,
                    Shape::TiltShelf => 1.0 / g.sqrt(),
                    _ => 1.0,
                };
            }
            Shape::LowCut | Shape::HighCut => {
                let high_pass = d.shape == Shape::LowCut;
                butterworth(&mut p, slope_order(d.slope), q, w0, high_pass);
            }
            Shape::Notch => {
                let mut s = Section::new([1.0, 0.0, 1.0], [1.0, 1.0 / q, 1.0], w0);
                s.kind = Kind::Notch;
                p.push(s);
            }
            Shape::BandPass => {
                let n = (slope_order(d.slope.min(8)) / 2).clamp(1, 8);
                // n identical sections narrow the -3 dB band by sqrt(2^(1/n) - 1).
                let qs = q * (2f64.powf(1.0 / n as f64) - 1.0).sqrt();
                for _ in 0..n {
                    p.push(Section::new([0.0, 1.0 / qs, 0.0], [1.0, 1.0 / qs, 1.0], w0));
                }
            }
            Shape::FlatTilt => {
                // First-order shelves an octave apart across 20 Hz..20 kHz approximate a
                // constant dB/oct slope; the result is pivoted to 0 dB at the band frequency.
                let step = d.gain_db / 10.0;
                let g = 10f64.powf(step / 20.0);
                for k in 0..10 {
                    let wk = 2.0 * PI * 20.0 * 2f64.powf(k as f64 + 0.5);
                    let r = g.sqrt();
                    p.push(Section::new([0.0, r, 1.0], [0.0, 1.0 / r, 1.0], wk));
                }
                p.gain = 1.0 / p.magnitude(d.freq).max(1e-12);
            }
            Shape::AllPass => {
                let mut s = Section::new([1.0, -1.0 / q, 1.0], [1.0, 1.0 / q, 1.0], w0);
                s.kind = Kind::AllPass;
                p.push(s);
            }
        }
        p
    }
}

/// Butterworth section dampings of order `n`, first-order section last for odd orders.
fn dampings(n: usize) -> impl Iterator<Item = f64> {
    (1..=n / 2).map(move |k| 2.0 * ((2 * k - 1) as f64 * PI / (2 * n) as f64).sin())
}

/// A Butterworth low or high pass of order `n`; Q other than 0.707 raises the resonance of
/// its least damped section.
fn butterworth(p: &mut Prototype, n: usize, q: f64, w0: f64, high_pass: bool) {
    for (k, d) in dampings(n).enumerate() {
        let d = if k == 0 { d * SQRT_HALF / q } else { d };
        let b = if high_pass { [1.0, 0.0, 0.0] } else { [0.0, 0.0, 1.0] };
        p.push(Section::new(b, [1.0, d, 1.0], w0));
    }
    if n % 2 == 1 {
        let b = if high_pass { [0.0, 1.0, 0.0] } else { [0.0, 0.0, 1.0] };
        p.push(Section::new(b, [0.0, 1.0, 1.0], w0));
    }
}

/// A Butterworth high shelf of order `n` going from 1 to `g` (amplitude), centred on `w0`
/// (half the gain in dB there): `B(s·r) / B(s/r)` with `r = g^(1/2n)` and `B` the Butterworth
/// polynomial (Holters & Zölzer, "Parametric Higher-Order Shelving Filters", 2006).
fn shelf(p: &mut Prototype, g: f64, n: usize, q: f64, w0: f64) {
    let r = g.powf(1.0 / (2 * n) as f64);
    for (k, d) in dampings(n).enumerate() {
        let d = if k == 0 { d * SQRT_HALF / q } else { d };
        p.push(Section::new([r * r, d * r, 1.0], [1.0 / (r * r), d / r, 1.0], w0));
    }
    if n % 2 == 1 {
        p.push(Section::new([0.0, r, 1.0], [0.0, 1.0 / r, 1.0], w0));
    }
}

/// A digital biquad `(b0 + b1 z⁻¹ + b2 z⁻²) / (1 + a1 z⁻¹ + a2 z⁻²)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Biquad {
    pub b: [f64; 3],
    pub a: [f64; 2],
}

impl Default for Biquad {
    fn default() -> Self {
        Self { b: [1.0, 0.0, 0.0], a: [0.0, 0.0] }
    }
}

impl Biquad {
    /// The response at `w` radians per sample.
    pub fn response(&self, w: f64) -> C64 {
        let z1 = C64::new(w.cos(), -w.sin());
        let z2 = C64::new((2.0 * w).cos(), -(2.0 * w).sin());
        let num = C64::new(self.b[0], 0.0).plus(z1.scale(self.b[1])).plus(z2.scale(self.b[2]));
        let den = C64::ONE.plus(z1.scale(self.a[0])).plus(z2.scale(self.a[1]));
        num.over(den)
    }

    /// The matched digital version of analog section `s` at `fs`.
    pub fn matched(s: &Section, fs: f64) -> Self {
        let t = 1.0 / fs;
        let nyq = PI * fs;
        if s.a[0] == 0.0 {
            // First order: pole at -a2 w0 / a1.
            let pole = -s.a[2] * s.w0 / s.a[1].max(1e-300);
            let a1 = -(pole * t).exp();
            let h0 = s.response(0.0).abs();
            let hn = s.response(nyq).abs();
            let (sb0, sb1) = (h0 * (1.0 + a1).abs(), hn * (1.0 - a1).abs());
            return Self { b: [(sb0 + sb1) / 2.0, (sb0 - sb1) / 2.0, 0.0], a: [a1, 0.0] };
        }
        let (a1, a2) = matched_poles(s, t);
        match s.kind {
            Kind::AllPass => Self { b: [a2, a1, 1.0], a: [a1, a2] },
            Kind::Notch => {
                let wz = s.w0 * (s.b[2] / s.b[0]).sqrt();
                let th = (wz * t).min(PI);
                let h0 = s.response(0.0).abs();
                let k = h0 * (1.0 + a1 + a2) / (2.0 - 2.0 * th.cos()).max(1e-300);
                Self { b: [k, -2.0 * th.cos() * k, k], a: [a1, a2] }
            }
            Kind::Matched if dips(s) => {
                // Its zeros are less damped than its poles: matching the reciprocal (a boost)
                // and inverting it keeps the narrow part exact, where damped matched poles
                // near Nyquist could not carry it.
                let inverse = Section { b: s.a, a: s.b, ..*s };
                let up = Self::matched(&inverse, fs);
                let k = up.b[0];
                Self { b: [1.0 / k, up.a[0] / k, up.a[1] / k], a: [up.b[1] / k, up.b[2] / k] }
            }
            Kind::Matched => Self { b: fit_numerator(s, fs, a1, a2), a: [a1, a2] },
        }
    }
}

/// Whether a full second-order section has narrower zeros than poles (a cut).
fn dips(s: &Section) -> bool {
    if s.b[0] <= 0.0 || s.b[2] <= 0.0 {
        return false;
    }
    let damping = |c: &[f64; 3]| c[1] / (2.0 * (c[0] * c[2]).sqrt());
    damping(&s.b) < damping(&s.a)
}

fn matched_poles(s: &Section, t: f64) -> (f64, f64) {
    let wn = s.w0 * (s.a[2] / s.a[0]).sqrt();
    let zeta = s.a[1] / (2.0 * (s.a[0] * s.a[2]).sqrt());
    if zeta < 1.0 {
        let r = (-zeta * wn * t).exp();
        let th = (wn * t * (1.0 - zeta * zeta).sqrt()).min(PI);
        (-2.0 * r * th.cos(), r * r)
    } else {
        let root = (zeta * zeta - 1.0).sqrt();
        let (p1, p2) = (-wn * (zeta - root), -wn * (zeta + root));
        (-((p1 * t).exp() + (p2 * t).exp()), ((p1 + p2) * t).exp())
    }
}

/// The minimum-phase numerator whose magnitude, over the matched poles, equals the analog
/// section's at DC, at Nyquist and at the section's own frequency (the geometric mean of its
/// pole and zero frequencies; a quarter of the sample rate when that lies near or past
/// Nyquist).
fn fit_numerator(s: &Section, fs: f64, a1: f64, a2: f64) -> [f64; 3] {
    let big_a = [(1.0 + a1 + a2).powi(2), (1.0 - a1 + a2).powi(2), -4.0 * a2];
    let nyq = PI * fs;
    let b0 = s.response(0.0).norm_sqr() * big_a[0];
    let b1 = s.response(nyq).norm_sqr() * big_a[1];
    let poles = s.w0 * (s.a[2] / s.a[0]).sqrt();
    let wm = if s.b[0] > 0.0 && s.b[2] > 0.0 { (poles * s.w0 * (s.b[2] / s.b[0]).sqrt()).sqrt() } else { poles };
    let w = wm / fs;
    let w = if w > 0.9 * PI { PI / 2.0 } else { w.max(1e-4) };
    let p1 = (w / 2.0).sin().powi(2);
    let p0 = 1.0 - p1;
    let p2 = 4.0 * p0 * p1;
    let target = s.response(w * fs).norm_sqr() * (big_a[0] * p0 + big_a[1] * p1 + big_a[2] * p2);
    let b2 = (target - b0 * p0 - b1 * p1) / p2;
    let (r0, r1) = (b0.max(0.0).sqrt(), b1.max(0.0).sqrt());
    let w = 0.5 * (r0 + r1);
    let b2 = b2.max(-w * w);
    let c0 = 0.5 * (w + (w * w + b2).sqrt());
    let c1 = 0.5 * (r0 - r1);
    let c2 = if c0 > 0.0 { -b2 / (4.0 * c0) } else { 0.0 };
    [c0, c1, c2]
}

/// A band made digital: its biquads and constant gain.
#[derive(Debug, Clone, Copy)]
pub struct Cascade {
    pub stages: [Biquad; MAX_SECTIONS],
    pub len: usize,
    pub gain: f64,
}

impl Default for Cascade {
    fn default() -> Self {
        Self { stages: [Biquad::default(); MAX_SECTIONS], len: 0, gain: 1.0 }
    }
}

impl Cascade {
    pub fn matched(p: &Prototype, fs: f64) -> Self {
        let mut c = Cascade { gain: p.gain, len: p.len, ..Default::default() };
        for (dst, s) in c.stages.iter_mut().zip(p.sections()) {
            *dst = Biquad::matched(s, fs);
        }
        c
    }

    pub fn stages(&self) -> &[Biquad] {
        &self.stages[..self.len]
    }

    /// The digital response at `hz`.
    pub fn response(&self, hz: f64, fs: f64) -> C64 {
        let w = 2.0 * PI * hz / fs;
        self.stages().iter().fold(C64::new(self.gain, 0.0), |acc, b| acc.times(b.response(w)))
    }
}

/// Filter memory for one channel of a cascade, in transposed direct form II.
#[derive(Debug, Clone, Copy)]
pub struct CascadeState {
    z: [[f64; 2]; MAX_SECTIONS],
}

impl Default for CascadeState {
    fn default() -> Self {
        Self { z: [[0.0; 2]; MAX_SECTIONS] }
    }
}

impl CascadeState {
    #[inline]
    pub fn run(&mut self, c: &Cascade, x: f64) -> f64 {
        let mut y = x * c.gain;
        for (b, z) in c.stages().iter().zip(self.z.iter_mut()) {
            let out = b.b[0] * y + z[0];
            z[0] = b.b[1] * y - b.a[0] * out + z[1];
            z[1] = b.b[2] * y - b.a[1] * out;
            y = out;
        }
        y
    }

    pub fn reset(&mut self) {
        self.z = [[0.0; 2]; MAX_SECTIONS];
    }

    /// Clears memories too small to matter, so silence never decays into subnormals.
    pub fn flush(&mut self) {
        for z in self.z.iter_mut().flatten() {
            if z.abs() < 1e-30 {
                *z = 0.0;
            }
        }
    }

    #[cfg(test)]
    pub fn values(&self) -> impl Iterator<Item = f64> + '_ {
        self.z.iter().flatten().copied()
    }
}

/// The 2nd-order filter that lets a band's region through when it is soloed or its detector
/// is auditioned.
pub fn audition_design(d: &Design) -> Design {
    let shape = match d.shape {
        Shape::LowShelf | Shape::HighCut => Shape::HighCut,
        Shape::HighShelf | Shape::LowCut | Shape::TiltShelf | Shape::FlatTilt => Shape::LowCut,
        _ => Shape::BandPass,
    };
    let q = if shape == Shape::BandPass { d.q.clamp(0.3, 30.0) } else { SQRT_HALF };
    let shape = match (d.shape, shape) {
        (Shape::LowShelf, _) | (Shape::HighCut, _) => Shape::HighCut,
        _ => shape,
    };
    Design { shape, freq: d.freq, gain_db: 0.0, q, slope: 1, gain_q: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(x: f64) -> f64 {
        20.0 * x.max(1e-30).log10()
    }

    fn worst(d: &Design, fs: f64, top: f64) -> (f64, f64) {
        let p = Prototype::design(d);
        let c = Cascade::matched(&p, fs);
        let mut worst = (0.0f64, 0.0);
        let mut f = 20.0;
        while f <= top {
            let (a, dg) = (db(p.magnitude(f)), db(c.response(f, fs).abs()));
            if a > -60.0 && (a - dg).abs() > worst.0 {
                worst = ((a - dg).abs(), f);
            }
            f *= 1.02;
        }
        worst
    }

    fn design(shape: Shape, freq: f64, gain_db: f64, q: f64, slope: usize) -> Design {
        Design { shape, freq, gain_db, q, slope, gain_q: false }
    }

    #[test]
    fn analog_prototypes_hit_their_nominal_points() {
        let p = Prototype::design(&design(Shape::Bell, 1000.0, 12.0, 2.0, 1));
        assert!((db(p.magnitude(1000.0)) - 12.0).abs() < 1e-9);
        assert!(db(p.magnitude(20.0)).abs() < 0.05);
        let p = Prototype::design(&design(Shape::LowShelf, 200.0, -9.0, SQRT_HALF, 1));
        assert!((db(p.magnitude(10.0)) + 9.0).abs() < 0.05);
        assert!((db(p.magnitude(200.0)) + 4.5).abs() < 1e-6);
        for slope in 0..9 {
            let order = slope_order(slope) as f64;
            let p = Prototype::design(&design(Shape::LowCut, 100.0, 0.0, SQRT_HALF, slope));
            assert!((db(p.magnitude(100.0)) + 3.0103).abs() < 1e-3, "slope {slope}");
            // Two octaves below the corner the response is on its asymptote.
            let below = db(p.magnitude(25.0));
            assert!((below + 6.0206 * 2.0 * order).abs() < 0.3, "slope {slope}: {below}");
        }
        for order in 1..=8 {
            let slope = SLOPES.iter().position(|s| *s as usize == order * 6).unwrap_or(1);
            let p = Prototype::design(&design(Shape::HighShelf, 2000.0, 10.0, SQRT_HALF, slope));
            assert!((db(p.magnitude(2000.0)) - 5.0).abs() < 1e-6);
            assert!((db(p.magnitude(30000.0)) - 10.0).abs() < 0.2, "order {order}");
        }
        let p = Prototype::design(&design(Shape::TiltShelf, 1000.0, 12.0, SQRT_HALF, 1));
        assert!((db(p.magnitude(10.0)) + 6.0).abs() < 0.05 && (db(p.magnitude(100000.0)) - 6.0).abs() < 0.05);
        let p = Prototype::design(&design(Shape::FlatTilt, 1000.0, 10.0, 1.0, 1));
        assert!(db(p.magnitude(1000.0)).abs() < 1e-9);
        // Constant slope: one dB per octave within half a dB across the audio band.
        for oct in -5..=4 {
            let f = 1000.0 * 2f64.powi(oct);
            assert!((db(p.magnitude(f)) - oct as f64).abs() < 0.5, "{f}: {}", db(p.magnitude(f)));
        }
        let p = Prototype::design(&design(Shape::Notch, 500.0, 0.0, 4.0, 1));
        assert!(p.magnitude(500.0) < 1e-9);
        let p = Prototype::design(&design(Shape::AllPass, 500.0, 0.0, 1.0, 1));
        assert!((p.magnitude(123.0) - 1.0).abs() < 1e-12);
        for slope in [1, 3, 6] {
            let p = Prototype::design(&design(Shape::BandPass, 1000.0, 0.0, 2.0, slope));
            assert!(db(p.magnitude(1000.0)).abs() < 1e-9);
            // Q 2 is half an octave between the -3 dB points: 1000·2^(±0.36).
            let lo = 1000.0 * ((1.0 + 1.0 / 16.0f64).sqrt() - 0.25);
            assert!((db(p.magnitude(lo)) + 3.0103).abs() < 0.05, "slope {slope}: {}", db(p.magnitude(lo)));
        }
    }

    /// The matched digital bands follow the analog prototype up to 20 kHz, where the
    /// bilinear transform would cramp them towards Nyquist.
    #[test]
    fn matched_bands_follow_the_analog_response_to_nyquist() {
        let cases = [
            (design(Shape::Bell, 1000.0, 12.0, 1.0, 1), 0.05f64),
            (design(Shape::Bell, 10000.0, 12.0, 1.0, 1), 0.8),
            (design(Shape::Bell, 15000.0, -12.0, 2.0, 1), 1.0),
            (design(Shape::Bell, 100.0, -18.0, 8.0, 1), 0.05),
            (design(Shape::HighShelf, 8000.0, 9.0, SQRT_HALF, 1), 0.3),
            (design(Shape::LowShelf, 100.0, -9.0, SQRT_HALF, 3), 0.05),
            (design(Shape::LowCut, 80.0, 0.0, SQRT_HALF, 3), 0.05),
            (design(Shape::HighCut, 12000.0, 0.0, SQRT_HALF, 1), 1.0),
            (design(Shape::BandPass, 3000.0, 0.0, 1.0, 1), 1.0),
            (design(Shape::TiltShelf, 1000.0, 6.0, SQRT_HALF, 1), 0.1),
            (design(Shape::FlatTilt, 1000.0, 6.0, 1.0, 1), 0.3),
        ];
        for fs in [44100.0, 48000.0, 96000.0, 192000.0] {
            for &(ref d, tol) in &cases {
                let (err, at) = worst(d, fs, 20000.0);
                let tol: f64 = if fs > 50000.0 { tol * 0.5 } else { tol };
                assert!(err < tol, "{:?} {} Hz at {fs}: {err:.3} dB off at {at:.0} Hz", d.shape, d.freq);
            }
        }
        // Against the bilinear transform: a 15 kHz bell's top end at 44.1 kHz.
        let d = design(Shape::Bell, 15000.0, 12.0, 1.0, 1);
        let p = Prototype::design(&d);
        let c = Cascade::matched(&p, 44100.0);
        let analog = db(p.magnitude(20000.0));
        assert!((db(c.response(20000.0, 44100.0).abs()) - analog).abs() < 1.5, "{analog}");
    }

    #[test]
    fn notch_and_all_pass_keep_their_character() {
        let fs = 48000.0;
        let c = Cascade::matched(&Prototype::design(&design(Shape::Notch, 1000.0, 0.0, 4.0, 1)), fs);
        assert!(c.response(1000.0, fs).abs() < 1e-9);
        assert!(db(c.response(20.0, fs).abs()).abs() < 0.01);
        let c = Cascade::matched(&Prototype::design(&design(Shape::AllPass, 1000.0, 0.0, 1.0, 1)), fs);
        for f in [30.0, 1000.0, 15000.0] {
            assert!((c.response(f, fs).abs() - 1.0).abs() < 1e-9);
        }
        // Phase passes -180° at the centre.
        let ph = c.response(1000.0, fs);
        assert!(ph.re < -0.99, "{ph:?}");
    }

    #[test]
    fn cascades_run_without_subnormals() {
        let fs = 48000.0;
        let c = Cascade::matched(&Prototype::design(&design(Shape::LowCut, 10.0, 0.0, 10.0, 8)), fs);
        let mut st = CascadeState::default();
        st.run(&c, 1.0);
        for i in 0..2_000_000 {
            st.run(&c, 0.0);
            if i % 256 == 0 {
                st.flush();
            }
        }
        assert!(st.values().all(|v| v == 0.0 || v.is_normal()));
    }
}
