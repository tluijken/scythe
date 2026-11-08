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

    /// Progress from 0 to 1 over a segment. Shapes 0-4 match REAPER renders; the bezier is an
    /// approximation (a cubic with both handles at the tension-shifted midpoint).
    pub fn progress(self, x: f64) -> f64 {
        let x = x.clamp(0.0, 1.0);
        match self {
            Self::Linear => x,
            Self::Square => 0.0,
            Self::SlowStartEnd => x * x * (3.0 - 2.0 * x),
            Self::FastStart => 1.0 - (1.0 - x).powi(3),
            Self::FastEnd => x.powi(3),
            Self::Bezier(t) => {
                let t = t.clamp(-1.0, 1.0);
                let (hx, hy) = ((1.0 + t) * 0.5, (1.0 - t) * 0.5);
                // Solve x(s) = x by bisection; x(s) is monotonic for handles inside the square.
                let bx = |s: f64| 3.0 * s * (1.0 - s) * hx + s.powi(3);
                let (mut lo, mut hi) = (0.0, 1.0);
                for _ in 0..40 {
                    let mid = (lo + hi) * 0.5;
                    if bx(mid) < x { lo = mid } else { hi = mid }
                }
                let s = (lo + hi) * 0.5;
                3.0 * s * (1.0 - s) * hy + s.powi(3)
            }
        }
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
        let k = p.shape.progress((t - p.time) / span);
        Some(p.value + (next.value - p.value) * k)
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
        let b = PointShape::Bezier(0.5);
        assert!(
            b.progress(0.5) < 0.25 && (b.progress(0.5) + PointShape::Bezier(-0.5).progress(0.5) - 1.0).abs() < 1e-6
        );
    }
}
