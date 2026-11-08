//! Gate and compressor that read the parameters ReaGate and ReaComp store.
//! Parameter layouts and scalings come from REAPER's parameter API; the detectors themselves
//! are textbook designs, so the result is close to REAPER but not sample identical.

use crate::clap_host::ParamInfo;
use crate::fx::{Effect, ProcessContext};

/// ReaGate and ReaComp chunks: two 0xdeadbeef/0xdeadf00d marker words, then every parameter
/// as an f32 in parameter order.
fn marked_params(chunk: &[u8]) -> Option<Vec<f32>> {
    let word = |i: usize| chunk.get(i * 4..i * 4 + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    if word(0)? != 0xdead_beef || word(1)? != 0xdead_f00d {
        return None;
    }
    Some(chunk[8..].as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect())
}

fn marked_chunk(params: &[f32]) -> Vec<u8> {
    let mut d = Vec::with_capacity(8 + params.len() * 4);
    d.extend(0xdead_beef_u32.to_le_bytes());
    d.extend(0xdead_f00d_u32.to_le_bytes());
    for p in params {
        d.extend(p.to_le_bytes());
    }
    d
}

pub(crate) fn param(id: u32, name: &str, min: f64, max: f64, default: f64, log: bool) -> ParamInfo {
    ParamInfo { id, name: name.into(), min, max, default, log }
}

pub(crate) fn db_value(gain: f64) -> (f64, String) {
    let db = 20.0 * gain.max(1e-9).log10();
    (db.max(-150.0), if db <= -149.0 { "-inf dB".into() } else { format!("{db:+.1} dB") })
}

pub(crate) fn ms_value(ms: f64) -> (f64, String) {
    (ms, if ms >= 1000.0 { format!("{:.2} s", ms / 1000.0) } else { format!("{ms:.1} ms") })
}

/// One-pole smoothing coefficient for a time constant in milliseconds.
fn coeff(ms: f32, sample_rate: u32) -> f32 {
    if ms <= 0.0 { 0.0 } else { (-1.0 / (ms * 0.001 * sample_rate as f32)).exp() }
}

pub struct Gate {
    params: Vec<f32>,
    sample_rate: u32,
    threshold: f32,
    close_threshold: f32,
    attack: f32,
    release: f32,
    hold: usize,
    dry: f32,
    wet: f32,
    gain: f32,
    held: usize,
    open: bool,
}

impl Gate {
    /// Parameters 0..: threshold, attack, release, pre-open, hold, lowpass, hipass, …, dry (9),
    /// wet (10), noise (11), hysteresis (12).
    pub fn from_reagate(chunk: &[u8], sample_rate: u32) -> Option<Self> {
        let p = marked_params(chunk)?;
        let get = |i: usize, d: f32| p.get(i).copied().unwrap_or(d);
        let threshold = get(0, 0.0);
        Some(Self {
            params: p.clone(),
            sample_rate,
            threshold,
            close_threshold: threshold * get(12, 1.0),
            attack: coeff(get(1, 0.006) * 500.0, sample_rate),
            release: coeff(get(2, 0.02) * 5000.0, sample_rate),
            hold: (get(4, 0.0) * 1.0 * sample_rate as f32) as usize,
            dry: get(9, 0.0),
            wet: get(10, 1.0),
            gain: 0.0,
            held: 0,
            open: false,
        })
    }
}

impl Effect for Gate {
    fn params(&mut self) -> Vec<ParamInfo> {
        vec![
            param(0, "Threshold (dB)", -90.0, 0.0, -90.0, false),
            param(1, "Attack (ms)", 0.1, 500.0, 3.0, true),
            param(2, "Release (ms)", 1.0, 5000.0, 100.0, true),
            param(12, "Hysteresis (dB)", -24.0, 0.0, 0.0, false),
        ]
    }

    fn param(&mut self, id: u32) -> Option<(f64, String)> {
        let raw = *self.params.get(id as usize)? as f64;
        Some(match id {
            0 | 12 => db_value(raw),
            1 => ms_value(raw * 500.0),
            _ => ms_value(raw * 5000.0),
        })
    }

    fn set_param(&mut self, id: u32, value: f64) {
        let raw = match id {
            0 | 12 => 10f64.powf(value / 20.0),
            1 => value / 500.0,
            2 => value / 5000.0,
            _ => return,
        };
        if let Some(slot) = self.params.get_mut(id as usize) {
            *slot = raw as f32;
            if let Some(fresh) = Gate::from_reagate(&marked_chunk(&self.params), self.sample_rate) {
                *self = Gate { gain: self.gain, open: self.open, held: self.held, ..fresh };
            }
        }
    }

    fn set_normalized(&mut self, index: usize, v: f64) -> bool {
        use crate::normalized::{amp2_db, linear};
        match index {
            0 | 12 => self.set_param(index as u32, amp2_db(v)),
            1 => self.set_param(1, linear(v, 0.0, 500.0)),
            2 => self.set_param(2, linear(v, 0.0, 5000.0)),
            _ => return false,
        }
        true
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        Some(marked_chunk(&self.params))
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], _ctx: &ProcessContext) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let level = l.abs().max(r.abs());
            if level >= self.threshold {
                self.open = true;
                self.held = self.hold;
            } else if level < self.close_threshold {
                if self.held > 0 {
                    self.held -= 1;
                } else {
                    self.open = false;
                }
            }
            let (target, c) = if self.open { (1.0, self.attack) } else { (0.0, self.release) };
            self.gain = target + (self.gain - target) * c;
            let g = self.dry + self.wet * self.gain;
            *l *= g;
            *r *= g;
        }
    }

    fn reset(&mut self) {
        self.gain = 0.0;
        self.open = false;
    }
}

pub struct Compressor {
    params: Vec<f32>,
    sample_rate: u32,
    threshold_db: f32,
    ratio: f32,
    knee_db: f32,
    attack: f32,
    release: f32,
    dry: f32,
    wet: f32,
    env_db: f32,
}

impl Compressor {
    /// Parameters 0..: threshold, ratio, attack, release, pre-comp, reserved, lowpass, hipass,
    /// …, dry (10), wet (11), …, knee (14).
    pub fn from_reacomp(chunk: &[u8], sample_rate: u32) -> Option<Self> {
        let p = marked_params(chunk)?;
        let get = |i: usize, d: f32| p.get(i).copied().unwrap_or(d);
        Some(Self {
            params: p.clone(),
            sample_rate,
            threshold_db: 20.0 * get(0, 1.0).max(1e-9).log10(),
            ratio: 1.0 + get(1, 0.0303) * 99.0,
            knee_db: get(14, 0.0) * 24.0,
            attack: coeff(get(2, 0.006) * 500.0, sample_rate),
            release: coeff(get(3, 0.02) * 5000.0, sample_rate),
            dry: get(10, 0.0),
            wet: get(11, 1.0),
            env_db: -150.0,
        })
    }

    fn gain_reduction_db(&self, level_db: f32) -> f32 {
        let over = level_db - self.threshold_db;
        let slope = 1.0 - 1.0 / self.ratio;
        if self.knee_db > 0.0 && over.abs() < self.knee_db / 2.0 {
            -slope * (over + self.knee_db / 2.0).powi(2) / (2.0 * self.knee_db)
        } else if over > 0.0 {
            -slope * over
        } else {
            0.0
        }
    }
}

impl Effect for Compressor {
    fn params(&mut self) -> Vec<ParamInfo> {
        vec![
            param(0, "Threshold (dB)", -60.0, 0.0, 0.0, false),
            param(1, "Ratio", 1.0, 100.0, 4.0, true),
            param(2, "Attack (ms)", 0.1, 500.0, 3.0, true),
            param(3, "Release (ms)", 1.0, 5000.0, 100.0, true),
        ]
    }

    fn param(&mut self, id: u32) -> Option<(f64, String)> {
        let raw = *self.params.get(id as usize)? as f64;
        Some(match id {
            0 => db_value(raw),
            1 => {
                let r = 1.0 + raw * 99.0;
                (r, format!("{r:.1}:1"))
            }
            2 => ms_value(raw * 500.0),
            _ => ms_value(raw * 5000.0),
        })
    }

    fn set_param(&mut self, id: u32, value: f64) {
        let raw = match id {
            0 => 10f64.powf(value / 20.0),
            1 => (value - 1.0) / 99.0,
            2 => value / 500.0,
            3 => value / 5000.0,
            _ => return,
        };
        if let Some(slot) = self.params.get_mut(id as usize) {
            *slot = raw as f32;
            if let Some(fresh) = Compressor::from_reacomp(&marked_chunk(&self.params), self.sample_rate) {
                *self = Compressor { env_db: self.env_db, ..fresh };
            }
        }
    }

    fn set_normalized(&mut self, index: usize, v: f64) -> bool {
        use crate::normalized::{amp2_db, linear};
        match index {
            0 => self.set_param(0, amp2_db(v)),
            1 => self.set_param(1, linear(v, 1.0, 100.0)),
            2 => self.set_param(2, linear(v, 0.0, 500.0)),
            3 => self.set_param(3, linear(v, 0.0, 5000.0)),
            _ => return false,
        }
        true
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        Some(marked_chunk(&self.params))
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], _ctx: &ProcessContext) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let level_db = 20.0 * l.abs().max(r.abs()).max(1e-8).log10();
            let c = if level_db > self.env_db { self.attack } else { self.release };
            self.env_db = level_db + (self.env_db - level_db) * c;
            let g = 10f32.powf(self.gain_reduction_db(self.env_db) / 20.0);
            let out = self.dry + self.wet * g;
            *l *= out;
            *r *= out;
        }
    }

    fn reset(&mut self) {
        self.env_db = -150.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(params: &[f32]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend(0xdead_beefu32.to_le_bytes());
        d.extend(0xdead_f00du32.to_le_bytes());
        for p in params {
            d.extend(p.to_le_bytes());
        }
        d
    }

    #[test]
    fn gate_silences_below_threshold() {
        let mut params = vec![0.0; 24];
        params[0] = 0.1;
        params[1] = 0.0;
        params[2] = 0.0002;
        params[10] = 1.0;
        params[12] = 1.0;
        let mut g = Gate::from_reagate(&chunk(&params), 48000).unwrap();
        let ctx = ProcessContext { sample_rate: 48000, position: 0, playing: true, notes: None, bpm: 120.0 };
        let (mut l, mut r) = (vec![0.01f32; 4800], vec![0.01f32; 4800]);
        g.process(&mut l, &mut r, &ctx);
        assert!(l[4799].abs() < 1e-4);
        let (mut l, mut r) = (vec![0.5f32; 4800], vec![0.5f32; 4800]);
        g.process(&mut l, &mut r, &ctx);
        assert!((l[4799] - 0.5).abs() < 1e-3);
    }

    #[test]
    fn compressor_reduces_by_ratio() {
        let mut params = vec![0.0; 24];
        params[0] = 0.1;
        params[1] = 3.0 / 99.0;
        params[2] = 0.0;
        params[3] = 0.0;
        params[11] = 1.0;
        let mut c = Compressor::from_reacomp(&chunk(&params), 48000).unwrap();
        let ctx = ProcessContext { sample_rate: 48000, position: 0, playing: true, notes: None, bpm: 120.0 };
        let (mut l, mut r) = (vec![1.0f32; 100], vec![1.0f32; 100]);
        c.process(&mut l, &mut r, &ctx);
        // 20 dB over the threshold at 4:1 leaves 5 dB over: -15 dB.
        assert!((20.0 * l[99].log10() + 15.0).abs() < 0.1, "{}", 20.0 * l[99].log10());
    }
}
