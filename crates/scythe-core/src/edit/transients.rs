//! Transient detection for tab-to-transient and dynamic split, with REAPER's two settings
//! (`transientsensitivity`, default 0.5, and `transientthreshold`, default -24 dB).
//!
//! A clean-room detector tuned against REAPER 7.73's tab-to-transient positions on recorded
//! drums: a transient starts where the 1 ms peak level, above the threshold, rises by more
//! than the sensitivity's step over the loudest level of the 40 ms before it, at the first
//! sample past halfway (in dB) between the two. Where it agrees with REAPER it agrees to the
//! sample (median 0.04 ms); it finds about four in five of REAPER's transients (docs/verification.md
//! has the numbers).

/// The detector's settings, as REAPER's "Transient detection settings" name them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransientSettings {
    /// 0 to 1: higher finds softer transients.
    pub sensitivity: f64,
    /// Level in dB a transient must reach.
    pub threshold_db: f64,
}

impl Default for TransientSettings {
    fn default() -> Self {
        Self { sensitivity: 0.5, threshold_db: -24.0 }
    }
}

/// The shortest gap between two transients, in seconds.
const MIN_GAP: f64 = 0.005;
/// How far back the level a transient rises from is taken, in seconds.
const LOOKBACK: f64 = 0.04;

impl TransientSettings {
    /// The rise in dB over the level before that counts as a transient.
    fn rise_db(&self) -> f64 {
        RISE_AT_HALF * (RISE_SLOPE * (0.5 - self.sensitivity.clamp(0.0, 1.0))).exp()
    }
}

const RISE_AT_HALF: f64 = 0.7;
const RISE_SLOPE: f64 = 4.2;

/// The transients in `channels` (planar, at `rate`), in seconds from the first sample.
pub fn detect(channels: &[Vec<f32>], rate: u32, settings: TransientSettings) -> Vec<f64> {
    let frames = channels.iter().map(Vec::len).min().unwrap_or(0);
    let n = channels.len().max(1) as f64;
    let mono = |i: usize| -> f64 { channels.iter().map(|c| f64::from(c[i])).sum::<f64>() / n };
    let hop = (rate as usize / 1000).max(1);
    let blocks = frames / hop;
    let level: Vec<f64> = (0..blocks)
        .map(|b| {
            let peak = (b * hop..(b + 1) * hop).map(|i| mono(i).abs()).fold(0.0, f64::max);
            20.0 * peak.max(1e-9).log10()
        })
        .collect();
    let back = ((LOOKBACK * rate as f64) as usize / hop).max(1);
    let rise = settings.rise_db();
    let mut out: Vec<f64> = Vec::new();
    for b in back..blocks {
        let before = level[b - back..b].iter().copied().fold(f64::NEG_INFINITY, f64::max);
        if level[b] <= settings.threshold_db || level[b] - before <= rise {
            continue;
        }
        let limit = 10f64.powf((before + 0.5 * rise) / 20.0);
        let from = (b - 1) * hop;
        let at = (from..(b + 1) * hop).find(|&i| mono(i).abs() > limit).unwrap_or(b * hop);
        let t = at as f64 / rate as f64;
        if out.last().is_none_or(|&last| t - last >= MIN_GAP) {
            out.push(t);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_hits_after_silence() {
        let rate = 48_000;
        let mut s = vec![0.0f32; rate as usize];
        for hit in [0.2, 0.5, 0.8] {
            let at = (hit * rate as f64) as usize;
            for i in 0..2400 {
                s[at + i] = 0.8 * (-(i as f32) / 600.0).exp() * if i % 2 == 0 { 1.0 } else { -1.0 };
            }
        }
        let found = detect(&[s.clone()], rate, TransientSettings::default());
        assert_eq!(found.len(), 3, "{found:?}");
        for (f, want) in found.iter().zip([0.2, 0.5, 0.8]) {
            assert!((f - want).abs() < 1e-4, "{f}");
        }
        let quiet: Vec<f32> = s.iter().map(|v| v * 0.01).collect();
        assert!(detect(&[quiet], rate, TransientSettings::default()).is_empty());
    }
}
