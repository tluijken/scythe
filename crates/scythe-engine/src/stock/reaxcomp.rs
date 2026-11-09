//! ReaXcomp, from the state it stores: `u32 56, u32 band count`, per band `f64 top frequency,
//! gain, threshold (both amplitudes), ratio, knee dB, i32 attack ms, release ms, RMS ms, u32
//! flags` (1 make-up, 2 auto release, 4 feedback detector, 8 off), then `u32, u32, f64 output
//! gain, f64, f64, u32`.
//!
//! Measured on REAPER 7.73 renders (`examples/stock_ref.rs`): the bands come from a cascade of
//! RBJ low-passes with Q 1, each band the low-pass of what the bands below left (so they sum
//! back to the input exactly), and every band runs ReaComp's detector, soft knee, timing and
//! make-up. A band that is off passes through untouched.

use super::reacomp::{Curve, CurveSpec, Envelope};
use super::{Biquad, Rms};
use crate::clap_host::ParamInfo;
use crate::fx::{Effect, ProcessContext};

const VERSION: u32 = 56;
const BAND_BYTES: usize = 56;
const PARAMS_PER_BAND: u32 = 12;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Band {
    top: f64,
    gain: f64,
    threshold: f64,
    ratio: f64,
    knee_db: f64,
    attack_ms: i32,
    release_ms: i32,
    rms_ms: i32,
    flags: u32,
}

const MAKEUP: u32 = 1;
const AUTO_RELEASE: u32 = 2;
const FEEDBACK: u32 = 4;
const OFF: u32 = 8;

impl Band {
    fn parse(b: &[u8]) -> Option<Self> {
        let f = |i: usize| f64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
        let w = |i: usize| i32::from_le_bytes(b[40 + i * 4..44 + i * 4].try_into().unwrap());
        (b.len() >= BAND_BYTES).then(|| Band {
            top: f(0),
            gain: f(1),
            threshold: f(2),
            ratio: f(3),
            knee_db: f(4),
            attack_ms: w(0),
            release_ms: w(1),
            rms_ms: w(2),
            flags: w(3) as u32,
        })
    }

    fn write(&self, out: &mut Vec<u8>) {
        for v in [self.top, self.gain, self.threshold, self.ratio, self.knee_db] {
            out.extend(v.to_le_bytes());
        }
        for v in [self.attack_ms, self.release_ms, self.rms_ms, self.flags as i32] {
            out.extend(v.to_le_bytes());
        }
    }

    fn curve(&self, sample_rate: u32) -> Curve {
        CurveSpec {
            threshold: self.threshold,
            ratio: self.ratio,
            knee_db: self.knee_db,
            soft_knee: true,
            classic_attack: false,
            attack_frames: self.attack_ms as f64 * 0.001 * sample_rate as f64,
            release_ms: self.release_ms as f64,
            auto_release: self.flags & AUTO_RELEASE != 0,
            auto_makeup: self.flags & MAKEUP != 0,
        }
        .build(sample_rate)
    }

    /// Parameter `k` of the band (REAPER's order) as shown, with its text.
    fn display(&self, k: u32) -> Option<(f64, String)> {
        let db = |v: f64| if v <= 1e-7 { -150.0 } else { 20.0 * v.log10() };
        let switch = |on: bool| (on as u8 as f64, if on { "on".to_owned() } else { "off".to_owned() });
        Some(match k {
            0 => (self.top, format!("{:.1} Hz", self.top)),
            1 => (db(self.gain), format!("{:+.1} dB", db(self.gain))),
            2 => (db(self.threshold), format!("{:+.1} dB", db(self.threshold))),
            3 => (self.ratio, format!("{:.2}:1", self.ratio)),
            4 => (self.knee_db, format!("{:.1} dB", self.knee_db)),
            5 => (self.attack_ms as f64, format!("{} ms", self.attack_ms)),
            6 => (self.release_ms as f64, format!("{} ms", self.release_ms)),
            7 => (self.rms_ms as f64, format!("{} ms", self.rms_ms)),
            8 => switch(self.flags & MAKEUP != 0),
            9 => switch(self.flags & AUTO_RELEASE != 0),
            10 => switch(self.flags & FEEDBACK != 0),
            11 => switch(self.flags & OFF == 0),
            _ => return None,
        })
    }

    fn set(&mut self, k: u32, v: f64) {
        let amp = |db: f64| if db <= -149.0 { 0.0 } else { 10f64.powf(db / 20.0) };
        let flag = |flags: u32, bit: u32, on: bool| if on { flags | bit } else { flags & !bit };
        match k {
            0 => self.top = v.clamp(20.0, 24000.0),
            1 => self.gain = amp(v.min(12.0)),
            2 => self.threshold = amp(v.min(12.0)),
            3 => self.ratio = v.clamp(0.1, 100.0),
            4 => self.knee_db = v.clamp(0.0, 24.0),
            5 => self.attack_ms = v.clamp(0.0, 250.0) as i32,
            6 => self.release_ms = v.clamp(0.0, 2000.0) as i32,
            7 => self.rms_ms = v.clamp(0.0, 250.0) as i32,
            8 => self.flags = flag(self.flags, MAKEUP, v >= 0.5),
            9 => self.flags = flag(self.flags, AUTO_RELEASE, v >= 0.5),
            10 => self.flags = flag(self.flags, FEEDBACK, v >= 0.5),
            11 => self.flags = flag(self.flags, OFF, v < 0.5),
            _ => {}
        }
    }

    /// Sets parameter `k` from REAPER's normalised value.
    fn set_normalized(&mut self, k: u32, v: f64) {
        let v = v.clamp(0.0, 1.0);
        let shown = match k {
            0 => crate::normalized::eq_freq(v),
            1 | 2 => {
                // An amplitude of 4v: 0 dB at a quarter, +12 dB at the top.
                let a = 4.0 * v;
                if a <= 1e-7 { -150.0 } else { 20.0 * a.log10() }
            }
            3 => {
                // The raw value runs from -1/3 to 1: below 0 the ratio falls linearly to 0.1,
                // above it rises as 1 + 99 v².
                let raw = -1.0 / 3.0 + v * 4.0 / 3.0;
                if raw < 0.0 { 1.0 + 2.7 * raw } else { 1.0 + 99.0 * raw * raw }
            }
            4 => v * 24.0,
            5 | 7 => (v * 250.0).trunc(),
            6 => (2000.0 * v * v).trunc(),
            _ => v,
        };
        self.set(k, shown);
    }
}

struct BandState {
    curve: Curve,
    envelope: Envelope,
    rms: Rms,
    /// The band's last output, which the feedback detector listens to.
    last: [f64; 2],
}

pub struct MultibandCompressor {
    bands: Vec<Band>,
    /// Everything after the bands, kept as stored; the output gain is its f64 at offset 8.
    tail: Vec<u8>,
    sample_rate: u32,
    crossovers: Vec<[Biquad; 2]>,
    states: Vec<BandState>,
}

impl MultibandCompressor {
    pub fn from_reaxcomp(chunk: &[u8], sample_rate: u32) -> Option<Self> {
        let word = |i: usize| chunk.get(i * 4..i * 4 + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
        if word(0)? != VERSION {
            return None;
        }
        let count = word(1)? as usize;
        if count == 0 || count > 64 {
            return None;
        }
        let bands = (0..count)
            .map(|k| Band::parse(chunk.get(8 + k * BAND_BYTES..8 + (k + 1) * BAND_BYTES)?))
            .collect::<Option<Vec<_>>>()?;
        let tail = chunk[8 + count * BAND_BYTES..].to_vec();
        let mut comp = Self { bands, tail, sample_rate, crossovers: Vec::new(), states: Vec::new() };
        comp.configure(true);
        Some(comp)
    }

    fn output_gain(&self) -> f64 {
        self.tail.get(8..16).map_or(1.0, |b| f64::from_le_bytes(b.try_into().unwrap()))
    }

    fn configure(&mut self, fresh: bool) {
        let sr = self.sample_rate;
        let nyquist = 0.49 * sr as f64;
        let crossovers: Vec<[Biquad; 2]> = self.bands[..self.bands.len() - 1]
            .iter()
            .map(|b| [Biquad::lowpass(b.top.clamp(1.0, nyquist), 1.0, sr); 2])
            .collect();
        if fresh || crossovers.len() != self.crossovers.len() {
            self.crossovers = crossovers;
        } else {
            for (old, new) in self.crossovers.iter_mut().zip(crossovers) {
                old[0].retune(&new[0]);
                old[1].retune(&new[1]);
            }
        }
        let frames = |ms: i32| ((ms.max(0) as f64 * 0.001 * sr as f64) as usize).max(1);
        let states: Vec<BandState> = self
            .bands
            .iter()
            .enumerate()
            .map(|(k, b)| {
                let old = (!fresh).then(|| self.states.get_mut(k)).flatten();
                let window = frames(b.rms_ms);
                match old {
                    Some(s) if s.rms.window() == window => BandState {
                        curve: b.curve(sr),
                        envelope: std::mem::take(&mut s.envelope),
                        rms: std::mem::replace(&mut s.rms, Rms::new(1)),
                        last: s.last,
                    },
                    _ => BandState {
                        curve: b.curve(sr),
                        envelope: Envelope::default(),
                        rms: Rms::new(window),
                        last: [0.0; 2],
                    },
                }
            })
            .collect();
        self.states = states;
    }

    fn chunk(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.bands.len() * BAND_BYTES + self.tail.len());
        out.extend(VERSION.to_le_bytes());
        out.extend((self.bands.len() as u32).to_le_bytes());
        for b in &self.bands {
            b.write(&mut out);
        }
        out.extend(&self.tail);
        out
    }

    fn band_param(&self, id: u32) -> Option<(usize, u32)> {
        let band = (id / PARAMS_PER_BAND) as usize;
        (band < self.bands.len()).then_some((band, id % PARAMS_PER_BAND))
    }
}

const PARAM_NAMES: [&str; 12] = [
    "Band top frequency (Hz)",
    "Gain (dB)",
    "Threshold (dB)",
    "Ratio",
    "Knee (dB)",
    "Attack (ms)",
    "Release (ms)",
    "RMS (ms)",
    "Make-up gain",
    "Auto release",
    "Feedback detector",
    "Active",
];

impl Effect for MultibandCompressor {
    fn params(&mut self) -> Vec<ParamInfo> {
        let ranges: [(f64, f64, f64); 12] = [
            (20.0, 24000.0, 1000.0),
            (-60.0, 12.0, 0.0),
            (-60.0, 12.0, 0.0),
            (0.1, 100.0, 2.0),
            (0.0, 24.0, 0.0),
            (0.0, 250.0, 15.0),
            (0.0, 2000.0, 150.0),
            (0.0, 250.0, 5.0),
            (0.0, 1.0, 1.0),
            (0.0, 1.0, 0.0),
            (0.0, 1.0, 0.0),
            (0.0, 1.0, 1.0),
        ];
        (0..self.bands.len() as u32)
            .flat_map(|b| {
                ranges.iter().enumerate().map(move |(k, &(lo, hi, default))| {
                    let name = format!("{}-{}", b + 1, PARAM_NAMES[k]);
                    super::param(b * PARAMS_PER_BAND + k as u32, &name, lo, hi, default, k == 0)
                })
            })
            .collect()
    }

    fn param(&mut self, id: u32) -> Option<(f64, String)> {
        let (band, k) = self.band_param(id)?;
        self.bands[band].display(k)
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some((band, k)) = self.band_param(id) {
            self.bands[band].set(k, value);
            self.configure(false);
        }
    }

    fn set_normalized(&mut self, index: usize, v: f64) -> bool {
        let Some((band, k)) = self.band_param(index as u32) else { return false };
        self.bands[band].set_normalized(k, v);
        self.configure(false);
        true
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        Some(self.chunk())
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], _ctx: &ProcessContext) {
        let out_gain = self.output_gain();
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let mut rest = [*l as f64, *r as f64];
            let mut out = [0.0f64; 2];
            for (k, (band, state)) in self.bands.iter().zip(self.states.iter_mut()).enumerate() {
                let signal = match self.crossovers.get_mut(k) {
                    Some([fl, fr]) => {
                        let low = [fl.run(rest[0]), fr.run(rest[1])];
                        rest = [rest[0] - low[0], rest[1] - low[1]];
                        low
                    }
                    None => rest,
                };
                if band.flags & OFF != 0 {
                    out[0] += signal[0];
                    out[1] += signal[1];
                    continue;
                }
                let detector = if band.flags & FEEDBACK != 0 { state.last } else { signal };
                let level = state.rms.run(detector[0], detector[1]);
                let g = state.envelope.step(&state.curve, level);
                state.last = [signal[0] * g * band.gain, signal[1] * g * band.gain];
                out[0] += state.last[0];
                out[1] += state.last[1];
            }
            *l = (out[0] * out_gain) as f32;
            *r = (out[1] * out_gain) as f32;
        }
    }

    fn reset(&mut self) {
        self.configure(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stock::testing::context;

    fn chunk(threshold: f64) -> Vec<u8> {
        let mut c = Vec::new();
        c.extend(VERSION.to_le_bytes());
        c.extend(4u32.to_le_bytes());
        for top in [200.0, 1000.0, 5000.0, 24000.0] {
            Band {
                top,
                gain: 1.0,
                threshold,
                ratio: 2.0,
                knee_db: 0.0,
                attack_ms: 15,
                release_ms: 150,
                rms_ms: 5,
                flags: 17,
            }
            .write(&mut c);
        }
        c.extend(1u32.to_le_bytes());
        c.extend(1u32.to_le_bytes());
        c.extend(1.0f64.to_le_bytes());
        c.extend(0.0f64.to_le_bytes());
        c.extend(0.0f64.to_le_bytes());
        c.extend(2u32.to_le_bytes());
        c
    }

    /// Below every threshold the bands add back up to the input, as REAPER renders an impulse.
    #[test]
    fn bands_sum_to_the_input() {
        let mut x = MultibandCompressor::from_reaxcomp(&chunk(1.0), 48000).unwrap();
        let mut l = vec![0.0f32; 4800];
        l[100] = 0.5;
        let mut r = l.clone();
        x.process(&mut l, &mut r, &context());
        assert!((l[100] - 0.5).abs() < 1e-6);
        assert!(l.iter().enumerate().all(|(i, v)| i == 100 || v.abs() < 1e-6));
    }

    #[test]
    fn state_round_trips() {
        let c = chunk(0.1);
        let mut x = MultibandCompressor::from_reaxcomp(&c, 48000).unwrap();
        assert_eq!(x.save_state().unwrap(), c);
        x.set_param(2, -6.0);
        let saved = x.save_state().unwrap();
        let back = MultibandCompressor::from_reaxcomp(&saved, 48000).unwrap();
        assert!((back.bands[0].threshold - 10f64.powf(-6.0 / 20.0)).abs() < 1e-12);
    }
}
