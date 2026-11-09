//! ReaGate, from the parameters it stores.

use super::{Lookahead, db_value, marked_chunk, marked_params, ms_value, param, samples, sidechain};
use crate::clap_host::ParamInfo;
use crate::fx::{Effect, ProcessContext};

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

    fn process(&mut self, left: &mut [f32], right: &mut [f32], ctx: &ProcessContext) {
        let key = sidechain(&self.params, 7, ctx);
        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            // The detector sees the incoming signal (or the sidechain); with pre-open the audio
            // is delayed so the gate is already open when a hit arrives.
            let level = match key {
                Some((kl, kr)) => kl[i].abs().max(kr[i].abs()),
                None => l.abs().max(r.abs()),
            };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stock::testing::{check, chunk, run};

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
}
