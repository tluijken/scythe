//! ReaGate and ReaComp, from the parameters they store. Both follow REAPER 7.73 renders
//! sample for sample for the detector, timing and gain settings measured (see the tests);
//! sidechain filters, soft knee and auto make-up are not modelled exactly.

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

/// A stored time (normalised parameter × full scale in ms) in samples, kept fractional for
/// ramps; REAPER counts whole samples from the f32 value it stores.
fn samples(raw: f32, full_ms: f64, sample_rate: u32) -> f64 {
    raw as f64 * full_ms * 0.001 * sample_rate as f64
}

/// A delay of the stereo signal by a fixed number of frames (ReaGate's pre-open lookahead).
struct Lookahead {
    l: Vec<f32>,
    r: Vec<f32>,
    at: usize,
}

impl Lookahead {
    fn new(frames: usize) -> Self {
        Self { l: vec![0.0; frames], r: vec![0.0; frames], at: 0 }
    }

    /// Pushes the newest frame and returns the one `frames` ago.
    fn swap(&mut self, l: f32, r: f32) -> (f32, f32) {
        if self.l.is_empty() {
            return (l, r);
        }
        let out = (self.l[self.at], self.r[self.at]);
        self.l[self.at] = l;
        self.r[self.at] = r;
        self.at = (self.at + 1) % self.l.len();
        out
    }
}

pub struct Gate {
    params: Vec<f32>,
    sample_rate: u32,
    threshold: f32,
    close_threshold: f32,
    /// Frames the gain takes to open or close fully (linear ramps).
    attack: f64,
    release: f64,
    /// Frames below the threshold before the gate starts closing.
    close_after: usize,
    lookahead: Lookahead,
    dry: f32,
    wet: f32,
    gain: f64,
    below: usize,
    open: bool,
}

impl Gate {
    /// Parameters 0..: threshold, attack, release, pre-open, hold, lowpass, hipass, …, dry (9),
    /// wet (10), noise (11), hysteresis (12). Gains are amplitudes; times are fractions of
    /// 500 ms (attack), 5 s (release), 250 ms (pre-open) and 1 s (hold).
    pub fn from_reagate(chunk: &[u8], sample_rate: u32) -> Option<Self> {
        let p = marked_params(chunk)?;
        let get = |i: usize, d: f32| p.get(i).copied().unwrap_or(d);
        let threshold = get(0, 0.0);
        let attack = samples(get(1, 0.006), 500.0, sample_rate);
        let pre = samples(get(3, 0.0), 250.0, sample_rate) as usize;
        let hold = samples(get(4, 0.0), 1000.0, sample_rate) as usize;
        Some(Self {
            params: p.clone(),
            sample_rate,
            threshold,
            close_threshold: threshold * get(12, 1.0),
            attack,
            release: samples(get(2, 0.02), 5000.0, sample_rate),
            close_after: hold + attack as usize + pre,
            lookahead: Lookahead::new(pre),
            dry: get(9, 0.0),
            wet: get(10, 1.0),
            gain: 0.0,
            below: 0,
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
                let lookahead = std::mem::replace(&mut self.lookahead, Lookahead::new(0));
                *self = Gate { gain: self.gain, open: self.open, below: self.below, lookahead, ..fresh };
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

    fn latency(&mut self) -> usize {
        self.lookahead.l.len()
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        Some(marked_chunk(&self.params))
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], _ctx: &ProcessContext) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            // The detector sees the incoming signal; with pre-open the audio is delayed so
            // the gate is already open when a hit arrives.
            let level = l.abs().max(r.abs());
            if level >= self.threshold {
                self.open = true;
                self.below = 0;
            } else if self.open && level < self.close_threshold {
                self.below += 1;
                if self.below >= self.close_after {
                    self.open = false;
                }
            }
            self.gain = if self.open {
                if self.attack < 1.0 { 1.0 } else { (self.gain + 1.0 / self.attack).min(1.0) }
            } else if self.release < 1.0 {
                0.0
            } else {
                (self.gain - 1.0 / self.release).max(0.0)
            };
            let (dl, dr) = self.lookahead.swap(*l, *r);
            let g = self.dry + self.wet * self.gain as f32;
            *l = dl * g;
            *r = dr * g;
        }
    }

    fn reset(&mut self) {
        self.gain = 0.0;
        self.open = false;
        self.below = 0;
        self.lookahead = Lookahead::new(self.lookahead.l.len());
    }
}

pub struct Compressor {
    params: Vec<f32>,
    sample_rate: u32,
    threshold: f64,
    threshold_db: f64,
    ratio: f64,
    knee_db: f64,
    attack: f64,
    release: f64,
    dry: f32,
    wet: f32,
    /// Squared levels over the RMS window, with their running sum.
    window: Vec<f64>,
    window_at: usize,
    window_sum: f64,
    env: f64,
}

impl Compressor {
    /// Parameters 0..: threshold, ratio, attack, release, pre-comp, reserved, lowpass, hipass,
    /// …, dry (10), wet (11), …, RMS size (13, fraction of 100 ms), knee (14).
    pub fn from_reacomp(chunk: &[u8], sample_rate: u32) -> Option<Self> {
        let p = marked_params(chunk)?;
        let get = |i: usize, d: f32| p.get(i).copied().unwrap_or(d);
        let threshold = get(0, 1.0).max(1e-9) as f64;
        let coeff = |frames: f64| if frames <= 0.0 { 0.0 } else { (-1.0 / frames).exp() };
        let window = (samples(get(13, 0.05), 100.0, sample_rate) as usize).max(1);
        Some(Self {
            params: p.clone(),
            sample_rate,
            threshold,
            threshold_db: 20.0 * threshold.log10(),
            // The top of the range is infinity rather than 100:1.
            ratio: if get(1, 0.0303) >= 1.0 { f64::INFINITY } else { 1.0 + get(1, 0.0303) as f64 * 99.0 },
            knee_db: get(14, 0.0) as f64 * 24.0,
            attack: coeff(samples(get(2, 0.006), 500.0, sample_rate)),
            release: coeff(samples(get(3, 0.02), 5000.0, sample_rate)),
            dry: get(10, 0.0),
            wet: get(11, 1.0),
            window: vec![0.0; window],
            window_at: 0,
            window_sum: 0.0,
            env: 0.0,
        })
    }

    fn gain_reduction_db(&self, level_db: f64) -> f64 {
        let over = level_db - self.threshold_db;
        let slope = 1.0 - 1.0 / self.ratio;
        if self.knee_db > 0.0 && over.abs() < self.knee_db / 2.0 {
            slope * (over + self.knee_db / 2.0).powi(2) / (2.0 * self.knee_db)
        } else {
            slope * over.max(0.0)
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
                let window = std::mem::take(&mut self.window);
                *self = Compressor {
                    env: self.env,
                    window,
                    window_at: self.window_at,
                    window_sum: self.window_sum,
                    ..fresh
                };
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
        let n = self.window.len();
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let sq = (*l as f64 * *l as f64).max(*r as f64 * *r as f64);
            self.window_sum += sq - self.window[self.window_at];
            self.window[self.window_at] = sq;
            self.window_at = (self.window_at + 1) % n;
            if self.window_at == 0 {
                // Resum now and then so rounding cannot drift.
                self.window_sum = self.window.iter().sum();
            }
            let level = (self.window_sum.max(0.0) / n as f64).sqrt();
            // Rising levels start from the threshold: below it the envelope is irrelevant.
            if level > self.env {
                self.env = self.env.max(self.threshold);
                self.env = level + self.attack * (self.env - level);
            } else {
                self.env = level + self.release * (self.env - level);
            }
            let db = 20.0 * self.env.max(1e-9).log10();
            let g = 10f64.powf(-self.gain_reduction_db(db) / 20.0) as f32;
            let out = self.dry + self.wet * g;
            *l *= out;
            *r *= out;
        }
    }

    fn reset(&mut self) {
        self.env = 0.0;
        self.window.fill(0.0);
        self.window_sum = 0.0;
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

    /// DC at 0.1, then 0.5 from 0.3 s to 0.7 s, then 0.1: REAPER's test signal.
    fn steps() -> Vec<f32> {
        (0..57600).map(|i| if (14400..33600).contains(&i) { 0.5 } else { 0.1 }).collect()
    }

    fn run(fx: &mut dyn Effect) -> Vec<f32> {
        let ctx = ProcessContext {
            sample_rate: 48000,
            position: 0,
            playing: true,
            notes: None,
            live: &[],
            live_channel: None,
            bpm: 120.0,
        };
        let input = steps();
        let (mut l, mut r) = (input.clone(), input.clone());
        fx.process(&mut l, &mut r, &ctx);
        l.iter().zip(&input).map(|(o, i)| o / i).collect()
    }

    /// Gains REAPER 7.73 rendered at these frames (threshold -12 dB).
    fn check(gain: &[f32], expect: &[(usize, f32)]) {
        for &(i, want) in expect {
            assert!((gain[i] - want).abs() < 2e-4, "frame {i}: {} vs {want}", gain[i]);
        }
    }

    #[test]
    fn gate_matches_reaper() {
        // Attack 10 ms, release 100 ms, then the same with 100 ms hold.
        let mut params = vec![0.0; 24];
        (params[0], params[1], params[2], params[10], params[12]) = (0.2512, 0.02, 0.02, 1.0, 1.0);
        let gain = run(&mut Gate::from_reagate(&chunk(&params), 48000).unwrap());
        check(
            &gain,
            &[
                (14399, 0.0),
                (14400, 0.0021),
                (14480, 0.1688),
                (14640, 0.5021),
                (14880, 1.0),
                (34080, 0.9994),
                (34560, 0.8994),
                (36000, 0.5994),
                (43200, 0.0),
            ],
        );
        params[4] = 0.1;
        let gain = run(&mut Gate::from_reagate(&chunk(&params), 48000).unwrap());
        check(&gain, &[(38400, 1.0), (43200, 0.0994)]);
    }

    #[test]
    fn compressor_matches_reaper() {
        // 4:1, attack 10 ms, release 100 ms, RMS 5 ms.
        let mut params = vec![0.0; 24];
        (params[0], params[1], params[2], params[3], params[11], params[13]) = (0.2512, 0.0303, 0.02, 0.02, 1.0, 0.05);
        let gain = run(&mut Compressor::from_reacomp(&chunk(&params), 48000).unwrap());
        check(
            &gain,
            &[
                (14420, 1.0),
                (14480, 0.9954),
                (14640, 0.8777),
                (15360, 0.6404),
                (24000, 0.5968),
                (34080, 0.622),
                (36000, 0.7796),
                (38400, 0.9986),
            ],
        );
    }
}
