//! The preferences REAPER keeps in reaper.ini rather than in the project, under the same keys
//! in a `[REAPER]` section, so a value can be copied over from REAPER's own file.

use crate::metronome::COUNT_IN_AT_MEASURE;
use crate::surface::Surface;

#[derive(Debug, Clone, PartialEq)]
pub struct Preferences {
    /// `preroll`: bits as `metronome::PREROLL_*` and `COUNT_IN_AT_MEASURE`.
    pub preroll: u32,
    /// `prerollmeas`: pre-roll length in measures.
    pub preroll_measures: f64,
    /// `metronome_defout`: where the click plays when the project names no output, encoded as
    /// a `METRONOMEOUT` value; -1 plays it with the master.
    pub metronome_default_output: i64,
    /// `csurf_cnt` and `csurf_<i>`: the control surfaces.
    pub surfaces: Vec<Surface>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self { preroll: 0, preroll_measures: 2.0, metronome_default_output: -1, surfaces: Vec::new() }
    }
}

const SECTION: &str = "[REAPER]";

/// The index of a `csurf_<i>` key.
fn surface_index(key: &str) -> Option<usize> {
    key.strip_prefix("csurf_").and_then(|i| i.parse().ok())
}

impl Preferences {
    /// Reads the keys it knows from the `[REAPER]` section; the others keep their defaults.
    pub fn parse(text: &str) -> Self {
        let mut prefs = Self::default();
        let mut in_section = false;
        let mut surface_count = 0;
        let mut surfaces = Vec::new();
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
                "csurf_cnt" => surface_count = value.parse().unwrap_or(0),
                key => {
                    if let Some(i) = surface_index(key) {
                        surfaces.push((i, Surface::parse(value)));
                    }
                }
            }
        }
        surfaces.sort_by_key(|(i, _)| *i);
        prefs.surfaces = surfaces.into_iter().filter(|(i, _)| *i < surface_count).map(|(_, s)| s).collect();
        prefs
    }

    /// `text` with these values in its `[REAPER]` section, everything else left as it was.
    pub fn write_into(&self, text: &str) -> String {
        let mut values = vec![
            ("preroll".to_owned(), self.preroll.to_string()),
            ("prerollmeas".to_owned(), format!("{:.14}", self.preroll_measures)),
            ("metronome_defout".to_owned(), self.metronome_default_output.to_string()),
        ];
        let has_surfaces = !self.surfaces.is_empty() || text.lines().any(|l| l.trim_start().starts_with("csurf_cnt="));
        if has_surfaces {
            values.push(("csurf_cnt".to_owned(), self.surfaces.len().to_string()));
        }
        values.extend(self.surfaces.iter().enumerate().map(|(i, s)| (format!("csurf_{i}"), s.line())));
        let stale = |key: &str| surface_index(key).is_some_and(|i| i >= self.surfaces.len());
        let mut written = vec![false; values.len()];
        let mut out = Vec::new();
        let mut in_section = false;
        let mut has_section = false;
        let flush = |out: &mut Vec<String>, written: &mut [bool]| {
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
            } else if in_section && let Some((key, _)) = trimmed.split_once('=') {
                let key = key.trim();
                if let Some(i) = values.iter().position(|(n, _)| n == key) {
                    out.push(format!("{}={}", values[i].0, values[i].1));
                    written[i] = true;
                    continue;
                }
                if stale(key) {
                    continue;
                }
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
    use crate::surface::{MidiKind, MidiSurface};

    #[test]
    fn reads_and_writes_reaper_ini_keys() {
        let text =
            "[audioconfig]\npreroll=99\n[REAPER]\nafxcfg=0\nprerollmeas=1.50000000000000\npreroll=9\n[other]\nx=1\n";
        let p = Preferences::parse(text);
        assert_eq!(p, Preferences { preroll: 9, preroll_measures: 1.5, ..Default::default() });
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

    #[test]
    fn control_surfaces_round_trip() {
        // As REAPER 7.73 keeps them in reaper.ini (its section is `[reaper]`, keys sorted).
        let text = "[reaper]\ncsurf_0=OSC \"TouchOSC\" 3 9123 \"127.0.0.1\" 9124 1024 10 \"\"\n\
                    csurf_1=MCU 0 0 2 3 0\ncsurf_2=BCF2000 0 1\ncsurf_cnt=3\nerrnowarn=5\n";
        let p = Preferences::parse(text);
        assert_eq!(p.surfaces.len(), 3);
        assert_eq!(p.surfaces[2].line(), "BCF2000 0 1");
        let again = p.write_into(text);
        assert_eq!(Preferences::parse(&again), p);
        for line in text.lines().filter(|l| l.starts_with("csurf")) {
            assert!(again.lines().any(|l| l == line), "{line} in\n{again}");
        }
        let mut fewer = p.clone();
        fewer.surfaces.remove(0);
        fewer.surfaces[0] = Surface::Midi(MidiSurface::new(MidiKind::McuExtender, 4, 5));
        let written = fewer.write_into(&again);
        assert!(written.contains("csurf_0=MCUEX 0 0 4 5 0\ncsurf_1=BCF2000 0 1\ncsurf_cnt=2\n"), "{written}");
        assert!(!written.contains("csurf_2"));
        assert_eq!(Preferences::parse(&written), fewer);
        let none = Preferences { surfaces: Vec::new(), ..fewer }.write_into(&written);
        assert!(none.contains("csurf_cnt=0") && !none.contains("csurf_0"), "{none}");
        assert!(Preferences::parse("[REAPER]\ncsurf_0=MCU 0 0 0 0 0\n").surfaces.is_empty());
    }
}
