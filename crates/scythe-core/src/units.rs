pub const MINUS_INF_DB: f64 = -150.0;

pub fn db_to_gain(db: f64) -> f64 {
    if db <= MINUS_INF_DB { 0.0 } else { 10f64.powf(db / 20.0) }
}

pub fn gain_to_db(gain: f64) -> f64 {
    if gain <= 0.0 { MINUS_INF_DB } else { (20.0 * gain.log10()).max(MINUS_INF_DB) }
}

/// Formats a gain the way mixers usually show it: `-3.4`, `+1.0`, `-inf`.
pub fn format_db(gain: f64) -> String {
    let db = gain_to_db(gain);
    if db <= -100.0 {
        "-inf".to_owned()
    } else if db.abs() < 0.05 {
        "0.0".to_owned()
    } else {
        format!("{db:+.1}")
    }
}

/// `pan` in -1.0..=1.0 rendered as `C`, `L80`, `R10`.
pub fn format_pan(pan: f64) -> String {
    let p = (pan * 100.0).round() as i32;
    match p {
        0 => "C".to_owned(),
        p if p < 0 => format!("L{}", -p),
        p => format!("R{p}"),
    }
}

/// Constant-power pan law: -3 dB per side at center, unity on the hard-panned side.
pub fn pan_gains(pan: f64) -> (f64, f64) {
    let angle = (pan.clamp(-1.0, 1.0) + 1.0) * std::f64::consts::FRAC_PI_4;
    (angle.cos(), angle.sin())
}

/// How the attenuated side falls off while panning away from it (RPP `PANLAWFLAGS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanTaper {
    Sine,
    Hybrid,
    Linear,
}

impl PanTaper {
    pub fn from_flags(flags: i64) -> Self {
        match flags {
            0 => Self::Sine,
            2 => Self::Linear,
            _ => Self::Hybrid,
        }
    }

    pub fn flags(self) -> i64 {
        match self {
            Self::Sine => 0,
            Self::Hybrid => 3,
            Self::Linear => 2,
        }
    }
}

/// Resolved pan law: the gain at centre relative to a hard-panned side (`PANLAW` / `D_PANLAW`:
/// 1 = 0 dB, 0.707 = -3 dB, 0.5 = -6 dB; values above 1 keep the centre at unity and boost the
/// sides instead) plus the taper.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PanLaw {
    pub law: f64,
    pub taper: PanTaper,
}

impl Default for PanLaw {
    fn default() -> Self {
        Self { law: 1.0, taper: PanTaper::Hybrid }
    }
}

impl PanLaw {
    /// Gains for a balance pan of a stereo (or mono-on-stereo) signal, matching REAPER's
    /// renders: linear tapers interpolate between the centre gain and the full side; sine
    /// tapers follow the constant-power curve; values above 1 keep the centre at unity and
    /// boost the sides. Laws of -6 dB and below are always linear, and a 0 dB law with the
    /// hybrid or linear taper is a plain balance control.
    ///
    /// Exact against REAPER for the linear taper at any law and for the sine and hybrid tapers
    /// at 0, -3 and -6 dB (and their compensated variants). Other sine/hybrid laws are
    /// interpolated and can be off by a few percent.
    pub fn gains(self, pan: f64) -> (f64, f64) {
        let pan = pan.clamp(-1.0, 1.0);
        let law = if self.law > 0.0 { self.law } else { 1.0 };
        let (centre, scale) = if law > 1.0 { (1.0 / law, law) } else { (law, 1.0) };
        let a = pan.abs();
        let linear = (centre * (1.0 - a), centre + (1.0 - centre) * a);
        let db = 20.0 * centre.log10();
        let sine = {
            let angle = (1.0 + a) * std::f64::consts::FRAC_PI_4;
            // Above -3 dB REAPER pulls the constant-power curve down by the same factor on both
            // sides, reaching unity on the hard-panned side; the dB amount scales with the law.
            // The table is measured at a 0 dB law.
            const DROP_DB: [f64; 5] = [0.0, 0.122, 0.523, 1.34, 3.0103];
            let pos = a * 4.0;
            let i = (pos as usize).min(3);
            let drop = DROP_DB[i] + (DROP_DB[i + 1] - DROP_DB[i]) * (pos - i as f64);
            let scale = ((db + 3.0103) / 3.0103).max(0.0);
            let k = centre * std::f64::consts::SQRT_2 * 10f64.powf(-drop * scale / 20.0);
            (k * angle.cos(), k * angle.sin())
        };
        // How far the centre gain is from -3 dB towards 0 dB or -6 dB, where tapers turn linear.
        let towards_linear = match self.taper {
            PanTaper::Linear => 1.0,
            PanTaper::Sine if db < -3.0103 => ((-3.0103 - db) / 3.0103).min(1.0),
            PanTaper::Sine => 0.0,
            PanTaper::Hybrid => ((db + 3.0103).abs() / 3.0103).min(1.0),
        };
        let mix = |l: f64, s: f64| s + (l - s) * towards_linear;
        let away = mix(linear.0, sine.0);
        let toward = mix(linear.1, sine.1);
        let (l, r) = if pan < 0.0 { (toward, away) } else { (away, toward) };
        (l * scale, r * scale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_round_trip() {
        for db in [-60.0, -12.0, -3.0, 0.0, 6.0] {
            assert!((gain_to_db(db_to_gain(db)) - db).abs() < 1e-9);
        }
        assert_eq!(db_to_gain(MINUS_INF_DB), 0.0);
    }

    #[test]
    fn pan_law_is_constant_power() {
        for pan in [-1.0, -0.5, 0.0, 0.3, 1.0] {
            let (l, r) = pan_gains(pan);
            assert!((l * l + r * r - 1.0).abs() < 1e-9, "pan {pan}: {l} {r}");
        }
        let (l, r) = pan_gains(-1.0);
        assert!((l - 1.0).abs() < 1e-9 && r.abs() < 1e-9);
    }

    /// Values measured from REAPER 7.73 renders (`examples/reaper_ref.rs` in scythe-engine).
    #[test]
    fn pan_laws_match_reaper() {
        let cases: &[(f64, i64, f64, (f64, f64))] = &[
            (1.0, 3, 0.5, (0.5, 1.0)),
            (1.0, 2, 0.75, (0.25, 1.0)),
            (0.707, 0, 0.25, (0.5554, 0.8314)),
            (0.707, 3, 0.5, (0.3826, 0.9238)),
            (0.707, 2, 0.5, (0.3536, 0.8536)),
            (0.841, 2, 0.25, (0.6308, 0.8808)),
            (0.5, 0, 0.25, (0.375, 0.625)),
            (0.5, 3, 0.75, (0.125, 0.875)),
            (1.414, 3, 0.5, (0.5412, 1.3064)),
            (1.414, 2, 0.75, (0.25, 1.3106)),
            (2.0, 0, 0.5, (0.5, 1.5)),
        ];
        for &(law, flags, pan, (l, r)) in cases {
            let (gl, gr) = PanLaw { law, taper: PanTaper::from_flags(flags) }.gains(pan);
            assert!((gl - l).abs() < 2e-3 && (gr - r).abs() < 2e-3, "law {law} flags {flags} pan {pan}: {gl} {gr}");
            let (ml, mr) = PanLaw { law, taper: PanTaper::from_flags(flags) }.gains(-pan);
            assert_eq!((ml, mr), (gr, gl));
        }
    }

    #[test]
    fn formatting() {
        assert_eq!(format_pan(0.0), "C");
        assert_eq!(format_pan(-0.8), "L80");
        assert_eq!(format_pan(0.1), "R10");
        assert_eq!(format_db(1.0), "0.0");
        assert_eq!(format_db(0.0), "-inf");
        assert_eq!(format_db(db_to_gain(-3.4)), "-3.4");
    }
}
