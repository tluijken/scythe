//! ReaComp, from the parameters it stores.

use super::{db_value, marked_chunk, marked_params, ms_value, param, samples, sidechain};
use crate::clap_host::ParamInfo;
use crate::fx::{Effect, ProcessContext};

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
        /// Inside REAPER's knee the slope grows with the level by this many dB per full slope,
        /// whatever the knee width (fitted to 6 and 12 dB knees, within 0.06 dB).
        const KNEE_RAMP_DB: f64 = 36.0;
        let over = level_db - self.threshold_db;
        let slope = 1.0 - 1.0 / self.ratio;
        let k = self.knee_db;
        if k > 0.0 && over.abs() < k {
            // The knee reaches the knee setting either side of the threshold; at its ends it
            // meets the flat and the hard curve, with a bend in slope.
            slope * (0.5 * (over + k) + (over * over - k * k) / (2.0 * KNEE_RAMP_DB))
        } else {
            slope * over.max(0.0)
        }
    }

    /// The level gain reduction starts at: the threshold, or the bottom of the knee.
    fn onset(&self) -> f64 {
        self.threshold * 10f64.powf(-self.knee_db / 20.0)
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

    fn process(&mut self, left: &mut [f32], right: &mut [f32], ctx: &ProcessContext) {
        let n = self.window.len();
        let key = sidechain(&self.params, 8, ctx);
        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            let (dl, dr) = key.map_or((*l, *r), |(kl, kr)| (kl[i], kr[i]));
            let sq = (dl as f64 * dl as f64).max(dr as f64 * dr as f64);
            self.window_sum += sq - self.window[self.window_at];
            self.window[self.window_at] = sq;
            self.window_at = (self.window_at + 1) % n;
            if self.window_at == 0 {
                // Resum now and then so rounding cannot drift.
                self.window_sum = self.window.iter().sum();
            }
            let level = (self.window_sum.max(0.0) / n as f64).sqrt();
            // Rising levels start from where reduction begins: below it the envelope is irrelevant.
            if level > self.env {
                self.env = self.env.max(self.onset());
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
    use crate::stock::testing::{check, chunk, run};

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
