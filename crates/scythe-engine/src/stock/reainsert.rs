//! ReaInsert, a hardware insert: the track's audio leaves on two device outputs and the
//! effect plays what comes back on two device inputs, reporting the round trip as latency so
//! the rest of the project lines up with it. The state REAPER 7.73 stores:
//! `u32 0x0dddeeee, u32 24, i32 output left, i32 output right, i32 input left, i32 input
//! right` (device channels counted from 0, -1 for none), `i32 -1, u32 0x00100000, f64 delay
//! compensation (a fraction of 200000 frames), f64 send volume, f64 return volume`.
//!
//! Without an audio device REAPER renders silence, and so does this emulation in offline
//! renders; in the live engine it uses the device through `crate::hardware_io`. A ping sends
//! one frame at -6 dB and takes the first input frame above -20 dB as the round trip.

use std::sync::Arc;

use super::param;
use crate::clap_host::ParamInfo;
use crate::fx::{Effect, ProcessContext};
use crate::hardware_io::HardwareIo;

const MAGIC: u32 = 0x0ddd_eeee;
/// The delay compensation's full range, in frames.
pub const MAX_DELAY: f64 = 200_000.0;
const PING_LEVEL: f32 = 0.5;
const PING_THRESHOLD: f32 = 0.1;

/// Parameter ids besides REAPER's three.
pub const OUTPUT: u32 = 10;
pub const INPUT: u32 = 11;
pub const PING: u32 = 12;

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub outputs: [i32; 2],
    pub inputs: [i32; 2],
    /// Frames the return is behind the send.
    pub delay: usize,
    pub send: f64,
    pub ret: f64,
}

fn word(chunk: &[u8], at: usize) -> Option<u32> {
    chunk.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()))
}

fn double(chunk: &[u8], at: usize) -> Option<f64> {
    chunk.get(at..at + 8).map(|b| f64::from_le_bytes(b.try_into().unwrap()))
}

impl Settings {
    pub fn parse(chunk: &[u8]) -> Option<Self> {
        if word(chunk, 0)? != MAGIC {
            return None;
        }
        let ch = |i: usize| word(chunk, 8 + i * 4).map(|w| w as i32);
        Some(Self {
            outputs: [ch(0)?, ch(1)?],
            inputs: [ch(2)?, ch(3)?],
            delay: (double(chunk, 32)?.clamp(0.0, 1.0) * MAX_DELAY).round() as usize,
            send: double(chunk, 40)?,
            ret: double(chunk, 48)?,
        })
    }

    fn write(&self, chunk: &mut Vec<u8>) {
        if chunk.len() < 56 {
            *chunk = vec![0; 56];
            chunk[0..4].copy_from_slice(&MAGIC.to_le_bytes());
            chunk[4..8].copy_from_slice(&24u32.to_le_bytes());
            chunk[24..28].copy_from_slice(&(-1i32).to_le_bytes());
            chunk[28..32].copy_from_slice(&0x0010_0000u32.to_le_bytes());
        }
        for (i, c) in self.outputs.iter().chain(&self.inputs).enumerate() {
            chunk[8 + i * 4..12 + i * 4].copy_from_slice(&c.to_le_bytes());
        }
        chunk[32..40].copy_from_slice(&(self.delay as f64 / MAX_DELAY).to_le_bytes());
        chunk[40..48].copy_from_slice(&self.send.to_le_bytes());
        chunk[48..56].copy_from_slice(&self.ret.to_le_bytes());
    }
}

enum Ping {
    Idle,
    /// The ping goes out with the next frame.
    Requested,
    /// Sent at this frame of the effect's own count.
    Waiting(u64),
}

/// Whether an enabled ReaInsert sits on a track or the master, so the live engine needs the
/// device's input.
pub fn in_project(project: &scythe_core::Project) -> bool {
    project
        .tracks
        .iter()
        .flat_map(|t| &t.fx)
        .chain(&project.master.fx)
        .any(|s| s.enabled && s.name.contains("ReaInsert (Cockos)"))
}

pub struct Insert {
    chunk: Vec<u8>,
    pub settings: Settings,
    hardware: Option<Arc<HardwareIo>>,
    segment: u64,
    at: usize,
    frame: u64,
    ping: Ping,
    timeout: u64,
}

impl Insert {
    pub fn new(chunk: &[u8], hardware: Option<Arc<HardwareIo>>, sample_rate: u32) -> Option<Self> {
        Some(Self {
            settings: Settings::parse(chunk)?,
            chunk: chunk.to_vec(),
            hardware,
            segment: u64::MAX,
            at: 0,
            frame: 0,
            ping: Ping::Idle,
            timeout: 2 * sample_rate as u64,
        })
    }

    /// Device outputs this insert needs.
    pub fn outputs(&self) -> usize {
        self.settings.outputs.iter().map(|&c| (c + 1).max(0) as usize).max().unwrap_or(0)
    }

    pub fn pinging(&self) -> bool {
        !matches!(self.ping, Ping::Idle)
    }
}

fn channel_value(c: i32) -> (f64, String) {
    if c < 0 { (0.0, "none".into()) } else { ((c + 1) as f64, format!("{} / {}", c + 1, c + 2)) }
}

impl Effect for Insert {
    fn params(&mut self) -> Vec<ParamInfo> {
        vec![
            param(0, "Delay compensation (frames)", 0.0, MAX_DELAY, 0.0, false),
            param(1, "Send volume", -60.0, 6.0, 0.0, false),
            param(2, "Return volume", -60.0, 6.0, 0.0, false),
            param(OUTPUT, "Send to outputs", 0.0, 63.0, 3.0, false),
            param(INPUT, "Return from inputs", 0.0, 63.0, 3.0, false),
            param(PING, "Ping-detect the delay", 0.0, 1.0, 0.0, false),
        ]
    }

    fn param(&mut self, id: u32) -> Option<(f64, String)> {
        let s = &self.settings;
        Some(match id {
            0 => (s.delay as f64, format!("{} frames", s.delay)),
            1 => super::db_value(s.send),
            2 => super::db_value(s.ret),
            OUTPUT => channel_value(s.outputs[0]),
            INPUT => channel_value(s.inputs[0]),
            PING => {
                let on = self.pinging();
                (on as u8 as f64, if on { "listening".into() } else { "idle".into() })
            }
            _ => return None,
        })
    }

    fn set_param(&mut self, id: u32, value: f64) {
        let gain = |db: f64| if db <= -59.9 { 0.0 } else { 10f64.powf(db.min(6.0) / 20.0) };
        let pair = |v: f64| {
            let first = v.round() as i32 - 1;
            if first < 0 { [-1, -1] } else { [first, first + 1] }
        };
        let s = &mut self.settings;
        match id {
            0 => s.delay = value.clamp(0.0, MAX_DELAY).round() as usize,
            1 => s.send = gain(value),
            2 => s.ret = gain(value),
            OUTPUT => s.outputs = pair(value),
            INPUT => s.inputs = pair(value),
            PING if value >= 0.5 => self.ping = Ping::Requested,
            PING => self.ping = Ping::Idle,
            _ => {}
        }
    }

    fn set_normalized(&mut self, index: usize, v: f64) -> bool {
        let v = v.clamp(0.0, 1.0);
        let s = &mut self.settings;
        match index {
            0 => s.delay = (v * MAX_DELAY).round() as usize,
            1 => s.send = 2.0 * v,
            2 => s.ret = 2.0 * v,
            _ => return false,
        }
        true
    }

    fn latency(&mut self) -> usize {
        self.settings.delay
    }

    fn device_outputs(&self) -> usize {
        self.outputs()
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        let mut chunk = self.chunk.clone();
        self.settings.write(&mut chunk);
        Some(chunk)
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], _ctx: &ProcessContext) {
        let n = left.len();
        let outputs = self.outputs();
        let Some(hw) = self.hardware.clone() else {
            left.fill(0.0);
            right.fill(0.0);
            return;
        };
        let s = &self.settings;
        let (send, ret) = (s.send as f32, s.ret as f32);
        let (outs, ins) = (s.outputs, s.inputs);
        let mut ping = std::mem::replace(&mut self.ping, Ping::Idle);
        let mut detected = None;
        let frame = self.frame;
        let timeout = self.timeout;
        let ran = hw.with_segment(&mut self.segment, &mut self.at, outputs, |at, input, sends| {
            let channel = |c: i32| (c >= 0).then_some(c as usize);
            for (k, buf) in [&*left, &*right].into_iter().enumerate() {
                if let Some(out) = channel(outs[k]).and_then(|c| sends.get_mut(c)) {
                    let end = (at + n).min(out.len());
                    out[at..end].iter_mut().zip(buf.iter()).for_each(|(o, x)| *o += x * send);
                }
            }
            if matches!(ping, Ping::Requested) {
                for c in outs.iter().filter_map(|&c| channel(c)) {
                    if let Some(s) = sends.get_mut(c).and_then(|o| o.get_mut(at)) {
                        *s += PING_LEVEL;
                    }
                }
                ping = Ping::Waiting(frame);
            }
            for (k, buf) in [&mut *left, &mut *right].into_iter().enumerate() {
                let src = channel(ins[k]).and_then(|c| input.get(c)).map(|b| &b[at.min(b.len())..]);
                for (i, o) in buf.iter_mut().enumerate() {
                    let x = src.and_then(|b| b.get(i)).copied().unwrap_or(0.0);
                    if let Ping::Waiting(sent) = ping
                        && detected.is_none()
                        && x.abs() > PING_THRESHOLD
                    {
                        detected = Some(frame + i as u64 - sent);
                    }
                    *o = x * ret;
                }
            }
        });
        if !ran {
            left.fill(0.0);
            right.fill(0.0);
        }
        self.at += n;
        self.frame += n as u64;
        self.ping = match (ping, detected) {
            (_, Some(d)) => {
                self.settings.delay = (d as usize).min(MAX_DELAY as usize);
                Ping::Idle
            }
            (Ping::Waiting(sent), None) if self.frame - sent > timeout => Ping::Idle,
            (p, None) => p,
        };
    }

    fn reset(&mut self) {
        self.segment = u64::MAX;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stock::testing::context;

    /// REAPER 7.73's fresh state: outputs and inputs 3/4, no delay, unity volumes.
    const DEFAULT: &str = "7u7dDRgAAAACAAAAAwAAAAIAAAADAAAA/////wAAEAAAAAAAAAAAAAAAAAAAAPA/AAAAAAAA8D8=";

    fn default_chunk() -> Vec<u8> {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.decode(DEFAULT).unwrap()
    }

    #[test]
    fn reads_and_writes_reapers_state() {
        let chunk = default_chunk();
        let mut fx = Insert::new(&chunk, None, 48000).unwrap();
        assert_eq!(fx.settings, Settings { outputs: [2, 3], inputs: [2, 3], delay: 0, send: 1.0, ret: 1.0 });
        assert_eq!(fx.save_state().unwrap(), chunk);
        fx.set_normalized(0, 0.5);
        assert_eq!(fx.latency(), 100_000);
    }

    #[test]
    fn offline_it_plays_silence() {
        let mut fx = Insert::new(&default_chunk(), None, 48000).unwrap();
        let (mut l, mut r) = (vec![0.5f32; 64], vec![0.5f32; 64]);
        fx.process(&mut l, &mut r, &context());
        assert!(l.iter().chain(&r).all(|&v| v == 0.0));
    }

    /// Through a loopback of 300 frames, a ping finds the round trip and the return is the
    /// send that many frames later.
    #[test]
    fn a_ping_measures_the_loopback() {
        let hw = Arc::new(HardwareIo::default());
        hw.set_loopback(Some(300));
        let mut fx = Insert::new(&default_chunk(), Some(hw.clone()), 48000).unwrap();
        fx.set_param(PING, 1.0);
        let mut out = Vec::new();
        for block in 0..20 {
            hw.begin(128, None);
            hw.segment(0);
            let mut l: Vec<f32> =
                (0..128).map(|i| if block >= 10 { ((block * 128 + i) as f32).sin() } else { 0.0 }).collect();
            let mut r = l.clone();
            fx.process(&mut l, &mut r, &context());
            out.extend(l);
            let (mut dl, mut dr, mut extra) = (vec![0.0; 128], vec![0.0; 128], vec![vec![0.0; 128]; 2]);
            hw.mix_into(&mut dl, &mut dr, &mut extra);
        }
        assert!(!fx.pinging());
        assert_eq!(fx.latency(), 300);
        let sent = |f: usize| if f >= 1280 { (f as f32).sin() } else { 0.0 };
        for (f, v) in out.iter().enumerate().take(2560).skip(1600) {
            assert!((v - sent(f - 300)).abs() < 1e-6, "frame {f}");
        }
    }
}
