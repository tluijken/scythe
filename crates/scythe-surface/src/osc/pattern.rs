//! REAPER's `.ReaperOSC` pattern files: device settings (`DEVICE_TRACK_COUNT 8`), then lines
//! of an action name followed by the OSC addresses that trigger it and carry its feedback,
//! each led by a flag for how its argument reads (`n/track/@/volume`). `@` stands for a
//! number (track, send, FX within the device's bank) or a comma list of them.

use std::collections::HashMap;

/// How a pattern's argument is read and written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Flag {
    /// `n`: 0..1, the control's position (a fader's slider position).
    Normalized,
    /// `f`: the raw value (dB, seconds, bpm).
    Float,
    /// `b`: 0 or 1, sets or clears.
    Binary,
    /// `t`: toggles (or triggers) on no argument or 1; feedback as for `b`.
    Toggle,
    /// `r`: rotary, forward above the centre and back below it; no feedback.
    Rotary,
    /// `s`: a string.
    Str,
    /// `i`: an integer from the device; no feedback.
    Int,
}

impl Flag {
    fn from_char(c: char) -> Option<Self> {
        Some(match c {
            'n' => Self::Normalized,
            'f' => Self::Float,
            'b' => Self::Binary,
            't' => Self::Toggle,
            'r' => Self::Rotary,
            's' => Self::Str,
            'i' => Self::Int,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    pub flag: Flag,
    /// The address split at `/`, `@` segments standing for numbers.
    pub segments: Vec<String>,
}

impl Pattern {
    pub fn wildcards(&self) -> usize {
        self.segments.iter().filter(|s| *s == "@").count()
    }

    /// The `@` values when `address` matches, each a list (the device may send `1,2,5`).
    pub fn matches(&self, address: &str) -> Option<Vec<Vec<String>>> {
        let parts: Vec<&str> = address.trim_start_matches('/').split('/').collect();
        if parts.len() != self.segments.len() {
            return None;
        }
        let mut values = Vec::new();
        for (part, seg) in parts.iter().zip(&self.segments) {
            if seg == "@" {
                if part.is_empty() {
                    return None;
                }
                values.push(part.split(',').map(str::to_owned).collect());
            } else if part != seg {
                return None;
            }
        }
        Some(values)
    }

    /// The address with `@` replaced by `values` in order.
    pub fn address(&self, values: &[usize]) -> Option<String> {
        let mut values = values.iter();
        let mut out = String::new();
        for seg in &self.segments {
            out.push('/');
            if seg == "@" {
                out.push_str(&values.next()?.to_string());
            } else {
                out.push_str(seg);
            }
        }
        Some(out)
    }
}

/// How the device chooses its tracks and FX, from the settings at the top of the file.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub track_count: usize,
    pub send_count: usize,
    pub receive_count: usize,
    pub fx_count: usize,
    pub fx_param_count: usize,
    pub fx_inst_param_count: usize,
    pub marker_count: usize,
    pub region_count: usize,
    /// `REAPER_TRACK_FOLLOWS DEVICE`: selecting a track on the device selects it in Scythe.
    pub reaper_follows_device: bool,
    /// `DEVICE_TRACK_FOLLOWS LAST_TOUCHED`: the device's track follows the one touched last.
    pub device_follows_last_touched: bool,
    /// `DEVICE_TRACK_BANK_FOLLOWS MIXER`.
    pub bank_follows_mixer: bool,
    /// `DEVICE_ROTARY_CENTER`: the rotary argument that means no change (0 or 0.5).
    pub rotary_center: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            track_count: 8,
            send_count: 4,
            receive_count: 4,
            fx_count: 8,
            fx_param_count: 16,
            fx_inst_param_count: 16,
            marker_count: 0,
            region_count: 0,
            reaper_follows_device: false,
            device_follows_last_touched: false,
            bank_follows_mixer: false,
            rotary_center: 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PatternFile {
    pub settings: Settings,
    /// Patterns by action name, in file order.
    pub actions: HashMap<String, Vec<Pattern>>,
    /// Lines that could not be read, as `line: problem`.
    pub warnings: Vec<String>,
}

/// Scythe's default pattern file: the actions it drives, under the names and addresses of
/// REAPER's `Default.ReaperOSC`, so a device layout made for REAPER works unchanged.
pub const DEFAULT: &str = include_str!("Default.ReaperOSC");

const SETTINGS: [&str; 14] = [
    "DEVICE_TRACK_COUNT",
    "DEVICE_SEND_COUNT",
    "DEVICE_RECEIVE_COUNT",
    "DEVICE_FX_COUNT",
    "DEVICE_FX_PARAM_COUNT",
    "DEVICE_FX_INST_PARAM_COUNT",
    "DEVICE_MARKER_COUNT",
    "DEVICE_REGION_COUNT",
    "REAPER_TRACK_FOLLOWS",
    "DEVICE_TRACK_FOLLOWS",
    "DEVICE_TRACK_BANK_FOLLOWS",
    "DEVICE_FX_FOLLOWS",
    "DEVICE_EQ",
    "DEVICE_ROTARY_CENTER",
];

impl PatternFile {
    pub fn parse(text: &str) -> Self {
        let mut file = Self::default();
        let s = &mut file.settings;
        for (n, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            let mut words = line.split_whitespace();
            let Some(name) = words.next() else { continue };
            let rest: Vec<&str> = words.collect();
            let count = |v: &mut usize| {
                if let Some(c) = rest.first().and_then(|w| w.parse().ok()) {
                    *v = c;
                }
            };
            let first = rest.first().copied().unwrap_or("");
            if SETTINGS.contains(&name) && !first.contains('/') {
                match name {
                    "DEVICE_TRACK_COUNT" => count(&mut s.track_count),
                    "DEVICE_SEND_COUNT" => count(&mut s.send_count),
                    "DEVICE_RECEIVE_COUNT" => count(&mut s.receive_count),
                    "DEVICE_FX_COUNT" => count(&mut s.fx_count),
                    "DEVICE_FX_PARAM_COUNT" => count(&mut s.fx_param_count),
                    "DEVICE_FX_INST_PARAM_COUNT" => count(&mut s.fx_inst_param_count),
                    "DEVICE_MARKER_COUNT" => count(&mut s.marker_count),
                    "DEVICE_REGION_COUNT" => count(&mut s.region_count),
                    "REAPER_TRACK_FOLLOWS" => s.reaper_follows_device = first.eq_ignore_ascii_case("DEVICE"),
                    "DEVICE_TRACK_FOLLOWS" => {
                        s.device_follows_last_touched = first.eq_ignore_ascii_case("LAST_TOUCHED")
                    }
                    "DEVICE_TRACK_BANK_FOLLOWS" => s.bank_follows_mixer = first.eq_ignore_ascii_case("MIXER"),
                    "DEVICE_ROTARY_CENTER" => s.rotary_center = first.parse().unwrap_or(0.0),
                    _ => {}
                }
                continue;
            }
            for word in rest {
                let mut chars = word.chars();
                let Some(flag) = chars.next().and_then(Flag::from_char) else {
                    file.warnings.push(format!("{}: pattern \"{word}\" starts with an unknown flag", n + 1));
                    continue;
                };
                let path = chars.as_str();
                if !path.starts_with('/') {
                    file.warnings.push(format!("{}: pattern \"{word}\" does not start with '/'", n + 1));
                    continue;
                }
                let segments = path.trim_start_matches('/').split('/').map(str::to_owned).collect();
                file.actions.entry(name.to_owned()).or_default().push(Pattern { flag, segments });
            }
        }
        file
    }

    pub fn patterns(&self, action: &str) -> &[Pattern] {
        self.actions.get(action).map_or(&[], Vec::as_slice)
    }

    /// The action and pattern `address` matches, with its `@` values. When several match, the
    /// one with the fewest wildcards wins (`/track/volume` before `/track/@/…`).
    pub fn find(&self, address: &str) -> Option<(&str, &Pattern, Vec<Vec<String>>)> {
        let mut best: Option<(&str, &Pattern, Vec<Vec<String>>)> = None;
        for (action, patterns) in &self.actions {
            for p in patterns {
                if let Some(values) = p.matches(address)
                    && best.as_ref().is_none_or(|(_, b, _)| p.wildcards() < b.wildcards())
                {
                    best = Some((action, p, values));
                }
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_reapers_default_file() {
        let f = PatternFile::parse(DEFAULT);
        assert!(f.warnings.is_empty(), "{:?}", f.warnings);
        assert_eq!(f.settings.track_count, 8);
        assert_eq!(f.settings.marker_count, 0);
        let vol = f.patterns("TRACK_VOLUME");
        assert!(vol.iter().any(|p| p.flag == Flag::Normalized && p.segments == ["track", "@", "volume"]));
        assert!(vol.iter().any(|p| p.flag == Flag::Float && p.segments == ["track", "volume", "db"]));
        let (action, p, values) = f.find("/track/3/volume").unwrap();
        assert_eq!((action, p.flag, values), ("TRACK_VOLUME", Flag::Normalized, vec![vec!["3".to_owned()]]));
        assert_eq!(f.find("/track/mute").unwrap().0, "TRACK_MUTE");
        assert_eq!(f.find("/action/40364").unwrap().0, "ACTION");
        assert_eq!(f.find("/fx/1,2/bypass").unwrap().2, vec![vec!["1".to_owned(), "2".to_owned()]]);
        assert!(f.find("/nothing/here").is_none());
        let p = vol.iter().find(|p| p.wildcards() == 1).unwrap();
        assert_eq!(p.address(&[4]).as_deref(), Some("/track/4/volume"));
    }

    #[test]
    fn reports_bad_lines_and_settings() {
        let f = PatternFile::parse(
            "DEVICE_TRACK_COUNT 16\nDEVICE_ROTARY_CENTER 0.5\nREAPER_TRACK_FOLLOWS DEVICE\n\
             TRACK_MUTE x/track/mute b/track/@/mute\nPLAY play # comment\n",
        );
        assert_eq!(f.settings.track_count, 16);
        assert_eq!(f.settings.rotary_center, 0.5);
        assert!(f.settings.reaper_follows_device);
        assert_eq!(f.patterns("TRACK_MUTE").len(), 1);
        assert_eq!(f.warnings.len(), 2, "{:?}", f.warnings);
    }
}
