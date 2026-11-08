//! A project's own render settings (`RENDER_FILE`, `RENDER_PATTERN`, `RENDER_FMT`,
//! `RENDER_RANGE`, `<RENDER_CFG`), turned into concrete render jobs.

use std::path::{Path, PathBuf};

use crate::model::{Project, TimeRange};
use crate::rpp::{Node, tokenize};

#[derive(Debug, Clone, PartialEq)]
pub enum Bounds {
    Custom(TimeRange),
    Project,
    TimeSelection,
    Regions,
    /// Bounds this reimplementation does not handle (selected items or regions …).
    Other(i64),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Format {
    /// WAV with this many bits (32 means float).
    Wav { bits: u16 },
    /// MP3 at a constant bitrate.
    Mp3 { kbps: u32 },
    /// Another sink, by its four-character id as REAPER prints it (`l3pm` is MP3).
    Other(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct RenderSettings {
    pub file: String,
    pub pattern: String,
    pub channels: u16,
    pub sample_rate: Option<u32>,
    pub bounds: Bounds,
    pub format: Format,
}

impl RenderSettings {
    pub fn from_project(p: &Project) -> Self {
        let mut s = Self {
            file: String::new(),
            pattern: String::new(),
            channels: 2,
            sample_rate: None,
            bounds: Bounds::Project,
            format: Format::Wav { bits: 24 },
        };
        for node in &p.extra {
            match node {
                Node::Line(l) => {
                    let t = tokenize(l);
                    let num = |i: usize| t.get(i).and_then(|v| v.parse::<f64>().ok());
                    match t[0].as_str() {
                        "RENDER_FILE" => s.file = t.get(1).cloned().unwrap_or_default(),
                        "RENDER_PATTERN" => s.pattern = t.get(1).cloned().unwrap_or_default(),
                        "RENDER_FMT" => {
                            s.channels = num(2).map_or(2, |c| c as u16).clamp(1, 2);
                            s.sample_rate = num(3).filter(|r| *r > 0.0).map(|r| r as u32);
                        }
                        "RENDER_RANGE" => {
                            s.bounds = match num(1).unwrap_or(1.0) as i64 {
                                0 => Bounds::Custom(TimeRange::new(num(2).unwrap_or(0.0), num(3).unwrap_or(0.0))),
                                1 => Bounds::Project,
                                2 => Bounds::TimeSelection,
                                3 => Bounds::Regions,
                                other => Bounds::Other(other),
                            }
                        }
                        _ => {}
                    }
                }
                Node::Chunk(c) if c.name() == "RENDER_CFG" => {
                    if let Some(fmt) = decode_cfg(c.children.iter().filter_map(|n| match n {
                        Node::Line(l) => Some(l.trim()),
                        _ => None,
                    })) {
                        s.format = fmt;
                    }
                }
                Node::Chunk(_) => {}
            }
        }
        s
    }

    /// Ranges and output paths to render; the extension follows the format (other sinks than
    /// WAV and MP3 get `.wav`).
    pub fn jobs(&self, p: &Project, unix_seconds: u64) -> Result<Vec<(TimeRange, PathBuf)>, String> {
        let ranges: Vec<(TimeRange, Option<(u32, String)>)> = match &self.bounds {
            Bounds::Custom(r) => vec![(*r, None)],
            Bounds::Project => vec![(TimeRange::new(0.0, p.length()), None)],
            Bounds::TimeSelection => {
                vec![(p.time_selection.ok_or("the project renders its time selection, but has none")?, None)]
            }
            Bounds::Regions => p
                .markers
                .iter()
                .filter_map(|m| {
                    m.region_end.map(|end| (TimeRange::new(m.position, end), Some((m.index, m.name.clone()))))
                })
                .collect(),
            Bounds::Other(n) => return Err(format!("render bounds mode {n} is not supported")),
        };
        if ranges.is_empty() {
            return Err("nothing to render".into());
        }
        let dir = p.path.as_deref().and_then(Path::parent).map(Path::to_owned).unwrap_or_default();
        let base = match self.file.as_str() {
            "" => dir.clone(),
            f if Path::new(f).is_absolute() => PathBuf::from(f),
            f => dir.join(f),
        };
        let pattern = match (self.pattern.as_str(), &self.bounds) {
            ("", Bounds::Regions) => "$project-$regionnumber",
            (pat, _) => pat,
        };
        Ok(ranges
            .into_iter()
            .map(|(range, region)| {
                let path = if pattern.is_empty() {
                    if self.file.is_empty() { base.join(p.name()) } else { base.clone() }
                } else {
                    base.join({
                        let name = expand(pattern, p, region.as_ref(), unix_seconds);
                        // Region wildcards outside region bounds leave only separators behind.
                        if name.trim_matches(|c: char| " -_.".contains(c)).is_empty() { p.name() } else { name }
                    })
                };
                let mut path = path.into_os_string();
                path.push(match self.format {
                    Format::Mp3 { .. } => ".mp3",
                    _ => ".wav",
                });
                (range, PathBuf::from(path))
            })
            .collect())
    }
}

/// `<RENDER_CFG` holds the sink configuration in base64: a four-character id (stored
/// reversed, so WAV prints as `evaw`) followed by sink-specific bytes; for WAV the first is
/// the bit depth.
fn decode_cfg<'a>(lines: impl Iterator<Item = &'a str>) -> Option<Format> {
    let text: String = lines.collect();
    let bytes = decode_base64(&text)?;
    let id = String::from_utf8_lossy(bytes.get(..4)?).into_owned();
    Some(match id.as_str() {
        "evaw" => Format::Wav { bits: bytes.get(4).map_or(24, |b| *b as u16) },
        "l3pm" => Format::Mp3 {
            kbps: bytes.get(4..8).map_or(320, |b| u32::from_le_bytes(b.try_into().unwrap())).clamp(8, 320),
        },
        _ => Format::Other(id),
    })
}

fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for c in text.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=') {
        acc = acc << 6 | value(c)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Expands the render wildcards this reimplementation knows; others stay as written.
fn expand(pattern: &str, p: &Project, region: Option<&(u32, String)>, unix_seconds: u64) -> String {
    let days = (unix_seconds / 86_400) as i64;
    let secs = unix_seconds % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + (month <= 2) as i64;
    let mut vars: Vec<(&str, String)> = vec![
        ("$regionnumber", region.map(|r| r.0.to_string()).unwrap_or_default()),
        ("$regionname", region.map(|r| r.1.clone()).unwrap_or_default()),
        ("$region", region.map(|r| r.1.clone()).unwrap_or_default()),
        ("$project", p.name()),
        ("$year", format!("{year:04}")),
        ("$month", format!("{month:02}")),
        ("$day", format!("{day:02}")),
        ("$hour", format!("{:02}", secs / 3600)),
        ("$minute", format!("{:02}", secs / 60 % 60)),
    ];
    // Longest names first, so `$regionname` is not read as `$region` + `name`.
    vars.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
    let mut out = pattern.to_owned();
    for (k, v) in vars {
        let safe: String = v.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '\0') { '_' } else { c }).collect();
        out = out.replace(k, &safe);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpp::project_from_rpp;

    const PROJECT: &str = "<REAPER_PROJECT 0.1 \"7.73\" 0\n  RENDER_FILE /out/split\n  RENDER_PATTERN $regionnumber-$regionname\n  RENDER_FMT 0 2 44100\n  RENDER_RANGE 3 0 0 0 1000\n  <RENDER_CFG\n    ZXZhdxgAAQ==\n  >\n  MARKER 1 10 Intro 1\n  MARKER 1 20 \"\" 1\n  MARKER 2 30 \"Verse/Chorus\" 1\n  MARKER 2 45 \"\" 1\n>\n";

    #[test]
    fn reads_settings_and_expands_regions() {
        let p = project_from_rpp(PROJECT).unwrap();
        let s = RenderSettings::from_project(&p);
        assert_eq!(
            (s.channels, s.sample_rate, s.bounds.clone(), s.format.clone()),
            (2, Some(44100), Bounds::Regions, Format::Wav { bits: 24 })
        );
        let jobs = s.jobs(&p, 0).unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0], (TimeRange::new(10.0, 20.0), PathBuf::from("/out/split/1-Intro.wav")));
        assert_eq!(jobs[1].1, PathBuf::from("/out/split/2-Verse_Chorus.wav"));
    }

    #[test]
    fn mp3_sinks_are_recognised() {
        let text = PROJECT.replace("ZXZhdxgAAQ==", "bDNwbUABAAAAAAAAAAAAAP////8EAAAAQAEAAAAAAAA=");
        let s = RenderSettings::from_project(&project_from_rpp(&text).unwrap());
        assert_eq!(s.format, Format::Mp3 { kbps: 320 });
        assert_eq!(s.jobs(&project_from_rpp(&text).unwrap(), 0).unwrap()[0].1, PathBuf::from("/out/split/1-Intro.mp3"));
    }
}
