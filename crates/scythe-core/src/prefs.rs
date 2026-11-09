//! The preferences REAPER keeps in reaper.ini rather than in the project, under the same keys
//! in a `[REAPER]` section, so a value can be copied over from REAPER's own file.

use crate::metronome::COUNT_IN_AT_MEASURE;

#[derive(Debug, Clone, PartialEq)]
pub struct Preferences {
    /// `preroll`: bits as `metronome::PREROLL_*` and `COUNT_IN_AT_MEASURE`.
    pub preroll: u32,
    /// `prerollmeas`: pre-roll length in measures.
    pub preroll_measures: f64,
    /// `metronome_defout`: where the click plays when the project names no output, encoded as
    /// a `METRONOMEOUT` value; -1 plays it with the master.
    pub metronome_default_output: i64,
}

impl Default for Preferences {
    fn default() -> Self {
        Self { preroll: 0, preroll_measures: 2.0, metronome_default_output: -1 }
    }
}

const SECTION: &str = "[REAPER]";

impl Preferences {
    /// Reads the keys it knows from the `[REAPER]` section; the others keep their defaults.
    pub fn parse(text: &str) -> Self {
        let mut prefs = Self::default();
        let mut in_section = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_section = line.eq_ignore_ascii_case(SECTION);
                continue;
            }
            let Some((key, value)) = line.split_once('=').filter(|_| in_section) else { continue };
            let value = value.trim();
            match key.trim() {
                "preroll" => prefs.preroll = value.parse().unwrap_or(prefs.preroll),
                "prerollmeas" => {
                    prefs.preroll_measures = value
                        .parse()
                        .ok()
                        .filter(|m: &f64| m.is_finite() && *m >= 0.0)
                        .unwrap_or(prefs.preroll_measures)
                }
                "metronome_defout" => {
                    prefs.metronome_default_output = value.parse().unwrap_or(prefs.metronome_default_output)
                }
                _ => {}
            }
        }
        prefs
    }

    /// `text` with these values in its `[REAPER]` section, everything else left as it was.
    pub fn write_into(&self, text: &str) -> String {
        let values = [
            ("preroll", self.preroll.to_string()),
            ("prerollmeas", format!("{:.14}", self.preroll_measures)),
            ("metronome_defout", self.metronome_default_output.to_string()),
        ];
        let mut written = [false; 3];
        let mut out = Vec::new();
        let mut in_section = false;
        let mut has_section = false;
        let flush = |out: &mut Vec<String>, written: &mut [bool; 3]| {
            for (i, (k, v)) in values.iter().enumerate() {
                if !written[i] {
                    out.push(format!("{k}={v}"));
                    written[i] = true;
                }
            }
        };
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                if in_section {
                    flush(&mut out, &mut written);
                }
                in_section = trimmed.eq_ignore_ascii_case(SECTION);
                has_section |= in_section;
            } else if in_section
                && let Some(i) =
                    trimmed.split_once('=').and_then(|(k, _)| values.iter().position(|(n, _)| *n == k.trim()))
            {
                out.push(format!("{}={}", values[i].0, values[i].1));
                written[i] = true;
                continue;
            }
            out.push(line.to_owned());
        }
        if !has_section {
            out.push(SECTION.to_owned());
        }
        if !has_section || in_section {
            flush(&mut out, &mut written);
        }
        out.join("\n") + "\n"
    }

    pub fn count_in_at_measure(&self) -> bool {
        self.preroll & COUNT_IN_AT_MEASURE != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_reaper_ini_keys() {
        let text =
            "[audioconfig]\npreroll=99\n[REAPER]\nafxcfg=0\nprerollmeas=1.50000000000000\npreroll=9\n[other]\nx=1\n";
        let p = Preferences::parse(text);
        assert_eq!(p, Preferences { preroll: 9, preroll_measures: 1.5, metronome_default_output: -1 });
        assert!(p.count_in_at_measure());
        let changed = Preferences { metronome_default_output: 1026, ..p }.write_into(text);
        assert_eq!(
            changed,
            "[audioconfig]\npreroll=99\n[REAPER]\nafxcfg=0\nprerollmeas=1.50000000000000\npreroll=9\n\
             metronome_defout=1026\n[other]\nx=1\n"
        );
        assert_eq!(Preferences::parse(&changed).metronome_default_output, 1026);
        let fresh = Preferences::default().write_into("");
        assert_eq!(fresh, "[REAPER]\npreroll=0\nprerollmeas=2.00000000000000\nmetronome_defout=-1\n");
        assert_eq!(Preferences::parse(&fresh), Preferences::default());
        assert_eq!(Preferences::parse("[REAPER]\nprerollmeas=-2\npreroll=x\n"), Preferences::default());
    }
}
