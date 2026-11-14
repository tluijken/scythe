//! ScyEQ-Pro's settings as the host sees them: one parameter per stored value, its id the
//! value's index in the state, so ids never move. Solo and audition are not stored, so hosts do
//! not see them: the editor sets them on its instance alone.

use std::sync::OnceLock;

use plugin_format::{Kind, Map, ParamInfo};
use scyeq_dsp::filters::{SLOPES, Shape, slope_label};
use scyeq_dsp::params::{self, Unit};

pub fn infos() -> &'static [ParamInfo] {
    static INFOS: OnceLock<Vec<ParamInfo>> = OnceLock::new();
    INFOS.get_or_init(|| {
        // A phase mode or resolution change builds a new FIR path, which allocates: hosts get no
        // automation lane that would do that during playback.
        let structural = [params::GLOBAL + params::global::PHASE, params::GLOBAL + params::global::RESOLUTION];
        let automatable: Vec<u32> = params::automatable().filter(|id| !structural.contains(id)).collect();
        params::ids()
            .filter(|&id| !params::transient(id))
            .filter_map(|id| {
                let spec = params::spec_of(id)?;
                let kind = match spec.unit {
                    Unit::Switch => Kind::Bool,
                    Unit::Choice(_) | Unit::Shape | Unit::Slope => {
                        Kind::List { min: spec.min as i32, max: spec.max as i32 }
                    }
                    _ => {
                        Kind::Float { min: spec.min, max: spec.max, map: if spec.log { Map::Log } else { Map::Linear } }
                    }
                };
                Some(ParamInfo {
                    id,
                    // Built once for the life of the library: the host keeps asking for them.
                    name: Box::leak(params::name(id).into_boxed_str()),
                    kind,
                    default: params::default(id),
                    automatable: automatable.contains(&id),
                })
            })
            .collect()
    })
}

/// The value `text` stands for, read the way `params::format` writes it; a bare number is
/// taken in the parameter's own unit (Hz, dB, ms).
pub fn from_text(id: u32, text: &str) -> Option<f64> {
    let spec = params::spec_of(id)?;
    let text = text.trim().to_lowercase();
    let number = |s: &str| s.trim().parse::<f64>().ok();
    let value = match spec.unit {
        Unit::Hz => match text.strip_suffix("khz").or_else(|| text.strip_suffix('k')) {
            Some(khz) => number(khz)? * 1000.0,
            None => number(text.strip_suffix("hz").unwrap_or(&text))?,
        },
        Unit::Db => number(text.strip_suffix("db").unwrap_or(&text))?,
        Unit::Q => number(&text)?,
        Unit::Ms => match text.strip_suffix("ms") {
            Some(ms) => number(ms)?,
            None => match text.strip_suffix('s') {
                Some(s) => number(s)? * 1000.0,
                None => number(&text)?,
            },
        },
        Unit::Percent => number(text.strip_suffix('%').unwrap_or(&text))?,
        Unit::Switch => match text.as_str() {
            "on" | "true" => 1.0,
            "off" | "false" => 0.0,
            _ => number(&text)?,
        },
        Unit::Choice(names) => choice(names.iter().map(|n| n.to_string()), &text)?,
        Unit::Shape => choice(Shape::ALL.iter().map(|s| s.label().to_string()), &text)?,
        Unit::Slope => choice((0..SLOPES.len()).map(slope_label), &text)?,
        Unit::Pan => {
            if text == "center" || text == "c" {
                0.0
            } else if let Some(left) = text.strip_suffix('l') {
                -number(left.trim().strip_suffix('%').unwrap_or(left))?
            } else if let Some(right) = text.strip_suffix('r') {
                number(right.trim().strip_suffix('%').unwrap_or(right))?
            } else {
                number(&text)?
            }
        }
    };
    Some(params::clamp(id, value))
}

/// The index of the label `text` names, or the index it gives as a number.
fn choice(labels: impl Iterator<Item = String>, text: &str) -> Option<f64> {
    let mut labels = labels;
    match labels.position(|label| label.to_lowercase() == text) {
        Some(index) => Some(index as f64),
        None => text.parse::<f64>().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_value_text_reads_back_to_the_same_text() {
        for id in params::ids() {
            let spec = params::spec_of(id).unwrap();
            for step in 0..=40 {
                let n = step as f64 / 40.0;
                let value = params::from_normalized(id, n);
                let text = params::format(id, value);
                let back = from_text(id, &text).unwrap_or_else(|| panic!("{} {text:?}", params::name(id)));
                assert_eq!(params::format(id, back), text, "{} ({:?})", params::name(id), spec.unit);
            }
        }
    }

    #[test]
    fn plain_numbers_and_other_spellings_are_taken() {
        let freq = params::band_param(0, params::band::FREQ);
        assert_eq!(from_text(freq, "440"), Some(440.0));
        assert_eq!(from_text(freq, "2.5k"), Some(2500.0));
        assert_eq!(from_text(freq, "1 MHz"), None);
        let attack = params::band_param(0, params::band::ATTACK);
        assert_eq!(from_text(attack, "0.25 s"), Some(250.0));
        let shape = params::band_param(0, params::band::SHAPE);
        assert_eq!(from_text(shape, "low CUT"), Some(Shape::LowCut.index() as f64));
        assert_eq!(from_text(shape, "3"), Some(3.0));
        let pan = params::GLOBAL + params::global::PAN;
        assert_eq!(from_text(pan, "30% L"), Some(-30.0));
        assert_eq!(from_text(pan, "1000% R"), Some(100.0), "clamped");
    }

    #[test]
    fn kinds_follow_the_units() {
        let kind = |offset| infos().iter().find(|p| p.id == params::band_param(2, offset)).unwrap().kind;
        assert!(matches!(kind(params::band::FREQ), Kind::Float { map: Map::Log, .. }));
        assert!(matches!(kind(params::band::GAIN), Kind::Float { map: Map::Linear, .. }));
        assert_eq!(kind(params::band::USED), Kind::Bool);
        assert_eq!(kind(params::band::SHAPE), Kind::List { min: 0, max: 9 });
        assert_eq!(kind(params::band::SLOPE), Kind::List { min: 0, max: 9 });
        assert_eq!(kind(params::band::STEREO), Kind::List { min: 0, max: 4 });
        for offset in [params::band::SOLO, params::band::AUDITION] {
            assert!(infos().iter().all(|p| p.id != params::band_param(2, offset)), "{offset} is not stored");
        }
        assert_eq!(infos().len(), params::ids().count() - 2 * params::BANDS);
        let automatable = |id| infos().iter().find(|p| p.id == id).unwrap().automatable;
        assert!(!automatable(params::GLOBAL + params::global::PHASE));
        assert!(!automatable(params::GLOBAL + params::global::RESOLUTION));
        assert!(automatable(params::band_param(0, params::band::FREQ)));
    }
}
