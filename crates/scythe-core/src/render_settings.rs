//! A project's own render settings (`RENDER_FILE`, `RENDER_PATTERN`, `RENDER_FMT`,
//! `RENDER_RANGE`, `RENDER_RESAMPLE`, `<RENDER_CFG`), turned into concrete render jobs.

use std::path::{Path, PathBuf};

use crate::model::{ItemId, Project, TimeRange, TrackId};
use crate::rpp::{Node, tokenize};

#[derive(Debug, Clone, PartialEq)]
pub enum Bounds {
    Custom(TimeRange),
    Project,
    TimeSelection,
    Regions,
    /// Each selected item's own extent.
    SelectedItems,
    /// The selected regions; with no region selection (Scythe keeps none, and REAPER's
    /// `-renderproject` has none) every region.
    SelectedRegions,
    /// Bounds this reimplementation does not handle (razor areas, markers …).
    Other(i64),
}

impl Bounds {
    /// REAPER's `RENDER_BOUNDSFLAG` number.
    pub fn code(&self) -> i64 {
        match self {
            Bounds::Custom(_) => 0,
            Bounds::Project => 1,
            Bounds::TimeSelection => 2,
            Bounds::Regions => 3,
            Bounds::SelectedItems => 4,
            Bounds::SelectedRegions => 5,
            Bounds::Other(n) => *n,
        }
    }

    fn regions(&self) -> bool {
        matches!(self, Bounds::Regions | Bounds::SelectedRegions)
    }
}

/// What a render job plays: the master mix, or part of the project on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderSource {
    Master,
    /// A track's own output (a stem): its items, children and receives through its FX and
    /// fader, but neither its parent folders nor the master.
    Stem(TrackId),
    /// A track (and its children) soloed through the master.
    TrackViaMaster(TrackId),
    /// One item alone through its own track.
    Item(ItemId),
    /// One item alone through its track, folders and the master.
    ItemViaMaster(ItemId),
}

/// One file to render.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderJob {
    pub range: TimeRange,
    pub path: PathBuf,
    pub source: RenderSource,
}

/// The tracks and items a render takes as "selected".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RenderSelection {
    pub tracks: Vec<TrackId>,
    pub items: Vec<ItemId>,
}

impl RenderSelection {
    /// The selection the project file saved (`SEL 1` on tracks and items), which is what
    /// REAPER's `-renderproject` renders.
    pub fn saved(p: &Project) -> Self {
        let selected = |lines: &crate::model::LineMemo, extra: &[Node]| {
            let on = |t: &[String]| t.first().map(String::as_str) == Some("SEL") && t.get(1).is_some_and(|v| v != "0");
            lines.get("SEL").is_some_and(|t| on(t))
                || extra.iter().any(|n| matches!(n, Node::Line(l) if on(&tokenize(l))))
        };
        Self {
            tracks: p.tracks.iter().filter(|t| selected(&t.lines, &t.extra)).map(|t| t.id).collect(),
            items: p
                .tracks
                .iter()
                .flat_map(|t| &t.items)
                .filter(|i| selected(&i.lines, &i.extra))
                .map(|i| i.id)
                .collect(),
        }
    }
}

/// `RENDER_STEMS`, REAPER's render source flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SourceFlags(pub i64);

impl SourceFlags {
    /// Bits 1 and 2: 0 the master mix, 1 the selected tracks' stems and the master, 3 the
    /// stems alone; 2 alone renders nothing.
    pub fn master(self) -> bool {
        self.0 & 3 <= 1
    }

    pub fn stems(self) -> bool {
        self.0 & 1 != 0
    }

    pub fn matrix(self) -> bool {
        self.0 & 8 != 0
    }

    pub fn items(self) -> bool {
        self.0 & 32 != 0
    }

    pub fn items_via_master(self) -> bool {
        self.0 & 64 != 0
    }

    pub fn tracks_via_master(self) -> bool {
        self.0 & 128 != 0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Format {
    /// WAV with this many bits: 8 is unsigned, 32 is float, 33 is 32-bit integer, 64 is double;
    /// 4 is IMA ADPCM, 2 REAPER's 2-bit cADPCM and 14 8-bit u-law.
    Wav { bits: u16, options: WavOptions },
    /// AIFF with 8, 16, 24 or 32-bit integer samples; `tempo` adds the beat count when the
    /// render is a whole number of beats.
    Aiff { bits: u16, tempo: bool },
    /// Core Audio Format, with WAV's PCM and float depths.
    Caf { bits: u16, markers: MarkerExport, tempo: bool },
    /// Animated GIF of the video.
    Gif(Animation),
    /// LICEcap's LCF animation of the video.
    Lcf(Animation),
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
    /// REAPER's FFmpeg video sink: a container with a video and an audio stream.
    Video(Video),
    /// Another sink, by its four-character id as REAPER prints it (`l3pm` is MP3).
    Other(String),
}

/// Which project markers and regions a WAV or CAF render embeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MarkerExport {
    pub markers: bool,
    pub regions: bool,
    /// Only those whose names start with `#` (the `#` is kept).
    pub hash_only: bool,
}

impl MarkerExport {
    /// The sinks' flag byte: 8 embeds, 0x40 limits it to markers and 0x80 to regions (both
    /// together mean both), 0x10 to names starting with `#`.
    fn from_flags(flags: u8) -> Self {
        let on = flags & 8 != 0;
        let (markers_only, regions_only) = (flags & 0x40 != 0, flags & 0x80 != 0);
        Self {
            markers: on && (markers_only || !regions_only),
            regions: on && (regions_only || !markers_only),
            hash_only: flags & 0x10 != 0,
        }
    }

    pub fn any(self) -> bool {
        self.markers || self.regions
    }
}

/// What a WAV render becomes when it outgrows 4 GB, or is always written as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LargeWav {
    /// WAV, with room in the header to turn into Wave64 (or RF64, or BW64).
    AutoWave64,
    AutoRf64,
    AutoBw64,
    /// WAV without that room.
    Wav,
    Wave64,
    Rf64,
    Bw64,
}

/// The WAV sink's options (`RENDER_CFG` id `evaw`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WavOptions {
    /// Write a Broadcast Wave `bext` chunk.
    pub bext: bool,
    /// Put the project file in the `bext` description (`RPP:<path>`).
    pub project_in_description: bool,
    pub markers: MarkerExport,
    /// An ACID chunk with the project tempo.
    pub tempo: bool,
    pub large: LargeWav,
}

impl Default for WavOptions {
    /// REAPER 7.73's defaults: `bext`, room for RF64.
    fn default() -> Self {
        Self::from_bytes(0, 1)
    }
}

impl WavOptions {
    /// The two bytes after the bit depth. The first: 1 leaves `bext` out, 2 forces WAV (over
    /// any forced form), 4 names the project, 0x20 writes the tempo, and the marker flags. The
    /// second: 0 Wave64, 1 RF64, 2 and 3 the same forced, 4 makes RF64 BW64.
    pub fn from_bytes(flags: u8, large: u8) -> Self {
        let large = match (flags & 2 != 0, large & 3, large & 4 != 0) {
            (true, ..) => LargeWav::Wav,
            (_, 0, _) => LargeWav::AutoWave64,
            (_, 1, false) => LargeWav::AutoRf64,
            (_, 1, true) => LargeWav::AutoBw64,
            (_, 2, _) => LargeWav::Wave64,
            (_, _, false) => LargeWav::Rf64,
            (_, _, true) => LargeWav::Bw64,
        };
        Self {
            bext: flags & 1 == 0,
            project_in_description: flags & 4 != 0,
            markers: MarkerExport::from_flags(flags),
            tempo: flags & 0x20 != 0,
            large,
        }
    }
}

/// REAPER's GIF and LCF sinks: frame size and the highest frame rate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Animation {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// GIF: keep a transparent colour.
    pub transparent: bool,
}

impl Animation {
    fn parse(bytes: &[u8]) -> Self {
        let size = |at, default| u32_at(bytes, at).filter(|n| *n > 0).unwrap_or(default);
        let fps = bytes.get(12..16).map_or(30.0, |b| f32::from_le_bytes(b.try_into().unwrap()));
        Self {
            width: size(4, 640),
            height: size(8, 360),
            fps: if fps.is_finite() && fps > 0.0 { fps as f64 } else { 30.0 },
            transparent: bytes.get(17).is_some_and(|b| *b != 0),
        }
    }
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

/// The containers of REAPER's video sink, in the order of its format list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Avi,
    Mpeg1,
    /// An MPEG transport stream.
    Mpeg2,
    /// "QT/MOV/MP4 (deprecated)": the MOV file, named `.mp4`.
    QuickTimeLegacy,
    Matroska,
    Flv,
    WebM,
    Mp4,
    Mov,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    H265,
    Av1,
    Vp8,
    Vp9,
    Mpeg1,
    Mpeg2,
    /// MPEG-4 part 2 tagged `xvid`.
    Xvid,
    Flv1,
    Ffv1,
    Huffyuv,
    Mjpeg,
    Dv,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    Mp3,
    Aac,
    Ac3,
    Mp2,
    Vorbis,
    Opus,
    Pcm16,
    Pcm24,
    Float32,
    DvAudio,
}

impl Container {
    fn from_index(i: u32) -> Self {
        use Container::*;
        // Out-of-range formats render as MPEG-4.
        [Avi, Mpeg1, Mpeg2, QuickTimeLegacy, Matroska, Flv, WebM, Mp4, Mov].get(i as usize).copied().unwrap_or(Mp4)
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Avi => "avi",
            Self::Mpeg1 | Self::Mpeg2 => "mpg",
            Self::QuickTimeLegacy | Self::Mp4 => "mp4",
            Self::Matroska => "mkv",
            Self::Flv => "flv",
            Self::WebM => "webm",
            Self::Mov => "mov",
        }
    }

    /// The codec list the sink offers for this container; `None` is the "NONE" entry.
    pub fn video_codecs(self) -> &'static [Option<VideoCodec>] {
        use VideoCodec::*;
        match self {
            Self::Avi => &[Some(Xvid), Some(H264), Some(Dv), Some(Mjpeg), Some(Ffv1), Some(Huffyuv), Some(Mpeg2), None],
            Self::Mpeg1 => &[Some(Mpeg1), None],
            Self::Mpeg2 => &[Some(Mpeg2), None],
            Self::QuickTimeLegacy => &[Some(H264), Some(Mpeg2), Some(Mjpeg), None],
            Self::Matroska => &[
                Some(H264),
                Some(Xvid),
                Some(Ffv1),
                Some(Huffyuv),
                Some(Mjpeg),
                Some(Mpeg2),
                None,
                Some(H265),
                Some(Av1),
            ],
            Self::Flv => &[Some(H264), Some(Flv1), None],
            Self::WebM => &[Some(Vp8), Some(Vp9), Some(Av1), None],
            Self::Mp4 | Self::Mov => &[Some(H264), Some(H265), Some(Av1), Some(Mpeg2), Some(Mjpeg), None],
        }
    }

    pub fn audio_codecs(self) -> &'static [Option<AudioCodec>] {
        use AudioCodec::*;
        match self {
            Self::Avi => {
                &[Some(Mp3), Some(Aac), Some(Ac3), Some(Pcm16), Some(Pcm24), Some(Float32), Some(DvAudio), None]
            }
            Self::Mpeg1 => &[Some(Mp3), Some(Mp2), None],
            Self::Mpeg2 => &[Some(Aac), Some(Mp3), Some(Mp2), None],
            Self::Matroska => &[Some(Mp3), Some(Aac), Some(Pcm16), Some(Pcm24), Some(Float32), None],
            Self::Flv => &[Some(Mp3), Some(Aac), None],
            Self::WebM => &[Some(Vorbis), Some(Opus), None],
            Self::QuickTimeLegacy | Self::Mp4 | Self::Mov => {
                &[Some(Aac), Some(Mp3), Some(Pcm16), Some(Pcm24), Some(Float32), None]
            }
        }
    }
}

/// REAPER's video sink settings (`RENDER_CFG` id `PMFF`).
#[derive(Debug, Clone, PartialEq)]
pub struct Video {
    pub container: Container,
    pub video: Option<VideoCodec>,
    pub audio: Option<AudioCodec>,
    pub video_kbps: u32,
    pub audio_kbps: u32,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// Scale video items into the frame keeping their aspect ratio, with black bars.
    pub keep_aspect: bool,
    /// The quality factor (0-100) of codecs that have no bitrate (MJPEG).
    pub quality: u32,
    /// FFmpeg encoder options: `key=value` pairs, up to a `;` or `#` comment.
    pub video_options: String,
    pub audio_options: String,
}

impl Video {
    /// The sink stores the container and codecs as list indexes (an index past a list picks
    /// its first entry), then the bitrates, the frame size, the frame rate as a float, the
    /// aspect flag, the quality factor and the audio, then the video encoder options as
    /// NUL-terminated strings. A side of 0 follows 16:9 from the other, both 0 is 1920x1080
    /// (the project's video size is not consulted), and odd sizes grow to even.
    fn parse(bytes: &[u8]) -> Self {
        let container = Container::from_index(u32_at(bytes, 4).unwrap_or(7));
        fn pick<T: Copy>(list: &[Option<T>], index: Option<u32>) -> Option<T> {
            *list.get(index.unwrap_or(0) as usize).unwrap_or(&list[0])
        }
        let side = |at| u32_at(bytes, at).map_or(0, |n| (n as i32).clamp(0, 16384) as u32);
        let (width, height) = match (side(24), side(28)) {
            (0, 0) => (1920, 1080),
            (w, 0) => (w, w * 9 / 16),
            (0, h) => (h * 16 / 9, h),
            size => size,
        };
        let fps = bytes.get(32..36).map_or(30.0, |b| f32::from_le_bytes(b.try_into().unwrap()));
        let mut strings = bytes.get(44..).unwrap_or_default().split(|&b| b == 0);
        let mut text = || strings.next().map(|s| String::from_utf8_lossy(s).into_owned()).unwrap_or_default();
        let audio_options = text();
        let video_options = text();
        Self {
            container,
            video: pick(container.video_codecs(), u32_at(bytes, 8)),
            audio: pick(container.audio_codecs(), u32_at(bytes, 16)),
            video_kbps: u32_at(bytes, 12).unwrap_or(2048),
            audio_kbps: u32_at(bytes, 20).unwrap_or(128),
            width: width.next_multiple_of(2),
            height: height.next_multiple_of(2),
            fps: if fps.is_finite() && fps > 0.0 { fps as f64 } else { 30.0 },
            keep_aspect: u32_at(bytes, 36).unwrap_or(1) != 0,
            quality: u32_at(bytes, 40).unwrap_or(95),
            video_options,
            audio_options,
        }
    }
}

/// How media at another sample rate, and takes at another play rate without pitch
/// preservation, are converted (`RENDER_RESAMPLE <mode>` for renders, in the order of REAPER's
/// `Resample_EnumModes`).
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
    /// r8brain free, with r8brain-free-src's default settings.
    R8brain,
}

impl Default for ResampleMode {
    /// The 192-point sinc, REAPER's render default.
    fn default() -> Self {
        Self::Sinc { half: 96, tabled: true }
    }
}

impl ResampleMode {
    /// REAPER's default playback mode (`playresamplemode=0` in reaper.ini), the 64-point sinc.
    pub const PLAYBACK: Self = Self::Sinc { half: 32, tabled: true };

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

/// The rate of a project that sets none (no `SAMPLERATE` line), as REAPER opens it.
pub const DEFAULT_PROJECT_RATE: u32 = 44_100;

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
    pub source: SourceFlags,
    /// `<RENDER_METADATA` tags (`BWF:Description`, `IXML:PROJECT` …) in REAPER's order, when
    /// the render embeds them (`RENDER_STEMS` flag 512); empty otherwise.
    pub metadata: Vec<(String, String)>,
}

/// Sets field `field` (1-based, after the key) of the project's `key` render line, adding the
/// line with `defaults` when the project has none.
pub fn set_project_field(p: &mut Project, key: &str, defaults: &[&str], field: usize, value: &str) {
    let at =
        p.extra.iter().position(|n| matches!(n, Node::Line(l) if tokenize(l).first().map(String::as_str) == Some(key)));
    let mut tokens = match at {
        Some(i) => match &p.extra[i] {
            Node::Line(l) => tokenize(l),
            Node::Chunk(_) => unreachable!(),
        },
        None => std::iter::once(key.to_owned()).chain(defaults.iter().map(|d| d.to_string())).collect(),
    };
    if tokens.len() <= field {
        tokens.resize(field + 1, "0".into());
    }
    tokens[field] = value.to_owned();
    let line = Node::Line(crate::rpp::join_tokens(tokens));
    match at {
        Some(i) => p.extra[i] = line,
        None => p.extra.push(line),
    }
}

impl RenderSettings {
    pub fn from_project(p: &Project) -> Self {
        let mut s = Self {
            file: String::new(),
            pattern: String::new(),
            channels: 2,
            sample_rate: None,
            bounds: Bounds::Project,
            format: Format::Wav { bits: 24, options: WavOptions::default() },
            normalize: None,
            dither: false,
            noise_shape: false,
            resample: ResampleMode::default(),
            source: SourceFlags::default(),
            metadata: Vec::new(),
        };
        let mut embed_metadata = false;
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
                        "RENDER_STEMS" => {
                            s.source = SourceFlags(num(1).unwrap_or(0.0) as i64);
                            embed_metadata = s.source.0 & 512 != 0;
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
                                4 => Bounds::SelectedItems,
                                5 => Bounds::SelectedRegions,
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
                Node::Chunk(c) if c.name() == "RENDER_METADATA" => {
                    s.metadata = c
                        .children
                        .iter()
                        .filter_map(|n| match n {
                            Node::Line(l) => {
                                let t = tokenize(l);
                                (t.len() >= 3 && t[0] == "TAG").then(|| (t[1].clone(), t[2].clone()))
                            }
                            _ => None,
                        })
                        .collect();
                }
                Node::Chunk(_) => {}
            }
        }
        if !embed_metadata {
            s.metadata.clear();
        }
        s
    }

    /// The rate a render runs at: the render settings' own (`RENDER_FMT`, REAPER's
    /// `RENDER_SRATE`), else the project's. As in REAPER, the audio device's rate plays no part.
    pub fn render_rate(&self, p: &Project) -> u32 {
        self.sample_rate.or(p.sample_rate).unwrap_or(DEFAULT_PROJECT_RATE)
    }

    /// The files to render, in REAPER's order, for the project saved selection (see
    /// [`RenderSelection::saved`]) or the editor's. The extension follows the format (sinks
    /// without an encoder here get `.wav`) and is not added twice; names that come out the
    /// same get REAPER's `-001`, `-002` … suffixes.
    pub fn jobs(&self, p: &Project, sel: &RenderSelection, unix_seconds: u64) -> Result<Vec<RenderJob>, String> {
        let mut planned: Vec<(TimeRange, RenderSource, Names)> = Vec::new();
        let source = self.source;
        if self.bounds == Bounds::SelectedItems || source.items() || source.items_via_master() {
            if self.bounds != Bounds::SelectedItems {
                return Err("selected media items render only with selected-items bounds".into());
            }
            for (ti, t) in p.tracks.iter().enumerate() {
                for (ii, item) in t.items.iter().enumerate() {
                    if !sel.items.contains(&item.id) {
                        continue;
                    }
                    let src = if source.items_via_master() {
                        RenderSource::ItemViaMaster(item.id)
                    } else {
                        RenderSource::Item(item.id)
                    };
                    let names = Names {
                        track: Some((ti + 1, t.name.clone())),
                        item: Some((ii + 1, item.take().map(|tk| tk.name.clone()).unwrap_or_default())),
                        region: None,
                    };
                    planned.push((TimeRange::new(item.position, item.end()), src, names));
                }
            }
        } else if source.matrix() {
            if source.0 & 3 != 0 {
                return Err("the region render matrix renders neither stems nor the master on top".into());
            }
            for m in p.markers.iter().filter(|m| m.region_end.is_some()) {
                let range = TimeRange::new(m.position, m.region_end.unwrap_or(m.position));
                let region = Some((m.index, m.name.clone()));
                if m.renders_master() {
                    planned.push((
                        range,
                        RenderSource::Master,
                        Names { track: Some((0, "Master".into())), item: None, region: region.clone() },
                    ));
                }
                for (ti, t) in p.tracks.iter().enumerate() {
                    if m.render_matrix.iter().any(|r| r.guid.eq_ignore_ascii_case(&t.guid)) {
                        let names = Names { track: Some((ti + 1, t.name.clone())), item: None, region: region.clone() };
                        planned.push((range, RenderSource::Stem(t.id), names));
                    }
                }
            }
        } else {
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
                Bounds::Regions | Bounds::SelectedRegions => p
                    .markers
                    .iter()
                    .filter_map(|m| {
                        m.region_end.map(|end| (TimeRange::new(m.position, end), Some((m.index, m.name.clone()))))
                    })
                    .collect(),
                Bounds::SelectedItems => unreachable!(),
                Bounds::Other(n) => return Err(format!("render bounds mode {n} is not supported")),
            };
            let selected: Vec<(usize, &crate::model::Track)> =
                p.tracks.iter().enumerate().filter(|(_, t)| sel.tracks.contains(&t.id)).collect();
            for (range, region) in ranges {
                if source.tracks_via_master() {
                    if source.0 & 3 == 0 {
                        for &(ti, t) in &selected {
                            let names =
                                Names { track: Some((ti + 1, t.name.clone())), item: None, region: region.clone() };
                            planned.push((range, RenderSource::TrackViaMaster(t.id), names));
                        }
                    }
                    continue;
                }
                if source.stems() {
                    for &(ti, t) in &selected {
                        let names = Names { track: Some((ti + 1, t.name.clone())), item: None, region: region.clone() };
                        planned.push((range, RenderSource::Stem(t.id), names));
                    }
                }
                if source.master() {
                    let names = Names { track: Some((0, "Master".into())), item: None, region: region.clone() };
                    planned.push((range, RenderSource::Master, names));
                }
            }
        }
        if planned.is_empty() {
            return Err("nothing to render".into());
        }
        let dir = p.path.as_deref().and_then(Path::parent).map(Path::to_owned).unwrap_or_default();
        let base = match self.file.as_str() {
            "" => dir.clone(),
            f if Path::new(f).is_absolute() => PathBuf::from(f),
            f => dir.join(f),
        };
        let pattern = match (self.pattern.as_str(), self.bounds.regions() && !source.matrix()) {
            ("", true) => "$project-$regionnumber",
            (pat, _) => pat,
        };
        let ext = match &self.format {
            Format::Mp3 { .. } => "mp3",
            Format::Flac { .. } => "flac",
            Format::Vorbis { .. } => "ogg",
            Format::Opus { .. } => "opus",
            Format::WavPack { .. } => "wv",
            Format::CdImage(_) => "cue",
            Format::Ddp => "DAT",
            Format::Video(v) => v.container.extension(),
            Format::Aiff { .. } => "aif",
            Format::Caf { .. } => "caf",
            Format::Gif(_) => "gif",
            Format::Lcf(_) => "lcf",
            Format::Wav { .. } | Format::Other(_) => "wav",
        };
        let stems: Vec<(PathBuf, String)> = planned
            .iter()
            .map(|(_, _, names)| {
                if pattern.is_empty() {
                    if self.file.is_empty() { base.join(p.name()) } else { base.clone() }
                } else {
                    base.join({
                        let name = expand(pattern, p, names, unix_seconds);
                        // Wildcards with nothing to name leave only separators behind.
                        if name.trim_matches(|c: char| " -_.".contains(c)).is_empty() { p.name() } else { name }
                    })
                }
            })
            .map(|path| match path.extension() {
                Some(e) if e.eq_ignore_ascii_case(ext) => {
                    let e = e.to_string_lossy().into_owned();
                    (path.with_extension(""), e)
                }
                _ => (path, ext.to_owned()),
            })
            .collect();
        let mut seen: std::collections::HashMap<&PathBuf, usize> = std::collections::HashMap::new();
        Ok(planned
            .iter()
            .zip(&stems)
            .map(|((range, source, _), (stem, ext))| {
                let mut path = stem.clone().into_os_string();
                if stems.iter().filter(|(s, _)| s == stem).count() > 1 {
                    let n = seen.entry(stem).or_insert(0);
                    *n += 1;
                    path.push(format!("-{n:03}"));
                }
                path.push(".");
                path.push(ext);
                RenderJob { range: *range, path: PathBuf::from(path), source: *source }
            })
            .collect())
    }
}

/// What the render wildcards of one file name: its track (number 0 is the master), item
/// (number within its track) and region.
#[derive(Debug, Clone, Default)]
struct Names {
    track: Option<(usize, String)>,
    item: Option<(usize, String)>,
    region: Option<(u32, String)>,
}

/// `<RENDER_CFG` holds the sink configuration in base64: a four-character id (stored
/// reversed, so WAV prints as `evaw`) followed by sink-specific bytes; for WAV the first is
/// the bit depth.
fn decode_cfg<'a>(lines: impl Iterator<Item = &'a str>) -> Option<Format> {
    let text: String = lines.collect();
    let bytes = decode_base64(&text)?;
    let id = String::from_utf8_lossy(bytes.get(..4)?).into_owned();
    Some(match id.as_str() {
        "evaw" => Format::Wav {
            bits: bytes.get(4).map_or(24, |b| *b as u16),
            options: WavOptions::from_bytes(bytes.get(5).copied().unwrap_or(0), bytes.get(6).copied().unwrap_or(1)),
        },
        // AIFF and CAF: the bit depth, then the flags of the WAV sink's first flag byte.
        "ffia" => Format::Aiff {
            bits: bytes.get(4).map_or(24, |b| *b as u16),
            tempo: bytes.get(5).is_some_and(|f| f & 0x20 != 0),
        },
        "ffac" => {
            let flags = bytes.get(5).copied().unwrap_or(0);
            Format::Caf {
                bits: bytes.get(4).map_or(24, |b| *b as u16),
                markers: MarkerExport::from_flags(flags),
                tempo: flags & 0x20 != 0,
            }
        }
        " FIG" => Format::Gif(Animation::parse(&bytes)),
        " FCL" => Format::Lcf(Animation::parse(&bytes)),
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
        "PMFF" => Format::Video(Video::parse(&bytes)),
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
fn expand(pattern: &str, p: &Project, names: &Names, unix_seconds: u64) -> String {
    let region = names.region.as_ref();
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
        ("$regionnumber", region.map(|r| format!("{:02}", r.0)).unwrap_or_default()),
        ("$tracknumber", names.track.as_ref().map(|t| format!("{:02}", t.0)).unwrap_or_default()),
        ("$track", names.track.as_ref().map(|t| t.1.clone()).unwrap_or_default()),
        ("$itemnumber", names.item.as_ref().map(|i| format!("{:02}", i.0)).unwrap_or_default()),
        ("$item", names.item.as_ref().map(|i| i.1.clone()).unwrap_or_default()),
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
            (2, Some(44100), Bounds::Regions, Format::Wav { bits: 24, options: WavOptions::default() })
        );
        let jobs = s.jobs(&p, &RenderSelection::default(), 0).unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(
            (jobs[0].range, jobs[0].path.clone()),
            (TimeRange::new(10.0, 20.0), PathBuf::from("/out/split/01-Intro.wav"))
        );
        assert_eq!(jobs[1].path, PathBuf::from("/out/split/02-Verse_Chorus.wav"));
    }

    #[test]
    fn mp3_sinks_are_recognised() {
        let text = PROJECT.replace("ZXZhdxgAAQ==", "bDNwbUABAAAAAAAAAAAAAP////8EAAAAQAEAAAAAAAA=");
        let s = RenderSettings::from_project(&project_from_rpp(&text).unwrap());
        assert_eq!(s.format, Format::Mp3 { kbps: 320 });
        assert_eq!(
            s.jobs(&project_from_rpp(&text).unwrap(), &RenderSelection::default(), 0).unwrap()[0].path,
            PathBuf::from("/out/split/01-Intro.mp3")
        );
    }

    #[test]
    fn an_existing_extension_is_kept() {
        let mut p = project_from_rpp(PROJECT).unwrap();
        let mut s = RenderSettings::from_project(&p);
        s.file = "/out/mix.WAV".into();
        s.pattern.clear();
        s.bounds = Bounds::Project;
        p.tracks.clear();
        assert_eq!(s.jobs(&p, &RenderSelection::default(), 0).unwrap()[0].path, PathBuf::from("/out/mix.WAV"));
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
        assert_eq!(format("ZXZhdyEAAQ=="), Format::Wav { bits: 33, options: WavOptions::default() });
        // 8-bit, 64-bit float and IMA ADPCM.
        assert_eq!(format("ZXZhdwgAAQ=="), Format::Wav { bits: 8, options: WavOptions::default() });
        assert_eq!(format("ZXZhd0AAAQ=="), Format::Wav { bits: 64, options: WavOptions::default() });
        assert_eq!(format("ZXZhdwQAAQ=="), Format::Wav { bits: 4, options: WavOptions::default() });
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
        assert_eq!(
            RenderSettings::from_project(&p).jobs(&p, &RenderSelection::default(), 0).unwrap()[0].path,
            PathBuf::from("/out/split/01-Intro.DAT")
        );
    }

    #[test]
    fn video_sink() {
        let project = |cfg: &str| project_from_rpp(&PROJECT.replace("ZXZhdxgAAQ==", cfg)).unwrap();
        let format = |cfg: &str| RenderSettings::from_project(&project(cfg)).format;
        // REAPER 7.73's default: MPEG-4 with H.264 and AAC, 1920x1080 at 30 fps.
        assert_eq!(
            format("UE1GRgcAAAAAAAAAAAgAAAAAAACAAAAAgAcAADgEAAAAAPBBAQAAAF8AAAAAAA=="),
            Format::Video(Video {
                container: Container::Mp4,
                video: Some(VideoCodec::H264),
                audio: Some(AudioCodec::Aac),
                video_kbps: 2048,
                audio_kbps: 128,
                width: 1920,
                height: 1080,
                fps: 30.0,
                keep_aspect: true,
                quality: 95,
                video_options: String::new(),
                audio_options: String::new(),
            })
        );
        // MJPEG at quality 20, 640 wide with no height, encoder options "b=96000" for the
        // audio and "crf=30 g=10" for the video.
        let Format::Video(v) =
            format("UE1GRgQAAAAEAAAAAAgAAAIAAACAAAAAgAIAAAAAAAAAAMhBAQAAABQAAABiPTk2MDAwAGNyZj0zMCBnPTEwAA==")
        else {
            panic!()
        };
        assert_eq!((v.video, v.width, v.height, v.quality), (Some(VideoCodec::Mjpeg), 640, 360, 20));
        assert_eq!((v.audio_options.as_str(), v.video_options.as_str()), ("b=96000", "crf=30 g=10"));
        // Only a height: 16:9, 853 wide grown to even.
        let Format::Video(v) = format("UE1GRgQAAAACAAAAAAgAAAIAAACAAAAAAAAAAOABAAAAAMhBAQAAAF8AAAAAAA==") else {
            panic!()
        };
        assert_eq!((v.width, v.height), (854, 480));
        // Matroska, FFV1 and 16-bit PCM at 320x240, 25 fps; then no video stream.
        let Format::Video(v) = format("UE1GRgQAAAACAAAAAAgAAAIAAACAAAAAQAEAAPAAAAAAAMhBAQAAAF8AAAAAAA==") else {
            panic!()
        };
        assert_eq!(
            (v.container, v.video, v.audio),
            (Container::Matroska, Some(VideoCodec::Ffv1), Some(AudioCodec::Pcm16))
        );
        assert_eq!((v.width, v.height, v.fps), (320, 240, 25.0));
        let Format::Video(v) = format("UE1GRgQAAAAGAAAAAAgAAAIAAACAAAAAQAEAAPAAAAAAAPBBAQAAAF8AAAAAAA==") else {
            panic!()
        };
        assert_eq!(v.video, None);
        let p = project("UE1GRgYAAAABAAAAAAgAAAEAAACAAAAAQAEAAPAAAAAAAPBBAQAAAF8AAAAAAA==");
        let s = RenderSettings::from_project(&p);
        let Format::Video(v) = &s.format else { panic!() };
        assert_eq!((v.video, v.audio), (Some(VideoCodec::Vp9), Some(AudioCodec::Opus)));
        assert_eq!(
            s.jobs(&p, &RenderSelection::default(), 0).unwrap()[0].path,
            PathBuf::from("/out/split/01-Intro.webm")
        );
    }

    #[test]
    fn custom_bounds_end_on_a_millisecond() {
        let p = crate::rpp::project_from_rpp(
            "<REAPER_PROJECT 0.1 \"7.73\" 0\n  RENDER_RANGE 0 1.00004 17.3333 0 1000\n>\n",
        )
        .unwrap();
        let r = RenderSettings::from_project(&p).jobs(&p, &RenderSelection::default(), 0).unwrap()[0].range;
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

    /// The project of `rs/stems.RPP` in the WP11 probes: two regions, the first with Kick and
    /// Bass in its matrix row, the second with Folder and the master.
    fn source_project(stems: i64, range: i64, pattern: &str) -> Project {
        let item = |pos: f64, name: &str, sel: u8| {
            format!(
                "    <ITEM\n      POSITION {pos}\n      LENGTH 2\n      SEL {sel}\n      NAME {name}\n      <SOURCE WAVE\n        FILE x.wav\n      >\n    >\n"
            )
        };
        let text = format!(
            "<REAPER_PROJECT 0.1 \"7.73\" 0\n  RENDER_FILE /out\n  RENDER_PATTERN \"{pattern}\"\n  RENDER_RANGE {range} 0 0 0 1000\n  RENDER_STEMS {stems}\n  MARKER 1 0 Intro 1 0 1 B {{A}} 0 1\n  <REGIONRENDER 1\n    TRACK {{K}} 0\n    TRACK {{B}} 0\n  >\n  MARKER 1 2 \"\" 1\n  MARKER 2 2 Verse 5 0 1 B {{C}} 0 1\n  <REGIONRENDER 2\n    TRACK {{F}} 0\n  >\n  MARKER 2 4 \"\" 5\n  <TRACK {{K}}\n    NAME Kick\n    SEL 1\n{}  >\n  <TRACK {{B}}\n    NAME Bass\n    SEL 0\n{}  >\n  <TRACK {{F}}\n    NAME Folder\n    SEL 1\n    ISBUS 1 1\n  >\n  <TRACK {{D}}\n    NAME Child\n    ISBUS 2 -1\n{}  >\n>\n",
            item(0.0, "it220", 0),
            item(1.0, "it330", 1),
            item(0.5, "it440", 1)
        );
        project_from_rpp(&text).unwrap()
    }

    fn names(stems: i64, range: i64, pattern: &str) -> Vec<String> {
        let p = source_project(stems, range, pattern);
        match RenderSettings::from_project(&p).jobs(&p, &RenderSelection::saved(&p), 0) {
            Ok(jobs) => jobs.iter().map(|j| j.path.file_name().unwrap().to_string_lossy().into_owned()).collect(),
            Err(_) => Vec::new(),
        }
    }

    #[test]
    fn render_sources_name_their_files_like_reaper() {
        assert_eq!(names(1, 1, "$track"), ["Kick.wav", "Folder.wav", "Master.wav"]);
        assert_eq!(names(3, 1, "$track"), ["Kick.wav", "Folder.wav"]);
        assert!(names(2, 1, "$track").is_empty());
        assert_eq!(names(128, 1, "$track"), ["Kick.wav", "Folder.wav"]);
        assert!(names(129, 1, "$track").is_empty());
        assert_eq!(names(1, 1, "$tracknumber-$track"), ["01-Kick.wav", "03-Folder.wav", "00-Master.wav"]);
        assert_eq!(
            names(32, 4, "$itemnumber-$item-$track-$tracknumber"),
            ["01-it330-Bass-02.wav", "01-it440-Child-04.wav"]
        );
        assert_eq!(names(0, 4, "$item"), ["it330.wav", "it440.wav"]);
        assert!(names(32, 1, "$item").is_empty());
        assert_eq!(names(8, 3, "$regionnumber-$tracknumber-$track").len(), 4);
        assert_eq!(
            names(8, 3, "$regionnumber-$region"),
            ["01-Intro-001.wav", "01-Intro-002.wav", "02-Verse-001.wav", "02-Verse-002.wav"]
        );
        assert!(names(9, 3, "$region-$track").is_empty());
        assert_eq!(names(1, 3, "$region-$track").len(), 6);
    }

    #[test]
    fn jobs_carry_their_sources() {
        let p = source_project(64, 4, "$item");
        let jobs = RenderSettings::from_project(&p).jobs(&p, &RenderSelection::saved(&p), 0).unwrap();
        let item = p.tracks[1].items[0].id;
        assert_eq!(jobs[0].source, RenderSource::ItemViaMaster(item));
        assert_eq!(jobs[0].range, TimeRange::new(1.0, 3.0));
        let p = source_project(8, 3, "$region-$track");
        let jobs = RenderSettings::from_project(&p).jobs(&p, &RenderSelection::default(), 0).unwrap();
        let sources: Vec<_> = jobs.iter().map(|j| j.source).collect();
        assert_eq!(
            sources,
            [
                RenderSource::Stem(p.tracks[0].id),
                RenderSource::Stem(p.tracks[1].id),
                RenderSource::Master,
                RenderSource::Stem(p.tracks[2].id)
            ]
        );
    }

    #[test]
    fn region_render_matrix_survives_a_save() {
        let p = source_project(8, 3, "");
        assert!(p.markers[1].renders_master() && !p.markers[0].renders_master());
        let text = crate::rpp::project_to_rpp(&p);
        let q = project_from_rpp(&text).unwrap();
        assert_eq!(q.markers[0].render_matrix, p.markers[0].render_matrix);
        assert_eq!(q.markers[1].render_matrix.len(), 1);
        assert!(q.markers[1].renders_master());
        let at = text.find("MARKER 1 0 Intro").unwrap();
        assert!(text[at..].trim_start_matches(|c| c != '\n').trim_start().starts_with("<REGIONRENDER 1"));
    }
}
