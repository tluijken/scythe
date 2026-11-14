//! Level detection and the gain computer shared by dynamic and spectral bands.
//!
//! A band moves from its static gain towards `gain + range` while its detector is above the
//! threshold: by half the overshoot in dB (a 2:1 ratio), with a 6 dB soft knee, never past the
//! range. With auto threshold the threshold follows the detector's own average level (1.5 s
//! time constant), so the band reacts to what stands out rather than to the overall level.

use super::filters::{Cascade, CascadeState, Design, Prototype, audition_design};

pub const KNEE_DB: f64 = 6.0;
const AUTO_SECONDS: f64 = 1.5;
pub const FLOOR_DB: f64 = -120.0;

/// The gain change in dB for a detector `over` dB above the threshold.
pub fn gain_change(over: f64, range: f64) -> f64 {
    let half = KNEE_DB / 2.0;
    let x = if over <= -half {
        0.0
    } else if over < half {
        (over + half).powi(2) / (2.0 * KNEE_DB)
    } else {
        over
    };
    let amount = (x / 2.0).min(range.abs());
    amount * range.signum()
}

/// One-pole smoothing coefficients for attack and release at a sample rate.
#[derive(Debug, Clone, Copy, Default)]
pub struct Ballistics {
    pub attack: f64,
    pub release: f64,
}

impl Ballistics {
    pub fn new(attack_ms: f64, release_ms: f64, rate: f64) -> Self {
        let coef = |ms: f64| (-1.0 / (ms.max(0.01) * 0.001 * rate)).exp();
        Self { attack: coef(attack_ms), release: coef(release_ms) }
    }
}

/// A dynamic band's detector: the band's region filtered out of the detector signal, its
/// mean square followed with attack and release, and the resulting gain change.
#[derive(Debug, Clone, Default)]
pub struct Detector {
    filter: Cascade,
    state: CascadeState,
    design: Option<Design>,
    env: f64,
    average_db: f64,
    gain_db: f64,
    level_db: f64,
}

impl Detector {
    pub fn retune(&mut self, d: &Design, fs: f64) {
        let ad = audition_design(d);
        if self
            .design
            .as_ref()
            .is_some_and(|old| (old.freq - ad.freq).abs() < 1e-6 && old.q == ad.q && old.shape == ad.shape)
        {
            return;
        }
        self.filter = Cascade::matched(&Prototype::design(&ad), fs);
        self.design = Some(ad);
    }

    /// The band-filtered detector sample, for auditioning.
    #[inline]
    pub fn filter(&mut self, x: f64) -> f64 {
        self.state.run(&self.filter, x)
    }

    /// Runs a block of detector samples; returns the gain change in dB at its end.
    pub fn run(&mut self, input: &[f64], b: Ballistics, threshold: f64, auto: bool, range: f64, fs: f64) -> f64 {
        self.feed(input, b, fs);
        self.settle(threshold, auto, range)
    }

    /// Follows detector samples without ending the block, so a block may arrive in pieces.
    pub fn feed(&mut self, input: &[f64], b: Ballistics, fs: f64) {
        let avg = (-1.0 / (AUTO_SECONDS * fs)).exp();
        let mut avg_db = self.average_db;
        for &x in input {
            let y = self.state.run(&self.filter, x);
            let sq = 2.0 * y * y;
            let c = if sq > self.env { b.attack } else { b.release };
            self.env = c * self.env + (1.0 - c) * sq;
            let lvl = 10.0 * self.env.max(1e-24).log10();
            if lvl > -90.0 {
                avg_db = avg * avg_db + (1.0 - avg) * lvl;
            }
        }
        self.average_db = avg_db;
    }

    /// Ends a block: returns the gain change in dB at its end.
    pub fn settle(&mut self, threshold: f64, auto: bool, range: f64) -> f64 {
        self.state.flush();
        if self.env < 1e-30 {
            self.env = 0.0;
        }
        self.level_db = (10.0 * self.env.max(1e-24).log10()).max(FLOOR_DB);
        let thr = if auto { self.average_db.max(-80.0) } else { threshold };
        self.gain_db = gain_change(self.level_db - thr, range);
        self.gain_db
    }

    pub fn gain_db(&self) -> f64 {
        self.gain_db
    }

    pub fn level_db(&self) -> f64 {
        self.level_db
    }

    /// The threshold auto threshold uses right now.
    pub fn auto_threshold(&self) -> f64 {
        self.average_db.max(-80.0)
    }

    pub fn reset(&mut self) {
        self.state.reset();
        self.env = 0.0;
        self.gain_db = 0.0;
        self.level_db = FLOOR_DB;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scythe_eq::filters::Shape;

    #[test]
    fn gain_computer_has_a_soft_knee_and_stops_at_the_range() {
        assert_eq!(gain_change(-10.0, -6.0), 0.0);
        assert!(gain_change(0.0, -6.0) < 0.0 && gain_change(0.0, -6.0) > -1.0);
        assert!((gain_change(8.0, -6.0) + 4.0).abs() < 1e-12);
        assert_eq!(gain_change(40.0, -6.0), -6.0);
        assert_eq!(gain_change(40.0, 9.0), 9.0);
    }

    /// A 1 kHz burst at -6 dBFS pulls a -20 dB threshold band down by its full range; quiet
    /// passages leave it alone, and the release brings it back.
    #[test]
    fn detector_follows_bursts_of_its_band_only() {
        let fs = 48000.0;
        let d = Design { shape: Shape::Bell, freq: 1000.0, gain_db: 0.0, q: 2.0, slope: 1, gain_q: false };
        let mut det = Detector::default();
        det.retune(&d, fs);
        let b = Ballistics::new(5.0, 50.0, fs);
        let tone = |f: f64, amp: f64, n: usize| -> Vec<f64> {
            (0..n).map(|i| amp * (2.0 * std::f64::consts::PI * f * i as f64 / fs).sin()).collect()
        };
        let g = det.run(&tone(1000.0, 0.5, 9600), b, -20.0, false, -6.0, fs);
        assert!((g + 6.0).abs() < 1e-9, "{g}");
        det.reset();
        let g = det.run(&tone(8000.0, 0.5, 9600), b, -20.0, false, -6.0, fs);
        assert!(g.abs() < 1e-6, "out of band: {g}");
        det.run(&tone(1000.0, 0.5, 9600), b, -20.0, false, -6.0, fs);
        let g = det.run(&vec![0.0; 24000], b, -20.0, false, -6.0, fs);
        assert!(g.abs() < 1e-6, "released: {g}");
    }

    #[test]
    fn auto_threshold_tracks_the_average_level() {
        let fs = 48000.0;
        let d = Design { shape: Shape::Bell, freq: 1000.0, gain_db: 0.0, q: 1.0, slope: 1, gain_q: false };
        let mut det = Detector::default();
        det.retune(&d, fs);
        let b = Ballistics::new(5.0, 50.0, fs);
        let steady: Vec<f64> =
            (0..fs as usize * 8).map(|i| 0.1 * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / fs).sin()).collect();
        let g = det.run(&steady, b, 0.0, true, -6.0, fs);
        // The detector reads a sine slightly hot: it rises faster than it falls.
        assert!((det.auto_threshold() + 20.0).abs() < 3.0, "{}", det.auto_threshold());
        assert!(g > -1.6, "a steady tone sits at its own average: {g}");
    }
}
