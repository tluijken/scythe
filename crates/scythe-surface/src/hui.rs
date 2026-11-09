//! HUI, partly, as REAPER drives it: the channel strips (faders, touch, select, mute, solo,
//! record arm, V-pots for pan, four-character names, meters) and the transport. Buttons and
//! LEDs are addressed as a zone and a port (CC 15 then CC 47 from the device, CC 12 then CC 44
//! to it); the host pings with note 0 every second and the device answers.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use scythe_core::surface::MidiSurface;
use scythe_session::Session;
use serde_json::json;

use crate::midi::{MidiPorts, split};
use crate::view::{self, View};
use crate::{Waker, exec, run_action, set_pan, set_volume};

/// Ports within a channel strip's zone (zones 0-7).
pub const PORT_FADER: u8 = 0;
pub const PORT_SELECT: u8 = 1;
pub const PORT_MUTE: u8 = 2;
pub const PORT_SOLO: u8 = 3;
pub const PORT_RECORD: u8 = 7;
/// The bank and channel buttons.
pub const ZONE_BANK: u8 = 0x0a;
pub const CHANNEL_LEFT: u8 = 0;
pub const BANK_LEFT: u8 = 1;
pub const CHANNEL_RIGHT: u8 = 2;
pub const BANK_RIGHT: u8 = 3;
/// The main transport buttons; REAPER lights stop, play and record here.
pub const ZONE_TRANSPORT: u8 = 0x0e;
pub const REWIND: u8 = 1;
pub const FORWARD: u8 = 2;
pub const STOP: u8 = 3;
pub const PLAY: u8 = 4;
pub const RECORD: u8 = 5;
/// Return to zero, end, loop.
pub const ZONE_TRANSPORT2: u8 = 0x0f;
pub const RTZ: u8 = 0;
pub const END: u8 = 1;
pub const LOOP: u8 = 3;

const PING_INTERVAL: Duration = Duration::from_secs(1);
const FADER_MAX: f64 = 16383.0;
const PAN_STEP: f64 = 1.0 / 31.0;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    Led(u8, u8),
    Fader(u8),
    Ring(u8),
    Name(u8),
    Meter(u8, u8),
}

pub struct Hui {
    pub offset: usize,
    /// The zone the device's last CC 15 chose.
    zone: u8,
    /// Fader MSBs waiting for their LSB.
    fader_msb: [Option<u8>; 8],
    touched: [bool; 8],
    sent: HashMap<Key, Vec<Vec<u8>>>,
}

impl Hui {
    pub fn new(offset: usize) -> Self {
        Self { offset, zone: 0, fader_msb: [None; 8], touched: [false; 8], sent: HashMap::new() }
    }

    pub fn reset(&mut self) {
        self.sent.clear();
    }

    pub fn handle(&mut self, session: &mut Session, bank: &mut usize, msg: &[u8]) {
        let view = View::read(session);
        let first = *bank + self.offset;
        let strip = |i: usize| view.track(first + i + 1).filter(|t| t.id.is_some()).cloned();
        match *msg {
            [0x90, 0x00, 0x7f] => {}
            [0xb0, 0x0f, zone] => self.zone = zone,
            [0xb0, 0x2f, port] => {
                let down = port & 0x40 != 0;
                self.button(session, bank, &view, self.zone, port & 0x0f, down);
            }
            [0xb0, cc @ 0x00..=0x07, msb] => self.fader_msb[cc as usize] = Some(msb),
            [0xb0, cc @ 0x20..=0x27, lsb] => {
                let i = (cc - 0x20) as usize;
                if let (Some(msb), Some(t)) = (self.fader_msb[i].take(), strip(i)) {
                    let position = ((msb as u16) << 7 | lsb as u16) as f64 / FADER_MAX;
                    set_volume(session, t.id, view::fader_to_gain(position));
                    self.sent.remove(&Key::Fader(i as u8));
                }
            }
            [0xb0, cc @ 0x40..=0x47, value] => {
                let ticks = (value & 0x3f) as f64 * if value & 0x40 != 0 { 1.0 } else { -1.0 };
                if let Some(t) = strip((cc - 0x40) as usize) {
                    set_pan(session, t.id, t.pan + ticks * PAN_STEP);
                }
            }
            _ => {}
        }
    }

    fn button(&mut self, session: &mut Session, bank: &mut usize, view: &View, zone: u8, port: u8, down: bool) {
        let first = *bank + self.offset;
        let strip = |i: u8| view.track(first + i as usize + 1).filter(|t| t.id.is_some()).cloned();
        if zone < 8 && port == PORT_FADER {
            self.touched[zone as usize] = down;
            if !down && let Some(id) = strip(zone).and_then(|t| t.id) {
                session.release_automation(id, scythe_session::AutoTarget::Volume);
            }
            return;
        }
        if !down {
            return;
        }
        if zone < 8 {
            let Some(t) = strip(zone) else { return };
            let Some(id) = t.id else { return };
            let (cmd, field, now) = match port {
                PORT_MUTE => ("track.mute", "muted", t.muted),
                PORT_SOLO => ("track.solo", "solo", t.solo),
                PORT_RECORD => ("track.arm", "armed", t.armed),
                PORT_SELECT => {
                    session.select_track(id);
                    return;
                }
                _ => return,
            };
            let _ = exec(session, json!({ "cmd": cmd, "track": id.0, field: !now }));
            return;
        }
        let last = view.tracks.len().saturating_sub(1);
        match (zone, port) {
            (ZONE_BANK, CHANNEL_LEFT) => *bank = bank.saturating_sub(1),
            (ZONE_BANK, BANK_LEFT) => *bank = bank.saturating_sub(8),
            (ZONE_BANK, CHANNEL_RIGHT) => *bank = (*bank + 1).min(last),
            (ZONE_BANK, BANK_RIGHT) => *bank = (*bank + 8).min(last),
            (ZONE_TRANSPORT, STOP) => run_action(session, "1016"),
            (ZONE_TRANSPORT, PLAY) => run_action(session, "1007"),
            (ZONE_TRANSPORT, RECORD) => run_action(session, "1013"),
            (ZONE_TRANSPORT, REWIND) => run_action(session, "40172"),
            (ZONE_TRANSPORT, FORWARD) => run_action(session, "40173"),
            (ZONE_TRANSPORT2, RTZ) => run_action(session, "40042"),
            (ZONE_TRANSPORT2, END) => run_action(session, "40043"),
            (ZONE_TRANSPORT2, LOOP) => run_action(session, "1068"),
            _ => {}
        }
    }

    pub fn feedback(&mut self, view: &View, bank: usize) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut sent = std::mem::take(&mut self.sent);
        let mut put = |key: Key, msgs: Vec<Vec<u8>>| {
            if sent.get(&key) != Some(&msgs) {
                out.extend(msgs.iter().cloned());
                sent.insert(key, msgs);
            }
        };
        let led = |zone: u8, port: u8, on: bool| {
            vec![vec![0xb0, 0x0c, zone], vec![0xb0, 0x2c, port | if on { 0x40 } else { 0 }]]
        };
        for i in 0..8u8 {
            let t = view.track(bank + self.offset + i as usize + 1).filter(|t| t.id.is_some());
            if !self.touched[i as usize] {
                let position = view::gain_to_fader(t.map_or(0.0, |t| t.volume));
                let v = (position * FADER_MAX).round() as u16;
                put(Key::Fader(i), vec![vec![0xb0, i, (v >> 7) as u8], vec![0xb0, 0x20 + i, (v & 0x7f) as u8]]);
            }
            let pan = t.map_or(0.0, |t| t.pan);
            put(Key::Ring(i), vec![vec![0xb0, 0x10 + i, crate::mcu::pan_ring(pan)]]);
            put(Key::Led(i, PORT_MUTE), led(i, PORT_MUTE, t.is_some_and(|t| t.muted)));
            put(Key::Led(i, PORT_SOLO), led(i, PORT_SOLO, t.is_some_and(|t| t.solo)));
            put(Key::Led(i, PORT_RECORD), led(i, PORT_RECORD, t.is_some_and(|t| t.armed)));
            let mut name = vec![0xf0, 0x00, 0x00, 0x66, 0x05, 0x00, 0x10, i];
            name.extend(four_chars(t.map_or("", |t| t.name.as_str())));
            name.push(0xf7);
            put(Key::Name(i), vec![name]);
            let (l, r) = t.map_or((0.0, 0.0), |t| t.peak);
            put(Key::Meter(i, 0), vec![vec![0xa0, i, meter_level(l)]]);
            put(Key::Meter(i, 1), vec![vec![0xa0, i, 0x10 | meter_level(r)]]);
        }
        // REAPER leaves the stop LED dark, stopped or not.
        put(Key::Led(ZONE_TRANSPORT, STOP), led(ZONE_TRANSPORT, STOP, false));
        put(Key::Led(ZONE_TRANSPORT, PLAY), led(ZONE_TRANSPORT, PLAY, view.playing));
        put(Key::Led(ZONE_TRANSPORT, RECORD), led(ZONE_TRANSPORT, RECORD, view.recording));
        self.sent = sent;
        out
    }
}

/// Four characters for a HUI scribble strip.
pub fn four_chars(name: &str) -> Vec<u8> {
    let mut out: Vec<u8> =
        name.chars().filter(|c| c.is_ascii() && !c.is_ascii_control()).take(4).map(|c| c as u8).collect();
    out.resize(4, b' ');
    out
}

/// A peak as one of HUI's twelve meter steps.
pub fn meter_level(peak: f32) -> u8 {
    crate::mcu::meter_level(peak)
}

/// A HUI on MIDI ports.
pub struct HuiSurface {
    pub hui: Hui,
    ports: MidiPorts,
    pinged: Instant,
}

impl HuiSurface {
    pub fn open(config: &MidiSurface, wake: Waker) -> Result<Self, String> {
        let ports = MidiPorts::open(config.input, config.output, wake)?;
        Ok(Self { hui: Hui::new(config.offset.max(0) as usize), ports, pinged: Instant::now() - PING_INTERVAL })
    }

    pub fn input(&mut self, session: &mut Session, bank: &mut usize) {
        while let Ok(bytes) = self.ports.rx.try_recv() {
            for msg in split(&bytes) {
                self.hui.handle(session, bank, msg);
            }
        }
        if self.pinged.elapsed() >= PING_INTERVAL {
            self.pinged = Instant::now();
            self.ports.send(&[0x90, 0x00, 0x00]);
        }
    }

    pub fn feedback(&mut self, view: &View, bank: usize) {
        for msg in self.hui.feedback(view, bank) {
            self.ports.send(&msg);
        }
    }
}
