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
}

impl Fade {
    pub const fn new(length: f64) -> Self {
        Self { length, auto_length: 0.0, shape: 1, curve: 0.0 }
    }

    pub fn effective_length(&self) -> f64 {
        if self.auto_length > 0.0 { self.auto_length } else { self.length.max(0.0) }
    }

    /// Gain at `x` in 0..=1, where 0 is silence and 1 the end of the fade (for a fade-out, `x`
    /// is the distance to the item end over the fade length).
    ///
    /// Shapes 0, 2, 3 and 4 and the curvature of shapes 0..=4 match REAPER renders exactly;
    /// curvature bends shapes 0, 3 and 4 towards a quartic, shape 2 along a parabola and shape 1
    /// towards a quartic (positive) or a steep rise (negative, approximated). Shapes 5 and 6
    /// ignore curvature.
    pub fn gain(&self, x: f64) -> f64 {
        let x = x.clamp(0.0, 1.0);
        let c = self.curve.clamp(-1.0, 1.0);
        let fast = |k: i32| 1.0 - (1.0 - x).powi(k);
        let towards = |base: f64, steep_rise: f64| {
            let target = if c < 0.0 { steep_rise } else { x.powi(4) };
            base + (target - base) * c.abs()
        };
        match self.shape {
            1 => towards(fast(2), fast(10)),
            2 => x - c * x * (1.0 - x),
            5 => x * x * (3.0 - 2.0 * x),
            6 if x < 0.5 => 0.5 * (2.0 * x).powi(4),
            6 => 1.0 - 0.5 * (2.0 * (1.0 - x)).powi(4),
            _ => towards(x, fast(4)),
        }
    }
}

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
    fn auto_length_wins() {
        let f = Fade { auto_length: 0.5, ..Fade::new(0.01) };
        assert_eq!(f.effective_length(), 0.5);
    }
}
