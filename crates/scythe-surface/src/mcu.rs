//! Mackie Control Universal and its extender, as the published Mackie Control protocol
//! describes them and REAPER drives them: faders as pitch bend per strip (14 bits, REAPER's
//! fader curve), buttons and LEDs as notes, V-pots as relative CCs 16-23 and their rings as
//! CCs 48-55, the scribble strips and meter modes as SysEx, levels as channel pressure, and the
//! time display as CCs 64-73.

use std::collections::HashMap;

use scythe_core::surface::{MidiKind, MidiSurface};
use scythe_core::{AutoMode, TrackId};
use scythe_session::Session;
use serde_json::json;

use crate::midi::{MidiPorts, split};
use crate::view::{self, TrackView, View};
use crate::{Waker, exec, run_action, set_pan, set_volume};

pub const REC: u8 = 0x00;
pub const SOLO: u8 = 0x08;
pub const MUTE: u8 = 0x10;
pub const SELECT: u8 = 0x18;
pub const VPOT_PUSH: u8 = 0x20;
pub const BANK_LEFT: u8 = 0x2e;
pub const BANK_RIGHT: u8 = 0x2f;
pub const CHANNEL_LEFT: u8 = 0x30;
pub const CHANNEL_RIGHT: u8 = 0x31;
pub const FLIP: u8 = 0x32;
pub const SMPTE_BEATS: u8 = 0x35;
pub const F1: u8 = 0x36;
pub const READ: u8 = 0x4a;
pub const WRITE: u8 = 0x4b;
pub const TRIM: u8 = 0x4c;
pub const TOUCH: u8 = 0x4d;
pub const LATCH: u8 = 0x4e;
pub const SAVE: u8 = 0x50;
pub const UNDO: u8 = 0x51;
pub const MARKER: u8 = 0x54;
pub const CYCLE: u8 = 0x56;
pub const CLICK: u8 = 0x59;
pub const REWIND: u8 = 0x5b;
pub const FORWARD: u8 = 0x5c;
pub const STOP: u8 = 0x5d;
pub const PLAY: u8 = 0x5e;
pub const RECORD: u8 = 0x5f;
pub const LEFT: u8 = 0x62;
pub const RIGHT: u8 = 0x63;
pub const USER_A: u8 = 0x66;
pub const USER_B: u8 = 0x67;
pub const FADER_TOUCH: u8 = 0x68;
pub const MASTER_TOUCH: u8 = 0x70;
pub const SMPTE_LED: u8 = 0x71;
pub const BEATS_LED: u8 = 0x72;

/// A full fader in pitch bend steps.
const FADER_MAX: f64 = 16383.0;
/// Pan change per V-pot tick: REAPER moves the pan by 1/31 for each.
const PAN_STEP: f64 = 1.0 / 31.0;

/// What has been sent, keyed by control, so only changes go out.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    Fader(u8),
    Led(u8),
    Ring(u8),
    Lcd(u8),
    Digit(u8),
    Meter(u8),
    MeterMode(u8),
}

/// The protocol side of a Mackie Control or extender: what the device's messages do to the
/// session, and the messages that show the session on the device.
pub struct Mcu {
    pub extender: bool,
    /// Tracks the first strip is moved on by.
    pub offset: usize,
    flip: bool,
    /// Faders held by a finger: strips 0-7, then the master.
    touched: [bool; 9],
    /// The time display shows minutes and seconds instead of bars and beats.
    smpte: bool,
    /// The track the surface selected: automation buttons and LEDs follow it.
    selected: Option<TrackId>,
    /// Rewind or fast forward held.
    shuttle: Option<f64>,
    sent: HashMap<Key, Vec<u8>>,
}

impl Mcu {
    pub fn new(extender: bool, offset: usize) -> Self {
        Self {
            extender,
            offset,
            flip: false,
            touched: [false; 9],
            smpte: false,
            selected: None,
            shuttle: None,
            sent: HashMap::new(),
        }
    }

    fn model(&self) -> u8 {
        if self.extender { 0x15 } else { 0x14 }
    }

    fn sysex(&self, body: &[u8]) -> Vec<u8> {
        let mut m = vec![0xf0, 0x00, 0x00, 0x66, self.model()];
        m.extend_from_slice(body);
        m.push(0xf7);
        m
    }

    /// The track number (from 1) strip `i` shows.
    fn strip_track(&self, bank: usize, i: usize) -> usize {
        bank + self.offset + i + 1
    }

    /// Forgets what was sent, so the next feedback sends everything (the device came online).
    pub fn reset(&mut self) {
        self.sent.clear();
    }

    /// Applies one message from the device.
    pub fn handle(&mut self, session: &mut Session, bank: &mut usize, msg: &[u8]) {
        let view = View::read(session);
        let first = *bank + self.offset;
        let strip = |i: usize| view.track(first + i + 1).filter(|t| t.id.is_some()).cloned();
        match *msg {
            [0xf0, 0x00, 0x00, 0x66, _, 0x01, ..] => self.reset(),
            [status, lsb, msb] if status & 0xf0 == 0xe0 => {
                let ch = (status & 0x0f) as usize;
                let position = ((msb as u16) << 7 | lsb as u16) as f64 / FADER_MAX;
                if ch == 8 && !self.extender {
                    set_volume(session, None, view::fader_to_gain(position));
                } else if let Some(t) = (ch < 8).then(|| strip(ch)).flatten() {
                    if self.flip {
                        set_pan(session, t.id, position * 2.0 - 1.0);
                    } else {
                        set_volume(session, t.id, view::fader_to_gain(position));
                    }
                    self.sent.remove(&Key::Fader(ch as u8));
                }
            }
            [0xb0, cc, value] if (0x10..0x18).contains(&cc) => {
                let ticks = (value & 0x3f) as f64 * if value & 0x40 != 0 { -1.0 } else { 1.0 };
                if let Some(t) = strip((cc - 0x10) as usize) {
                    if self.flip {
                        let position = view::gain_to_fader(t.volume) + ticks / 100.0;
                        set_volume(session, t.id, view::fader_to_gain(position.clamp(0.0, 1.0)));
                    } else {
                        set_pan(session, t.id, t.pan + ticks * PAN_STEP);
                    }
                }
            }
            [0xb0, 0x3c, value] => {
                let ticks = (value & 0x3f) as f64 * if value & 0x40 != 0 { -1.0 } else { 1.0 };
                let beat = 60.0 / view.tempo.max(1.0);
                let to = (session.position() + ticks * beat).max(0.0);
                let _ = exec(session, json!({ "cmd": "transport.seek", "position": to }));
            }
            [status, note, velocity] if status & 0xf0 == 0x90 => {
                self.button(session, bank, &view, note, velocity > 0);
            }
            _ => {}
        }
    }

    fn button(&mut self, session: &mut Session, bank: &mut usize, view: &View, note: u8, down: bool) {
        let first = *bank + self.offset;
        let strip = |i: u8| view.track(first + i as usize + 1).filter(|t| t.id.is_some()).cloned();
        match note {
            FADER_TOUCH..=0x6f => {
                let i = (note - FADER_TOUCH) as usize;
                self.touched[i] = down;
                if !down && let Some(id) = strip(i as u8).and_then(|t| t.id) {
                    let target =
                        if self.flip { scythe_session::AutoTarget::Pan } else { scythe_session::AutoTarget::Volume };
                    session.release_automation(id, target);
                }
                self.sent.remove(&Key::Fader(i as u8));
                return;
            }
            MASTER_TOUCH => {
                self.touched[8] = down;
                return;
            }
            REWIND | FORWARD => {
                self.shuttle = down.then_some(if note == REWIND { -4.0 } else { 4.0 });
                self.sent.remove(&Key::Led(note));
                return;
            }
            _ => {}
        }
        if !down {
            return;
        }
        let toggle = |session: &mut Session, t: &TrackView, cmd: &str, field: &str, now: bool| {
            if let Some(id) = t.id {
                let _ = exec(session, json!({ "cmd": cmd, "track": id.0, field: !now }));
            }
        };
        match note {
            REC..=0x07 => {
                if let Some(t) = strip(note - REC) {
                    toggle(session, &t, "track.arm", "armed", t.armed);
                }
            }
            SOLO..=0x0f => {
                if let Some(t) = strip(note - SOLO) {
                    toggle(session, &t, "track.solo", "solo", t.solo);
                }
            }
            MUTE..=0x17 => {
                if let Some(t) = strip(note - MUTE) {
                    toggle(session, &t, "track.mute", "muted", t.muted);
                }
            }
            SELECT..=0x1f => {
                if let Some(id) = strip(note - SELECT).and_then(|t| t.id) {
                    self.selected = Some(id);
                    session.select_track(id);
                }
            }
            VPOT_PUSH..=0x27 => {
                if let Some(t) = strip(note - VPOT_PUSH) {
                    if self.flip {
                        set_volume(session, t.id, 1.0);
                    } else {
                        set_pan(session, t.id, 0.0);
                    }
                }
            }
            BANK_LEFT => *bank = bank.saturating_sub(8),
            BANK_RIGHT => *bank = (*bank + 8).min(view.tracks.len().saturating_sub(1)),
            CHANNEL_LEFT => *bank = bank.saturating_sub(1),
            CHANNEL_RIGHT => *bank = (*bank + 1).min(view.tracks.len().saturating_sub(1)),
            FLIP => {
                self.flip = !self.flip;
                self.sent.retain(|k, _| !matches!(k, Key::Fader(_) | Key::Ring(_)));
            }
            SMPTE_BEATS => self.smpte = !self.smpte,
            F1..=0x3d => {
                let _ = exec(session, json!({ "cmd": "marker.goto", "marker": note - F1 + 1, "region": false }));
            }
            READ..=LATCH => {
                if let Some(id) = self.selected.filter(|id| session.project.track(*id).is_some()) {
                    let mode = match note {
                        READ => AutoMode::Read,
                        WRITE => AutoMode::Write,
                        TRIM => AutoMode::TrimRead,
                        TOUCH => AutoMode::Touch,
                        _ => AutoMode::Latch,
                    };
                    session.edit("Automation mode", |p| {
                        if let Some(t) = p.track_mut(id) {
                            t.auto_mode = mode;
                        }
                    });
                }
            }
            SAVE => {
                run_action(session, "40026");
            }
            UNDO => {
                run_action(session, "40029");
            }
            MARKER => {
                let _ = exec(session, json!({ "cmd": "marker.add", "position": session.position() }));
            }
            CYCLE => {
                run_action(session, "1068");
            }
            CLICK => session.toggle_metronome(),
            STOP => {
                run_action(session, "1016");
            }
            PLAY => {
                run_action(session, "1007");
            }
            RECORD => {
                run_action(session, "1013");
            }
            LEFT => {
                run_action(session, "40172");
            }
            RIGHT => {
                run_action(session, "40173");
            }
            USER_A => {
                run_action(session, "_SCYTHE_SETLIST_PLAY_STOP");
            }
            USER_B => {
                run_action(session, "_SCYTHE_SETLIST_NEXT");
            }
            _ => {}
        }
    }

    /// Moves the playhead while rewind or fast forward is held, `dt` seconds after the last call.
    pub fn shuttle(&self, session: &mut Session, dt: f64) {
        if let Some(speed) = self.shuttle {
            let to = (session.position() + speed * dt).max(0.0);
            let _ = exec(session, json!({ "cmd": "transport.seek", "position": to }));
        }
    }

    /// The messages that bring the device in line with `view`: only what changed since the
    /// last call.
    pub fn feedback(&mut self, view: &View, bank: usize) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut put = |sent: &mut HashMap<Key, Vec<u8>>, key: Key, msg: Vec<u8>| {
            if sent.get(&key) != Some(&msg) {
                sent.insert(key, msg.clone());
                out.push(msg);
            }
        };
        let mut sent = std::mem::take(&mut self.sent);
        for i in 0..8u8 {
            let t = view.track(self.strip_track(bank, i as usize)).filter(|t| t.id.is_some());
            put(&mut sent, Key::MeterMode(i), self.sysex(&[0x20, i, 0x03]));
            let level = t.map_or(0.0, |t| view::gain_to_fader(t.volume));
            let pan = t.map_or(0.0, |t| t.pan);
            let fader = if self.flip { (pan + 1.0) / 2.0 } else { t.map_or(view::gain_to_fader(0.0), |_| level) };
            if !self.touched[i as usize] {
                put(&mut sent, Key::Fader(i), pitch_bend(i, fader));
            }
            let ring = if self.flip { level_ring(level) } else { pan_ring(pan) };
            put(&mut sent, Key::Ring(i), vec![0xb0, 0x30 + i, ring]);
            let on = |b: bool| if b { 0x7f } else { 0x00 };
            put(&mut sent, Key::Led(REC + i), vec![0x90, REC + i, on(t.is_some_and(|t| t.armed))]);
            put(&mut sent, Key::Led(SOLO + i), vec![0x90, SOLO + i, on(t.is_some_and(|t| t.solo))]);
            put(&mut sent, Key::Led(MUTE + i), vec![0x90, MUTE + i, on(t.is_some_and(|t| t.muted))]);
            let selected = t.is_some_and(|t| t.id == self.selected);
            put(&mut sent, Key::Led(SELECT + i), vec![0x90, SELECT + i, on(selected)]);
            let mut text = vec![0x12, i * 7];
            text.extend(scribble(t.map_or("", |t| t.name.as_str())));
            put(&mut sent, Key::Lcd(i), self.sysex(&text));
            let peak = t.map_or(0.0, |t| t.peak.0.max(t.peak.1));
            put(&mut sent, Key::Meter(i), vec![0xd0, i << 4 | meter_level(peak)]);
        }
        if !self.extender {
            if !self.touched[8] {
                put(&mut sent, Key::Fader(8), pitch_bend(8, view::gain_to_fader(view.master.volume)));
            }
            let on = |b: bool| if b { 0x7f } else { 0x00 };
            let mode = self.selected.and_then(|id| view.tracks.iter().find(|t| t.id == Some(id))).map(|t| t.auto_mode);
            let lit = match mode {
                Some(AutoMode::Read) => Some(READ),
                Some(AutoMode::Write) => Some(WRITE),
                Some(AutoMode::TrimRead) => Some(TRIM),
                Some(AutoMode::Touch) => Some(TOUCH),
                Some(AutoMode::Latch | AutoMode::LatchPreview) => Some(LATCH),
                None => None,
            };
            for note in READ..=LATCH {
                put(&mut sent, Key::Led(note), vec![0x90, note, on(lit == Some(note))]);
            }
            let playing = view.playing || view.recording;
            for (note, state) in [
                (PLAY, view.playing && !view.paused),
                (STOP, !playing),
                (RECORD, view.recording),
                (CYCLE, view.loop_enabled),
                (CLICK, view.metronome),
                (REWIND, self.shuttle.is_some_and(|s| s < 0.0)),
                (FORWARD, self.shuttle.is_some_and(|s| s > 0.0)),
                (FLIP, self.flip),
                (SMPTE_LED, self.smpte),
                (BEATS_LED, !self.smpte),
            ] {
                put(&mut sent, Key::Led(note), vec![0x90, note, on(state)]);
            }
            for (i, digit) in self.time_digits(view).into_iter().enumerate() {
                put(&mut sent, Key::Digit(i as u8), vec![0xb0, 0x40 + i as u8, digit]);
            }
        }
        self.sent = sent;
        out
    }

    pub fn selected(&self) -> Option<TrackId> {
        self.selected
    }

    /// The ten time display digits, rightmost first (CC 64 is the rightmost): bars, beats and
    /// thousandths of a beat as REAPER shows them, or hours, minutes, seconds and frames.
    fn time_digits(&self, view: &View) -> [u8; 10] {
        let mut d = [0u8; 10];
        let mut put = |from: usize, width: usize, value: u64, pad: bool| {
            let text = value.to_string();
            let bytes = text.as_bytes();
            for k in 0..width {
                d[from + k] = match bytes.len().checked_sub(k + 1) {
                    Some(i) => bytes[i],
                    None if pad => b'0',
                    None => 0,
                };
            }
        };
        if self.smpte {
            let t = view.position.max(0.0);
            let frames = ((t.fract()) * 30.0).floor() as u64;
            let secs = t.floor() as u64;
            put(0, 2, frames, true);
            put(2, 2, secs % 60, true);
            put(4, 2, secs / 60 % 60, true);
            put(6, 3, secs / 3600, false);
        } else {
            let (bar, beat, thousandths) = view.bar_beat_ticks;
            put(0, 3, thousandths as u64, true);
            put(5, 2, beat as u64, false);
            put(7, 3, bar.max(0) as u64, false);
        }
        d
    }
}

/// A fader position 0..1 as the pitch bend message for channel `ch`.
pub fn pitch_bend(ch: u8, position: f64) -> Vec<u8> {
    let v = (position.clamp(0.0, 1.0) * FADER_MAX).round() as u16;
    vec![0xe0 | ch, (v & 0x7f) as u8, (v >> 7) as u8]
}

/// A V-pot ring showing pan as a single dot: positions 1 (left) to 11 (right), 6 centred.
pub fn pan_ring(pan: f64) -> u8 {
    1 + ((pan.clamp(-1.0, 1.0) + 1.0) * 5.0).round() as u8
}

/// A V-pot ring showing a level as a bar filling from the left.
pub fn level_ring(position: f64) -> u8 {
    0x20 | (1 + (position.clamp(0.0, 1.0) * 10.0).round() as u8)
}

/// A strip's name for the top line: six characters and a space, as REAPER writes them.
pub fn scribble(name: &str) -> Vec<u8> {
    let mut out: Vec<u8> =
        name.chars().filter(|c| c.is_ascii() && !c.is_ascii_control()).take(6).map(|c| c as u8).collect();
    out.resize(7, b' ');
    out
}

/// A peak as a meter step: 0 below -60 dB, then one step per 5 dB up to 12 at 0 dB.
pub fn meter_level(peak: f32) -> u8 {
    if peak <= 0.0 {
        return 0;
    }
    let db = scythe_core::units::gain_to_db(peak as f64);
    if db < -60.0 { 0 } else { ((db + 60.0) / 5.0).floor().clamp(0.0, 12.0) as u8 }
}

/// A Mackie Control (or extender) on MIDI ports.
pub struct McuSurface {
    pub mcu: Mcu,
    ports: MidiPorts,
    shuttled: std::time::Instant,
    online: bool,
}

impl McuSurface {
    pub fn open(config: &MidiSurface, wake: Waker) -> Result<Self, String> {
        let ports = MidiPorts::open(config.input, config.output, wake)?;
        let mcu = Mcu::new(config.kind == MidiKind::McuExtender, config.offset.max(0) as usize);
        let mut s = Self { mcu, ports, shuttled: std::time::Instant::now(), online: false };
        let query = s.mcu.sysex(&[0x00]);
        s.ports.send(&query);
        Ok(s)
    }

    pub fn input(&mut self, session: &mut Session, bank: &mut usize) {
        while let Ok(bytes) = self.ports.rx.try_recv() {
            for msg in split(&bytes) {
                self.mcu.handle(session, bank, msg);
            }
        }
        let dt = self.shuttled.elapsed().as_secs_f64();
        self.shuttled = std::time::Instant::now();
        self.mcu.shuttle(session, dt);
    }

    pub fn feedback(&mut self, view: &View, bank: usize) {
        if !self.online {
            self.online = true;
            self.mcu.reset();
        }
        for msg in self.mcu.feedback(view, bank) {
            self.ports.send(&msg);
        }
    }
}
