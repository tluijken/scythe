//! A project's own render settings (`RENDER_FILE`, `RENDER_PATTERN`, `RENDER_FMT`,
//! `RENDER_RANGE`, `RENDER_RESAMPLE`, `<RENDER_CFG`), turned into concrete render jobs.

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
    /// WAV with this many bits: 32 is float, 33 is 32-bit integer.
    Wav { bits: u16 },
    /// MP3 at a constant bitrate.
    Mp3 { kbps: u32 },
    /// FLAC with this many bits per sample and compression level 0-8.
    Flac { bits: u16, level: u32 },
    /// Ogg Vorbis: variable bitrate at `quality` (0-1), or managed around `kbps` when `vbr` is off.
    Vorbis { vbr: bool, quality: f32, kbps: u32 },
    /// Ogg Opus at `kbps` per channel; `mode` 0 VBR, 1 constrained VBR, 2 hard CBR;
    /// encoder complexity 0-10.
    Opus { kbps: f32, mode: u8, complexity: u8 },
    /// WavPack lossless with this many bits (32 with `float` for floating point); `mode` 0
    /// normal, 1 and 2 fast, 3 high, 4 very high.
    WavPack { bits: u16, float: bool, mode: u32 },
    /// Audio CD image: a cue sheet and a raw 44.1 kHz 16-bit stereo `.bin`.
    CdImage(CdImage),
    /// DDP 2.00 fileset for CD manufacturing, laid out from `#`, `!` and `@` markers.
    Ddp,
    /// Another sink, by its four-character id as REAPER prints it (`l3pm` is MP3).
    Other(String),
}

/// How REAPER's CD image sink splits the render into tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CdTracks {
    /// Each marker starts a track, at least 4 seconds after the previous one.
    Markers,
    /// Each region is a track; audio outside regions is left out.
    Regions,
    /// The whole render is one track.
    Single,
}

/// The CD image sink's settings (`RENDER_CFG` id ` osi`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CdImage {
    pub tracks: CdTracks,
    /// Silence before the first track, in milliseconds.
    pub lead_in_ms: u32,
    /// Only markers whose names start with `#` start tracks; the `#` is left out of titles.
    pub hash_markers_only: bool,
}

/// How media at another sample rate is converted for a render (`RENDER_RESAMPLE <mode>`, in
/// the order of REAPER's `Resample_EnumModes`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResampleMode {
    /// Windowed sinc over `half` input frames each side; `tabled` kernels are read from a table
    /// of 32 steps per frame with linear interpolation.
    Sinc {
        half: u16,
        tabled: bool,
    },
    Linear,
    Point,
    /// Linear interpolation with `stages` 2-pole low-pass filters.
    LinearIir {
        stages: u8,
    },
    /// r8brain free; converted with the 192-point sinc.
    R8brain,
}

impl Default for ResampleMode {
    /// The 192-point sinc, REAPER's render default.
    fn default() -> Self {
        Self::Sinc { half: 96, tabled: true }
    }
}

impl ResampleMode {
    pub fn from_reaper(mode: i64) -> Self {
        match mode {
            0 => Self::Sinc { half: 32, tabled: true },
            1 => Self::Linear,
            2 => Self::Point,
            4 => Self::Sinc { half: 192, tabled: false },
            5 => Self::LinearIir { stages: 1 },
            6 => Self::LinearIir { stages: 2 },
            7 => Self::Sinc { half: 8, tabled: true },
            8 => Self::Sinc { half: 256, tabled: false },
            9 => Self::Sinc { half: 384, tabled: false },
            10 => Self::R8brain,
            _ => Self::default(),
        }
    }
}

/// What a normalised render measures (`RENDER_NORMALIZE` bits 1-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormalizeSource {
    LufsIntegrated,
    Rms,
    Peak,
    TruePeak,
    LufsMomentaryMax,
    LufsShortTermMax,
}

/// `RENDER_NORMALIZE <flags> <target>`: scale the render so its measure hits the target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Normalize {
    pub source: NormalizeSource,
    /// Target as an amplitude (LUFS and dB values stored as `10^(x/20)`).
    pub target: f64,
}

impl Normalize {
    pub fn parse(flags: i64, target: f64) -> Option<Self> {
        if flags & 1 == 0 || target.is_nan() || target <= 0.0 {
            return None;
        }
        let source = match (flags >> 1) & 7 {
            1 => NormalizeSource::Rms,
            2 => NormalizeSource::Peak,
            3 => NormalizeSource::TruePeak,
            4 => NormalizeSource::LufsMomentaryMax,
            5 => NormalizeSource::LufsShortTermMax,
            _ => NormalizeSource::LufsIntegrated,
        };
        Some(Self { source, target })
    }

    pub fn target_db(&self) -> f64 {
        20.0 * self.target.log10()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RenderSettings {
    pub file: String,
    pub pattern: String,
    pub channels: u16,
    pub sample_rate: Option<u32>,
    pub bounds: Bounds,
    pub format: Format,
    pub normalize: Option<Normalize>,
    /// `RENDER_DITHER` bit 0: dither the master render; bit 1: noise-shape it.
    pub dither: bool,
    pub noise_shape: bool,
    pub resample: ResampleMode,
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
            normalize: None,
            dither: false,
            noise_shape: false,
            resample: ResampleMode::default(),
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
                            s.channels = num(2).map_or(2, |c| c as u16).clamp(1, 64);
                            s.sample_rate = num(3).filter(|r| *r > 0.0).map(|r| r as u32);
                        }
                        "RENDER_NORMALIZE" => {
                            s.normalize = Normalize::parse(num(1).unwrap_or(0.0) as i64, num(2).unwrap_or(0.0));
                        }
                        "RENDER_RESAMPLE" => s.resample = ResampleMode::from_reaper(num(1).unwrap_or(3.0) as i64),
                        "RENDER_DITHER" => {
                            let flags = num(1).unwrap_or(0.0) as i64;
                            (s.dither, s.noise_shape) = (flags & 1 != 0, flags & 2 != 0);
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

    /// Ranges and output paths to render; the extension follows the format (sinks without an
    /// encoder here get `.wav`) and is not added twice.
    pub fn jobs(&self, p: &Project, unix_seconds: u64) -> Result<Vec<(TimeRange, PathBuf)>, String> {
        let ranges: Vec<(TimeRange, Option<(u32, String)>)> = match &self.bounds {
            // REAPER renders custom bounds to the nearest millisecond.
            Bounds::Custom(r) => {
                let ms = |t: f64| (t * 1000.0).round() / 1000.0;
                vec![(TimeRange::new(ms(r.start), ms(r.end)), None)]
            }
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
                let ext = match self.format {
                    Format::Mp3 { .. } => "mp3",
                    Format::Flac { .. } => "flac",
                    Format::Vorbis { .. } => "ogg",
                    Format::Opus { .. } => "opus",
                    Format::WavPack { .. } => "wv",
                    Format::CdImage(_) => "cue",
                    Format::Ddp => "DAT",
                    _ => "wav",
                };
                if path.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)) {
                    return (range, path);
                }
                let mut path = path.into_os_string();
                path.push(".");
                path.push(ext);
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
        "calf" => Format::Flac {
            bits: u32_at(&bytes, 4).map_or(24, |b| b as u16),
            level: u32_at(&bytes, 8).unwrap_or(5).min(8),
        },
        "vggo" => Format::Vorbis {
            vbr: bytes.get(8).is_none_or(|m| *m == 0),
            quality: bytes.get(4..8).map_or(0.5, |b| f32::from_le_bytes(b.try_into().unwrap())).clamp(0.0, 1.0),
            kbps: u32_at(&bytes, 9).unwrap_or(128).clamp(32, 500),
        },
        "SggO" => Format::Opus {
            kbps: bytes.get(4..8).map_or(128.0, |b| f32::from_le_bytes(b.try_into().unwrap())).clamp(1.0, 512.0),
            mode: bytes.get(8).copied().unwrap_or(0).min(2),
            complexity: bytes.get(9).copied().unwrap_or(10).min(10),
        },
        "kpvw" => {
            let (bits, float) = match u32_at(&bytes, 8).unwrap_or(1) {
                0 => (16, false),
                2 => (32, false),
                3 => (32, true),
                _ => (24, false),
            };
            Format::WavPack { bits, float, mode: u32_at(&bytes, 4).unwrap_or(0) }
        }
        // The CD sink stores five integers: lead-in in milliseconds, two burning options,
        // the track mode and the `#`-only flag.
        " osi" => Format::CdImage(CdImage {
            lead_in_ms: u32_at(&bytes, 4).unwrap_or(0),
            tracks: match u32_at(&bytes, 16).unwrap_or(0) {
                1 => CdTracks::Regions,
                2 => CdTracks::Single,
                _ => CdTracks::Markers,
            },
            hash_markers_only: u32_at(&bytes, 20).unwrap_or(0) != 0,
        }),
        " pdd" => Format::Ddp,
        _ => Format::Other(id),
    })
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
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

    #[test]
    fn an_existing_extension_is_kept() {
        let mut p = project_from_rpp(PROJECT).unwrap();
        let mut s = RenderSettings::from_project(&p);
        s.file = "/out/mix.WAV".into();
        s.pattern.clear();
        s.bounds = Bounds::Project;
        p.tracks.clear();
        assert_eq!(s.jobs(&p, 0).unwrap()[0].1, PathBuf::from("/out/mix.WAV"));
    }

    #[test]
    fn flac_and_vorbis_sinks_use_reapers_defaults() {
        let format = |cfg: &str| {
            RenderSettings::from_project(&project_from_rpp(&PROJECT.replace("ZXZhdxgAAQ==", cfg)).unwrap()).format
        };
        assert_eq!(format("Y2FsZhAAAAAFAAAA"), Format::Flac { bits: 16, level: 5 });
        assert_eq!(
            format("dmdnbwAAAD8AgAAAAIAAAAAgAAAAAAEAAA=="),
            Format::Vorbis { vbr: true, quality: 0.5, kbps: 128 }
        );
        // REAPER 7.73's defaults for its Opus and WavPack sinks.
        assert_eq!(format("U2dnTwAAAEMACgAAAAAAAAA="), Format::Opus { kbps: 128.0, mode: 0, complexity: 10 });
        assert_eq!(format("a3B2dwAAAAABAAAAAAAAAAAAAAA="), Format::WavPack { bits: 24, float: false, mode: 0 });
        // 32-bit integer WAV, as REAPER 7.73 stores it.
        assert_eq!(format("ZXZhdyEAAQ=="), Format::Wav { bits: 33 });
    }

    #[test]
    fn cd_image_and_ddp_sinks() {
        let project = |cfg: &str| project_from_rpp(&PROJECT.replace("ZXZhdxgAAQ==", cfg)).unwrap();
        let format = |cfg: &str| RenderSettings::from_project(&project(cfg)).format;
        // REAPER's defaults, then regions with a 333 ms lead-in.
        assert_eq!(
            format("IG9zaQAAAAAAAAAAAAAAAAAAAAAAAAAA"),
            Format::CdImage(CdImage { tracks: CdTracks::Markers, lead_in_ms: 0, hash_markers_only: false })
        );
        assert_eq!(
            format("IG9zaU0BAAAAAAAAAAAAAAEAAAABAAAA"),
            Format::CdImage(CdImage { tracks: CdTracks::Regions, lead_in_ms: 333, hash_markers_only: true })
        );
        assert_eq!(format("IHBkZA=="), Format::Ddp);
        let p = project("IHBkZA==");
        assert_eq!(RenderSettings::from_project(&p).jobs(&p, 0).unwrap()[0].1, PathBuf::from("/out/split/1-Intro.DAT"));
    }

    #[test]
    fn custom_bounds_end_on_a_millisecond() {
        let p = crate::rpp::project_from_rpp(
            "<REAPER_PROJECT 0.1 \"7.73\" 0\n  RENDER_RANGE 0 1.00004 17.3333 0 1000\n>\n",
        )
        .unwrap();
        let r = RenderSettings::from_project(&p).jobs(&p, 0).unwrap()[0].0;
        assert_eq!((r.start, r.end), (1.0, 17.333));
    }

    #[test]
    fn normalize_and_dither() {
        let p = crate::rpp::project_from_rpp(
            "<REAPER_PROJECT 0.1 \"7.73\" 0\n  RENDER_DITHER 3\n  RENDER_NORMALIZE 5 0.891251 0 0 0 1 1\n>\n",
        )
        .unwrap();
        let s = RenderSettings::from_project(&p);
        let n = s.normalize.unwrap();
        assert_eq!(n.source, NormalizeSource::Peak);
        assert!((n.target_db() + 1.0).abs() < 1e-4);
        assert!(s.dither && s.noise_shape);
        assert_eq!(Normalize::parse(1, 0.199526).unwrap().source, NormalizeSource::LufsIntegrated);
        assert_eq!(Normalize::parse(4, 0.5), None);
    }

    #[test]
    fn resample_mode() {
        let read = |text: &str| RenderSettings::from_project(&crate::rpp::project_from_rpp(text).unwrap()).resample;
        assert_eq!(read("<REAPER_PROJECT 0.1 \"7.73\" 0\n>\n"), ResampleMode::default());
        assert_eq!(read("<REAPER_PROJECT 0.1 \"7.73\" 0\n  RENDER_RESAMPLE 3 0 1\n>\n"), ResampleMode::default());
        assert_eq!(
            read("<REAPER_PROJECT 0.1 \"7.73\" 0\n  RENDER_RESAMPLE 9 0 1\n>\n"),
            ResampleMode::Sinc { half: 384, tabled: false }
        );
        assert_eq!(read("<REAPER_PROJECT 0.1 \"7.73\" 0\n  RENDER_RESAMPLE 1 0 1\n>\n"), ResampleMode::Linear);
    }
}
