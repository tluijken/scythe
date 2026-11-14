//! ScyEQ-Pro's parameters and the state it stores: a flat table of values in their own units
//! (Hz, dB, ms), `BAND_STRIDE` per band followed by the globals.

use crate::clap_host::ParamInfo;

use super::filters::{SLOPES, Shape, slope_label};

pub const BANDS: usize = 24;
pub const BAND_STRIDE: u32 = 20;
pub const GLOBAL: u32 = BANDS as u32 * BAND_STRIDE;

/// Band parameters, as offsets inside a band's stride.
pub mod band {
    pub const USED: u32 = 0;
    pub const ENABLED: u32 = 1;
    pub const FREQ: u32 = 2;
    pub const GAIN: u32 = 3;
    pub const Q: u32 = 4;
    pub const SHAPE: u32 = 5;
    pub const SLOPE: u32 = 6;
    pub const STEREO: u32 = 7;
    /// 0 static, 1 dynamic, 2 spectral.
    pub const DYNAMIC: u32 = 8;
    pub const RANGE: u32 = 9;
    pub const THRESHOLD: u32 = 10;
    pub const AUTO_THRESHOLD: u32 = 11;
    pub const ATTACK: u32 = 12;
    pub const RELEASE: u32 = 13;
    pub const SIDECHAIN: u32 = 14;
    pub const SOLO: u32 = 15;
    pub const AUDITION: u32 = 16;
    pub const COUNT: u32 = 17;
}

/// Global parameters, as offsets from `GLOBAL`.
pub mod global {
    pub const OUTPUT: u32 = 0;
    pub const AUTO_GAIN: u32 = 1;
    pub const GAIN_SCALE: u32 = 2;
    pub const PAN: u32 = 3;
    pub const WIDTH: u32 = 4;
    pub const PHASE: u32 = 5;
    pub const RESOLUTION: u32 = 6;
    pub const CHARACTER: u32 = 7;
    pub const GAIN_Q: u32 = 8;
    /// Analyzer settings: stored with the instance but not automatable.
    pub const ANALYZER: u32 = 9;
    pub const ANALYZER_RESOLUTION: u32 = 10;
    pub const ANALYZER_SPEED: u32 = 11;
    pub const ANALYZER_TILT: u32 = 12;
    pub const ANALYZER_RANGE: u32 = 13;
    pub const ANALYZER_PEAK_HOLD: u32 = 14;
    pub const COUNT: u32 = 15;
    /// Globals listed as automatable parameters.
    pub const AUTOMATABLE: u32 = 9;
}

pub const TOTAL: usize = (GLOBAL + global::COUNT) as usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    Stereo,
    Left,
    Right,
    Mid,
    Side,
}

impl Placement {
    pub const ALL: [Placement; 5] = [Self::Stereo, Self::Left, Self::Right, Self::Mid, Self::Side];

    pub fn from_index(i: usize) -> Self {
        Self::ALL.get(i).copied().unwrap_or(Self::Stereo)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Stereo => "Stereo",
            Self::Left => "Left",
            Self::Right => "Right",
            Self::Mid => "Mid",
            Self::Side => "Side",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseMode {
    ZeroLatency,
    Natural,
    Linear,
}

impl PhaseMode {
    pub fn from_index(i: usize) -> Self {
        match i {
            1 => Self::Natural,
            2 => Self::Linear,
            _ => Self::ZeroLatency,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ZeroLatency => "Zero latency",
            Self::Natural => "Natural phase",
            Self::Linear => "Linear phase",
        }
    }
}

pub const RESOLUTIONS: [&str; 5] = ["Low", "Medium", "High", "Very high", "Maximum"];
pub const CHARACTERS: [&str; 3] = ["Clean", "Subtle", "Warm"];
pub const DYNAMIC_MODES: [&str; 3] = ["Off", "Dynamic", "Spectral"];

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Unit {
    Hz,
    Db,
    Q,
    Ms,
    Percent,
    Switch,
    Choice(&'static [&'static str]),
    Shape,
    Slope,
    Pan,
}

#[derive(Debug, Clone, Copy)]
pub struct Spec {
    pub name: &'static str,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub log: bool,
    pub unit: Unit,
}

const fn spec(name: &'static str, min: f64, max: f64, default: f64, log: bool, unit: Unit) -> Spec {
    Spec { name, min, max, default, log, unit }
}

const SHAPE_COUNT: f64 = (Shape::ALL.len() - 1) as f64;
const SLOPE_COUNT: f64 = (SLOPES.len() - 1) as f64;

pub const BAND_SPECS: [Spec; band::COUNT as usize] = [
    spec("Used", 0.0, 1.0, 0.0, false, Unit::Switch),
    spec("Enabled", 0.0, 1.0, 1.0, false, Unit::Switch),
    spec("Frequency", 10.0, 30000.0, 1000.0, true, Unit::Hz),
    spec("Gain", -30.0, 30.0, 0.0, false, Unit::Db),
    spec("Q", 0.025, 40.0, 1.0, true, Unit::Q),
    spec("Shape", 0.0, SHAPE_COUNT, 0.0, false, Unit::Shape),
    spec("Slope", 0.0, SLOPE_COUNT, 1.0, false, Unit::Slope),
    spec("Stereo", 0.0, 4.0, 0.0, false, Unit::Choice(&["Stereo", "Left", "Right", "Mid", "Side"])),
    spec("Dynamic", 0.0, 2.0, 0.0, false, Unit::Choice(&DYNAMIC_MODES)),
    spec("Range", -30.0, 30.0, -6.0, false, Unit::Db),
    spec("Threshold", -80.0, 0.0, -30.0, false, Unit::Db),
    spec("Auto threshold", 0.0, 1.0, 0.0, false, Unit::Switch),
    spec("Attack", 0.1, 500.0, 10.0, true, Unit::Ms),
    spec("Release", 1.0, 5000.0, 100.0, true, Unit::Ms),
    spec("Sidechain", 0.0, 1.0, 0.0, false, Unit::Choice(&["Internal", "External"])),
    spec("Solo", 0.0, 1.0, 0.0, false, Unit::Switch),
    spec("Sidechain audition", 0.0, 1.0, 0.0, false, Unit::Switch),
];

pub const GLOBAL_SPECS: [Spec; global::COUNT as usize] = [
    spec("Output gain", -36.0, 36.0, 0.0, false, Unit::Db),
    spec("Auto gain", 0.0, 1.0, 0.0, false, Unit::Switch),
    spec("Gain scale", -100.0, 200.0, 100.0, false, Unit::Percent),
    spec("Output pan", -100.0, 100.0, 0.0, false, Unit::Pan),
    spec("Output width", 0.0, 200.0, 100.0, false, Unit::Percent),
    spec("Phase mode", 0.0, 2.0, 0.0, false, Unit::Choice(&["Zero latency", "Natural phase", "Linear phase"])),
    spec("Linear phase resolution", 0.0, 4.0, 1.0, false, Unit::Choice(&RESOLUTIONS)),
    spec("Character", 0.0, 2.0, 0.0, false, Unit::Choice(&CHARACTERS)),
    spec("Gain-Q interaction", 0.0, 1.0, 0.0, false, Unit::Switch),
    spec(
        "Analyzer",
        0.0,
        7.0,
        3.0,
        false,
        Unit::Choice(&["Off", "Pre", "Post", "Pre+Post", "SC", "Pre+SC", "Post+SC", "All"]),
    ),
    spec("Analyzer resolution", 0.0, 3.0, 2.0, false, Unit::Choice(&["Low", "Medium", "High", "Maximum"])),
    spec("Analyzer speed", 0.0, 4.0, 2.0, false, Unit::Choice(&["Very slow", "Slow", "Medium", "Fast", "Very fast"])),
    spec("Analyzer tilt", 0.0, 6.0, 4.5, false, Unit::Db),
    spec("Analyzer range", 0.0, 2.0, 1.0, false, Unit::Choice(&["60 dB", "90 dB", "120 dB"])),
    spec("Analyzer peak hold", 0.0, 1.0, 0.0, false, Unit::Switch),
];

/// The table entry for parameter `id`.
pub fn spec_of(id: u32) -> Option<&'static Spec> {
    if id < GLOBAL { BAND_SPECS.get((id % BAND_STRIDE) as usize) } else { GLOBAL_SPECS.get((id - GLOBAL) as usize) }
}

pub fn band_param(band: usize, offset: u32) -> u32 {
    band as u32 * BAND_STRIDE + offset
}

/// Which parameter ids exist (the band strides have spare room).
pub fn ids() -> impl Iterator<Item = u32> {
    (0..BANDS as u32)
        .flat_map(|b| (0..band::COUNT).map(move |o| b * BAND_STRIDE + o))
        .chain(GLOBAL..GLOBAL + global::COUNT)
}

/// The automatable parameters, in the order the host lists them.
pub fn automatable() -> impl Iterator<Item = u32> {
    (0..BANDS as u32)
        .flat_map(|b| {
            (0..band::COUNT).filter(|o| *o != band::SOLO && *o != band::AUDITION).map(move |o| b * BAND_STRIDE + o)
        })
        .chain(GLOBAL..GLOBAL + global::AUTOMATABLE)
}

pub fn name(id: u32) -> String {
    let Some(s) = spec_of(id) else { return String::new() };
    if id < GLOBAL { format!("Band {} {}", id / BAND_STRIDE + 1, s.name) } else { s.name.to_owned() }
}

pub fn info(id: u32) -> Option<ParamInfo> {
    let s = spec_of(id)?;
    Some(ParamInfo { id, name: name(id), min: s.min, max: s.max, default: default(id), log: s.log })
}

/// Default value; bands start spread over the spectrum so a fresh band lands somewhere useful.
pub fn default(id: u32) -> f64 {
    let Some(s) = spec_of(id) else { return 0.0 };
    if id < GLOBAL && id % BAND_STRIDE == band::FREQ {
        let b = (id / BAND_STRIDE) as f64;
        return 50.0 * 2f64.powf(b * 0.4);
    }
    s.default
}

pub fn clamp(id: u32, v: f64) -> f64 {
    let Some(s) = spec_of(id) else { return v };
    let v = if v.is_finite() { v.clamp(s.min, s.max) } else { s.default };
    match s.unit {
        Unit::Switch | Unit::Choice(_) | Unit::Shape | Unit::Slope => v.round(),
        _ => v,
    }
}

pub fn to_normalized(id: u32, v: f64) -> f64 {
    let Some(s) = spec_of(id) else { return 0.0 };
    let v = clamp(id, v);
    if s.log { (v / s.min).ln() / (s.max / s.min).ln() } else { (v - s.min) / (s.max - s.min) }
}

pub fn from_normalized(id: u32, n: f64) -> f64 {
    let Some(s) = spec_of(id) else { return 0.0 };
    let n = n.clamp(0.0, 1.0);
    clamp(id, if s.log { s.min * (s.max / s.min).powf(n) } else { s.min + n * (s.max - s.min) })
}

pub fn format(id: u32, v: f64) -> String {
    let Some(s) = spec_of(id) else { return String::new() };
    match s.unit {
        Unit::Hz => {
            if v >= 1000.0 {
                format!("{:.2} kHz", v / 1000.0)
            } else {
                format!("{v:.1} Hz")
            }
        }
        Unit::Db => format!("{v:+.2} dB"),
        Unit::Q => format!("{v:.3}"),
        Unit::Ms => {
            if v >= 1000.0 {
                format!("{:.2} s", v / 1000.0)
            } else {
                format!("{v:.1} ms")
            }
        }
        Unit::Percent => format!("{v:.0}%"),
        Unit::Switch => if v >= 0.5 { "on" } else { "off" }.into(),
        Unit::Choice(names) => names.get(v.round().max(0.0) as usize).copied().unwrap_or("").into(),
        Unit::Shape => Shape::from_index(v.round().max(0.0) as usize).label().into(),
        Unit::Slope => slope_label(v.round().max(0.0) as usize),
        Unit::Pan => {
            if v.abs() < 0.5 {
                "center".into()
            } else if v < 0.0 {
                format!("{:.0}% L", -v)
            } else {
                format!("{v:.0}% R")
            }
        }
    }
}

const MAGIC: &[u8; 4] = b"SCEQ";
pub const VERSION: u32 = 1;

/// Not stored: soloing and auditioning are momentary.
fn transient(id: u32) -> bool {
    id < GLOBAL && matches!(id % BAND_STRIDE, band::SOLO | band::AUDITION)
}

/// A stored state: the values this version knows, those a newer version adds (kept so they
/// survive a save), and the version that wrote it.
pub struct Decoded {
    pub values: Vec<f64>,
    pub extra: Vec<f64>,
    pub version: u32,
}

/// The stored state: magic, version, value count, then every value as an f64.
/// A state a newer version wrote keeps its version.
pub fn encode(values: &[f64], extra: &[f64], version: u32) -> Vec<u8> {
    let mut d = Vec::with_capacity(12 + (values.len() + extra.len()) * 8);
    d.extend(MAGIC);
    d.extend(version.max(VERSION).to_le_bytes());
    d.extend(((values.len() + extra.len()) as u32).to_le_bytes());
    for (id, v) in values.iter().enumerate() {
        let v = if transient(id as u32) { 0.0 } else { *v };
        d.extend(v.to_le_bytes());
    }
    for v in extra {
        d.extend(v.to_le_bytes());
    }
    d
}

/// Values from a stored state; values missing from an older version take their defaults.
pub fn decode(data: &[u8]) -> Option<Decoded> {
    if data.get(..4)? != MAGIC {
        return None;
    }
    let word = |i: usize| data.get(i..i + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let version = word(4)?;
    if version == 0 {
        return None;
    }
    let n = word(8)? as usize;
    let mut values = defaults();
    let mut extra = Vec::new();
    for (i, b) in data.get(12..)?.as_chunks::<8>().0.iter().take(n).enumerate() {
        let v = f64::from_le_bytes(*b);
        match values.get_mut(i) {
            Some(slot) => *slot = clamp(i as u32, v),
            None => extra.push(v),
        }
    }
    Some(Decoded { values, extra, version })
}

pub fn defaults() -> Vec<f64> {
    (0..TOTAL as u32).map(default).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_without_momentary_switches() {
        let mut v = defaults();
        v[band_param(3, band::GAIN) as usize] = 4.5;
        v[band_param(3, band::SOLO) as usize] = 1.0;
        v[(GLOBAL + global::PHASE) as usize] = 2.0;
        let back = decode(&encode(&v, &[], VERSION)).unwrap().values;
        assert_eq!(back[band_param(3, band::GAIN) as usize], 4.5);
        assert_eq!(back[band_param(3, band::SOLO) as usize], 0.0);
        assert_eq!(back[(GLOBAL + global::PHASE) as usize], 2.0);
        assert!(decode(b"nope").is_none());
    }

    #[test]
    fn normalized_values_map_both_ways() {
        let f = band_param(0, band::FREQ);
        assert!((from_normalized(f, to_normalized(f, 440.0)) - 440.0).abs() < 1e-9);
        assert_eq!(from_normalized(band_param(0, band::SHAPE), 1.0), 9.0);
        assert_eq!(format(f, 2500.0), "2.50 kHz");
        assert_eq!(name(band_param(1, band::GAIN)), "Band 2 Gain");
        assert_eq!(automatable().count(), BANDS * 15 + 9);
    }
}
