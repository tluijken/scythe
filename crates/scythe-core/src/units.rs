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
    /// renders. Laws above 1 keep the centre at unity and scale everything up.
    ///
    /// The linear taper runs straight from the centre gain to 0 and 1. The sine taper is the
    /// constant-power curve through the centre gain, blended towards the one that ends at
    /// unity by `(1/cos(a pi/4) - 1) / (sqrt 2 - 1)` of the pan amount `a`; both are exact
    /// for every law. The hybrid taper equals the sine taper from -3 dB down and follows sine
    /// arcs above it (see `hybrid`).
    pub fn gains(self, pan: f64) -> (f64, f64) {
        use std::f64::consts::{FRAC_PI_4, SQRT_2};
        let pan = pan.clamp(-1.0, 1.0);
        let law = if self.law > 0.0 { self.law } else { 1.0 };
        let (centre, scale) = if law > 1.0 { (1.0 / law, law) } else { (law, 1.0) };
        let a = pan.abs();
        let linear = (centre * (1.0 - a), centre + (1.0 - centre) * a);
        let sine = {
            let angle = (1.0 + a) * FRAC_PI_4;
            let towards_unity = (1.0 / (a * FRAC_PI_4).cos() - 1.0) / (SQRT_2 - 1.0);
            let k = SQRT_2 * centre * (1.0 + (1.0 / (SQRT_2 * centre) - 1.0) * towards_unity);
            (k * angle.cos(), k * angle.sin())
        };
        let (away, toward) = match self.taper {
            // REAPER special-cases a centre of exactly -6 dB; 0.5012 already takes the curve.
            _ if (centre - 0.5).abs() < 0.00119 => linear,
            PanTaper::Linear => linear,
            PanTaper::Sine => sine,
            PanTaper::Hybrid if centre <= std::f64::consts::FRAC_1_SQRT_2 + 1e-4 => sine,
            PanTaper::Hybrid => hybrid(centre, a),
        };
        let (l, r) = if pan < 0.0 { (toward, away) } else { (away, toward) };
        (l * scale, r * scale)
    }
}

/// The hybrid taper above a centre of `sin 1`, measured from REAPER renders: per centre gain,
/// the pan amount at which the near side reaches unity, and how much slower the far side's arc
/// runs (the far side is the balance line `1 - a` times that slowed arc).
const HYBRID_MEASURED: [(f64, f64, f64); 16] = [
    (0.8414, 0.5713, 1.7760),
    (0.8511, 0.5368, 1.7228),
    (0.8610, 0.5016, 1.6753),
    (0.8710, 0.4659, 1.6333),
    (0.8810, 0.4300, 1.5976),
    (0.8912, 0.3934, 1.5629),
    (0.9016, 0.3563, 1.5308),
    (0.9120, 0.3191, 1.5003),
    (0.9226, 0.2813, 1.4725),
    (0.9333, 0.2427, 1.4473),
    (0.9441, 0.2036, 1.4248),
    (0.9550, 0.1644, 1.4018),
    (0.9660, 0.1253, 1.3717),
    (0.9772, 0.0861, 1.3429),
    (0.9886, 0.0469, 1.2341),
    (1.0, 0.0, 1.2341),
];

/// Hybrid taper gains `(far, near)` above -3 dB. Both sides are sine arcs starting at the
/// centre gain. Up to a centre of `sin 1` (-1.5 dB) the near side reaches unity at
/// `a = acos(c) / asin(c)` and the far side turns at `asin(c)` per unit of pan, held to the
/// balance line; above it both follow the measured table.
fn hybrid(centre: f64, a: f64) -> (f64, f64) {
    use std::f64::consts::FRAC_PI_2;
    let start = centre.asin();
    if centre <= HYBRID_MEASURED[0].0 {
        let unity = centre.acos() / start;
        let near = (start + (FRAC_PI_2 - start) * (a / unity).min(1.0)).sin();
        let far = (centre.acos() + start * a).min(FRAC_PI_2).cos().min(1.0 - a);
        return (far, near);
    }
    let i = HYBRID_MEASURED.partition_point(|p| p.0 < centre).clamp(1, HYBRID_MEASURED.len() - 1);
    let (lo, hi) = (HYBRID_MEASURED[i - 1], HYBRID_MEASURED[i]);
    let k = (centre - lo.0) / (hi.0 - lo.0);
    let (unity, slow) = (lo.1 + (hi.1 - lo.1) * k, lo.2 + (hi.2 - lo.2) * k);
    let arc = |speed: f64| (start + (FRAC_PI_2 - start) * (a * speed).min(1.0)).sin();
    if unity <= 0.0 {
        return (1.0 - a, 1.0);
    }
    ((1.0 - a) * arc(1.0 / (unity * slow)), arc(1.0 / unity))
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
            (0.8414, 0, 0.5, (0.4409, 1.0644)),
            (0.8414, 0, 0.9, (0.082, 1.0422)),
            (0.5957, 0, 0.9, (0.0755, 0.9593)),
            (0.5957, 3, 0.5, (0.3344, 0.8073)),
            (1.5849, 0, 0.5, (0.5542, 1.3379)),
            (1.5849, 0, 0.9, (0.1211, 1.5393)),
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
