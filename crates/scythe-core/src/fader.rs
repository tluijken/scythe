//! REAPER's fader law: slider positions 0..1000 against dB, with 0 dB at 716 and +12 dB at the
//! top. Used so faders feel and read the same as in REAPER.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slider_mapping_round_trips() {
        for db in [-200.0, -150.0, -45.0, -6.0, 0.0, 7.5, 12.0] {
            assert!((slider_to_db(db_to_slider(db)) - db).abs() < 1e-9, "{db}");
        }
        assert_eq!(db_to_slider(0.0), 716.0);
        assert_eq!(slider_to_db(1000.0), 12.0);
    }
}
