//! Controller lanes: the events of one kind (a controller, a 14-bit controller pair, pitch bend,
//! program, channel or poly aftertouch) as points of value over time, read from a source and
//! put back.

use super::view::LaneKind;
use super::{CcShape, MUTED, MidiEvent, MidiSource, SELECTED};

/// A lane value at a tick: 0-127, or 0-16383 for pitch bend and 14-bit controllers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LanePoint {
    pub tick: u64,
    pub value: u16,
    pub channel: u8,
    /// Poly aftertouch: the key.
    pub key: u8,
    pub shape: Option<CcShape>,
    pub selected: bool,
    pub muted: bool,
}

impl LanePoint {
    pub fn new(tick: u64, value: u16, channel: u8) -> Self {
        Self { tick, value, channel, key: 0, shape: None, selected: false, muted: false }
    }
}

impl LaneKind {
    /// Whether the lane holds `e`; for a 14-bit lane, its MSB event.
    fn holds(self, e: &MidiEvent) -> bool {
        let status = e.bytes[0] & 0xf0;
        match self {
            Self::Cc(n) => status == 0xb0 && e.bytes[1] == n,
            Self::Cc14(n) => status == 0xb0 && e.bytes[1] == n,
            Self::PitchBend => status == 0xe0,
            Self::Program => status == 0xc0,
            Self::ChannelPressure => status == 0xd0,
            Self::PolyAftertouch => status == 0xa0,
            _ => false,
        }
    }

    /// Whether editing the lane replaces `e` (a 14-bit lane also owns the LSB events).
    fn owns(self, e: &MidiEvent) -> bool {
        match self {
            Self::Cc14(n) => self.holds(e) || (e.bytes[0] & 0xf0 == 0xb0 && e.bytes[1] == n + 32),
            _ => self.holds(e),
        }
    }

    /// Whether the lane is made of channel messages with a value.
    pub fn has_values(self) -> bool {
        matches!(
            self,
            Self::Cc(_)
                | Self::Cc14(_)
                | Self::PitchBend
                | Self::Program
                | Self::ChannelPressure
                | Self::PolyAftertouch
        )
    }

    fn events(self, p: &LanePoint) -> Vec<MidiEvent> {
        let ch = p.channel & 0x0f;
        let v = p.value.min(self.max());
        let flags = if p.selected { SELECTED } else { 0 } | if p.muted { MUTED } else { 0 };
        let ev = |bytes: [u8; 3], len: u8, shape: Option<CcShape>| MidiEvent { tick: p.tick, bytes, len, flags, shape };
        match self {
            Self::Cc(n) => vec![ev([0xb0 | ch, n, v as u8], 3, p.shape)],
            Self::Cc14(n) => vec![
                ev([0xb0 | ch, n, (v >> 7) as u8], 3, p.shape),
                ev([0xb0 | ch, n + 32, (v & 0x7f) as u8], 3, p.shape),
            ],
            Self::PitchBend => vec![ev([0xe0 | ch, (v & 0x7f) as u8, (v >> 7) as u8], 3, p.shape)],
            Self::Program => vec![ev([0xc0 | ch, v as u8, 0], 2, None)],
            Self::ChannelPressure => vec![ev([0xd0 | ch, v as u8, 0], 2, p.shape)],
            Self::PolyAftertouch => vec![ev([0xa0 | ch, p.key & 0x7f, v as u8], 3, p.shape)],
            _ => Vec::new(),
        }
    }
}

impl MidiSource {
    /// The lane's points in time order. The end-of-source marker is not a value.
    pub fn lane(&self, kind: LaneKind) -> Vec<LanePoint> {
        let lsb = |e: &MidiEvent, n: u8| {
            self.events
                .iter()
                .find(|l| l.tick == e.tick && l.bytes[0] == e.bytes[0] && l.bytes[1] == n + 32)
                .map_or(0, |l| l.bytes[2] as u16)
        };
        self.events
            .iter()
            .filter(|e| kind.holds(e) && !self.is_end_marker(e))
            .map(|e| {
                let value = match kind {
                    LaneKind::Cc14(n) => (e.bytes[2] as u16) << 7 | lsb(e, n),
                    LaneKind::PitchBend => (e.bytes[2] as u16) << 7 | e.bytes[1] as u16,
                    LaneKind::Program | LaneKind::ChannelPressure => e.bytes[1] as u16,
                    _ => e.bytes[2] as u16,
                };
                LanePoint {
                    tick: e.tick,
                    value,
                    channel: e.bytes[0] & 0x0f,
                    key: if kind == LaneKind::PolyAftertouch { e.bytes[1] } else { 0 },
                    shape: e.shape,
                    selected: e.flags & SELECTED != 0,
                    muted: e.muted(),
                }
            })
            .collect()
    }

    /// The same source with the lane's events replaced by `points`; the source grows if a point
    /// lies past its end.
    pub fn with_lane(&self, kind: LaneKind, points: &[LanePoint]) -> MidiSource {
        let length = self.length_ticks.max(points.iter().map(|p| p.tick).max().unwrap_or(0));
        let mut events: Vec<MidiEvent> = self
            .events
            .iter()
            .filter(|e| !kind.owns(e) || self.is_end_marker(e))
            .map(|e| if self.is_end_marker(e) { MidiEvent { tick: length, ..e.clone() } } else { e.clone() })
            .collect();
        events.extend(points.iter().flat_map(|p| kind.events(p)));
        let end_marker = |e: &MidiEvent| e.tick == length && e.bytes[0] & 0xf0 == 0xb0 && e.bytes[1] == 0x7b;
        events.sort_by_key(|e| (e.tick, end_marker(e)));
        MidiSource { events, length_ticks: length, ..self.without_events() }
    }

    /// The lanes that hold any event, in REAPER's lane order.
    pub fn used_lanes(&self) -> Vec<LaneKind> {
        let mut out: Vec<LaneKind> = self
            .events
            .iter()
            .filter(|e| !self.is_end_marker(e))
            .filter_map(|e| match e.bytes[0] & 0xf0 {
                0xb0 => Some(LaneKind::Cc(e.bytes[1])),
                0xe0 => Some(LaneKind::PitchBend),
                0xc0 => Some(LaneKind::Program),
                0xd0 => Some(LaneKind::ChannelPressure),
                0xa0 => Some(LaneKind::PolyAftertouch),
                _ => None,
            })
            .collect();
        if self.long.iter().any(|x| x.data.first() == Some(&0xf0)) {
            out.push(LaneKind::Sysex);
        }
        if self.long.iter().any(|x| x.data.first() == Some(&0xff)) {
            out.push(LaneKind::Text);
        }
        out.sort_by_key(|k| k.code());
        out.dedup();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpp::parse;

    fn source(lines: &str) -> (crate::rpp::Chunk, MidiSource) {
        let c = parse(&format!("<SOURCE MIDI\n  HASDATA 1 960 QN\n{lines}>\n")).unwrap();
        let m = MidiSource::parse(&c).unwrap();
        (c, m)
    }

    #[test]
    fn edits_controller_lanes() {
        let (c, m) = source("  E 0 b0 01 10\n  E 480 e0 00 40\n  E 480 90 3c 50\n  E 960 80 3c 00\n  E 0 b0 7b 00\n");
        assert_eq!(m.lane(LaneKind::Cc(1)), [LanePoint::new(0, 16, 0)]);
        assert_eq!(m.lane(LaneKind::PitchBend), [LanePoint::new(480, 8192, 0)]);
        assert!(m.lane(LaneKind::Cc(0x7b)).is_empty(), "the end marker is not a lane value");
        let points = [LanePoint::new(240, 100, 0), LanePoint::new(2880, 0, 0)];
        let edited = m.with_lane(LaneKind::Cc(1), &points);
        assert_eq!(edited.lane(LaneKind::Cc(1)), points);
        assert_eq!(edited.length_qn(), 3.0);
        assert_eq!(edited.lane(LaneKind::PitchBend), m.lane(LaneKind::PitchBend));
        assert_eq!(edited.notes(), m.notes());
        let last = edited.events.last().unwrap();
        assert_eq!((last.tick, last.bytes), (2880, [0xb0, 0x7b, 0]));
        assert_eq!(MidiSource::parse(&edited.write_chunk(&c)).unwrap(), edited);
    }

    #[test]
    fn fourteen_bit_and_other_lanes() {
        let (_, m) = source("  E 0 b1 07 40\n  E 0 b1 27 05\n  E 0 c1 05 00\n  E 0 a1 3c 22\n  E 960 b0 7b 00\n");
        assert_eq!(m.lane(LaneKind::Cc14(7)), [LanePoint::new(0, 64 << 7 | 5, 1)]);
        assert_eq!(m.lane(LaneKind::Program)[0].value, 5);
        assert_eq!(m.lane(LaneKind::PolyAftertouch)[0].key, 60);
        let edited = m.with_lane(LaneKind::Cc14(7), &[LanePoint::new(480, 16383, 1)]);
        assert_eq!(edited.lane(LaneKind::Cc14(7)), [LanePoint::new(480, 16383, 1)]);
        assert!(edited.lane(LaneKind::Cc(39)).iter().all(|p| p.tick == 480), "the old LSB goes too");
        assert_eq!(m.used_lanes(), [LaneKind::Cc(7), LaneKind::Cc(39), LaneKind::Program, LaneKind::PolyAftertouch]);
    }
}
