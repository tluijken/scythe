//! ReaControlMIDI, from the state it stores: transposes and snaps passing notes to a scale,
//! and at the start of playback (and when a setting changes) sends the bank and program and
//! the volume, pan and pitch wheel it is set to. Measured from REAPER 7.73 with a JSFX that
//! turns each MIDI message into a sample (`examples/stock_ref.rs`):
//!
//! - notes (and their note-offs and poly aftertouch) are transposed, clamped to 0..=127, then
//!   moved down to the nearest note of the scale (an octave up below 0);
//! - the sends go to the chosen channel or to all 16: bank select (CC 0 and 32) on every
//!   channel, then the program changes, then per channel volume and pan as 14-bit pairs (CC
//!   7/39, 10/42) and the pitch wheel, each only once moved off 16383, 8192 and 8192.
//!
//! The two user CC sliders of its window are kept in the state but not sent.

use crate::clap_host::ParamInfo;
use crate::fx::{Effect, MidiBlock, ProcessContext};
use crate::stock::param;

/// Where the fields after the bank file path sit, counted from the path's end.
const BANK_ENABLE: usize = 8;
const CC_ENABLE: usize = 12;
const TRANSPOSE: usize = 45;
const SNAP: usize = 49;
const ROOT: usize = 53;
const SCALE_NAME: usize = 65;

const DEFAULT_VOLUME: i32 = 16383;
const CENTRE: i32 = 8192;

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// 0-based, `None` for all channels.
    pub channel: Option<u8>,
    pub bank_msb: i32,
    pub bank_lsb: i32,
    pub program: i32,
    /// 14-bit values.
    pub volume: i32,
    pub pan: i32,
    pub pitch: i32,
    pub send_program: bool,
    pub send_controls: bool,
    pub transpose: i32,
    pub snap: bool,
    pub root: i32,
    /// Which of the twelve semitones above the root are in the scale.
    pub scale: [bool; 12],
}

fn word(chunk: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_le_bytes(chunk.get(at..at + 4)?.try_into().ok()?))
}

/// Offset of the end of the bank file path.
fn path_end(chunk: &[u8]) -> Option<usize> {
    Some(48 + usize::try_from(word(chunk, 44)?).ok()?)
}

impl Settings {
    pub fn parse(chunk: &[u8]) -> Option<Self> {
        let e = path_end(chunk)?;
        let name_len = usize::try_from(word(chunk, e + SCALE_NAME)?).ok()?;
        let pattern_at = e + SCALE_NAME + 4 + name_len;
        let pattern_len = usize::try_from(word(chunk, pattern_at)?).ok()?;
        let pattern = chunk.get(pattern_at + 4..pattern_at + 4 + pattern_len)?;
        let mut scale = [false; 12];
        for (s, &c) in scale.iter_mut().zip(pattern) {
            *s = c != b'0' && c != 0;
        }
        let channel = word(chunk, 0)?;
        Some(Self {
            channel: (0..16).contains(&channel).then_some(channel as u8),
            bank_msb: word(chunk, 4)?,
            bank_lsb: word(chunk, 8)?,
            program: word(chunk, 12)?,
            volume: word(chunk, 28)?,
            pan: word(chunk, 32)?,
            pitch: word(chunk, 36)?,
            send_program: word(chunk, e + BANK_ENABLE)? != 0,
            send_controls: word(chunk, e + CC_ENABLE)? != 0,
            transpose: word(chunk, e + TRANSPOSE)?,
            snap: word(chunk, e + SNAP)? != 0,
            root: word(chunk, e + ROOT)?,
            scale,
        })
    }

    /// `chunk` with these settings written into it.
    fn write(&self, chunk: &mut [u8]) {
        let Some(e) = path_end(chunk) else { return };
        let mut put = |at: usize, v: i32| {
            if let Some(b) = chunk.get_mut(at..at + 4) {
                b.copy_from_slice(&v.to_le_bytes());
            }
        };
        put(0, self.channel.map_or(-1, i32::from));
        put(4, self.bank_msb);
        put(8, self.bank_lsb);
        put(12, self.program);
        put(28, self.volume);
        put(32, self.pan);
        put(36, self.pitch);
        put(e + BANK_ENABLE, self.send_program as i32);
        put(e + CC_ENABLE, self.send_controls as i32);
        put(e + TRANSPOSE, self.transpose);
        put(e + SNAP, self.snap as i32);
        put(e + ROOT, self.root);
    }

    /// The key a note comes out on.
    pub fn map_key(&self, key: u8) -> u8 {
        let mut k = (key as i32 + self.transpose).clamp(0, 127);
        if self.snap && self.scale.iter().any(|&s| s) {
            while !self.scale[(k - self.root).rem_euclid(12) as usize] {
                k -= 1;
            }
            if k < 0 {
                k += 12;
            }
        }
        k as u8
    }

    /// What goes out at the start of playback, by kind and channel.
    fn sends(&self) -> Vec<[u8; 3]> {
        let channels: Vec<u8> = self.channel.map_or((0..16).collect(), |c| vec![c]);
        let b7 = |v: i32| (v.clamp(0, 127)) as u8;
        let mut out = Vec::new();
        if self.send_program {
            for &c in &channels {
                out.push([0xb0 | c, 0, b7(self.bank_msb)]);
                out.push([0xb0 | c, 32, b7(self.bank_lsb)]);
            }
            out.extend(channels.iter().map(|&c| [0xc0 | c, b7(self.program), 0]));
        }
        if self.send_controls {
            for &c in &channels {
                if self.volume != DEFAULT_VOLUME {
                    out.extend([[0xb0 | c, 7, b7(self.volume >> 7)], [0xb0 | c, 39, b7(self.volume & 0x7f)]]);
                }
                if self.pan != CENTRE {
                    out.extend([[0xb0 | c, 10, b7(self.pan >> 7)], [0xb0 | c, 42, b7(self.pan & 0x7f)]]);
                }
                if self.pitch != CENTRE {
                    out.push([0xe0 | c, b7(self.pitch & 0x7f), b7(self.pitch >> 7)]);
                }
            }
        }
        out
    }
}

pub struct ControlMidi {
    settings: Settings,
    chunk: Vec<u8>,
    /// Sends still to go out at the start of the next block.
    pending: bool,
    sent: MidiBlock,
    events: Vec<(usize, [u8; 3])>,
    long: Vec<(usize, Vec<u8>)>,
}

impl ControlMidi {
    pub fn from_chunk(chunk: &[u8]) -> Option<Self> {
        Some(Self {
            settings: Settings::parse(chunk)?,
            chunk: chunk.to_vec(),
            pending: true,
            sent: MidiBlock::default(),
            events: Vec::new(),
            long: Vec::new(),
        })
    }
}

fn round(v: f64, steps: f64) -> i32 {
    (v.clamp(0.0, 1.0) * steps).round() as i32
}

impl ControlMidi {
    /// Which of the twelve semitones above the root are in the scale notes snap to.
    pub fn scale(&self) -> [bool; 12] {
        self.settings.scale
    }
}

impl Effect for ControlMidi {
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn params(&mut self) -> Vec<ParamInfo> {
        vec![
            param(0, "Bank MSB", 0.0, 127.0, 0.0, false),
            param(1, "Bank LSB", 0.0, 127.0, 0.0, false),
            param(2, "Program", 0.0, 127.0, 0.0, false),
            param(3, "Volume", 0.0, 16383.0, 16383.0, false),
            param(4, "Pan", 0.0, 16383.0, 8192.0, false),
            param(5, "Pitch wheel", 0.0, 16383.0, 8192.0, false),
            param(8, "Channel (0 = all)", 0.0, 16.0, 0.0, false),
            param(9, "Transpose", -12.0, 12.0, 0.0, false),
            param(10, "Snap to scale", 0.0, 1.0, 0.0, false),
            param(11, "Scale root", 0.0, 11.0, 0.0, false),
            param(13, "Bank/program enable", 0.0, 1.0, 0.0, false),
            param(14, "CC enable", 0.0, 1.0, 0.0, false),
        ]
    }

    fn param(&mut self, id: u32) -> Option<(f64, String)> {
        let s = &self.settings;
        let v = match id {
            0 => s.bank_msb,
            1 => s.bank_lsb,
            2 => s.program,
            3 => s.volume,
            4 => s.pan,
            5 => s.pitch,
            8 => s.channel.map_or(0, |c| c as i32 + 1),
            9 => s.transpose,
            10 => s.snap as i32,
            11 => s.root,
            13 => s.send_program as i32,
            14 => s.send_controls as i32,
            _ => return None,
        };
        let text = match id {
            8 if v == 0 => "all".into(),
            10 | 13 | 14 => (if v != 0 { "on" } else { "off" }).into(),
            11 => ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"][v.rem_euclid(12) as usize].into(),
            _ => v.to_string(),
        };
        Some((v as f64, text))
    }

    fn set_param(&mut self, id: u32, value: f64) {
        let v = value.round() as i32;
        let s = &mut self.settings;
        match id {
            0 => s.bank_msb = v.clamp(0, 127),
            1 => s.bank_lsb = v.clamp(0, 127),
            2 => s.program = v.clamp(0, 127),
            3 => s.volume = v.clamp(0, 16383),
            4 => s.pan = v.clamp(0, 16383),
            5 => s.pitch = v.clamp(0, 16383),
            8 => s.channel = (1..=16).contains(&v).then(|| (v - 1) as u8),
            9 => s.transpose = v.clamp(-12, 12),
            10 => s.snap = v != 0,
            11 => s.root = v.clamp(0, 11),
            13 => s.send_program = v != 0,
            14 => s.send_controls = v != 0,
            _ => return,
        }
        self.pending = true;
    }

    /// REAPER's normalised values: 7-bit steps of 1/127, 14-bit ones of 1/16383, channel in
    /// sixteenths, transpose ±12 around the middle, the root in elevenths, switches from 0.5.
    fn set_normalized(&mut self, index: usize, v: f64) -> bool {
        let value = match index {
            0..=2 => round(v, 127.0),
            3..=5 => round(v, 16383.0),
            8 => round(v, 16.0),
            9 => round(v, 24.0) - 12,
            11 => round(v, 11.0),
            10 | 13 | 14 => (v >= 0.5) as i32,
            _ => return false,
        };
        self.set_param(index as u32, value as f64);
        true
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        let mut c = self.chunk.clone();
        self.settings.write(&mut c);
        Some(c)
    }

    fn take_midi(&mut self, out: &mut MidiBlock) -> bool {
        out.short.append(&mut self.sent.short);
        out.long.append(&mut self.sent.long);
        true
    }

    fn process(&mut self, left: &mut [f32], _right: &mut [f32], ctx: &ProcessContext) {
        let n = left.len();
        let from = ctx.midi_from;
        if std::mem::take(&mut self.pending) && ctx.playing {
            self.sent.short.extend(self.settings.sends().into_iter().map(|m| (from, m)));
        }
        ctx.block_midi(n, &mut self.events);
        for &(at, mut msg) in &self.events {
            if matches!(msg[0] & 0xf0, 0x80 | 0x90 | 0xa0) {
                msg[1] = self.settings.map_key(msg[1]);
            }
            self.sent.short.push((from + at, msg));
        }
        ctx.block_long(n, &mut self.long);
        self.sent.long.extend(self.long.drain(..).map(|(at, d)| (from + at, d)));
    }

    fn reset(&mut self) {
        self.pending = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            channel: None,
            bank_msb: 0,
            bank_lsb: 0,
            program: 0,
            volume: DEFAULT_VOLUME,
            pan: CENTRE,
            pitch: CENTRE,
            send_program: false,
            send_controls: false,
            transpose: 0,
            snap: true,
            root: 2,
            scale: [true, false, true, false, true, true, false, true, false, true, false, true],
        }
    }

    /// REAPER 7.73 snapping C4..B4 and the extremes to D major.
    #[test]
    fn snaps_like_reaper() {
        let s = settings();
        let got: Vec<u8> = (60..72).chain([127, 0]).map(|k| s.map_key(k)).collect();
        assert_eq!(got, [59, 61, 62, 62, 64, 64, 66, 67, 67, 69, 69, 71, 127, 11]);
        let up = Settings { transpose: 1, ..settings() };
        assert_eq!(up.map_key(60), 61);
        assert_eq!(up.map_key(0), 1);
    }

    #[test]
    fn sends_group_by_kind() {
        let s = Settings { send_program: true, program: 38, channel: Some(2), bank_msb: 13, ..settings() };
        assert_eq!(s.sends(), [[0xb2, 0, 13], [0xb2, 32, 0], [0xc2, 38, 0]]);
    }
}
