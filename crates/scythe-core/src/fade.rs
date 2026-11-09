/// An item fade as stored in RPP `FADEIN` / `FADEOUT` (`C_FADE*SHAPE`, `D_FADE*DIR`, …).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fade {
    pub length: f64,
    /// Length set by auto-crossfading; when positive it replaces `length`.
    pub auto_length: f64,
    /// REAPER's shape index, 0..=6; see `gain` for what each one does.
    pub shape: u8,
    /// Curvature in -1..=1 bending the shape further.
    pub curve: f64,
    /// Shape `1.1`, which REAPER writes where comp areas cross-fade: an equal-power curve.
    pub power: bool,
}

impl Fade {
    pub const fn new(length: f64) -> Self {
        Self { length, auto_length: 0.0, shape: 1, curve: 0.0, power: false }
    }

    pub fn effective_length(&self) -> f64 {
        if self.auto_length > 0.0 { self.auto_length } else { self.length.max(0.0) }
    }

    /// Gain of a fade-in at `x` in 0..=1, where 0 is silence and 1 the end of the fade.
    ///
    /// Curvature bends shapes 0, 3 and 4 towards a quartic, shape 2 along a parabola and
    /// shape 1 towards a quartic (positive) or `sqrt(1 - (1-x)^8)` (negative). Shapes 5 and 6
    /// keep their S-curve and warp the time axis instead; that warp is a polynomial fitted to
    /// REAPER 7.73 renders (within 3e-4). Everything else matches REAPER exactly.
    pub fn gain(&self, x: f64) -> f64 {
        let x = x.clamp(0.0, 1.0);
        let c = self.curve.clamp(-1.0, 1.0);
        let fast = |k: i32| 1.0 - (1.0 - x).powi(k);
        let towards = |base: f64, steep_rise: f64| {
            let target = if c < 0.0 { steep_rise } else { x.powi(4) };
            base + (target - base) * c.abs()
        };
        if self.power && self.shape == 1 {
            // A quartic through the quarter sine, measured from REAPER 7.73 renders (exact
            // within 4e-8). Positive curvature bends it towards x^4; negative curvature is the
            // equal-power partner of the positive curve mirrored in time (exact too).
            use std::f64::consts::PI;
            let quarter_sine =
                |x: f64| x * (PI / 2.0 + x * ((4.0 - 1.25 * PI) + x * ((PI - 4.0) + x * (1.0 - PI / 4.0))));
            let bent = |x: f64| quarter_sine(x) + (x.powi(4) - quarter_sine(x)) * c.abs();
            return if c < 0.0 { (1.0 - bent(1.0 - x).powi(2)).max(0.0).sqrt() } else { bent(x) };
        }
        match self.shape {
            1 => towards(fast(2), fast(8).sqrt()),
            2 => x - c * x * (1.0 - x),
            5 | 6 => s_curve(self.shape, x, c),
            _ => towards(x, fast(4)),
        }
    }

    /// Gain of a fade-out at `x`, the distance to the item end over the fade length. REAPER
    /// mirrors the curvature: a fade-out with curvature `c` follows the fade-in with `-c`.
    pub fn out_gain(&self, x: f64) -> f64 {
        Fade { curve: -self.curve, ..*self }.gain(x)
    }

    /// The shape as REAPER's API numbers it (`C_FADEINSHAPE`): 0 to 6, and 7 for the
    /// equal-power curve.
    pub fn api_shape(&self) -> u8 {
        if self.power && self.shape == 1 { 7 } else { self.shape }
    }

    pub fn set_api_shape(&mut self, shape: u8) {
        self.power = shape == 7;
        self.shape = if shape == 7 { 1 } else { shape.min(6) };
    }

    /// Applies one of REAPER's seven fade types ("type 1 (linear)" to "type 7 (sharp
    /// S-curve)", 0-based) the way its "Set fade-in/out shape" actions do: each type is a
    /// shape with a preset curvature, mirrored for fade-outs. `equal_power` makes type 2 the
    /// equal-power curve, as the crossfade actions do.
    pub fn set_type(&mut self, kind: usize, fade_out: bool, equal_power: bool) {
        const TYPES: [(u8, f64); 7] = [(0, 0.0), (1, 0.0), (2, 1.0), (3, -1.0), (4, 1.0), (5, 0.0), (6, 0.0)];
        let (shape, curve) = TYPES[kind.min(6)];
        self.set_api_shape(if equal_power && kind == 1 { 7 } else { shape });
        self.curve = if fade_out { -curve } else { curve };
    }

    /// Which of REAPER's seven fade types this is, if it is one of them.
    pub fn kind(&self, fade_out: bool) -> Option<usize> {
        (0..7).find(|&k| {
            let mut f = *self;
            f.set_type(k, fade_out, self.power);
            f.shape == self.shape && f.power == self.power && (f.curve - self.curve).abs() < 1e-9
        })
    }
}

fn s_curve(shape: u8, x: f64, c: f64) -> f64 {
    if c < 0.0 {
        return 1.0 - s_curve(shape, 1.0 - x, -c);
    }
    let warp: &[[f64; 4]] = if shape == 5 { &WARP5 } else { &WARP6 };
    let u = 2.0 * x - 1.0;
    let (mut t0, mut t1) = (1.0, u);
    let mut sum = 0.0;
    for (i, row) in warp.iter().enumerate() {
        let t = match i {
            0 => t0,
            1 => t1,
            _ => {
                let t2 = 2.0 * u * t1 - t0;
                (t0, t1) = (t1, t2);
                t2
            }
        };
        let poly = row.iter().rev().fold(0.0, |acc, k| acc * c + k) * c;
        sum += t * poly;
    }
    let w = (x + x * (1.0 - x) * sum).clamp(0.0, 1.0);
    if shape == 5 {
        w * w * (3.0 - 2.0 * w)
    } else if w < 0.5 {
        0.5 * (2.0 * w).powi(4)
    } else {
        1.0 - 0.5 * (2.0 * (1.0 - w)).powi(4)
    }
}

/// Time warp of the S-curves under positive curvature: `x + x(1-x) * sum T_i(2x-1) * P_i(c)`
/// with Chebyshev polynomials `T_i` and `P_i(c) = sum_j row[j] * c^(j+1)`.
const WARP5: [[f64; 4]; 7] = [
    [-0.999135578, -0.0147857489, -0.0513778001, -0.046033335],
    [0.0142086351, -0.55398008, 0.0883056404, -0.127228051],
    [0.00197861181, -0.0286258724, -0.0750980362, -0.087280076],
    [0.00948986252, -0.0385892976, 0.0677507196, -0.101588071],
    [0.00195752302, -0.0235823533, 0.065962485, -0.0693242916],
    [0.00414344769, -0.0201267767, 0.0407957655, -0.0316098246],
    [0.000642100175, -0.00732156494, 0.0184502062, -0.0148847538],
];
const WARP6: [[f64; 4]; 9] = [
    [-1.22773215, -0.205006095, 0.382294564, -0.500627253],
    [0.0168924152, -0.75906458, -0.246711807, 0.0903895062],
    [0.0403417796, -0.364794392, 0.696426134, -0.887627082],
    [0.012538119, -0.00314835241, -0.116811947, 4.90551207e-05],
    [0.0289371759, -0.254661407, 0.68989988, -0.610239163],
    [0.00970841527, -0.0360575163, 0.0334434442, -0.0105120158],
    [0.0138072993, -0.114747706, 0.294594124, -0.235062745],
    [0.00265636847, -0.0109567305, 0.0112841358, -0.00079900434],
    [0.00327909137, -0.0266476105, 0.0659041443, -0.0499431687],
];

impl Default for Fade {
    fn default() -> Self {
        Self::new(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shape_spans_silence_to_unity() {
        for shape in 0..=6 {
            for curve in [-1.0, 0.0, 0.5] {
                let f = Fade { shape, curve, ..Fade::new(1.0) };
                assert!(f.gain(0.0).abs() < 1e-12, "shape {shape}");
                assert!((f.gain(1.0) - 1.0).abs() < 1e-12, "shape {shape}");
                let mut last = 0.0;
                for i in 0..=20 {
                    let g = f.gain(i as f64 / 20.0);
                    assert!(g >= last - 1e-12, "shape {shape} not monotonic");
                    last = g;
                }
            }
        }
    }

    /// Values measured from REAPER 7.73 renders (`examples/reaper_ref.rs` in scythe-engine).
    #[test]
    fn shapes_match_reaper() {
        let cases: &[(u8, f64, [f64; 5])] = &[
            (0, 0.0, [0.1, 0.25, 0.5, 0.75, 0.9]),
            (1, 0.0, [0.19, 0.4376, 0.75, 0.9376, 0.99]),
            (2, 0.0, [0.1, 0.25, 0.5, 0.75, 0.9]),
            (5, 0.0, [0.028, 0.1562, 0.5, 0.8438, 0.972]),
            (6, 0.0, [0.0008, 0.0312, 0.5, 0.9688, 0.9992]),
            (0, -0.5, [0.222, 0.4668, 0.7188, 0.873, 0.95]),
            (0, 1.0, [0.0, 0.004, 0.0624, 0.3164, 0.656]),
            (3, -1.0, [0.3438, 0.6836, 0.9376, 0.996, 0.9998]),
            (2, -0.5, [0.145, 0.3438, 0.625, 0.8438, 0.945]),
            (2, 1.0, [0.01, 0.0624, 0.25, 0.5624, 0.81]),
            (1, 0.5, [0.095, 0.2208, 0.4062, 0.627, 0.823]),
            (1, 1.0, [0.0, 0.004, 0.0624, 0.3164, 0.656]),
            (1, -1.0, [0.7547, 0.9486, 0.998, 1.0, 1.0]),
            (1, -0.5, [0.4723, 0.69306, 0.874, 0.9687, 0.995]),
            (5, 0.5, [0.0113, 0.0747, 0.3191, 0.7106, 0.9348]),
            (5, -1.0, [0.147, 0.4731, 0.8278, 0.9697, 0.9961]),
            (6, 1.0, [0.0, 0.0003, 0.0165, 0.328, 0.9352]),
            (6, -0.5, [0.0073, 0.1767, 0.8835, 0.9959, 0.9999]),
            (6, 0.3, [0.0002, 0.0096, 0.2196, 0.9086, 0.997]),
        ];
        for &(shape, curve, expect) in cases {
            let f = Fade { shape, curve, ..Fade::new(1.0) };
            for (x, want) in [0.1, 0.25, 0.5, 0.75, 0.9].into_iter().zip(expect) {
                let got = f.gain(x);
                assert!((got - want).abs() < 1e-3, "shape {shape} curve {curve} at {x}: {got} vs {want}");
            }
        }
    }

    #[test]
    fn fade_outs_mirror_the_curvature() {
        let f = Fade { shape: 1, curve: 1.0, ..Fade::new(1.0) };
        assert!((f.out_gain(0.1) - 0.7547).abs() < 1e-3);
        assert_eq!(f.out_gain(0.3), Fade { curve: -1.0, ..f }.gain(0.3));
    }

    #[test]
    fn auto_length_wins() {
        let f = Fade { auto_length: 0.5, ..Fade::new(0.01) };
        assert_eq!(f.effective_length(), 0.5);
    }
}
