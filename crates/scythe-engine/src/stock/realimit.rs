//! ReaLimit, from the settings it stores: `u32 version, u32 flags, f64 threshold dB, f64
//! ceiling dB, u32, u32, f64, f64 release`, the release stored as `10^(-dB per second / 20)`
//! (0 for "inf"). The fields between ceiling and release do not change the sound; bit 0 of the
//! flags (true peak) is not modelled.
//!
//! The model, measured on REAPER 7.73 renders (`examples/stock_ref.rs`): the audio is delayed by
//! 10 ms. Each frame needs the gain that puts it at the ceiling (times 0.999); the gain heads in
//! straight lines for the most demanding of the needs ahead, starting once the first of them is
//! 2 ms away when the limiter is idle and as soon as one enters the 10 ms window when it is not.
//! After a peak the gain climbs back to the make-up gain in a straight line over 10 ms. A slow
//! stage holds the gain reduction back: it follows the fast one in dB at four times the release
//! rate and falls at the release rate, and the deeper of the two applies.

use std::collections::VecDeque;

use crate::clap_host::ParamInfo;
use crate::fx::{Effect, ProcessContext};

/// The release REAPER uses for "inf dB/s".
const FASTEST_RELEASE: f64 = 150.0;

pub struct Limiter {
    chunk: Vec<u8>,
    threshold_db: f64,
    ceiling_db: f64,
    /// dB per second; `None` for "inf".
    release_db_s: Option<f64>,
    sample_rate: u32,
    makeup: f64,
    target: f64,
    /// Frames of lookahead (10 ms), and how close a need gets before an idle limiter moves (2 ms).
    lookahead: usize,
    trigger: usize,
    audio: Vec<[f32; 2]>,
    at: usize,
    /// Gains needed by frames still in the lookahead, by frame number.
    needs: VecDeque<(u64, f64)>,
    frame: u64,
    fast: f64,
    climb: f64,
    slow_db: f64,
}

fn f64_at(chunk: &[u8], at: usize) -> Option<f64> {
    chunk.get(at..at + 8).map(|b| f64::from_le_bytes(b.try_into().unwrap()))
}

impl Limiter {
    pub fn from_realimit(chunk: &[u8], sample_rate: u32) -> Option<Self> {
        let (threshold, ceiling) = (f64_at(chunk, 8)?, f64_at(chunk, 16)?);
        if !(threshold.is_finite() && ceiling.is_finite()) {
            return None;
        }
        let release = f64_at(chunk, 40).unwrap_or(10f64.powf(-15.0 / 20.0));
        let lookahead = (0.01 * sample_rate as f64).round() as usize;
        let mut lim = Self {
            chunk: chunk.to_vec(),
            threshold_db: threshold,
            ceiling_db: ceiling,
            release_db_s: (release > 0.0 && release < 1.0).then(|| -20.0 * release.log10()),
            sample_rate,
            makeup: 1.0,
            target: 1.0,
            lookahead,
            trigger: lookahead / 5,
            audio: vec![[0.0; 2]; lookahead],
            at: 0,
            needs: VecDeque::new(),
            frame: 0,
            fast: 1.0,
            climb: 0.0,
            slow_db: 0.0,
        };
        lim.update();
        lim.fast = lim.makeup;
        Some(lim)
    }

    fn update(&mut self) {
        self.makeup = 10f64.powf((self.ceiling_db - self.threshold_db) / 20.0);
        self.target = 10f64.powf(self.ceiling_db / 20.0) * 0.999;
        self.fast = self.fast.min(self.makeup);
    }

    fn release_db_s(&self) -> f64 {
        self.release_db_s.unwrap_or(FASTEST_RELEASE)
    }

    /// The release from REAPER's normalised value: 6 dB/s at 1, 6 dB more per halving, "inf" at 0.
    fn set_release_normalized(&mut self, v: f64) {
        self.release_db_s = (v > 0.0).then(|| 6.0 - 20.0 * v.min(1.0).log10());
    }

    fn gain(&mut self) -> f64 {
        let m = self.makeup;
        // The frame leaving the delay line now.
        let now = self.frame - self.lookahead as u64;
        while self.needs.front().is_some_and(|&(f, _)| f < now) {
            self.needs.pop_front();
        }
        let idle = self.fast >= m && self.needs.front().is_none_or(|&(f, _)| (f - now) as usize >= self.trigger);
        let mut best = if self.fast < m { (self.fast + self.climb).min(m) } else { m };
        let mut down = false;
        if !idle {
            for &(f, need) in &self.needs {
                let steps = (f - now) as f64 + 1.0;
                let line = self.fast + (need - self.fast) / steps;
                if line < best {
                    best = line;
                    down = line < self.fast;
                }
            }
        }
        if down {
            self.climb = (m - best) / self.lookahead as f64;
        }
        self.fast = best;
        let release = self.release_db_s() / self.sample_rate as f64;
        let fast_db = 20.0 * (m / self.fast).log10();
        self.slow_db = if fast_db > self.slow_db {
            fast_db.min(self.slow_db + 4.0 * release)
        } else {
            (self.slow_db - release).max(0.0)
        };
        self.fast.min(m * 10f64.powf(-self.slow_db / 20.0))
    }
}

impl Effect for Limiter {
    fn params(&mut self) -> Vec<ParamInfo> {
        use super::param;
        vec![
            param(0, "Threshold (dB)", -60.0, 12.0, 0.0, false),
            param(1, "Ceiling (dB)", -24.0, 0.0, 0.0, false),
            param(2, "Release (dB/s)", 6.0, FASTEST_RELEASE, 15.0, false),
        ]
    }

    fn param(&mut self, id: u32) -> Option<(f64, String)> {
        Some(match id {
            0 => (self.threshold_db, format!("{:+.2} dB", self.threshold_db)),
            1 => (self.ceiling_db, format!("{:+.2} dB", self.ceiling_db)),
            2 => match self.release_db_s {
                Some(db) => (db, format!("{db:.1} dB/s")),
                None => (FASTEST_RELEASE, "inf dB/s".into()),
            },
            _ => return None,
        })
    }

    fn set_param(&mut self, id: u32, value: f64) {
        match id {
            0 => self.threshold_db = value.clamp(-60.0, 12.0),
            1 => self.ceiling_db = value.clamp(-24.0, 0.0),
            2 => self.release_db_s = (value < FASTEST_RELEASE).then(|| value.max(6.0)),
            _ => return,
        }
        self.update();
    }

    fn set_normalized(&mut self, index: usize, v: f64) -> bool {
        use crate::normalized::linear;
        match index {
            0 => self.threshold_db = linear(v, -60.0, 12.0),
            1 => self.ceiling_db = linear(v, -24.0, 0.0),
            2 => self.set_release_normalized(v),
            _ => return false,
        }
        self.update();
        true
    }

    fn latency(&mut self) -> usize {
        self.lookahead
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        let mut chunk = self.chunk.clone();
        if chunk.len() < 48 {
            chunk.resize(48, 0);
            chunk[0..4].copy_from_slice(&3u32.to_le_bytes());
        }
        chunk[8..16].copy_from_slice(&self.threshold_db.to_le_bytes());
        chunk[16..24].copy_from_slice(&self.ceiling_db.to_le_bytes());
        let release = self.release_db_s.map_or(0.0, |db| 10f64.powf(-db / 20.0));
        chunk[40..48].copy_from_slice(&release.to_le_bytes());
        Some(chunk)
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], _ctx: &ProcessContext) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let peak = l.abs().max(r.abs()) as f64;
            if peak * self.makeup > self.target {
                self.needs.push_back((self.frame, self.target / peak));
            }
            // Until the delay line has filled, its output is silence.
            let gain = if self.frame >= self.lookahead as u64 { self.gain() } else { self.makeup };
            let [dl, dr] = std::mem::replace(&mut self.audio[self.at], [*l, *r]);
            self.at = (self.at + 1) % self.lookahead.max(1);
            *l = (dl as f64 * gain) as f32;
            *r = (dr as f64 * gain) as f32;
            self.frame += 1;
        }
    }

    fn reset(&mut self) {
        self.fast = self.makeup;
        self.climb = 0.0;
        self.slow_db = 0.0;
        self.needs.clear();
        self.audio.fill([0.0; 2]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stock::testing::context;

    fn chunk(threshold_db: f64, ceiling_db: f64, release_db_s: f64) -> Vec<u8> {
        let mut c = vec![0u8; 48];
        c[0..4].copy_from_slice(&3u32.to_le_bytes());
        c[8..16].copy_from_slice(&threshold_db.to_le_bytes());
        c[16..24].copy_from_slice(&ceiling_db.to_le_bytes());
        c[24..28].copy_from_slice(&2u32.to_le_bytes());
        c[40..48].copy_from_slice(&10f64.powf(-release_db_s / 20.0).to_le_bytes());
        c
    }

    /// A 1.0 peak over DC at 0.1, threshold -12 dB: REAPER 7.73 ramps the gain down over the 96
    /// frames before the peak onto 0.999, then back up linearly over 480 frames until the slow
    /// stage (0.80 dB charged at 60 dB/s) takes over.
    #[test]
    fn impulse_matches_reaper() {
        let mut lim = Limiter::from_realimit(&chunk(-12.0, 0.0, 15.0), 48000).unwrap();
        assert_eq!(lim.latency(), 480);
        let mut l: Vec<f32> = (0..20000).map(|i| if i == 5000 { 1.0 } else { 0.1 }).collect();
        let mut r = l.clone();
        lim.process(&mut l, &mut r, &context());
        let gain = |i: usize| l[i + 480] as f64 / if i == 5000 { 1.0 } else { 0.1 };
        for (i, want) in [(4904, 3.981061), (4905, 3.949998), (4950, 2.552156), (5000, 0.998999), (5100, 1.620262)] {
            assert!((gain(i) - want).abs() < 2e-5, "{i}: {} vs {want}", gain(i));
        }
        for (i, want) in [(5480, 3.695346), (6000, 3.765128)] {
            assert!((gain(i) - want).abs() < 2e-4, "{i}: {} vs {want}", gain(i));
        }
    }

    #[test]
    fn state_round_trips() {
        let c = chunk(-6.0, -1.0, 20.0);
        let mut lim = Limiter::from_realimit(&c, 48000).unwrap();
        let saved = lim.save_state().unwrap();
        assert_eq!(&saved[..40], &c[..40]);
        assert!((f64_at(&saved, 40).unwrap() - f64_at(&c, 40).unwrap()).abs() < 1e-12);
    }
}
