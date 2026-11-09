//! The MIDI editor settings REAPER keeps in each take's source once the editor has opened it:
//!
//! - `CFGEDIT …`: field 5 the mode (0 piano roll, 1 named notes, 4 event list), field 8 the
//!   channel for new events (1-16), field 11 the grid in quarter notes, field 24 swing on and
//!   field 25 the swing strength.
//! - `KEYSNAP 0|1`: key snap on.
//! - `EVTFILTER mask …`: field 0 the channels shown, one bit per channel (0 shows all).
//! - `VELLANE lane height …`: one line per controller lane; see [`LaneKind`] for the numbers.
//!
//! Unknown fields and lines stay as they are.

use crate::rpp::{Chunk, Node, line_key, tokenize};

/// A controller lane as REAPER numbers it in `VELLANE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LaneKind {
    Velocity,
    /// Note-off velocity.
    OffVelocity,
    Cc(u8),
    /// A 14-bit controller: MSB `n` (0-31) with LSB `n + 32`.
    Cc14(u8),
    PitchBend,
    Program,
    ChannelPressure,
    BankProgram,
    Text,
    Sysex,
    PolyAftertouch,
    Notation,
}

impl LaneKind {
    pub fn code(self) -> i32 {
        match self {
            Self::Velocity => -1,
            Self::Cc(n) => n.min(127) as i32,
            Self::PitchBend => 128,
            Self::Program => 129,
            Self::ChannelPressure => 130,
            Self::BankProgram => 131,
            Self::Text => 132,
            Self::Sysex => 133,
            Self::Notation => 135,
            Self::OffVelocity => 167,
            Self::PolyAftertouch => 168,
            Self::Cc14(n) => 256 + n.min(31) as i32,
        }
    }

    pub fn from_code(code: i32) -> Option<Self> {
        Some(match code {
            -1 => Self::Velocity,
            0..=127 => Self::Cc(code as u8),
            128 => Self::PitchBend,
            129 => Self::Program,
            130 => Self::ChannelPressure,
            131 => Self::BankProgram,
            132 => Self::Text,
            133 => Self::Sysex,
            135 => Self::Notation,
            167 => Self::OffVelocity,
            168 => Self::PolyAftertouch,
            256..=287 => Self::Cc14((code - 256) as u8),
            _ => return None,
        })
    }

    pub fn label(self) -> String {
        match self {
            Self::Velocity => "Velocity".into(),
            Self::OffVelocity => "Off velocity".into(),
            Self::Cc(n) => match cc_name(n) {
                Some(name) => format!("CC {n} {name}"),
                None => format!("CC {n}"),
            },
            Self::Cc14(n) => format!("CC {n}/{} 14-bit", n + 32),
            Self::PitchBend => "Pitch bend".into(),
            Self::Program => "Program".into(),
            Self::ChannelPressure => "Channel pressure".into(),
            Self::BankProgram => "Bank/program".into(),
            Self::Text => "Text events".into(),
            Self::Sysex => "Sysex".into(),
            Self::PolyAftertouch => "Poly aftertouch".into(),
            Self::Notation => "Notation".into(),
        }
    }

    /// The highest value of a lane of numbers.
    pub fn max(self) -> u16 {
        match self {
            Self::PitchBend | Self::Cc14(_) => 16383,
            _ => 127,
        }
    }
}

/// The General MIDI names of the common controllers.
pub fn cc_name(n: u8) -> Option<&'static str> {
    Some(match n {
        0 => "Bank select",
        1 => "Modulation",
        2 => "Breath",
        4 => "Foot pedal",
        5 => "Portamento time",
        6 => "Data entry",
        7 => "Volume",
        8 => "Balance",
        10 => "Pan",
        11 => "Expression",
        64 => "Sustain",
        65 => "Portamento",
        66 => "Sostenuto",
        67 => "Soft pedal",
        71 => "Resonance",
        74 => "Cutoff",
        91 => "Reverb",
        93 => "Chorus",
        _ => return None,
    })
}

/// The editor's view of a take, from and back into its source chunk.
#[derive(Debug, Clone, PartialEq)]
pub struct EditorView {
    pub mode: EditorMode,
    /// Channel for new events, 0-15.
    pub channel: u8,
    pub grid_qn: f64,
    pub swing: Option<f64>,
    pub key_snap: bool,
    /// Bit per channel shown; 0 shows all.
    pub channel_filter: u16,
    /// (lane, height in pixels).
    pub lanes: Vec<(LaneKind, u32)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditorMode {
    #[default]
    PianoRoll,
    NamedNotes,
    EventList,
}

impl EditorMode {
    fn code(self) -> &'static str {
        match self {
            Self::PianoRoll => "0",
            Self::NamedNotes => "1",
            Self::EventList => "4",
        }
    }
}

impl Default for EditorView {
    fn default() -> Self {
        Self {
            mode: EditorMode::PianoRoll,
            channel: 0,
            grid_qn: 0.25,
            swing: None,
            key_snap: false,
            channel_filter: 0,
            lanes: vec![(LaneKind::Velocity, 100)],
        }
    }
}

/// The fields of the first line with `key`.
fn fields(c: &Chunk, key: &str) -> Option<Vec<String>> {
    c.children.iter().find_map(|n| match n {
        Node::Line(l) if line_key(l) == key => Some(tokenize(l)),
        _ => None,
    })
}

/// What REAPER writes the first time its editor opens a take (7.73), with the mode, channel and
/// grid fields replaced.
const CFGEDIT_DEFAULT: &str = "CFGEDIT 1 1 0 1 0 0 1 1 1 1 1 0.25 0 0 1054 408 0 0 0 0 0 0 0 0 0 0.5 0 0 1 64";

impl EditorView {
    pub fn read(c: &Chunk) -> Self {
        let mut v = Self::default();
        if let Some(t) = fields(c, "CFGEDIT") {
            let num = |i: usize| t.get(i + 1).and_then(|s| s.parse::<f64>().ok());
            v.mode = match num(5).unwrap_or(0.0) as i64 {
                1 => EditorMode::NamedNotes,
                4 => EditorMode::EventList,
                _ => EditorMode::PianoRoll,
            };
            v.channel = (num(8).unwrap_or(1.0) as i64 - 1).clamp(0, 15) as u8;
            v.grid_qn = num(11).filter(|g| *g > 0.0).unwrap_or(0.25);
            v.swing = (num(24).unwrap_or(0.0) != 0.0).then(|| num(25).unwrap_or(0.0).clamp(-1.0, 1.0));
        }
        v.key_snap = fields(c, "KEYSNAP").and_then(|t| t.get(1).map(|s| s != "0")).unwrap_or(false);
        v.channel_filter =
            fields(c, "EVTFILTER").and_then(|t| t.get(1).and_then(|s| s.parse::<i64>().ok())).unwrap_or(0) as u16;
        let lanes: Vec<(LaneKind, u32)> = c
            .children
            .iter()
            .filter_map(|n| match n {
                Node::Line(l) if line_key(l) == "VELLANE" => {
                    let t = tokenize(l);
                    let kind = LaneKind::from_code(t.get(1)?.parse().ok()?)?;
                    Some((kind, t.get(2).and_then(|h| h.parse().ok()).unwrap_or(100)))
                }
                _ => None,
            })
            .collect();
        if !lanes.is_empty() {
            v.lanes = lanes;
        }
        v
    }

    /// Writes the settings into the chunk, changing only the fields this view knows.
    pub fn write(&self, c: &mut Chunk) {
        let set = |c: &mut Chunk, key: &str, default: &str, edit: &dyn Fn(&mut Vec<String>)| {
            let at = c.children.iter().position(|n| matches!(n, Node::Line(l) if line_key(l) == key));
            let mut t = match at {
                Some(i) => match &c.children[i] {
                    Node::Line(l) => tokenize(l),
                    Node::Chunk(_) => unreachable!(),
                },
                None => tokenize(default),
            };
            edit(&mut t);
            let line = Node::Line(crate::rpp::join_tokens(&t));
            match at {
                Some(i) => c.children[i] = line,
                None => c.children.push(line),
            }
        };
        let put = |t: &mut Vec<String>, i: usize, v: String| {
            while t.len() <= i + 1 {
                t.push("0".into());
            }
            t[i + 1] = v;
        };
        set(c, "EVTFILTER", "EVTFILTER 0 -1 -1 -1 -1 0 0 0 0 -1 -1 -1 -1 0 -1 0 -1 -1", &|t| {
            put(t, 0, self.channel_filter.to_string())
        });
        // Lanes: replace the VELLANE lines in place, or add them where REAPER writes them.
        let first = c.children.iter().position(|n| matches!(n, Node::Line(l) if line_key(l) == "VELLANE"));
        let old: Vec<Vec<String>> = c
            .children
            .iter()
            .filter_map(|n| match n {
                Node::Line(l) if line_key(l) == "VELLANE" => Some(tokenize(l)),
                _ => None,
            })
            .collect();
        c.children.retain(|n| !matches!(n, Node::Line(l) if line_key(l) == "VELLANE"));
        let lines: Vec<Node> = self
            .lanes
            .iter()
            .enumerate()
            .map(|(i, (kind, height))| {
                let mut t = old.get(i).cloned().unwrap_or_else(|| tokenize("VELLANE -1 100 0 0 1"));
                put(&mut t, 0, kind.code().to_string());
                put(&mut t, 1, height.to_string());
                Node::Line(crate::rpp::join_tokens(&t))
            })
            .collect();
        let at = first.unwrap_or(c.children.len()).min(c.children.len());
        c.children.splice(at..at, lines);
        set(c, "KEYSNAP", "KEYSNAP 0", &|t| put(t, 0, if self.key_snap { "1" } else { "0" }.into()));
        set(c, "CFGEDIT", CFGEDIT_DEFAULT, &|t| {
            put(t, 5, self.mode.code().into());
            put(t, 8, (self.channel as u32 + 1).to_string());
            put(t, 11, format!("{}", self.grid_qn));
            put(t, 24, if self.swing.is_some() { "1" } else { "0" }.into());
            if let Some(s) = self.swing {
                put(t, 25, format!("{s}"));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpp::{parse, write};

    /// As REAPER 7.73 saved a take after "Show only channel 3", a CC 1 lane, a 1/16 swing grid,
    /// named notes mode and key snap.
    const SAVED: &str = "<SOURCE MIDI\n  HASDATA 1 960 QN\n  E 0 b0 7b 00\n  EVTFILTER 4 -1 -1 -1 -1 2 1 0 0 -1 -1 -1 -1 0 -1 0 -1 -1\n  VELLANE 1 100 0 0 1\n  CFGEDITVIEW 0 0.113411 61 12 0 0 0 0 0 0.5\n  KEYSNAP 1\n  TRACKSEL 0\n  CFGEDIT 1 1 0 1 0 1 1 1 3 1 1 0.25 0 0 1054 408 0 0 0 0 0 0 0 0 1 0.5 0 1 1 64\n>\n";

    #[test]
    fn reads_reaper_settings() {
        let c = parse(SAVED).unwrap();
        let v = EditorView::read(&c);
        assert_eq!(v.mode, EditorMode::NamedNotes);
        assert_eq!((v.channel, v.grid_qn, v.swing, v.key_snap), (2, 0.25, Some(0.5), true));
        assert_eq!(v.channel_filter, 4);
        assert_eq!(v.lanes, [(LaneKind::Cc(1), 100)]);
        let mut again = c.clone();
        v.write(&mut again);
        assert_eq!(write(&again), SAVED, "writing unchanged settings changes nothing");
    }

    #[test]
    fn writes_lanes_and_mode() {
        let mut c = parse(SAVED).unwrap();
        let mut v = EditorView::read(&c);
        v.lanes = vec![(LaneKind::Velocity, 100), (LaneKind::Cc(7), 60), (LaneKind::PitchBend, 50)];
        v.mode = EditorMode::EventList;
        v.key_snap = false;
        v.write(&mut c);
        let text = write(&c);
        assert!(text.contains("  VELLANE -1 100 0 0 1\n  VELLANE 7 60 0 0 1\n  VELLANE 128 50 0 0 1\n  CFGEDITVIEW"));
        assert!(text.contains("KEYSNAP 0") && text.contains("CFGEDIT 1 1 0 1 0 4 1 1 3"));
        assert_eq!(EditorView::read(&c), v);
        for code in [-1, 0, 127, 128, 133, 167, 168, 256, 287] {
            assert_eq!(LaneKind::from_code(code).map(LaneKind::code), Some(code));
        }
    }
}
