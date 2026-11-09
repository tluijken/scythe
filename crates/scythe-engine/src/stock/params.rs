//! The parameters of ReaComp and ReaGate: their chunks hold every parameter as the f32 value
//! REAPER's `TrackFX_GetParam` reports, and each parameter maps that value to what it shows.

use super::{marked_chunk, param};
use crate::clap_host::ParamInfo;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Unit {
    /// An amplitude, 1 at 0 dB (REAPER's range is 0..2, +6 dB).
    Gain,
    /// A fraction of this many milliseconds.
    Ms(f64),
    /// A fraction of 20 kHz.
    Hz,
    /// ReaComp's ratio: 1 + 99 × value, infinite from 99:1 up.
    Ratio,
    /// A fraction of 24 dB.
    Knee,
    /// Off below 0.5.
    Switch,
    /// A choice among `n` steps of the full range (ReaComp's legacy options, MIDI note).
    Steps(f64),
}

/// A parameter: its index in REAPER's order, name, unit and the raw range `TrackFX_GetParam`
/// reports (normalised values are linear in it).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Spec {
    pub index: u32,
    pub name: &'static str,
    pub unit: Unit,
    pub range: f32,
}

pub(crate) const fn spec(index: u32, name: &'static str, unit: Unit, range: f32) -> Spec {
    Spec { index, name, unit, range }
}

fn db(gain: f64) -> f64 {
    if gain <= 1e-7 { -150.0 } else { 20.0 * gain.log10() }
}

impl Spec {
    /// The shown value of raw value `raw`, with its text.
    pub fn display(&self, raw: f32) -> (f64, String) {
        let raw = raw as f64;
        match self.unit {
            Unit::Gain => {
                let d = db(raw);
                (d, if d <= -149.0 { "-inf dB".into() } else { format!("{d:+.1} dB") })
            }
            Unit::Ms(full) => super::ms_value(raw * full),
            Unit::Hz => {
                let hz = raw * 20000.0;
                (hz, format!("{hz:.0} Hz"))
            }
            Unit::Ratio => {
                let r = 1.0 + raw * 99.0;
                if r >= 99.0 { (100.0, "inf:1".into()) } else { (r, format!("{r:.2}:1")) }
            }
            Unit::Knee => (raw * 24.0, format!("{:.1} dB", raw * 24.0)),
            Unit::Switch => {
                let on = raw >= 0.5;
                (on as u8 as f64, if on { "on".into() } else { "off".into() })
            }
            Unit::Steps(n) => {
                let v = (raw * n).round();
                (v, format!("{v}"))
            }
        }
    }

    /// The raw value for shown value `v`.
    pub fn raw(&self, v: f64) -> f32 {
        (match self.unit {
            Unit::Gain => {
                if v <= -149.0 {
                    0.0
                } else {
                    10f64.powf(v / 20.0)
                }
            }
            Unit::Ms(full) => v / full,
            Unit::Hz => v / 20000.0,
            Unit::Ratio => {
                if v >= 99.0 {
                    1.0
                } else {
                    (v - 1.0) / 99.0
                }
            }
            Unit::Knee => v / 24.0,
            Unit::Switch => (v >= 0.5) as u8 as f64,
            Unit::Steps(n) => v.round() / n,
        }) as f32
    }

    pub fn info(&self, default: f32) -> ParamInfo {
        let (lo, hi, log) = match self.unit {
            Unit::Gain => (-60.0, db(self.range as f64), false),
            Unit::Ms(full) => (0.0, full * self.range as f64, false),
            Unit::Hz => (0.0, 20000.0, false),
            Unit::Ratio => (1.0, 100.0, false),
            Unit::Knee => (0.0, 24.0, false),
            Unit::Switch => (0.0, 1.0, false),
            Unit::Steps(n) => (0.0, n, false),
        };
        param(self.index, self.name, lo, hi, self.display(default).0.clamp(lo, hi), log)
    }
}

/// Raw parameter values with the table describing them.
pub(crate) struct Params {
    pub values: Vec<f32>,
    pub specs: &'static [Spec],
    defaults: Vec<f32>,
}

impl Params {
    pub fn new(values: Vec<f32>, specs: &'static [Spec], defaults: &[f32]) -> Self {
        let mut values = values;
        if values.len() < defaults.len() {
            values.extend_from_slice(&defaults[values.len()..]);
        }
        Self { values, specs, defaults: defaults.to_vec() }
    }

    pub fn get(&self, i: usize) -> f32 {
        self.values.get(i).copied().unwrap_or(0.0)
    }

    fn spec(&self, id: u32) -> Option<&'static Spec> {
        self.specs.iter().find(|s| s.index == id)
    }

    pub fn infos(&self) -> Vec<ParamInfo> {
        self.specs.iter().map(|s| s.info(self.defaults.get(s.index as usize).copied().unwrap_or(0.0))).collect()
    }

    pub fn display(&self, id: u32) -> Option<(f64, String)> {
        Some(self.spec(id)?.display(self.get(id as usize)))
    }

    /// Sets a parameter from its shown value; false for parameters not in the table.
    pub fn set(&mut self, id: u32, v: f64) -> bool {
        let Some(spec) = self.spec(id) else { return false };
        self.values[id as usize] = spec.raw(v);
        true
    }

    /// Sets parameter `index` from a normalised value (linear in its raw range).
    pub fn set_normalized(&mut self, index: usize, v: f64) -> bool {
        let Some(spec) = self.spec(index as u32) else { return false };
        self.values[index] = (v.clamp(0.0, 1.0) * spec.range as f64) as f32;
        true
    }

    pub fn chunk(&self) -> Vec<u8> {
        marked_chunk(&self.values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Values REAPER 7.73 shows for these raw values (`TrackFX_FormatParamValue`).
    #[test]
    fn shown_values_match_reaper() {
        let gain = spec(0, "Threshold", Unit::Gain, 2.0);
        assert_eq!(gain.display(0.5).1, "-6.0 dB");
        assert_eq!(gain.display(0.0).1, "-inf dB");
        assert!((gain.raw(-6.0206) - 0.5).abs() < 1e-6);
        let ratio = spec(1, "Ratio", Unit::Ratio, 1.0);
        assert_eq!(ratio.display(0.05).1, "5.95:1");
        assert_eq!(ratio.display(1.0).1, "inf:1");
        assert!((spec(6, "Lowpass", Unit::Hz, 1.0).display(0.05).0 - 1000.0).abs() < 1e-3);
        assert_eq!(spec(14, "Knee", Unit::Knee, 1.0).display(0.25).0, 6.0);
        assert_eq!(spec(13, "RMS", Unit::Ms(100.0), 10.0).display(0.5).0, 50.0);
    }
}
