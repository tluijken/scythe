//! REAPER's fader law: slider positions 0..1000 against dB, with 0 dB at 716 and +12 dB at the
//! top. Used so faders feel and read the same as in REAPER. Also the pan modes' gains.

use crate::units::PanLaw;

/// REAPER's fader slider positions (0..1000, 0 dB at 716) for a range of dB values, measured
/// with `reaper.DB2SLIDER` in REAPER 7.73.
const SLIDER_TABLE: [(f64, f64); 15] = [
    (-150.0, 2.513_872_979_395_647),
    (-120.0, 7.9495630678164),
    (-90.0, 25.138596787751663),
    (-60.0, 79.482_342_854_381_86),
    (-40.0, 170.9837428707412),
    (-30.0, 250.09007842261482),
    (-20.0, 363.27144175838055),
    (-12.0, 483.9496089500646),
    (-6.0, 592.848_335_994_545_2),
    (-3.0, 652.838_835_595_524_4),
    (0.0, 716.0),
    (3.0, 782.758_923_678_708),
    (6.0, 852.286_980_303_257_2),
    (12.0, 1000.0),
    (24.0, 1332.4540140734498),
];

/// dB to REAPER fader slider position; interpolated between measured points, following the
/// measured cube-root-of-amplitude slope below -150 dB.
pub fn db_to_slider(db: f64) -> f64 {
    let (lo_db, lo_s) = SLIDER_TABLE[0];
    if db <= lo_db {
        return lo_s * 10f64.powf((db - lo_db) / 60.0);
    }
    let i = SLIDER_TABLE.partition_point(|&(d, _)| d <= db).min(SLIDER_TABLE.len() - 1);
    let (d0, s0) = SLIDER_TABLE[i - 1];
    let (d1, s1) = SLIDER_TABLE[i];
    s0 + (s1 - s0) * (db - d0) / (d1 - d0)
}

pub fn slider_to_db(slider: f64) -> f64 {
    let (lo_db, lo_s) = SLIDER_TABLE[0];
    if slider <= 0.0 {
        return -1000.0;
    }
    if slider <= lo_s {
        return lo_db + 60.0 * (slider / lo_s).log10();
    }
    let i = SLIDER_TABLE.partition_point(|&(_, s)| s <= slider).min(SLIDER_TABLE.len() - 1);
    let (d0, s0) = SLIDER_TABLE[i - 1];
    let (d1, s1) = SLIDER_TABLE[i];
    d0 + (d1 - d0) * (slider - s0) / (s1 - s0)
}

/// REAPER's fader scaling of volume envelopes (`VOLTYPE 1`, `ScaleToEnvelopeMode`):
/// `gain = 10^0.6 / 2 * x * (1 + x)` with `x` the cube of the position over 1000, fitted to
/// `ScaleToEnvelopeMode` in REAPER 7.73 (exact to 1e-9 from -150 to +24 dB). Such envelopes
/// interpolate linearly in position.
const ENVELOPE_GAIN: f64 = 1.990_535_852_767_486;

/// Gain to fader-scaled envelope position (0..1000, +12 dB at 1000).
pub fn envelope_position(gain: f64) -> f64 {
    if gain <= 0.0 {
        return 0.0;
    }
    let x = ((1.0 + 4.0 * gain / ENVELOPE_GAIN).sqrt() - 1.0) / 2.0;
    1000.0 * x.cbrt()
}

pub fn envelope_gain(position: f64) -> f64 {
    if position <= 0.0 {
        return 0.0;
    }
    let x = (position / 1000.0).powi(3);
    ENVELOPE_GAIN * x * (1.0 + x)
}

/// How a track pans its pair (`PANMODE`, `I_PANMODE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanMode {
    /// REAPER 3.x balance (0): the near side comes up towards unity as the far side fades.
    Classic,
    /// Stereo balance / mono pan (3), REAPER's default.
    #[default]
    Balance,
    /// Stereo pan (5): the pair narrows and moves as a whole.
    Stereo,
    /// Dual pan (6): each channel is panned on its own.
    Dual,
}

impl PanMode {
    pub const ALL: [PanMode; 4] = [Self::Balance, Self::Classic, Self::Stereo, Self::Dual];

    pub fn from_code(code: i64) -> Self {
        match code {
            0 => Self::Classic,
            5 => Self::Stereo,
            6 => Self::Dual,
            _ => Self::Balance,
        }
    }

    pub fn code(self) -> i64 {
        match self {
            Self::Classic => 0,
            Self::Balance => 3,
            Self::Stereo => 5,
            Self::Dual => 6,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Classic => "Stereo balance (3.x)",
            Self::Balance => "Stereo balance / mono pan",
            Self::Stereo => "Stereo pan",
            Self::Dual => "Dual pan",
        }
    }

    /// Short name for the API.
    pub fn name(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Balance => "balance",
            Self::Stereo => "stereo",
            Self::Dual => "dual",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name().eq_ignore_ascii_case(name))
    }
}

/// A track's pan controls under one pan law; [`Panner::matrix`] turns them into gains.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Panner {
    pub mode: PanMode,
    pub pan: f64,
    pub width: f64,
    /// Left and right channel positions in dual pan.
    pub dual: [f64; 2],
    pub law: PanLaw,
}

impl Panner {
    /// The 2x2 gain matrix `[l<-l, l<-r, r<-l, r<-r]`, measured against REAPER 7.73 renders.
    /// Every mode but dual pan first narrows the pair by the width (mid/side), then:
    /// - balance attenuates one side with the pan law's gains;
    /// - 3.x balance brings the near side from the centre gain `c` up to unity along
    ///   `c + (1 - c) sqrt(a / (2 - a))` for pan amount `a`, and the far side is that times
    ///   `1 - a`;
    /// - stereo pan keeps the far channel at its balance gain and moves a share of it into
    ///   the near side: `a / 2` at a 0 dB law, `a / (1 + a)` at any other;
    /// - dual pan pans each channel as a mono source.
    pub fn matrix(&self) -> [f64; 4] {
        let pan = self.pan.clamp(-1.0, 1.0);
        let [a, b, c, d] = match self.mode {
            PanMode::Dual => {
                let (ll, lr) = self.law.gains(self.dual[0]);
                let (rl, rr) = self.law.gains(self.dual[1]);
                return [ll, rl, lr, rr];
            }
            PanMode::Balance => {
                let (l, r) = self.law.gains(pan);
                [l, 0.0, 0.0, r]
            }
            PanMode::Classic => {
                let law = if self.law.law > 0.0 { self.law.law } else { 1.0 };
                let (centre, scale) = if law > 1.0 { (1.0 / law, law) } else { (law, 1.0) };
                let x = pan.abs();
                let near = scale * (centre + (1.0 - centre) * (x / (2.0 - x)).sqrt());
                let far = near * (1.0 - x);
                if pan < 0.0 { [near, 0.0, 0.0, far] } else { [far, 0.0, 0.0, near] }
            }
            PanMode::Stereo => {
                let (l, r) = self.law.gains(pan);
                let x = pan.abs();
                let unity = self.law.law <= 0.0 || (self.law.law - 1.0).abs() < 1e-9;
                let share = if unity { x / 2.0 } else { x / (1.0 + x) };
                if pan < 0.0 { [l * (1.0 - share), l * share, 0.0, r] } else { [l, 0.0, r * share, r * (1.0 - share)] }
            }
        };
        let w = self.width.clamp(-1.0, 1.0);
        let (direct, cross) = ((1.0 + w) / 2.0, (1.0 - w) / 2.0);
        [a * direct + b * cross, a * cross + b * direct, c * direct + d * cross, c * cross + d * direct]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panner(mode: PanMode, law: f64, pan: f64, width: f64) -> Panner {
        Panner { mode, pan, width, dual: [-0.3, 0.6], law: PanLaw { law, ..PanLaw::default() } }
    }

    // Read from REAPER 7.73 renders of a left-only, then right-only source.
    #[test]
    fn pan_modes_match_reaper() {
        let cases = [
            (PanMode::Stereo, 1.0, 0.4, 1.0, [0.6, 0.0, 0.2, 0.8]),
            (PanMode::Stereo, 1.0, -0.6, 1.0, [0.7, 0.3, 0.0, 0.4]),
            (PanMode::Stereo, 1.0, 0.5, 0.8, [0.45, 0.05, 0.3, 0.7]),
            (PanMode::Stereo, 1.0, 0.5, -1.0, [0.0, 0.5, 0.75, 0.25]),
            (PanMode::Stereo, 0.5, 0.4, 0.5, [0.225, 0.075, 0.275, 0.425]),
            (PanMode::Stereo, 0.5, 0.8, 1.0, [0.1, 0.0, 0.4, 0.5]),
            (PanMode::Stereo, 2.0, 0.4, 0.5, [0.45, 0.15, 0.55, 0.85]),
            (PanMode::Classic, 0.5, 0.25, 1.0, [0.516737, 0.0, 0.0, 0.688982]),
            (PanMode::Classic, 0.25, 0.6, 1.0, [0.296396, 0.0, 0.0, 0.740990]),
            (PanMode::Classic, 1.4125, -0.4, 1.0, [1.20625, 0.0, 0.0, 0.72375]),
            (PanMode::Classic, 0.5, 0.4, 0.5, [0.3375, 0.1125, 0.1875, 0.5625]),
            (PanMode::Balance, 1.0, 0.3, 0.5, [0.525, 0.175, 0.25, 0.75]),
            (PanMode::Dual, 0.5, 0.0, 0.5, [0.65, 0.2, 0.35, 0.8]),
            (PanMode::Dual, 2.0, 0.0, 0.5, [1.3, 0.4, 0.7, 1.6]),
        ];
        for (mode, law, pan, width, want) in cases {
            let got = panner(mode, law, pan, width).matrix();
            let close = got.iter().zip(want).all(|(x, y)| (x - y).abs() < 2e-5);
            assert!(close, "{mode:?} law {law} pan {pan} width {width}: {got:?} vs {want:?}");
        }
    }

    #[test]
    fn slider_mapping_round_trips() {
        for db in [-200.0, -150.0, -45.0, -6.0, 0.0, 7.5, 12.0] {
            assert!((slider_to_db(db_to_slider(db)) - db).abs() < 1e-9, "{db}");
        }
        assert_eq!(db_to_slider(0.0), 716.0);
        assert_eq!(slider_to_db(1000.0), 12.0);
    }
}
