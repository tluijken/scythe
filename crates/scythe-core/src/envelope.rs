use crate::automation_item::{AutomationItem, value_with_items};
use crate::fader::{envelope_gain, envelope_position};
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
    /// `WIDTHENV2`: stereo width, multiplied with the track width.
    Width,
    /// `DUALPANENVL2`: dual pan of the left channel, stored like `Pan` and added to it.
    DualPanLeft,
    /// `DUALPANENV2`: dual pan of the right channel, stored like `Pan` and added to it.
    DualPanRight,
    /// `PITCHENV`, takes only: semitones added to the take's pitch.
    Pitch,
}

impl EnvelopeKind {
    /// Volume, pre-FX volume and trim: the kinds REAPER can fader-scale.
    pub fn is_volume(self) -> bool {
        matches!(self, Self::Volume | Self::VolumePreFx | Self::Trim)
    }

    pub const ALL: [EnvelopeKind; 8] = [
        Self::Volume,
        Self::VolumePreFx,
        Self::Trim,
        Self::Pan,
        Self::Mute,
        Self::Width,
        Self::DualPanLeft,
        Self::DualPanRight,
    ];

    pub fn chunk_name(self) -> &'static str {
        match self {
            Self::Volume => "VOLENV2",
            Self::VolumePreFx => "VOLENV",
            Self::Trim => "VOLENV3",
            Self::Pan => "PANENV2",
            Self::Mute => "MUTEENV",
            Self::Width => "WIDTHENV2",
            Self::DualPanLeft => "DUALPANENVL2",
            Self::DualPanRight => "DUALPANENV2",
            Self::Pitch => "PITCHENV",
        }
    }

    pub fn from_chunk_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.chunk_name() == name)
    }

    /// A send's envelopes (`AUXVOLENV`, `AUXPANENV`, `AUXMUTEENV` after its `AUXRECV`) use the
    /// volume, pan and mute kinds.
    pub fn from_send_chunk_name(name: &str) -> Option<Self> {
        [Self::Volume, Self::Pan, Self::Mute].into_iter().find(|k| k.send_chunk_name() == Some(name))
    }

    pub fn send_chunk_name(self) -> Option<&'static str> {
        match self {
            Self::Volume => Some("AUXVOLENV"),
            Self::Pan => Some("AUXPANENV"),
            Self::Mute => Some("AUXMUTEENV"),
            _ => None,
        }
    }

    /// A take's envelopes (`VOLENV`, `PANENV`, `MUTEENV`, `PITCHENV` after its source) use the
    /// volume, pan, mute and pitch kinds.
    pub fn from_take_chunk_name(name: &str) -> Option<Self> {
        [Self::Volume, Self::Pan, Self::Mute, Self::Pitch].into_iter().find(|k| k.take_chunk_name() == Some(name))
    }

    pub fn take_chunk_name(self) -> Option<&'static str> {
        match self {
            Self::Volume => Some("VOLENV"),
            Self::Pan => Some("PANENV"),
            Self::Mute => Some("MUTEENV"),
            Self::Pitch => Some("PITCHENV"),
            _ => None,
        }
    }

    /// The name the control API uses.
    pub fn name(self) -> &'static str {
        match self {
            Self::Volume => "volume",
            Self::VolumePreFx => "volume_pre_fx",
            Self::Trim => "trim",
            Self::Pan => "pan",
            Self::Mute => "mute",
            Self::Width => "width",
            Self::DualPanLeft => "dual_pan_left",
            Self::DualPanRight => "dual_pan_right",
            Self::Pitch => "pitch",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().chain([Self::Pitch]).find(|k| k.name() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Volume => "Volume",
            Self::VolumePreFx => "Volume (pre-FX)",
            Self::Trim => "Trim volume",
            Self::Pan => "Pan",
            Self::Mute => "Mute",
            Self::Width => "Width",
            Self::DualPanLeft => "Pan (left)",
            Self::DualPanRight => "Pan (right)",
            Self::Pitch => "Pitch",
        }
    }

    /// Value range of the stored points.
    pub fn range(self) -> (f64, f64) {
        match self {
            Self::Volume | Self::VolumePreFx | Self::Trim => (0.0, 2.0),
            Self::Pan | Self::Width | Self::DualPanLeft | Self::DualPanRight => (-1.0, 1.0),
            Self::Mute => (0.0, 1.0),
            Self::Pitch => (-24.0, 24.0),
        }
    }

    /// Value of a new envelope that leaves the track as it is.
    pub fn neutral(self) -> f64 {
        match self {
            Self::Pan | Self::DualPanLeft | Self::DualPanRight | Self::Pitch => 0.0,
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

    pub const NAMES: [&'static str; 6] = ["linear", "square", "slow_start_end", "fast_start", "fast_end", "bezier"];

    /// The name the control API uses.
    pub fn name(self) -> &'static str {
        Self::NAMES[self.rpp_code() as usize]
    }

    pub fn from_name(name: &str, tension: f64) -> Option<Self> {
        let code = Self::NAMES.iter().position(|n| *n == name)?;
        Some(Self::from_rpp(code as i64, tension.clamp(-1.0, 1.0)))
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
    /// Shown in a lane of its own rather than over the track (`VIS <visible> <in lane>`).
    pub in_lane: bool,
    /// Armed for writing (`ARM`): automation modes write only armed envelopes.
    pub armed: bool,
    pub points: Vec<EnvPoint>,
    /// A volume envelope in fader scaling (`VOLTYPE 1`): it moves linearly in fader position
    /// rather than in gain between points. REAPER makes new volume envelopes so.
    pub fader_scaled: bool,
    /// Value range of a parameter envelope (`PARMENV … <min> <max>`); track, send and take
    /// envelopes take their kind's.
    pub range: Option<(f64, f64)>,
    /// Automation items (`POOLEDENVINST`), in the order they are stored.
    pub items: Vec<AutomationItem>,
    /// The original chunk, so its other lines survive a save.
    pub raw: Option<Chunk>,
}

impl Envelope {
    pub fn new(kind: EnvelopeKind) -> Self {
        Self {
            kind,
            active: true,
            visible: true,
            in_lane: true,
            armed: true,
            points: Vec::new(),
            fader_scaled: false,
            range: None,
            items: Vec::new(),
            raw: None,
        }
    }

    /// Envelope value at `t` seconds, automation items included, or `None` when it has no
    /// points (REAPER then ignores its items too).
    pub fn value_at(&self, t: f64) -> Option<f64> {
        let under = self.curve_at(t)?;
        if self.items.is_empty() {
            return Some(under);
        }
        Some(value_with_items(&self.items, t, |t| self.curve_at(t).unwrap_or(under)))
    }

    /// The value of the points alone, without automation items.
    pub fn curve_at(&self, t: f64) -> Option<f64> {
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
        if self.fader_scaled && k > 0.0 && k < 1.0 {
            let (a, b) = (envelope_position(p.value), envelope_position(next.value));
            return Some(envelope_gain(a + (b - a) * k));
        }
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
        // A point at the same time as the start (the wrap of a looped automation item) is not
        // the neighbour before; the one before it is.
        let s0 = match self.points[..i - 1].iter().rev().find(|q| q.time < p.time) {
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

    /// Whether an automation item (with its lead-in) covers part of `(t0, t1]`: item values
    /// change at its own points and edges, so they are worked out frame by frame.
    pub fn items_between(&self, t0: f64, t1: f64) -> bool {
        self.items.iter().any(|i| i.position - 1e-5 <= t1 && i.end() > t0)
    }

    /// The automation item edges (starts, ends and the ends of their transitions) around `t`:
    /// the last at or before it and the first after it, when `t` is between two. REAPER works
    /// out per-block values (pan) at these as well as at block boundaries.
    pub fn item_edges_around(&self, t: f64) -> Option<(f64, f64)> {
        let edges = self.items.iter().flat_map(|i| {
            let tr = i.transition.clamp(0.0, i.length / 2.0);
            [i.position, i.position + tr, i.end() - tr, i.end()].into_iter().chain(i.point_times())
        });
        let (mut lo, mut hi) = (f64::NEG_INFINITY, f64::INFINITY);
        for e in edges {
            if e <= t {
                lo = lo.max(e);
            } else {
                hi = hi.min(e);
            }
        }
        (lo.is_finite() || hi.is_finite()).then_some((lo, hi))
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

    // A VOLTYPE 1 envelope from 0 dB to 0.25 halfway, as REAPER 7.73 renders it.
    #[test]
    fn fader_scaled_envelopes_move_in_fader_position() {
        let mut e = env(&[(0.0, 1.0, PointShape::Linear), (1.0, 0.25, PointShape::Linear)]);
        e.fader_scaled = true;
        let at = |t: f64| e.value_at(t).unwrap();
        assert!((at(24163.0 / 48000.0) - 0.519588).abs() < 1e-6 && (at(4123.0 / 48000.0) - 0.89839).abs() < 1e-5);
        assert_eq!((at(0.0), at(1.0)), (1.0, 0.25));
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
