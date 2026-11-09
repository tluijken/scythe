//! MIDI routing as REAPER encodes it: a track's MIDI record input (`I_RECINPUT`, the `REC`
//! line), the MIDI half of a send (`I_MIDIFLAGS`, the `AUXRECV` line) and external timecode
//! synchronization (`SMPTESYNC`).

/// `I_RECINPUT` bit marking a MIDI input.
pub const MIDI_INPUT_FLAG: i64 = 4096;
/// Device number of the virtual MIDI keyboard.
pub const VIRTUAL_KEYBOARD: u8 = 62;
/// Device number meaning every enabled input.
pub const ALL_DEVICES: u8 = 63;
/// Highest device number REAPER hands out.
pub const MAX_DEVICE: u8 = 127;

/// Where a MIDI record input listens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MidiDevice {
    All,
    VirtualKeyboard,
    /// A hardware or virtual port by its stable device number (`reaper-midihw` cache).
    Port(u8),
}

impl MidiDevice {
    pub fn number(self) -> u8 {
        match self {
            Self::All => ALL_DEVICES,
            Self::VirtualKeyboard => VIRTUAL_KEYBOARD,
            Self::Port(n) => n,
        }
    }

    pub fn from_number(n: u8) -> Self {
        match n {
            ALL_DEVICES => Self::All,
            VIRTUAL_KEYBOARD => Self::VirtualKeyboard,
            n => Self::Port(n),
        }
    }

    /// Whether a message from device `from` reaches an input listening here.
    pub fn hears(self, from: u8) -> bool {
        match self {
            Self::All => true,
            d => d.number() == from,
        }
    }
}

/// A decoded MIDI record input: the device and the channel (0-15, `None` for all).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MidiInput {
    pub device: MidiDevice,
    pub channel: Option<u8>,
}

impl MidiInput {
    pub const ALL: Self = Self { device: MidiDevice::All, channel: None };

    /// Decodes `I_RECINPUT`; `None` when the input is audio or absent.
    pub fn from_rec_input(v: i64) -> Option<Self> {
        if v < 0 || v & MIDI_INPUT_FLAG == 0 {
            return None;
        }
        let ch = (v & 31) as u8;
        let device = ((v >> 5) & 127) as u8;
        Some(Self { device: MidiDevice::from_number(device), channel: (1..=16).contains(&ch).then(|| ch - 1) })
    }

    pub fn rec_input(self) -> i64 {
        MIDI_INPUT_FLAG + ((self.device.number() as i64) << 5) + self.channel.map_or(0, |c| c as i64 + 1)
    }

    /// Whether a message on `channel` (0-15) from device `device` is for this input.
    pub fn hears(self, device: u8, channel: u8) -> bool {
        self.device.hears(device) && self.channel.is_none_or(|c| c == channel)
    }
}

/// The `REC` line stores device numbers in an older five-bit layout: 30 is the virtual
/// keyboard, 31 all inputs, and ports from 30 up move two places to make room. `I_RECINPUT`
/// (and Scythe's model) uses 62 and 63 instead.
pub fn rec_input_from_rpp(v: i64) -> i64 {
    if v < 0 || v & MIDI_INPUT_FLAG == 0 {
        return v;
    }
    let field = (v - MIDI_INPUT_FLAG) >> 5;
    let device = match field {
        30 => VIRTUAL_KEYBOARD as i64,
        31 => ALL_DEVICES as i64,
        n if n >= 32 => n - 2,
        n => n,
    };
    MIDI_INPUT_FLAG + (device.min(MAX_DEVICE as i64) << 5) + (v & 31)
}

pub fn rec_input_to_rpp(v: i64) -> i64 {
    if v < 0 || v & MIDI_INPUT_FLAG == 0 {
        return v;
    }
    let device = (v >> 5) & 127;
    let field = match device {
        62 => 30,
        63 => 31,
        n if n >= 30 => n + 2,
        n => n,
    };
    MIDI_INPUT_FLAG + (field << 5) + (v & 31)
}

/// The MIDI side of a send (`I_MIDIFLAGS`, `AUXRECV` field 11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendMidi {
    /// Whether the send carries MIDI at all.
    pub enabled: bool,
    /// Source channel filter (0-15), `None` for all.
    pub src_channel: Option<u8>,
    /// Channel every message is moved to (0-15), `None` to keep it.
    pub dst_channel: Option<u8>,
    /// Source bus filter (0-15), `None` for all buses.
    pub src_bus: Option<u8>,
    /// Bus the messages arrive on (0-15), `None` to keep their own.
    pub dst_bus: Option<u8>,
    /// Send fader moves go out as MIDI volume and pan (CC 7 and 10).
    pub faders: bool,
}

/// REAPER's flags for a send without MIDI, as its routing window writes them.
pub const SEND_MIDI_OFF: i64 = 31 | 255 << 14;

impl SendMidi {
    pub const ALL: Self =
        Self { enabled: true, src_channel: None, dst_channel: None, src_bus: None, dst_bus: None, faders: false };

    pub fn from_flags(v: i64) -> Self {
        let src = (v & 31) as u8;
        let dst = ((v >> 5) & 31) as u8;
        let bus = |b: i64| ((1..=16).contains(&b)).then(|| b as u8 - 1);
        Self {
            enabled: src != 31,
            src_channel: (1..=16).contains(&src).then(|| src - 1),
            dst_channel: (1..=16).contains(&dst).then(|| dst - 1),
            src_bus: bus((v >> 14) & 255),
            dst_bus: bus((v >> 22) & 255),
            faders: v & 1024 != 0,
        }
    }

    pub fn flags(self) -> i64 {
        if !self.enabled {
            return SEND_MIDI_OFF | if self.faders { 1024 } else { 0 };
        }
        let ch = |c: Option<u8>| c.map_or(0, |c| c as i64 + 1);
        ch(self.src_channel)
            | ch(self.dst_channel) << 5
            | if self.faders { 1024 } else { 0 }
            | ch(self.src_bus) << 14
            | ch(self.dst_bus) << 22
    }

    /// The message as it arrives at the destination, or `None` when the send filters it out.
    /// `status` and `bus` are the source message's.
    pub fn route(self, status: u8, bus: u8) -> Option<(u8, u8)> {
        if !self.enabled || self.src_bus.is_some_and(|b| b != bus) {
            return None;
        }
        if status < 0xf0 {
            if self.src_channel.is_some_and(|c| c != status & 15) {
                return None;
            }
            let status = self.dst_channel.map_or(status, |c| status & 0xf0 | c);
            return Some((status, self.dst_bus.unwrap_or(bus)));
        }
        Some((status, self.dst_bus.unwrap_or(bus)))
    }
}

/// External timecode synchronization (`SMPTESYNC`): chasing MTC or LTC from an input.
/// Field order as REAPER writes it; values it does not interpret are kept.
#[derive(Debug, Clone, PartialEq)]
pub struct TimecodeSync {
    /// Field 1, `projsmptesync`: bit 1 enables chasing.
    pub flags: i64,
    /// Field 2: the frame rate expected when the project's is not used.
    pub frame_rate: f64,
    /// Field 3, `projsmpteresync`: drift in milliseconds that makes playback jump back in line.
    pub resync_ms: i64,
    /// Field 4, `projsmpteskip`.
    pub skip: i64,
    /// Field 5, `projsmpteahead`: milliseconds of timecode to wait for before starting.
    pub ahead_ms: i64,
    /// Field 6, `projsmptemaxfree`: milliseconds playback carries on after timecode stops.
    pub freewheel_ms: i64,
    /// Field 7, `projsmpteinput`: the input timecode is read from.
    pub input: i64,
    /// Field 8, `projsmpteoffs`: seconds subtracted from the incoming timecode.
    pub offset: f64,
    /// Field 9, `projsmpterateuseproj`.
    pub use_project_rate: bool,
    /// Fields 10 and 11, `projsmpteresync_rec` and `projsmptefw_rec`.
    pub record_resync: i64,
    pub record_freewheel: i64,
}

impl Default for TimecodeSync {
    fn default() -> Self {
        Self {
            flags: 0,
            frame_rate: 30.0,
            resync_ms: 100,
            skip: 40,
            ahead_ms: 1000,
            freewheel_ms: 300,
            input: 0,
            offset: 0.0,
            use_project_rate: true,
            record_resync: 0,
            record_freewheel: 0,
        }
    }
}

impl TimecodeSync {
    pub fn enabled(&self) -> bool {
        self.flags & 1 != 0
    }

    pub fn set_enabled(&mut self, on: bool) {
        self.flags = if on { self.flags | 1 } else { self.flags & !1 };
    }

    /// Parses the tokens after `SMPTESYNC`.
    pub fn parse(t: &[String]) -> Self {
        let d = Self::default();
        let int = |i: usize, def: i64| t.get(i).and_then(|v| v.parse::<f64>().ok()).map_or(def, |v| v as i64);
        let float = |i: usize, def: f64| t.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(def);
        Self {
            flags: int(0, d.flags),
            frame_rate: float(1, d.frame_rate),
            resync_ms: int(2, d.resync_ms),
            skip: int(3, d.skip),
            ahead_ms: int(4, d.ahead_ms),
            freewheel_ms: int(5, d.freewheel_ms),
            input: int(6, d.input),
            offset: float(7, d.offset),
            use_project_rate: int(8, 1) != 0,
            record_resync: int(9, 0),
            record_freewheel: int(10, 0),
        }
    }

    pub fn tokens(&self) -> Vec<String> {
        let f = |v: f64| {
            let s = format!("{v}");
            if s.contains('e') { format!("{v:.6}") } else { s }
        };
        vec![
            "SMPTESYNC".into(),
            self.flags.to_string(),
            f(self.frame_rate),
            self.resync_ms.to_string(),
            self.skip.to_string(),
            self.ahead_ms.to_string(),
            self.freewheel_ms.to_string(),
            self.input.to_string(),
            f(self.offset),
            (self.use_project_rate as i64).to_string(),
            self.record_resync.to_string(),
            self.record_freewheel.to_string(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pairs measured with REAPER 7.73: `I_RECINPUT` device set through the API against the
    /// device field of the `REC` line it saved.
    #[test]
    fn rec_device_field_matches_reaper() {
        let pairs = [(0, 0), (1, 1), (29, 29), (30, 32), (31, 33), (61, 63), (62, 30), (63, 31), (64, 66), (125, 127)];
        for (api, rpp) in pairs {
            let v = MIDI_INPUT_FLAG + (api << 5) + 3;
            let r = MIDI_INPUT_FLAG + (rpp << 5) + 3;
            assert_eq!(rec_input_to_rpp(v), r, "api {api}");
            assert_eq!(rec_input_from_rpp(r), v, "rpp {rpp}");
        }
        assert_eq!(rec_input_from_rpp(MIDI_INPUT_FLAG + (64 << 5)), MIDI_INPUT_FLAG + (62 << 5));
        assert_eq!(rec_input_to_rpp(1024), 1024);
        assert_eq!(rec_input_from_rpp(-1), -1);
    }

    #[test]
    fn decodes_record_inputs() {
        let all = MidiInput::from_rec_input(4096 + (63 << 5)).unwrap();
        assert_eq!(all, MidiInput::ALL);
        let vkb = MidiInput::from_rec_input(4096 + (62 << 5) + 5).unwrap();
        assert_eq!(vkb, MidiInput { device: MidiDevice::VirtualKeyboard, channel: Some(4) });
        assert_eq!(vkb.rec_input(), 4096 + (62 << 5) + 5);
        assert!(vkb.hears(62, 4) && !vkb.hears(62, 3) && !vkb.hears(1, 4));
        assert!(MidiInput::ALL.hears(9, 9));
        assert_eq!(MidiInput::from_rec_input(1024), None);
        assert_eq!(MidiInput::from_rec_input(-1), None);
    }

    #[test]
    fn send_flags_round_trip() {
        for v in [0, 1, 16, 32, 64, 512, 1024, 97, 1 << 14, 2 << 14, 1 << 22, 3 << 22, SEND_MIDI_OFF] {
            assert_eq!(SendMidi::from_flags(v).flags(), v, "{v}");
        }
        let m = SendMidi::from_flags(2 | 5 << 5 | 3 << 22);
        assert_eq!((m.src_channel, m.dst_channel, m.src_bus, m.dst_bus), (Some(1), Some(4), None, Some(2)));
        assert_eq!(m.route(0x91, 0), Some((0x94, 2)));
        assert_eq!(m.route(0x90, 0), None);
        assert!(!SendMidi::from_flags(SEND_MIDI_OFF).enabled);
        assert_eq!(SendMidi::from_flags(1 << 14).route(0x90, 1), None);
    }

    #[test]
    fn smpte_sync_fields() {
        let line = "SMPTESYNC 2 30 55 88 99 111 122 4.5 1 66 77";
        let t: Vec<String> = line.split_whitespace().skip(1).map(String::from).collect();
        let s = TimecodeSync::parse(&t);
        assert_eq!((s.resync_ms, s.skip, s.ahead_ms, s.freewheel_ms, s.input), (55, 88, 99, 111, 122));
        assert_eq!((s.offset, s.record_resync, s.record_freewheel), (4.5, 66, 77));
        assert_eq!(s.tokens().join(" "), line);
        assert_eq!(TimecodeSync::default().tokens().join(" "), "SMPTESYNC 0 30 100 40 1000 300 0 0 1 0 0");
    }
}
