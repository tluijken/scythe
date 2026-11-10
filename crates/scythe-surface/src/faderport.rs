//! PreSonus FaderPort (the 2007 one) and FaderPort v2 (2018) as REAPER 7.73 drives them, one
//! channel strip that starts on the master. Measured by playing a scripted device into REAPER
//! over JACK MIDI and logging what it changed and sent back:
//!
//! - FaderPort: buttons are `A0 <id> 7f` (pressed) and `A0 <id> 00`; their LEDs are `A0 <id ^ 7>
//!   <0|1>`, a pressed transport or navigation button lighting with `7f` while held. The fader
//!   arrives as `B0 00 <msb>`, `B0 20 <lsb>` read over 14 bits, and goes back over 10 bits; its
//!   touch is `A0 7f <1|0>`; the pan knob is `E0 00 <ticks>`, two's complement, 0.02 per tick.
//! - FaderPort v2: buttons and LEDs are notes (`90 <id> 7f|00`), the fader pitch bend both ways
//!   (14 bits) with its touch on note 0x68, the encoder `B0 10 <ticks>` with bit 6 for left.
//!   Shift latches; with it, Loop adds a marker. Pan puts the pan on the fader (top is left) and
//!   the volume on the encoder, 0.22 dB per tick.
//!
//! Left and Right (Prev and Next) step the strip through the tracks and select the track, the
//! automation buttons set its mode (read 1, write 3, touch 2, off back to trim/read), and the
//! transport buttons play, stop, record, loop, rewind and fast-forward.

use std::collections::HashMap;

use scythe_core::surface::{MidiKind, MidiSurface};
use scythe_core::{AutoMode, TrackId};
use scythe_session::Session;
use serde_json::json;

use crate::midi::{MidiPorts, split};
use crate::view::{self, View};
use crate::{Waker, exec, run_action, set_pan, set_volume};

/// FaderPort button ids (as the device sends them).
pub mod classic {
    pub const REWIND: u8 = 3;
    pub const FORWARD: u8 = 4;
    pub const STOP: u8 = 5;
    pub const PLAY: u8 = 6;
    pub const RECORD: u8 = 7;
    pub const TOUCH: u8 = 8;
    pub const WRITE: u8 = 9;
    pub const READ: u8 = 10;
    pub const UNDO: u8 = 14;
    pub const LOOP: u8 = 15;
    pub const ARM: u8 = 16;
    pub const SOLO: u8 = 17;
    pub const MUTE: u8 = 18;
    pub const LEFT: u8 = 19;
    pub const BANK: u8 = 20;
    pub const RIGHT: u8 = 21;
    pub const OUTPUT: u8 = 22;
    pub const OFF: u8 = 23;
    /// The fader's touch sensor, on the button status.
    pub const FADER_TOUCH: u8 = 0x7f;

    /// The LED of button `id`.
    pub fn led(id: u8) -> u8 {
        id ^ 7
    }
}

/// FaderPort v2 note numbers.
pub mod v2 {
    pub const ARM: u8 = 0x00;
    pub const SOLO: u8 = 0x08;
    pub const MUTE: u8 = 0x10;
    pub const PAN: u8 = 0x2a;
    pub const PREV: u8 = 0x2e;
    pub const NEXT: u8 = 0x2f;
    pub const SHIFT: u8 = 0x46;
    pub const READ: u8 = 0x4a;
    pub const WRITE: u8 = 0x4b;
    pub const TOUCH: u8 = 0x4d;
    pub const LOOP: u8 = 0x56;
    pub const REWIND: u8 = 0x5b;
    pub const FORWARD: u8 = 0x5c;
    pub const STOP: u8 = 0x5d;
    pub const PLAY: u8 = 0x5e;
    pub const RECORD: u8 = 0x5f;
    pub const FADER_TOUCH: u8 = 0x68;
    /// The encoder's controller.
    pub const ENCODER: u8 = 0x10;
}

const PAN_STEP: f64 = 0.02;
const VOLUME_STEP_DB: f64 = 0.22;
const FINE: f64 = 16383.0;

/// What the device does, the same for both models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Press {
    Rewind,
    Forward,
    Stop,
    Play,
    Record,
    Loop,
    Undo,
    Mode(AutoMode),
    Arm,
    Solo,
    Mute,
    Left,
    Right,
    Bank,
    Output,
    Shift,
    Pan,
    FaderTouch,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    Led(u8),
    Fader,
}

pub struct FaderPort {
    pub kind: MidiKind,
    /// The track the strip shows: 0 the master, else its number from 1.
    pub current: usize,
    selected: Option<TrackId>,
    shift: bool,
    pan_mode: bool,
    output: bool,
    touched: bool,
    fader_msb: Option<u8>,
    sent: HashMap<Key, Vec<Vec<u8>>>,
}

impl FaderPort {
    pub fn new(kind: MidiKind) -> Self {
        Self {
            kind,
            current: 0,
            selected: None,
            shift: false,
            pan_mode: false,
            output: false,
            touched: false,
            fader_msb: None,
            sent: HashMap::new(),
        }
    }

    fn v2(&self) -> bool {
        self.kind == MidiKind::FaderPort2
    }

    fn press_of(&self, id: u8) -> Option<Press> {
        use Press::*;
        Some(if self.v2() {
            match id {
                v2::REWIND => Rewind,
                v2::FORWARD => Forward,
                v2::STOP => Stop,
                v2::PLAY => Play,
                v2::RECORD => Record,
                v2::LOOP => Loop,
                v2::READ => Mode(AutoMode::Read),
                v2::WRITE => Mode(AutoMode::Write),
                v2::TOUCH => Mode(AutoMode::Touch),
                v2::ARM => Arm,
                v2::SOLO => Solo,
                v2::MUTE => Mute,
                v2::PREV => Left,
                v2::NEXT => Right,
                v2::SHIFT => Shift,
                v2::PAN => Pan,
                v2::FADER_TOUCH => FaderTouch,
                _ => return None,
            }
        } else {
            match id {
                classic::REWIND => Rewind,
                classic::FORWARD => Forward,
                classic::STOP => Stop,
                classic::PLAY => Play,
                classic::RECORD => Record,
                classic::LOOP => Loop,
                classic::UNDO => Undo,
                classic::READ => Mode(AutoMode::Read),
                classic::WRITE => Mode(AutoMode::Write),
                classic::TOUCH => Mode(AutoMode::Touch),
                classic::OFF => Mode(AutoMode::TrimRead),
                classic::ARM => Arm,
                classic::SOLO => Solo,
                classic::MUTE => Mute,
                classic::LEFT => Left,
                classic::RIGHT => Right,
                classic::BANK => Bank,
                classic::OUTPUT => Output,
                classic::FADER_TOUCH => FaderTouch,
                _ => return None,
            }
        })
    }

    /// The track the fader and buttons act on, `None` for the master.
    fn target(&self, view: &View) -> Option<Option<TrackId>> {
        if self.output {
            return Some(None);
        }
        view.track(self.current).map(|t| t.id)
    }

    pub fn handle(&mut self, session: &mut Session, msg: &[u8]) {
        let view = View::read(session);
        self.follow(&view);
        match *msg {
            [0xa0, id, v] if !self.v2() => {
                self.button(session, &view, id, v != 0 && (id == classic::FADER_TOUCH || v == 0x7f))
            }
            [0x90, id, v] | [0x80, id, v] if self.v2() => self.button(session, &view, id, msg[0] == 0x90 && v != 0),
            [0xb0, 0x00, msb] if !self.v2() => self.fader_msb = Some(msb),
            [0xb0, 0x20, lsb] if !self.v2() => {
                if let Some(msb) = self.fader_msb.take() {
                    self.fader(session, &view, ((msb as u16) << 7 | lsb as u16) as f64 / FINE);
                }
            }
            [0xe0, lsb, msb] if self.v2() => self.fader(session, &view, ((msb as u16) << 7 | lsb as u16) as f64 / FINE),
            [0xe0, 0x00, ticks] if !self.v2() => {
                let ticks = if ticks >= 0x40 { ticks as f64 - 128.0 } else { ticks as f64 };
                self.encoder(session, &view, ticks);
            }
            [0xb0, v2::ENCODER, v] if self.v2() => {
                let ticks = (v & 0x3f) as f64 * if v & 0x40 != 0 { -1.0 } else { 1.0 };
                self.encoder(session, &view, ticks);
            }
            _ => {}
        }
    }

    /// Follows a track selected elsewhere.
    fn follow(&mut self, view: &View) {
        if view.selected != self.selected {
            self.selected = view.selected;
            if let Some(n) =
                view.selected.and_then(|id| view.tracks.iter().find(|t| t.id == Some(id))).map(|t| t.number)
            {
                self.current = n;
            }
        }
    }

    fn fader(&mut self, session: &mut Session, view: &View, position: f64) {
        let Some(target) = self.target(view) else { return };
        if self.pan_mode {
            set_pan(session, target, 1.0 - 2.0 * position);
        } else {
            set_volume(session, target, view::fader_to_gain(position));
        }
        self.sent.remove(&Key::Fader);
    }

    fn encoder(&mut self, session: &mut Session, view: &View, ticks: f64) {
        let Some(target) = self.target(view) else { return };
        let Some(t) = view.track(if self.output { 0 } else { self.current }) else { return };
        if self.pan_mode {
            set_volume(session, target, t.volume * scythe_core::units::db_to_gain(ticks * VOLUME_STEP_DB));
        } else {
            set_pan(session, target, t.pan + ticks * PAN_STEP);
        }
    }

    fn button(&mut self, session: &mut Session, view: &View, id: u8, down: bool) {
        let Some(press) = self.press_of(id) else { return };
        if press == Press::FaderTouch {
            self.touched = down;
            if !down && let Some(Some(id)) = self.target(view) {
                session.release_automation(id, scythe_session::AutoTarget::Volume);
            }
            return;
        }
        if !down {
            return;
        }
        let target = self.target(view);
        let strip = view.track(if self.output { 0 } else { self.current });
        match press {
            Press::Rewind => run_action(session, "40172"),
            Press::Forward => run_action(session, "40173"),
            Press::Stop => run_action(session, "1016"),
            Press::Play => run_action(session, "1007"),
            Press::Record => run_action(session, "1013"),
            Press::Loop if self.shift => run_action(session, "40157"),
            Press::Loop => run_action(session, "1068"),
            Press::Undo => run_action(session, "40029"),
            Press::Shift => self.shift = !self.shift,
            Press::Pan => {
                self.pan_mode = !self.pan_mode;
                self.sent.remove(&Key::Fader);
            }
            Press::Output => {
                self.output = !self.output;
                self.sent.remove(&Key::Fader);
            }
            Press::Bank => {}
            Press::Left | Press::Right => {
                let last = view.tracks.len();
                self.current =
                    if press == Press::Left { self.current.saturating_sub(1) } else { (self.current + 1).min(last) };
                if let Some(id) = view.track(self.current).and_then(|t| t.id) {
                    session.select_track(id);
                    self.selected = Some(id);
                }
                self.sent.remove(&Key::Fader);
            }
            Press::Mode(mode) => {
                session.edit("Automation mode", |p| match target {
                    Some(Some(id)) => {
                        if let Some(t) = p.track_mut(id) {
                            t.auto_mode = mode;
                        }
                    }
                    Some(None) => p.master.auto_mode = mode,
                    None => {}
                });
            }
            Press::Arm | Press::Solo | Press::Mute => {
                let (Some(Some(id)), Some(t)) = (target, strip) else {
                    if let (Press::Mute, Some(None), Some(t)) = (press, target, strip) {
                        let _ = exec(session, json!({ "cmd": "track.mute", "track": "master", "muted": !t.muted }));
                    }
                    return;
                };
                let (cmd, field, now) = match press {
                    Press::Arm => ("track.arm", "armed", t.armed),
                    Press::Solo => ("track.solo", "solo", t.solo),
                    _ => ("track.mute", "muted", t.muted),
                };
                let _ = exec(session, json!({ "cmd": cmd, "track": id.0, field: !now }));
            }
            Press::FaderTouch => {}
        }
    }

    pub fn reset(&mut self) {
        self.sent.clear();
    }

    /// What the device should show that it does not yet.
    pub fn feedback(&mut self, view: &View) -> Vec<Vec<u8>> {
        self.follow(view);
        let mut out = Vec::new();
        let mut sent = std::mem::take(&mut self.sent);
        let mut put = |key: Key, msgs: Vec<Vec<u8>>| {
            if sent.get(&key) != Some(&msgs) {
                out.extend(msgs.iter().cloned());
                sent.insert(key, msgs);
            }
        };
        let strip = view.track(if self.output { 0 } else { self.current });
        if !self.touched {
            let position = match strip {
                Some(t) if self.pan_mode => (1.0 - t.pan) / 2.0,
                Some(t) => view::gain_to_fader(t.volume),
                None => 0.0,
            };
            put(Key::Fader, fader_messages(self.kind, position));
        }
        let mode = strip.map(|t| t.auto_mode);
        let flag = |b: bool| strip.is_some_and(|_| b);
        let leds: Vec<(Press, bool)> = vec![
            (Press::Mute, flag(strip.is_some_and(|t| t.muted))),
            (Press::Solo, flag(strip.is_some_and(|t| t.solo))),
            (Press::Arm, flag(strip.is_some_and(|t| t.armed))),
            (Press::Stop, !view.playing && !view.recording),
            (Press::Play, view.playing || view.recording),
            (Press::Record, view.recording),
            (Press::Loop, view.loop_enabled),
            (Press::Mode(AutoMode::Read), mode == Some(AutoMode::Read)),
            (Press::Mode(AutoMode::Write), mode == Some(AutoMode::Write)),
            (Press::Mode(AutoMode::Touch), mode == Some(AutoMode::Touch)),
            (Press::Mode(AutoMode::TrimRead), false),
            (Press::Output, self.output),
            (Press::Shift, self.shift),
            (Press::Pan, self.pan_mode),
        ];
        for (press, on) in leds {
            if let Some(msg) = led_message(self.kind, press, on) {
                put(Key::Led(msg[1]), vec![msg]);
            }
        }
        self.sent = sent;
        out
    }
}

/// The fader at `position` (0..1) as the model takes it.
fn fader_messages(kind: MidiKind, position: f64) -> Vec<Vec<u8>> {
    let position = position.clamp(0.0, 1.0);
    if kind == MidiKind::FaderPort2 {
        let v = (position * FINE).round() as u16;
        vec![vec![0xe0, (v & 0x7f) as u8, (v >> 7) as u8]]
    } else {
        // REAPER drops the low four bits of the 14-bit position (unity: 11730 to 733).
        let v = (position * FINE).round() as u16 >> 4;
        vec![vec![0xb0, 0x00, (v >> 7) as u8], vec![0xb0, 0x20, (v & 0x7f) as u8]]
    }
}

/// The LED of `press`, if the model has one.
fn led_message(kind: MidiKind, press: Press, on: bool) -> Option<Vec<u8>> {
    use Press::*;
    if kind == MidiKind::FaderPort2 {
        let note = match press {
            Mute => v2::MUTE,
            Solo => v2::SOLO,
            Arm => v2::ARM,
            Stop => v2::STOP,
            Play => v2::PLAY,
            Record => v2::RECORD,
            Loop => v2::LOOP,
            Shift => v2::SHIFT,
            Pan => return Some(vec![0x90, v2::PAN, on as u8]),
            _ => return None,
        };
        return Some(vec![0x90, note, if on { 0x7f } else { 0 }]);
    }
    let id = match press {
        Mute => classic::MUTE,
        Solo => classic::SOLO,
        Arm => classic::ARM,
        Stop => classic::STOP,
        Play => classic::PLAY,
        Record => classic::RECORD,
        Loop => classic::LOOP,
        Mode(AutoMode::Read) => classic::READ,
        Mode(AutoMode::Write) => classic::WRITE,
        Mode(AutoMode::Touch) => classic::TOUCH,
        Mode(AutoMode::TrimRead) => classic::OFF,
        Output => classic::OUTPUT,
        _ => return None,
    };
    Some(vec![0xa0, classic::led(id), on as u8])
}

/// A FaderPort on MIDI ports.
pub struct FaderPortSurface {
    pub fp: FaderPort,
    ports: MidiPorts,
}

impl FaderPortSurface {
    pub fn open(config: &MidiSurface, wake: Waker) -> Result<Self, String> {
        let ports = MidiPorts::open(config.input, config.output, wake)?;
        Ok(Self { fp: FaderPort::new(config.kind), ports })
    }

    pub fn input(&mut self, session: &mut Session) {
        while let Ok(bytes) = self.ports.rx.try_recv() {
            for msg in split(&bytes) {
                self.fp.handle(session, msg);
            }
        }
    }

    pub fn feedback(&mut self, view: &View) {
        for msg in self.fp.feedback(view) {
            self.ports.send(&msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        let pool = std::sync::Arc::new(scythe_engine::MediaPool::with_resample(
            scythe_core::render_settings::ResampleMode::PLAYBACK,
        ));
        let mut s = Session::new(scythe_engine::Engine::start_offline(pool, 48_000));
        for name in ["Kick", "Bass", "Vox"] {
            s.project.add_track(name);
        }
        s
    }

    #[test]
    fn classic_buttons_fader_and_leds() {
        let mut s = session();
        let mut fp = FaderPort::new(MidiKind::FaderPort);
        // The strip starts on the master; unity shows as 733 of 1023 and the stop LED is lit.
        let out = fp.feedback(&View::read(&s));
        assert!(out.contains(&vec![0xb0, 0x00, 0x05]) && out.contains(&vec![0xb0, 0x20, 0x5d]), "{out:?}");
        assert!(out.contains(&vec![0xa0, 0x02, 0x01]), "{out:?}");
        fp.handle(&mut s, &[0xa0, classic::RIGHT, 0x7f]);
        let kick = s.project.tracks[0].id;
        assert_eq!((fp.current, s.selected_track), (1, Some(kick)));
        // A press is 7f; 01 does nothing.
        fp.handle(&mut s, &[0xa0, classic::MUTE, 0x01]);
        assert!(!s.project.tracks[0].muted);
        for id in [classic::MUTE, classic::SOLO, classic::ARM] {
            fp.handle(&mut s, &[0xa0, id, 0x7f]);
            fp.handle(&mut s, &[0xa0, id, 0x00]);
        }
        let t = &s.project.tracks[0];
        assert!(t.muted && t.armed && t.solo, "{t:?}");
        fp.handle(&mut s, &[0xa0, classic::WRITE, 0x7f]);
        assert_eq!(s.project.tracks[0].auto_mode, AutoMode::Write);
        let out = fp.feedback(&View::read(&s));
        for led in [0x15, 0x16, 0x17, 0x0e] {
            assert!(out.contains(&vec![0xa0, led, 0x01]), "LED {led:#x} in {out:?}");
        }
        // The fader is read over 14 bits: 8192 is half way, about -12 dB.
        fp.handle(&mut s, &[0xb0, 0x00, 0x40]);
        fp.handle(&mut s, &[0xb0, 0x20, 0x00]);
        assert!((s.project.tracks[0].volume - 0.279_976).abs() < 1e-5, "{}", s.project.tracks[0].volume);
        fp.handle(&mut s, &[0xe0, 0x00, 0x01]);
        assert!((s.project.tracks[0].pan - 0.02).abs() < 1e-9);
        fp.handle(&mut s, &[0xa0, classic::LEFT, 0x7f]);
        assert_eq!(fp.current, 0);
        assert_eq!(classic::led(classic::OFF), 0x10);
    }

    #[test]
    fn v2_notes_pan_mode_and_shift() {
        let mut s = session();
        let mut fp = FaderPort::new(MidiKind::FaderPort2);
        let out = fp.feedback(&View::read(&s));
        assert!(out.contains(&vec![0xe0, 0x52, 0x5b]), "unity as 11730: {out:?}");
        assert!(out.contains(&vec![0x90, v2::STOP, 0x7f]));
        fp.handle(&mut s, &[0x90, v2::NEXT, 0x7f]);
        fp.handle(&mut s, &[0x90, v2::NEXT, 0x00]);
        fp.handle(&mut s, &[0x90, v2::MUTE, 0x7f]);
        assert!(s.project.tracks[0].muted);
        fp.handle(&mut s, &[0x90, v2::TOUCH, 0x7f]);
        assert_eq!(s.project.tracks[0].auto_mode, AutoMode::Touch);
        fp.handle(&mut s, &[0xb0, v2::ENCODER, 0x05]);
        fp.handle(&mut s, &[0xb0, v2::ENCODER, 0x41]);
        assert!((s.project.tracks[0].pan - 0.08).abs() < 1e-9);
        fp.handle(&mut s, &[0xe0, 0x00, 0x40]);
        assert!((s.project.tracks[0].volume - 0.279_976).abs() < 1e-5);
        // Pan mode: the fader is the pan with left at the top, the encoder the volume.
        fp.handle(&mut s, &[0x90, v2::PAN, 0x7f]);
        let out = fp.feedback(&View::read(&s));
        let fader = fader_messages(MidiKind::FaderPort2, 0.46).remove(0);
        assert!(out.contains(&vec![0x90, v2::PAN, 0x01]) && out.contains(&fader), "{out:?}");
        fp.handle(&mut s, &[0xe0, 0x00, 0x60]);
        assert!((s.project.tracks[0].pan + 0.5001).abs() < 1e-3);
        fp.handle(&mut s, &[0xb0, v2::ENCODER, 0x03]);
        assert!((s.project.tracks[0].volume - 0.302_079).abs() < 1e-4, "{}", s.project.tracks[0].volume);
        fp.handle(&mut s, &[0x90, v2::SHIFT, 0x7f]);
        fp.handle(&mut s, &[0x90, v2::LOOP, 0x7f]);
        assert_eq!(s.project.markers.len(), 1, "shift and loop add a marker");
    }
}
