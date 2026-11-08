//! How REAPER maps a normalised (0..=1) parameter value of its own effects to the value they
//! show, as measured with `TrackFX_SetParamNormalized` / `TrackFX_GetFormattedParamValue`.
//! Parameter envelopes on those effects carry normalised values.

/// Gains shown in dB that are an amplitude of `2v` (0 dB in the middle, +6 dB at the top):
/// thresholds, wet/dry, volumes and feedback.
pub fn amp2_db(v: f64) -> f64 {
    db(2.0 * v)
}

/// ReaEQ band and global gain: `2v` up to the middle, then rising linearly to 4 (+12 dB).
pub fn eq_gain_db(v: f64) -> f64 {
    db(if v <= 0.5 { 2.0 * v } else { 1.0 + 6.0 * (v - 0.5) })
}

/// ReaEQ frequency: an exponential from 20 Hz to 24 kHz through 1160.5 Hz in the middle.
pub fn eq_freq(v: f64) -> f64 {
    // e^(b/2) = 20.026 puts the middle at 1160.5 Hz.
    let b = 2.0 * 20.026f64.ln();
    20.0 + 23980.0 * ((b * v).exp() - 1.0) / (b.exp() - 1.0)
}

pub fn linear(v: f64, lo: f64, hi: f64) -> f64 {
    lo + (hi - lo) * v
}

fn db(amp: f64) -> f64 {
    if amp <= 1e-8 { -150.0 } else { 20.0 * amp.log10() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Values REAPER 7.73 shows at normalised 0.05, 0.25, 0.5, 0.75 and 0.95.
    #[test]
    fn curves_match_reaper() {
        let at = [0.05, 0.25, 0.5, 0.75, 0.95];
        type Case = (fn(f64) -> f64, [f64; 5], f64);
        let cases: [Case; 3] = [
            (eq_freq, [40.9, 228.3, 1160.5, 5332.2, 17774.7], 0.002),
            (eq_gain_db, [-20.0, -6.0, 0.0, 8.0, 11.4], 0.06),
            (amp2_db, [-20.0, -6.0, 0.0, 3.5, 5.6], 0.06),
        ];
        for (f, want, tol) in cases {
            for (v, w) in at.into_iter().zip(want) {
                let got = f(v);
                let err = if tol < 0.01 { (got - w).abs() / w } else { (got - w).abs() };
                assert!(err < tol, "{v}: {got} vs {w}");
            }
        }
    }
}
