//! REAPER 7's CC shapes: a controller event can carry a shape (an `ENV shape flags tension` line
//! after it in the source) that moves the value on to the next event of the same lane. REAPER
//! plays the curve as extra events every `1/CCINTERP` of a quarter note from the shaped event,
//! sending a step only when the value changes.

use super::{MidiEvent, MidiSource};
use crate::envelope::Bezier;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeKind {
    Square,
    Linear,
    SlowStartEnd,
    FastStart,
    FastEnd,
    Bezier,
}

impl ShapeKind {
    pub const ALL: [ShapeKind; 6] =
        [Self::Square, Self::Linear, Self::SlowStartEnd, Self::FastStart, Self::FastEnd, Self::Bezier];

    /// The number REAPER stores (and `MIDI_SetCCShape` takes).
    pub fn code(self) -> u8 {
        self as u8
    }

    pub fn from_code(code: i64) -> Option<Self> {
        Self::ALL.get(usize::try_from(code).ok()?).copied()
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Square => "square",
            Self::Linear => "linear",
            Self::SlowStartEnd => "slow_start_end",
            Self::FastStart => "fast_start",
            Self::FastEnd => "fast_end",
            Self::Bezier => "bezier",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CcShape {
    pub kind: ShapeKind,
    /// Bezier tension, -1 to 1.
    pub tension: f64,
    /// The `ENV` line's second field, kept as it was.
    pub flags: i64,
}

impl CcShape {
    pub fn new(kind: ShapeKind, tension: f64) -> Self {
        Self { kind, tension: tension.clamp(-1.0, 1.0), flags: 0 }
    }

    /// From the tokens of an `ENV shape flags tension` line.
    pub(crate) fn parse(tokens: &[String]) -> Option<Self> {
        let kind = ShapeKind::from_code(tokens.get(1)?.parse().ok()?)?;
        let flags = tokens.get(2).and_then(|v| v.parse().ok()).unwrap_or(0);
        let tension = tokens.get(3).and_then(|v| v.parse().ok()).unwrap_or(0.0);
        Some(Self { kind, tension, flags })
    }

    pub(crate) fn line(self) -> String {
        format!("ENV {} {} {}", self.kind.code(), self.flags, self.tension)
    }

    /// Progress from 0 to 1 at `x` through the segment. `s0` is the bezier's start tangent
    /// relative to the straight line.
    fn progress(self, x: f64, s0: f64) -> f64 {
        match self.kind {
            ShapeKind::Square => 0.0,
            ShapeKind::Linear => x,
            ShapeKind::SlowStartEnd => x * x * (3.0 - 2.0 * x),
            ShapeKind::FastStart => 1.0 - (1.0 - x).powi(3),
            ShapeKind::FastEnd => x.powi(3),
            // REAPER ends every CC bezier at half the straight line's slope.
            ShapeKind::Bezier => Bezier::new(self.tension, s0, 0.5).value(x),
        }
    }
}

/// A controller event at a tick that may fall between ticks (interpolated steps do).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapedEvent {
    pub tick: f64,
    pub bytes: [u8; 3],
    pub len: u8,
}

/// What a shape interpolates: the message type, channel and controller (or key for poly
/// aftertouch). Program changes and other messages have no lane value to move.
fn lane(e: &MidiEvent) -> Option<(u8, u8, u8)> {
    let (status, channel) = (e.bytes[0] & 0xf0, e.bytes[0] & 0x0f);
    match status {
        0xa0 | 0xb0 => Some((status, channel, e.bytes[1])),
        0xd0 | 0xe0 => Some((status, channel, 0)),
        _ => None,
    }
}

fn value(e: &MidiEvent) -> i32 {
    match e.bytes[0] & 0xf0 {
        0xd0 => e.bytes[1] as i32,
        0xe0 => (e.bytes[2] as i32) << 7 | e.bytes[1] as i32,
        _ => e.bytes[2] as i32,
    }
}

fn with_value(e: &MidiEvent, v: i32) -> [u8; 3] {
    let mut b = e.bytes;
    match e.bytes[0] & 0xf0 {
        0xd0 => b[1] = v.clamp(0, 127) as u8,
        0xe0 => {
            let v = v.clamp(0, 16383);
            b[1] = (v & 0x7f) as u8;
            b[2] = (v >> 7) as u8;
        }
        _ => b[2] = v.clamp(0, 127) as u8,
    }
    b
}

impl MidiSource {
    /// The channel messages other than notes that play, in time order, with the steps of every
    /// shaped controller between its event and the next one of its lane. Muted events neither
    /// play nor end a shape; the end-of-source marker is left out.
    pub fn shaped_controls(&self) -> Vec<ShapedEvent> {
        let controls: Vec<&MidiEvent> = self
            .events
            .iter()
            .filter(|e| matches!(e.bytes[0] & 0xf0, 0xa0..=0xe0) && !e.muted() && !self.is_end_marker(e))
            .collect();
        let mut out: Vec<ShapedEvent> =
            controls.iter().map(|e| ShapedEvent { tick: e.tick as f64, bytes: e.bytes, len: e.len }).collect();
        let step = self.ticks_per_qn as f64 / self.cc_interp.max(1) as f64;
        for (i, e) in controls.iter().enumerate() {
            let Some(shape) = e.shape.filter(|s| s.kind != ShapeKind::Square) else { continue };
            let Some(key) = lane(e) else { continue };
            let Some(next) = controls[i + 1..].iter().find(|n| lane(n) == Some(key)) else { continue };
            let (t0, t1) = (e.tick as f64, next.tick as f64);
            let (v0, v1) = (value(e) as f64, value(next) as f64);
            if t1 <= t0 || v0 == v1 {
                continue;
            }
            let chord = (v1 - v0) / (t1 - t0);
            let s0 = controls[..i]
                .iter()
                .rev()
                .find(|p| lane(p) == Some(key))
                .filter(|p| p.tick < e.tick)
                .map_or(0.5, |p| ((v0 - value(p) as f64) / (t0 - p.tick as f64) + chord) / 2.0 / chord);
            let pitch = key.0 == 0xe0;
            let mut last = value(e);
            let mut k = 1.0;
            while t0 + k * step < t1 - 1e-9 {
                let tick = t0 + k * step;
                let v = v0 + (v1 - v0) * shape.progress((tick - t0) / (t1 - t0), s0);
                let v = if pitch { v.floor() } else { (v + 0.5).floor() } as i32;
                if v != last {
                    out.push(ShapedEvent { tick, bytes: with_value(e, v), len: e.len });
                    last = v;
                }
                k += 1.0;
            }
        }
        out.sort_by(|a, b| a.tick.total_cmp(&b.tick));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpp::parse;

    fn source(lines: &str) -> MidiSource {
        let text = format!("<SOURCE MIDI\n  HASDATA 1 960 QN\n{lines}>\n");
        MidiSource::parse(&parse(&text).unwrap()).unwrap()
    }

    fn values(m: &MidiSource) -> Vec<(f64, i32)> {
        m.shaped_controls()
            .iter()
            .map(|e| (e.tick, value(&MidiEvent { tick: 0, bytes: e.bytes, len: e.len, flags: 0, shape: None })))
            .collect()
    }

    #[test]
    fn shapes_survive_a_round_trip() {
        let text = "<SOURCE MIDI\n  HASDATA 1 960 QN\n  CCINTERP 32\n  E 0 b0 01 0a\n  ENV 5 0 0.5\n  E 960 b0 01 64\n  E 960 b0 7b 00\n>\n";
        let c = parse(text).unwrap();
        let m = MidiSource::parse(&c).unwrap();
        assert_eq!(m.events[0].shape, Some(CcShape::new(ShapeKind::Bezier, 0.5)));
        assert_eq!(m.cc_interp, 32);
        assert_eq!(crate::rpp::write(&m.write_chunk(&c)), crate::rpp::write(&c));
    }

    /// Values measured from REAPER 7.73 renders of these shapes (`scythe/ccdc` JSFX).
    #[test]
    fn steps_match_reaper() {
        let lin = source("  CCINTERP 32\n  E 0 b0 01 0a\n  ENV 1 0 0\n  E 1920 b0 01 64\n");
        let v = values(&lin);
        assert_eq!(v[..4], [(0.0, 10), (30.0, 11), (60.0, 13), (90.0, 14)]);
        assert_eq!(v.len(), 65);
        let pb = source("  CCINTERP 32\n  E 0 e0 00 00\n  ENV 1 0 0\n  E 1920 e0 7f 7f\n");
        assert_eq!(values(&pb)[1..3], [(30.0, 255), (60.0, 511)], "pitch bend steps round down");
        let slow = source("  CCINTERP 32\n  E 0 b0 01 0a\n  ENV 1 0 0\n  E 1920 b0 01 0c\n");
        assert_eq!(values(&slow), [(0.0, 10), (480.0, 11), (1440.0, 12), (1920.0, 12)], "only changes are sent");
        let bz = source("  CCINTERP 128\n  E 0 e0 00 00\n  ENV 5 0 0.5\n  E 1920 e0 7f 7f\n");
        let at = |k: usize| values(&bz).into_iter().find(|e| e.0 == k as f64 * 15.0).map(|e| e.1);
        assert_eq!([at(8), at(64), at(120)], [Some(122), Some(2559), Some(12600)]);
    }

    #[test]
    fn a_bezier_starts_from_the_previous_slope() {
        // Two bezier segments: the second starts at the average of both chords.
        let two =
            source("  CCINTERP 128\n  E 0 e0 00 00\n  ENV 5 0 0\n  E 960 e0 00 60\n  ENV 5 0 0\n  E 960 e0 7f 7f\n");
        let v = values(&two);
        let at = |t: f64| v.iter().rev().find(|e| e.0 <= t).unwrap().1;
        assert_eq!([at(120.0), at(240.0), at(840.0), at(1080.0), at(1800.0)], [1127, 2656, 11160, 13157, 16092]);
    }

    #[test]
    fn muted_events_and_other_lanes_are_skipped() {
        let m = source("  E 0 b0 01 0a\n  ENV 1 0 0\n  Em 960 b0 01 7f\n  E 0 b1 01 7f\n  E 960 b0 01 40\n");
        let v = values(&m);
        assert!(v.iter().all(|e| e.1 <= 64 || e.1 == 127));
        assert_eq!(v.last(), Some(&(1920.0, 64)));
    }
}
