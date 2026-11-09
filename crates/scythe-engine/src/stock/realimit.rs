//! ReaLimit, from the settings it stores.

use crate::fx::{Effect, ProcessContext};

/// Peak limiter matching ReaLimit: gain is brought up by `ceiling - threshold`; a 2 ms
/// lookahead ramps the gain linearly down so each peak lands exactly on the ceiling, and the
/// gain recovers at a fixed rate in dB per second.
pub struct Limiter {
    threshold_db: f64,
    ceiling_db: f64,
    /// Gain applied before limiting: ceiling minus threshold.
    makeup: f64,
    ceiling: f64,
    /// Gain recovery per frame, as a factor (REAPER sets it in dB per second).
    release: f64,
    sample_rate: u32,
    /// Frames the gain ramps ahead of a peak; the audio is delayed by the same amount.
    lookahead: usize,
    delay_l: Vec<f32>,
    delay_r: Vec<f32>,
    /// Required gains still ahead: (frame they apply at, gain).
    pending: std::collections::VecDeque<(u64, f64)>,
    frame: u64,
    gain: f64,
}

impl Limiter {
    /// `u32 version, u32, f64 threshold dB, f64 ceiling dB, f64, f64, f64 release` where the
    /// release is stored as `10^(-dB per second / 20)`.
    pub fn from_realimit(chunk: &[u8], sample_rate: u32) -> Option<Self> {
        let f = |at: usize| chunk.get(at..at + 8).map(|b| f64::from_le_bytes(b.try_into().unwrap()));
        let (threshold, ceiling) = (f(8)?, f(16)?);
        if !(threshold.is_finite() && ceiling.is_finite()) {
            return None;
        }
        let db_per_s = f(40).filter(|v| *v > 0.0 && *v < 1.0).map_or(15.0, |v| -20.0 * v.log10());
        let lookahead = (0.002 * sample_rate as f64).round() as usize;
        let mut lim = Self {
            threshold_db: threshold,
            ceiling_db: ceiling,
            makeup: 1.0,
            ceiling: 1.0,
            release: 10f64.powf(db_per_s / 20.0 / sample_rate as f64),
            sample_rate,
            lookahead,
            delay_l: vec![0.0; lookahead],
            delay_r: vec![0.0; lookahead],
            pending: Default::default(),
            frame: 0,
            gain: 1.0,
        };
        lim.update();
        lim.gain = lim.makeup;
        Some(lim)
    }

    fn update(&mut self) {
        self.makeup = 10f64.powf((self.ceiling_db - self.threshold_db) / 20.0);
        self.ceiling = 10f64.powf(self.ceiling_db / 20.0);
    }
}

impl Effect for Limiter {
    fn set_normalized(&mut self, index: usize, v: f64) -> bool {
        use crate::normalized::linear;
        match index {
            0 => self.threshold_db = linear(v, -60.0, 12.0),
            1 => self.ceiling_db = linear(v, -24.0, 0.0),
            2 => {
                let db_per_s = 6.0 - 20.0 * v.max(1e-6).log10();
                self.release = 10f64.powf(db_per_s / 20.0 / self.sample_rate as f64);
            }
            _ => return false,
        }
        self.update();
        true
    }

    fn latency(&mut self) -> usize {
        self.lookahead
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], _ctx: &ProcessContext) {
        let n = self.lookahead.max(1);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            // The gain a frame needs, due when it leaves the delay line.
            let peak = l.abs().max(r.abs()) as f64;
            let need = if peak * self.makeup > self.ceiling { self.ceiling / peak } else { self.makeup };
            while self.pending.front().is_some_and(|&(d, _)| d < self.frame) {
                self.pending.pop_front();
            }
            // Recover towards the make-up gain, but ramp linearly down to every requirement
            // ahead so it is met exactly when its frame plays.
            let mut g = (self.gain * self.release).min(self.makeup);
            for &(d, req) in &self.pending {
                if req <= self.gain {
                    let steps = (d - self.frame) as f64 + 1.0;
                    g = g.min(self.gain - (self.gain - req) / steps);
                }
            }
            self.gain = if self.lookahead == 0 { g.min(need) } else { g };
            // A new requirement only starts pulling the gain down from the next frame on.
            if need < self.makeup {
                self.pending.push_back((self.frame + self.lookahead as u64, need));
            }
            let (dl, dr) = if self.lookahead == 0 {
                (*l, *r)
            } else {
                let i = (self.frame % n as u64) as usize;
                let out = (self.delay_l[i], self.delay_r[i]);
                self.delay_l[i] = *l;
                self.delay_r[i] = *r;
                out
            };
            *l = (dl as f64 * self.gain) as f32;
            *r = (dr as f64 * self.gain) as f32;
            self.frame += 1;
        }
    }

    fn reset(&mut self) {
        self.gain = self.makeup;
        self.pending.clear();
        self.delay_l.fill(0.0);
        self.delay_r.fill(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_ramps_onto_the_ceiling() {
        let mut c = vec![0u8; 48];
        c[8..16].copy_from_slice(&(-10f64).to_le_bytes());
        c[16..24].copy_from_slice(&(-1f64).to_le_bytes());
        c[40..48].copy_from_slice(&10f64.powf(-15.0 / 20.0).to_le_bytes());
        let mut lim = Limiter::from_realimit(&c, 48000).unwrap();
        assert_eq!(lim.latency(), 96);
        let mut l: Vec<f32> = (0..48000).map(|i| if (1000..1100).contains(&i) { 1.0 } else { 0.1 }).collect();
        let mut r = l.clone();
        lim.process(
            &mut l,
            &mut r,
            &ProcessContext {
                midi: None,
                midi_from: 0,
                sample_rate: 48000,
                position: 0,
                playing: true,
                notes: None,
                live: &[],
                live_channel: None,
                aux: None,
                bpm: 120.0,
            },
        );
        let makeup = 10f64.powf(9.0 / 20.0) as f32;
        let ceiling = 10f64.powf(-1.0 / 20.0) as f32;
        assert!((l[1000] - 0.1 * makeup).abs() < 1e-6);
        // REAPER: the ramp starts 95 frames before the peak leaves the delay line.
        let step = l[1001] - l[1002];
        assert!(step > 0.0 && (l[1094] - l[1095] - step).abs() < 1e-6);
        assert!((l[1096] - ceiling).abs() < 1e-5);
        assert!(l[1096..1196].iter().all(|v| (v - ceiling).abs() < 1e-5));
        // Recovery at 15 dB/s after the last peak.
        let db = |v: f32| 20.0 * (v as f64 / (0.1 * makeup as f64)).log10();
        let (a, b) = (db(l[1196 + 4800]), db(l[1196 + 9600]));
        assert!((b - a - 1.5).abs() < 0.01, "{a} {b}");
    }
}
