use crate::rpp::Chunk;

/// Track envelope kinds that the engine applies, by RPP chunk name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EnvelopeKind {
    /// `VOLENV2`: post-FX volume, multiplied with the fader.
    Volume,
    /// `VOLENV`: pre-FX volume.
    VolumePreFx,
    /// `VOLENV3`: trim volume.
    Trim,
    /// `PANENV2`: pan, stored with the sign flipped (positive values pan left) and added to the
    /// track pan.
    Pan,
    /// `MUTEENV`: 0 mutes, 1 plays.
    Mute,
}

impl EnvelopeKind {
    pub const ALL: [EnvelopeKind; 5] = [Self::Volume, Self::VolumePreFx, Self::Trim, Self::Pan, Self::Mute];

    pub fn chunk_name(self) -> &'static str {
        match self {
            Self::Volume => "VOLENV2",
            Self::VolumePreFx => "VOLENV",
            Self::Trim => "VOLENV3",
            Self::Pan => "PANENV2",
            Self::Mute => "MUTEENV",
        }
    }

    pub fn from_chunk_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.chunk_name() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Volume => "Volume",
            Self::VolumePreFx => "Volume (pre-FX)",
            Self::Trim => "Trim volume",
            Self::Pan => "Pan",
            Self::Mute => "Mute",
        }
    }

    /// Value range of the stored points.
    pub fn range(self) -> (f64, f64) {
        match self {
            Self::Volume | Self::VolumePreFx | Self::Trim => (0.0, 2.0),
            Self::Pan => (-1.0, 1.0),
            Self::Mute => (0.0, 1.0),
        }
    }

    /// Value of a new envelope that leaves the track as it is.
    pub fn neutral(self) -> f64 {
        match self {
            Self::Pan => 0.0,
            _ => 1.0,
        }
    }
}

/// How the value moves from a point to the next one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PointShape {
    Linear,
    Square,
    SlowStartEnd,
    FastStart,
    FastEnd,
    /// Bezier with a tension in -1..=1.
    Bezier(f64),
}

impl PointShape {
    pub fn from_rpp(shape: i64, tension: f64) -> Self {
        match shape {
            1 => Self::Square,
            2 => Self::SlowStartEnd,
            3 => Self::FastStart,
            4 => Self::FastEnd,
            5 => Self::Bezier(tension),
            _ => Self::Linear,
        }
    }

    pub fn rpp_code(self) -> i64 {
        match self {
            Self::Linear => 0,
            Self::Square => 1,
            Self::SlowStartEnd => 2,
            Self::FastStart => 3,
            Self::FastEnd => 4,
            Self::Bezier(_) => 5,
        }
    }

    /// Progress from 0 to 1 over a segment. A bezier here has no neighbours to take its
    /// tangents from; `Envelope::value_at` supplies them.
    pub fn progress(self, x: f64) -> f64 {
        let x = x.clamp(0.0, 1.0);
        match self {
            Self::Linear => x,
            Self::Square => 0.0,
            Self::SlowStartEnd => x * x * (3.0 - 2.0 * x),
            Self::FastStart => 1.0 - (1.0 - x).powi(3),
            Self::FastEnd => x.powi(3),
            Self::Bezier(t) => Bezier::new(t, 1.0, 1.0).value(x),
        }
    }
}

/// REAPER's bezier segment in unit coordinates (time and value both normalised to 0..=1).
/// The handles sit a quarter of the way in, at the heights the tangents `s0` and `s1` (relative
/// to the straight line) give them, kept inside the segment; the tension pulls both towards
/// the corner (1, 0) when positive or (0, 1) when negative. REAPER plays the curve as a
/// polyline through `VERTICES` steps of the curve parameter.
#[derive(Debug, Clone, Copy)]
struct Bezier {
    ax: f64,
    ay: f64,
    bx: f64,
    by: f64,
}

impl Bezier {
    const VERTICES: usize = 256;

    fn new(tension: f64, s0: f64, s1: f64) -> Self {
        let t = tension.clamp(-1.0, 1.0);
        let (cx, cy) = if t < 0.0 { (0.0, 1.0) } else { (1.0, 0.0) };
        let pull = |(hx, hy): (f64, f64)| (hx + (cx - hx) * t.abs(), hy + (cy - hy) * t.abs());
        let (ax, ay) = pull((0.25, (0.25 * s0).clamp(0.0, 1.0)));
        let (bx, by) = pull((0.75, (1.0 - 0.25 * s1).clamp(0.0, 1.0)));
        Self { ax, ay, bx, by }
    }

    fn vertex(self, k: usize) -> (f64, f64) {
        let s = k as f64 / Self::VERTICES as f64;
        let cubic = |a: f64, b: f64| 3.0 * (1.0 - s) * (1.0 - s) * s * a + 3.0 * (1.0 - s) * s * s * b + s.powi(3);
        (cubic(self.ax, self.bx), cubic(self.ay, self.by))
    }

    /// The polyline step containing `x`, as its two vertices.
    fn step(self, x: f64) -> ((f64, f64), (f64, f64)) {
        let (mut lo, mut hi) = (0, Self::VERTICES);
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if self.vertex(mid).0 <= x { lo = mid } else { hi = mid }
        }
        (self.vertex(lo), self.vertex(hi))
    }

    fn value(self, x: f64) -> f64 {
        let ((x0, y0), (x1, y1)) = self.step(x);
        if x1 > x0 { y0 + (y1 - y0) * ((x - x0) / (x1 - x0)).clamp(0.0, 1.0) } else { y1 }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnvPoint {
    pub time: f64,
    pub value: f64,
    pub shape: PointShape,
    /// Original tokens of the `PT` line, so unmodelled fields survive a save.
    pub raw: Vec<String>,
}

impl EnvPoint {
    pub fn new(time: f64, value: f64) -> Self {
        Self { time, value, shape: PointShape::Linear, raw: Vec::new() }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    pub kind: EnvelopeKind,
    pub active: bool,
    pub visible: bool,
    pub points: Vec<EnvPoint>,
    /// The original chunk, so its other lines survive a save.
    pub raw: Option<Chunk>,
}

impl Envelope {
    pub fn new(kind: EnvelopeKind) -> Self {
        Self { kind, active: true, visible: true, points: Vec::new(), raw: None }
    }

    /// Envelope value at `t` seconds, or `None` when it has no points.
    pub fn value_at(&self, t: f64) -> Option<f64> {
        let first = self.points.first()?;
        let i = self.points.partition_point(|p| p.time <= t);
        if i == 0 {
            return Some(first.value);
        }
        let p = &self.points[i - 1];
        let Some(next) = self.points.get(i) else { return Some(p.value) };
        let span = next.time - p.time;
        if span <= 0.0 {
            return Some(next.value);
        }
        let x = (t - p.time) / span;
        let k = match self.bezier_at(i) {
            Some(curve) => curve.value(x),
            None => p.shape.progress(x),
        };
        Some(p.value + (next.value - p.value) * k)
    }

    /// Inside a bezier segment, the times of the polyline vertices around `t`. REAPER computes
    /// pan gains at these vertices and ramps them in between.
    pub fn bezier_step(&self, t: f64) -> Option<(f64, f64)> {
        let i = self.points.partition_point(|p| p.time <= t);
        let curve = self.bezier_at(i)?;
        let (p, next) = (&self.points[i - 1], &self.points[i]);
        let span = next.time - p.time;
        let ((x0, _), (x1, _)) = curve.step((t - p.time) / span);
        Some((p.time + x0 * span, p.time + x1 * span))
    }

    /// The curve of the segment ending at point `i`, when it is a bezier that moves.
    fn bezier_at(&self, i: usize) -> Option<Bezier> {
        let p = self.points.get(i.checked_sub(1)?)?;
        let next = self.points.get(i)?;
        let PointShape::Bezier(tension) = p.shape else { return None };
        let span = next.time - p.time;
        if span <= 0.0 || next.value == p.value {
            return None;
        }
        // A neighbouring point only shapes the tangent when it is a bezier point itself;
        // otherwise that end follows the straight line.
        let chord = (next.value - p.value) / span;
        let is_bezier = |q: &EnvPoint| matches!(q.shape, PointShape::Bezier(_));
        let central = |a: &EnvPoint, b: &EnvPoint| (b.value - a.value) / (b.time - a.time) / chord;
        let s0 = match i.checked_sub(2).map(|j| &self.points[j]) {
            Some(before) if is_bezier(before) && next.time > before.time => central(before, next),
            _ => 1.0,
        };
        let s1 = match self.points.get(i + 1) {
            Some(after) if is_bezier(after) && after.time > p.time => central(p, after),
            _ => 1.0,
        };
        Some(Bezier::new(tension, s0, s1))
    }

    pub fn sort(&mut self) {
        self.points.sort_by(|a, b| a.time.total_cmp(&b.time));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(points: &[(f64, f64, PointShape)]) -> Envelope {
        let mut e = Envelope::new(EnvelopeKind::Volume);
        e.points = points.iter().map(|&(t, v, s)| EnvPoint { shape: s, ..EnvPoint::new(t, v) }).collect();
        e
    }

    #[test]
    fn interpolates_between_points() {
        let e = env(&[(1.0, 0.0, PointShape::Linear), (3.0, 1.0, PointShape::Square), (4.0, 0.5, PointShape::Linear)]);
        assert_eq!(e.value_at(0.0), Some(0.0));
        assert_eq!(e.value_at(2.0), Some(0.5));
        assert_eq!(e.value_at(3.5), Some(1.0));
        assert_eq!(e.value_at(10.0), Some(0.5));
        assert_eq!(Envelope::new(EnvelopeKind::Pan).value_at(1.0), None);
    }

    /// Values measured from REAPER 7.73 renders (`examples/reaper_ref.rs` in scythe-engine).
    #[test]
    fn shapes_match_reaper() {
        let cases = [
            (PointShape::SlowStartEnd, [0.028, 0.1562, 0.5, 0.8438, 0.972]),
            (PointShape::FastStart, [0.271, 0.5782, 0.875, 0.9844, 0.999]),
            (PointShape::FastEnd, [0.001, 0.0156, 0.125, 0.4218, 0.729]),
            (PointShape::Bezier(0.0), [0.1, 0.25, 0.5, 0.75, 0.9]),
        ];
        for (shape, expect) in cases {
            for (x, want) in [0.1, 0.25, 0.5, 0.75, 0.9].into_iter().zip(expect) {
                assert!((shape.progress(x) - want).abs() < 1e-3, "{shape:?} at {x}");
            }
        }
    }

    /// Bezier envelopes rendered by REAPER 7.73: a lone segment, tangents from neighbouring
    /// bezier points, a clamped tangent at a peak, and a linear neighbour that does not count.
    #[test]
    fn beziers_match_reaper() {
        let b = |t: f64| PointShape::Bezier(t);
        type Case<'a> = (&'a [(f64, f64, PointShape)], [f64; 6]);
        let cases: [Case; 5] = [
            (&[(0.0, 0.0, b(0.5)), (1.0, 1.0, PointShape::Linear)], [0.0104, 0.0344, 0.0633, 0.1437, 0.2773, 0.5377]),
            (
                &[(0.0, 0.0, b(0.0)), (0.2, 0.5, b(0.0)), (0.7, 0.6, b(0.0)), (1.0, 1.0, PointShape::Linear)],
                [0.1383, 0.4105, 0.5309, 0.5781, 0.5929, 0.7801],
            ),
            (
                &[(0.0, 0.2, b(-0.5)), (0.3, 0.9, b(-0.5)), (0.6, 0.1, b(-0.5)), (1.0, 0.5, PointShape::Linear)],
                [0.5635, 0.8195, 0.8931, 0.2566, 0.2431, 0.4502],
            ),
            (
                &[(0.0, 0.0, b(0.0)), (0.5, 0.6, b(0.0)), (1.0, 1.0, b(0.0))],
                [0.061, 0.1852, 0.3094, 0.547, 0.7311, 0.8852],
            ),
            (
                &[
                    (0.0, 0.0, b(0.0)),
                    (0.25, 0.5, b(0.0)),
                    (0.5, 0.7, PointShape::Linear),
                    (1.0, 1.0, PointShape::Linear),
                ],
                [0.1, 0.3, 0.5, 0.6644, 0.79, 0.91],
            ),
        ];
        for (points, expect) in cases {
            let e = env(points);
            for (x, want) in [0.05, 0.15, 0.25, 0.45, 0.65, 0.85].into_iter().zip(expect) {
                let got = e.value_at(x).unwrap();
                assert!((got - want).abs() < 1e-3, "{points:?} at {x}: {got} vs {want}");
            }
        }
    }
}
