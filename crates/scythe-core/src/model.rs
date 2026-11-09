use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::envelope::{Envelope, EnvelopeKind};
use crate::fade::Fade;
use crate::rpp::{Chunk, Node};
use crate::tempo::TempoMap;
use crate::units::PanLaw;

/// Original tokens of each modelled RPP line, keyed by its keyword. Writing starts from these
/// and overwrites only the modelled fields, so values the model does not cover survive a save.
pub type LineMemo = BTreeMap<String, Vec<String>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TrackId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemId(pub u64);

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

impl TrackId {
    pub fn new() -> Self {
        Self(next_id())
    }
}

impl Default for TrackId {
    fn default() -> Self {
        Self::new()
    }
}

impl ItemId {
    pub fn new() -> Self {
        Self(next_id())
    }
}

impl Default for ItemId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub fn from_hex(hex: u32) -> Self {
        Self::rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
    }

    pub fn to_hex(self) -> u32 {
        (self.r as u32) << 16 | (self.g as u32) << 8 | self.b as u32
    }
}

/// Track colors cycled through for new tracks, taken from the redesign palette.
pub const TRACK_PALETTE: [Color; 6] = [
    Color::rgb(0x66, 0xd9, 0xef),
    Color::rgb(0xa6, 0xe2, 0x2e),
    Color::rgb(0xe6, 0xdb, 0x74),
    Color::rgb(0xfd, 0x97, 0x1f),
    Color::rgb(0xf9, 0x26, 0x72),
    Color::rgb(0xae, 0x81, 0xff),
];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeRange {
    pub start: f64,
    pub end: f64,
}

impl TimeRange {
    pub fn new(a: f64, b: f64) -> Self {
        Self { start: a.min(b), end: a.max(b) }
    }

    pub fn length(&self) -> f64 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.length() <= 0.0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    File(PathBuf),
    Empty,
    /// A sub-range of another source, optionally played backwards (RPP `<SOURCE SECTION`).
    Section(Section),
    /// A source type that is not decoded yet (MIDI, video…), kept verbatim.
    Raw(Chunk),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub inner: Box<Source>,
    /// Start within the inner source, in seconds.
    pub start: f64,
    /// Length in seconds; zero or less means up to the end of the inner source.
    pub length: f64,
    pub reverse: bool,
    /// The original chunk, so lines the model does not cover survive a save.
    pub raw: Chunk,
}

impl Source {
    /// The media file this source ultimately reads from.
    pub fn file(&self) -> Option<&PathBuf> {
        match self {
            Self::File(p) => Some(p),
            Self::Section(s) => s.inner.file(),
            Self::Empty | Self::Raw(_) => None,
        }
    }
}

/// What a take's source goes through before playback: stretched by `time` and shifted by
/// `pitch` with the pitch shifter `mode` (`Take::pitch_mode`), then resampled by `rate`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Processing {
    pub time: f64,
    pub pitch: f64,
    pub rate: f64,
    pub mode: i32,
}

impl Processing {
    /// REAPER's default pitch shifter for projects without `DEFPITCHMODE`: élastique 3 Pro.
    pub const DEFAULT_MODE: i32 = 9 << 16;

    /// The processed copy's length relative to the source's.
    pub fn length_ratio(&self) -> f64 {
        self.time / self.rate
    }

    /// Whether the pitch shifter runs at all (a plain rate change only resamples).
    pub fn shifts(&self) -> bool {
        (self.time - 1.0).abs() > 1e-9 || (self.pitch - 1.0).abs() > 1e-9
    }

    /// Bits identifying the processing, for caches.
    pub fn key(&self) -> (u64, u64, u64, i32) {
        (self.time.to_bits(), self.pitch.to_bits(), self.rate.to_bits(), self.mode)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Take {
    pub name: String,
    pub source: Source,
    /// Offset into the source in seconds (RPP `SOFFS`).
    pub source_offset: f64,
    pub playrate: f64,
    /// Keep the pitch when `playrate` is not 1 (time-stretch instead of resampling).
    pub preserve_pitch: bool,
    /// The pitch shifter in the high 16 bits and its sub-mode in the low ones (`PLAYRATE` field
    /// 4); -1 uses the project's default.
    pub pitch_mode: i32,
    /// Pitch shift in semitones.
    pub pitch: f64,
    pub volume: f64,
    pub pan: f64,
    /// Take volume, pan and mute envelopes, in seconds from the item start.
    pub envelopes: Vec<Envelope>,
    pub lines: LineMemo,
    pub extra: Vec<Node>,
}

impl Take {
    /// Maps a time relative to the item start onto the underlying media file, following the
    /// section, reverse and loop settings. `None` where the item plays silence.
    pub fn source_time(&self, rel: f64, looped: bool, media_len: f64) -> Option<f64> {
        let (start, len, reverse) = match &self.source {
            Source::Section(s) => {
                let start = s.start.clamp(0.0, media_len);
                let avail = media_len - start;
                (start, if s.length > 0.0 { s.length.min(avail) } else { avail }, s.reverse)
            }
            _ => (0.0, media_len, false),
        };
        if len <= 0.0 {
            return None;
        }
        let mut pos = self.source_offset + rel * self.playrate;
        if looped {
            pos = pos.rem_euclid(len);
        }
        if !(0.0..len).contains(&pos) {
            return None;
        }
        Some(if reverse { start + len - pos } else { start + pos })
    }

    /// How the source is processed before playback, when it is: a rate change that keeps the
    /// pitch stretches the time axis by `1 / playrate`, a pitch shift scales the frequencies,
    /// and a rate change without `preserve_pitch` resamples by `playrate`. `default_mode` is the
    /// project's pitch shifter (`Project::default_pitch_mode`).
    pub fn processing(&self, default_mode: i32) -> Option<Processing> {
        let changed = self.playrate > 0.0 && (self.playrate - 1.0).abs() > 1e-9;
        let time = if self.preserve_pitch && changed { 1.0 / self.playrate } else { 1.0 };
        let rate = if !self.preserve_pitch && changed { self.playrate } else { 1.0 };
        let pitch = 2f64.powf(self.pitch / 12.0);
        let mode = if self.pitch_mode < 0 { default_mode } else { self.pitch_mode };
        ((time - 1.0).abs() > 1e-9 || (pitch - 1.0).abs() > 1e-9 || rate != 1.0).then_some(Processing {
            time,
            pitch,
            rate,
            mode,
        })
    }

    pub fn from_file(path: PathBuf) -> Self {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Self {
            name,
            source: Source::File(path),
            source_offset: 0.0,
            playrate: 1.0,
            preserve_pitch: true,
            pitch_mode: -1,
            pitch: 0.0,
            volume: 1.0,
            pan: 0.0,
            envelopes: Vec::new(),
            lines: LineMemo::new(),
            extra: Vec::new(),
        }
    }
}

/// Shortest item an edit can leave behind, in seconds.
pub const MIN_ITEM_LENGTH: f64 = 0.001;

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: ItemId,
    pub guid: String,
    pub position: f64,
    pub length: f64,
    pub fade_in: Fade,
    pub fade_out: Fade,
    pub volume: f64,
    pub muted: bool,
    pub looped: bool,
    /// Vertical placement within the track as (top, height) fractions (RPP `YPOS`), used by
    /// fixed lanes and free item positioning.
    pub ypos: Option<(f64, f64)>,
    pub takes: Vec<Take>,
    pub active_take: usize,
    pub lines: LineMemo,
    pub extra: Vec<Node>,
}

impl Item {
    pub fn new(position: f64, length: f64, take: Take) -> Self {
        Self {
            id: ItemId::new(),
            guid: new_guid(),
            position,
            length,
            fade_in: Fade::new(0.01),
            fade_out: Fade::new(0.01),
            volume: 1.0,
            muted: false,
            looped: false,
            ypos: None,
            takes: vec![take],
            active_take: 0,
            lines: LineMemo::new(),
            extra: Vec::new(),
        }
    }

    pub fn end(&self) -> f64 {
        self.position + self.length
    }

    /// Moves the left edge to `start`, keeping the audio where it is on the timeline.
    pub fn trim_start(&mut self, start: f64) {
        let start = start.clamp(0.0, self.end() - MIN_ITEM_LENGTH);
        let delta = start - self.position;
        for take in &mut self.takes {
            take.source_offset += delta * take.playrate;
        }
        self.position = start;
        self.length -= delta;
        self.clamp_fades();
    }

    /// Moves the right edge to `end`.
    pub fn trim_end(&mut self, end: f64) {
        self.length = (end - self.position).max(MIN_ITEM_LENGTH);
        self.clamp_fades();
    }

    fn clamp_fades(&mut self) {
        self.fade_in.length = self.fade_in.length.min(self.length);
        self.fade_out.length = self.fade_out.length.min(self.length);
    }

    /// Splits at `at`, keeping the left part and returning the right one (with a fresh
    /// identity). `None` when `at` is not strictly inside the item.
    pub fn split_at(&mut self, at: f64) -> Option<Item> {
        if at <= self.position + MIN_ITEM_LENGTH || at >= self.end() - MIN_ITEM_LENGTH {
            return None;
        }
        let cut = at - self.position;
        let mut right = self.duplicate();
        for take in &mut right.takes {
            take.source_offset += cut * take.playrate;
        }
        right.position = at;
        right.length = self.length - cut;
        right.fade_in = Fade::new(0.01);
        self.length = cut;
        self.fade_out = Fade::new(0.01);
        Some(right)
    }

    /// A copy with a fresh identity, for paste and duplicate.
    pub fn duplicate(&self) -> Self {
        Self { id: ItemId::new(), guid: new_guid(), ..self.clone() }
    }

    pub fn take(&self) -> Option<&Take> {
        self.takes.get(self.active_take)
    }

    /// The fixed lane the item sits in, from its vertical placement.
    pub fn lane(&self) -> usize {
        match self.ypos {
            Some((top, height)) if height > 0.0 => (top / height).round().max(0.0) as usize,
            _ => 0,
        }
    }

    pub fn name(&self) -> &str {
        self.take().map(|t| t.name.as_str()).unwrap_or("")
    }
}

/// A plugin slot. Only identity and bypass state are modelled; the opaque plugin chunk is kept
/// verbatim so projects round-trip until plugin hosting exists.
#[derive(Debug, Clone, PartialEq)]
pub struct FxSlot {
    pub name: String,
    pub enabled: bool,
    pub raw: Vec<Node>,
}

/// Where a send or hardware output taps its track (`I_SENDMODE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SendMode {
    /// After the fader, pan and volume envelopes.
    #[default]
    PostFader,
    /// Before the effects: the items and live input, with the pre-FX volume envelope.
    PreFx,
    /// Code 2, an older spelling of `PostFx` that REAPER still reads and plays the same.
    PostFxLegacy,
    /// After the effects, before the fader: what a monitor mix wants.
    PostFx,
}

impl SendMode {
    /// The modes offered for new sends, in REAPER's menu order.
    pub const CHOICES: [SendMode; 3] = [Self::PostFader, Self::PreFx, Self::PostFx];

    pub fn from_code(v: i64) -> Self {
        match v {
            1 => Self::PreFx,
            2 => Self::PostFxLegacy,
            3 => Self::PostFx,
            _ => Self::PostFader,
        }
    }

    pub fn code(self) -> i64 {
        match self {
            Self::PostFader => 0,
            Self::PreFx => 1,
            Self::PostFxLegacy => 2,
            Self::PostFx => 3,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::PostFader => "Post-fader",
            Self::PreFx => "Pre-FX",
            Self::PostFxLegacy | Self::PostFx => "Pre-fader (post-FX)",
        }
    }

    /// Whether the track's fader, pan and volume envelope act on what is tapped.
    pub fn post_fader(self) -> bool {
        self == Self::PostFader
    }
}

/// The source channels of `I_SRCCHAN` as (first, count): the first channel in the low 10 bits,
/// above them 0 for a pair, 1 for one channel, n for 2n channels. `None` for -1 (no audio).
pub fn source_span(src_channels: i64) -> Option<(usize, usize)> {
    if src_channels < 0 {
        return None;
    }
    let count = match src_channels >> 10 {
        0 => 2,
        1 => 1,
        n => 2 * n as usize,
    };
    Some(((src_channels & 1023) as usize, count))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Send {
    pub dest: TrackId,
    pub volume: f64,
    pub pan: f64,
    pub muted: bool,
    pub mode: SendMode,
    /// Mix the source channels down to mono before panning (`B_MONO`).
    pub mono: bool,
    /// Invert the polarity (`B_PHASE`).
    pub phase: bool,
    /// Send volume, pan and mute envelopes.
    pub envelopes: Vec<Envelope>,
    /// Source channels as REAPER's `I_SRCCHAN` (see `source_span`); -1 sends no audio.
    pub src_channels: i64,
    /// Destination as `I_DSTCHAN`: the first channel, plus 1024 to mix down to mono.
    pub dst_channels: i64,
    /// Tokens of the original `AUXRECV` line, so channel mapping, MIDI flags and the rest
    /// survive a save when only volume, pan or mute changed.
    pub raw: Option<Vec<String>>,
}

impl Send {
    pub fn new(dest: TrackId) -> Self {
        Self {
            dest,
            volume: 1.0,
            pan: 0.0,
            muted: false,
            mode: SendMode::PostFader,
            mono: false,
            phase: false,
            envelopes: Vec::new(),
            src_channels: 0,
            dst_channels: 0,
            raw: None,
        }
    }
}

/// A track's (or the master's) audio hardware output: REAPER's `HWOUT` / `MASTERHWOUT`.
#[derive(Debug, Clone, PartialEq)]
pub struct HwOut {
    /// `I_DSTCHAN`: the first device output (0-based) in the low 10 bits, plus 1024 to mix the
    /// source down to that one output, plus 512 for a ReaRoute channel (which plays nowhere).
    pub dst_channels: i64,
    pub mode: SendMode,
    pub volume: f64,
    pub pan: f64,
    pub muted: bool,
    pub phase: bool,
    /// Source channels as `I_SRCCHAN` (see `source_span`); -1 sends no audio.
    pub src_channels: i64,
    /// `D_PANLAW`; negative uses the project's.
    pub pan_law: f64,
    /// Tokens of the original line, so fields the model does not cover survive a save.
    pub raw: Option<Vec<String>>,
    /// Lines REAPER writes right after this output's own (`HWOUT_WITHOUT_PANLAW_BOOST`).
    pub trailing: Vec<Node>,
}

impl Default for HwOut {
    fn default() -> Self {
        Self::stereo(0)
    }
}

impl HwOut {
    pub const MONO: i64 = 1024;
    pub const REAROUTE: i64 = 512;

    /// The track's first channel pair to device outputs `first` and `first + 1` (0-based).
    pub fn stereo(first: u32) -> Self {
        Self {
            dst_channels: first as i64,
            mode: SendMode::PostFader,
            volume: 1.0,
            pan: 0.0,
            muted: false,
            phase: false,
            src_channels: 0,
            pan_law: -1.0,
            raw: None,
            trailing: Vec::new(),
        }
    }

    /// The track's first channel pair mixed down to device output `output` (0-based).
    pub fn mono(output: u32) -> Self {
        Self { dst_channels: output as i64 | Self::MONO, ..Self::stereo(0) }
    }

    /// First device output, 0-based.
    pub fn first_output(&self) -> usize {
        (self.dst_channels & 511) as usize
    }

    pub fn is_mono(&self) -> bool {
        self.dst_channels & Self::MONO != 0
    }

    pub fn is_rearoute(&self) -> bool {
        self.dst_channels & Self::REAROUTE != 0
    }

    /// Device outputs this output reaches, as a range of 0-based channels.
    pub fn outputs(&self) -> std::ops::Range<usize> {
        let first = self.first_output();
        match source_span(self.src_channels) {
            _ if self.is_rearoute() => first..first,
            None => first..first,
            Some(_) if self.is_mono() => first..first + 1,
            Some((_, 1)) => first..first + 2,
            Some((_, n)) => first..first + n,
        }
    }

    /// "Out 3/4" or "Out 5 (mono)": what the output is called in REAPER's routing window.
    pub fn label(&self) -> String {
        let r = self.outputs();
        match r.len() {
            0 => "None".to_owned(),
            1 => format!("Out {} (mono)", r.start + 1),
            2 => format!("Out {}/{}", r.start + 1, r.start + 2),
            _ => format!("Out {}-{}", r.start + 1, r.end),
        }
    }
}

/// REAPER's track automation modes, in `AUTOMODE` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AutoMode {
    /// Envelopes play; the fader trims on top of them.
    #[default]
    TrimRead,
    /// Envelopes play and drive the controls.
    Read,
    /// Moving a control writes while it is held, then the envelope takes over again.
    Touch,
    /// Every control writes its current value for the whole pass.
    Write,
    /// Moving a control writes from then on until playback stops.
    Latch,
    /// Like latch, but only remembers the values for writing later.
    LatchPreview,
}

impl AutoMode {
    pub const ALL: [AutoMode; 6] =
        [Self::TrimRead, Self::Read, Self::Touch, Self::Write, Self::Latch, Self::LatchPreview];

    pub fn from_rpp(v: i64) -> Self {
        Self::ALL.get(v.max(0) as usize).copied().unwrap_or_default()
    }

    pub fn rpp_code(self) -> i64 {
        Self::ALL.iter().position(|m| *m == self).unwrap_or(0) as i64
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::TrimRead => "Trim/Read",
            Self::Read => "Read",
            Self::Touch => "Touch",
            Self::Write => "Write",
            Self::Latch => "Latch",
            Self::LatchPreview => "Latch preview",
        }
    }

    /// Whether moving a control while playing writes envelope points.
    pub fn writes(self) -> bool {
        matches!(self, Self::Touch | Self::Write | Self::Latch)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub id: TrackId,
    pub guid: String,
    pub name: String,
    pub color: Option<Color>,
    pub volume: f64,
    pub pan: f64,
    pub muted: bool,
    pub solo: bool,
    pub armed: bool,
    /// Record input in REAPER's `I_RECINPUT` encoding (`REC` field 2): hardware channel in the
    /// low bits, +1024 for a stereo pair, negative for none.
    pub rec_input: i64,
    /// Hear the record input through the track while armed (`REC` field 3).
    pub monitor: bool,
    /// Folder structure delta after this track: +1 opens a folder (this track is its parent),
    /// -n closes n levels (this track is the last child).
    pub folder_depth: i32,
    /// Whether the track feeds its parent folder, or the master when it has none.
    pub main_send: bool,
    /// Track pan law (`VOLPAN` field 3); negative means the project default.
    pub pan_law: f64,
    /// Automation mode (`AUTOMODE`): what moving a control does while playing.
    pub auto_mode: AutoMode,
    /// Hardware MIDI output (`MIDIOUT`): device index and channel (0 keeps each message's
    /// own channel, 1-16 forces one).
    pub midi_out: Option<(u8, u8)>,
    /// Audio channels (`NCHAN`, even, 2-128).
    pub channels: u32,
    /// First channel of the parent (or master) the track's output goes to (`MAINSEND`).
    pub main_send_offset: u32,
    /// The track's items sit in fixed lanes (`FREEMODE 2`), so lane soloing applies.
    pub fixed_lanes: bool,
    /// Fixed lanes that play (`LANESOLO`): a bitmask per 32 lanes; `None` plays every lane.
    pub lane_solo: Option<Vec<u32>>,
    pub items: Vec<Item>,
    pub fx: Vec<FxSlot>,
    /// FX-chain level lines (window state and such) that precede the first plugin.
    pub fx_chain_prelude: Vec<Node>,
    pub sends: Vec<Send>,
    /// Audio hardware outputs (`HWOUT`, `MASTERHWOUT` on the master).
    pub hw_outs: Vec<HwOut>,
    pub envelopes: Vec<Envelope>,
    pub lines: LineMemo,
    pub extra: Vec<Node>,
}

impl Track {
    /// A value from the track's extension data (`P_EXT:<key>`, the `<EXT` chunk), which REAPER
    /// keeps for extensions and scripts.
    pub fn ext(&self, key: &str) -> Option<String> {
        self.extra.iter().find_map(|n| match n {
            Node::Chunk(c) if c.name() == "EXT" => c.children.iter().find_map(|l| match l {
                Node::Line(l) => {
                    let t = crate::rpp::tokenize(l);
                    (t.first().map(String::as_str) == Some(key)).then(|| t[1..].join(" "))
                }
                Node::Chunk(_) => None,
            }),
            _ => None,
        })
    }

    /// Sets (or with `None` removes) an extension value; the `<EXT` chunk goes when it empties.
    pub fn set_ext(&mut self, key: &str, value: Option<&str>) {
        let is_key =
            |n: &Node| matches!(n, Node::Line(l) if crate::rpp::tokenize(l).first().map(String::as_str) == Some(key));
        let at = self.extra.iter().position(|n| matches!(n, Node::Chunk(c) if c.name() == "EXT"));
        let at = match (at, value) {
            (Some(i), _) => i,
            (None, None) => return,
            (None, Some(_)) => {
                self.extra.push(Node::Chunk(Chunk::new("EXT", &[])));
                self.extra.len() - 1
            }
        };
        let Node::Chunk(c) = &mut self.extra[at] else { return };
        match value {
            Some(v) => {
                let line = Node::Line(crate::rpp::join_tokens([key, v]));
                match c.children.iter().position(is_key) {
                    Some(i) => c.children[i] = line,
                    None => c.children.push(line),
                }
            }
            None => c.children.retain(|n| !is_key(n)),
        }
        if c.children.is_empty() {
            self.extra.remove(at);
        }
    }

    /// Whether items in `lane` play: every lane does unless the track has fixed lanes and
    /// some are soloed.
    pub fn lane_plays(&self, lane: usize) -> bool {
        if !self.fixed_lanes {
            return true;
        }
        match &self.lane_solo {
            Some(words) if words.iter().any(|w| *w != 0) => {
                words.get(lane / 32).is_some_and(|w| w & (1 << (lane % 32)) != 0)
            }
            _ => true,
        }
    }

    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: TrackId::new(),
            guid: new_guid(),
            name: name.into(),
            color: None,
            volume: 1.0,
            pan: 0.0,
            muted: false,
            solo: false,
            armed: false,
            rec_input: 0,
            monitor: true,
            folder_depth: 0,
            main_send: true,
            pan_law: -1.0,
            auto_mode: AutoMode::TrimRead,
            midi_out: None,
            channels: 2,
            main_send_offset: 0,
            fixed_lanes: false,
            lane_solo: None,
            items: Vec::new(),
            fx: Vec::new(),
            fx_chain_prelude: Vec::new(),
            sends: Vec::new(),
            hw_outs: Vec::new(),
            envelopes: Vec::new(),
            lines: LineMemo::new(),
            extra: Vec::new(),
        }
    }

    pub fn item(&self, id: ItemId) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    pub fn item_mut(&mut self, id: ItemId) -> Option<&mut Item> {
        self.items.iter_mut().find(|i| i.id == id)
    }

    pub fn envelope(&self, kind: EnvelopeKind) -> Option<&Envelope> {
        self.envelopes.iter().find(|e| e.kind == kind)
    }

    pub fn envelope_mut(&mut self, kind: EnvelopeKind) -> &mut Envelope {
        match self.envelopes.iter().position(|e| e.kind == kind) {
            Some(i) => &mut self.envelopes[i],
            None => {
                self.envelopes.push(Envelope::new(kind));
                self.envelopes.last_mut().unwrap()
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    pub index: u32,
    pub position: f64,
    pub name: String,
    pub color: Option<Color>,
    /// Some(end) for regions.
    pub region_end: Option<f64>,
    pub lines: LineMemo,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    pub path: Option<PathBuf>,
    pub tempo: TempoMap,
    pub sample_rate: Option<u32>,
    /// Default pan law for tracks that do not set their own (`PANLAW`, `PANLAWFLAGS`).
    pub pan_law: PanLaw,
    /// The pitch shifter of takes that do not pick one (`DEFPITCHMODE`, as `Take::pitch_mode`).
    pub default_pitch_mode: i32,
    pub master: Track,
    pub tracks: Vec<Track>,
    pub markers: Vec<Marker>,
    pub time_selection: Option<TimeRange>,
    pub loop_enabled: bool,
    pub cursor: f64,
    pub lines: LineMemo,
    pub extra: Vec<Node>,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            path: None,
            tempo: TempoMap::default(),
            sample_rate: None,
            pan_law: PanLaw::default(),
            default_pitch_mode: Processing::DEFAULT_MODE,
            master: Track { hw_outs: vec![HwOut::default()], ..Track::new("Master") },
            tracks: Vec::new(),
            markers: Vec::new(),
            time_selection: None,
            loop_enabled: false,
            cursor: 0.0,
            lines: LineMemo::new(),
            extra: Vec::new(),
        }
    }
}

impl Project {
    pub fn name(&self) -> String {
        self.path
            .as_deref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".to_owned())
    }

    /// The pan law that applies to `track`.
    pub fn pan_law_for(&self, track: &Track) -> PanLaw {
        if track.pan_law > 0.0 { PanLaw { law: track.pan_law, ..self.pan_law } } else { self.pan_law }
    }

    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    pub fn track_index(&self, id: TrackId) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    pub fn add_track(&mut self, name: impl Into<String>) -> TrackId {
        let mut track = Track::new(name);
        track.color = Some(TRACK_PALETTE[self.tracks.len() % TRACK_PALETTE.len()]);
        let id = track.id;
        self.tracks.push(track);
        id
    }

    /// Whether the master plays through REAPER's default hardware output (its first pair to
    /// outputs 1/2, untouched). Scythe then sends every master channel to the device output of
    /// the same number, which is what a stereo master does in REAPER too.
    pub fn master_default_output(&self) -> bool {
        matches!(self.master.hw_outs.as_slice(), [o] if HwOut { raw: None, trailing: Vec::new(), ..o.clone() } == HwOut::default())
    }

    pub fn remove_track(&mut self, id: TrackId) -> Option<Track> {
        let idx = self.track_index(id)?;
        for t in &mut self.tracks {
            t.sends.retain(|s| s.dest != id);
        }
        Some(self.tracks.remove(idx))
    }

    /// Parent folder of every track, derived from the folder depth deltas.
    pub fn folder_parents(&self) -> Vec<Option<usize>> {
        let mut stack: Vec<usize> = Vec::new();
        let mut parents = Vec::with_capacity(self.tracks.len());
        for (i, t) in self.tracks.iter().enumerate() {
            parents.push(stack.last().copied());
            if t.folder_depth > 0 {
                stack.push(i);
            } else {
                for _ in 0..(-t.folder_depth) {
                    stack.pop();
                }
            }
        }
        parents
    }

    /// Nesting level of every track (0 = top level).
    pub fn folder_levels(&self) -> Vec<usize> {
        let parents = self.folder_parents();
        let mut levels = vec![0usize; parents.len()];
        for i in 0..parents.len() {
            levels[i] = parents[i].map_or(0, |p| levels[p] + 1);
        }
        levels
    }

    /// Rewrites the folder depth deltas from per-track nesting levels, clamping each level to at
    /// most one deeper than the track above.
    pub fn set_folder_levels(&mut self, levels: &[usize]) {
        let mut fixed = Vec::with_capacity(levels.len());
        for (i, &l) in levels.iter().enumerate() {
            let max = if i == 0 { 0 } else { fixed[i - 1] + 1 };
            fixed.push(l.min(max));
        }
        for i in 0..self.tracks.len() {
            let next = fixed.get(i + 1).copied().unwrap_or(0) as i32;
            self.tracks[i].folder_depth = next - fixed[i] as i32;
        }
    }

    /// Moves a track one place up or down; it takes the nesting level of the place it lands on.
    pub fn move_track(&mut self, id: TrackId, up: bool) -> bool {
        let Some(i) = self.track_index(id) else { return false };
        let j = if up { i.checked_sub(1) } else { (i + 1 < self.tracks.len()).then_some(i + 1) };
        let Some(j) = j else { return false };
        let levels = self.folder_levels();
        self.tracks.swap(i, j);
        self.set_folder_levels(&levels);
        true
    }

    /// Moves a track one folder level deeper (into the folder above) or shallower.
    pub fn indent_track(&mut self, id: TrackId, deeper: bool) -> bool {
        let Some(i) = self.track_index(id) else { return false };
        let mut levels = self.folder_levels();
        let old = levels[i];
        levels[i] = if deeper { old + 1 } else { old.saturating_sub(1) };
        self.set_folder_levels(&levels);
        self.folder_levels()[i] != old
    }

    pub fn any_solo(&self) -> bool {
        self.tracks.iter().any(|t| t.solo)
    }

    /// Whether a track is heard, taking mute and exclusive solo into account. Soloing a folder
    /// solos its children, and soloing a child keeps its parent folders audible.
    pub fn is_audible(&self, track: &Track) -> bool {
        let parents = self.folder_parents();
        let Some(idx) = self.track_index(track.id) else { return !track.muted };
        let ancestors = || std::iter::successors(parents[idx], |&p| parents[p]);
        if track.muted || ancestors().any(|p| self.tracks[p].muted) {
            return false;
        }
        if !self.any_solo() || track.solo || ancestors().any(|p| self.tracks[p].solo) {
            return true;
        }
        self.tracks
            .iter()
            .enumerate()
            .any(|(i, t)| t.solo && std::iter::successors(parents[i], |&p| parents[p]).any(|p| p == idx))
    }

    /// Per track, whether it is not heard itself but sends (directly or through other sends)
    /// into a track that is while others are soloed. REAPER keeps such a track playing into its
    /// sends and silences only its own output, so soloing a bus still hears its sources.
    pub fn solo_feeds(&self) -> Vec<bool> {
        let n = self.tracks.len();
        let mut feeds = vec![false; n];
        if !self.any_solo() {
            return feeds;
        }
        let parents = self.folder_parents();
        let audible: Vec<bool> = self.tracks.iter().map(|t| self.is_audible(t)).collect();
        let muted: Vec<bool> = (0..n)
            .map(|i| std::iter::once(i).chain(std::iter::successors(parents[i], |&p| parents[p])))
            .map(|mut chain| chain.any(|j| self.tracks[j].muted))
            .collect();
        let index: BTreeMap<TrackId, usize> = self.tracks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
        loop {
            let mut changed = false;
            for i in 0..n {
                if audible[i] || muted[i] || feeds[i] {
                    continue;
                }
                let reaches = self.tracks[i]
                    .sends
                    .iter()
                    .filter(|s| !s.muted)
                    .filter_map(|s| index.get(&s.dest))
                    .any(|&d| audible[d] || feeds[d]);
                if reaches {
                    feeds[i] = true;
                    changed = true;
                }
            }
            if !changed {
                return feeds;
            }
        }
    }

    /// End of the last item, i.e. the natural project length.
    pub fn length(&self) -> f64 {
        self.tracks.iter().flat_map(|t| &t.items).map(Item::end).fold(0.0, f64::max)
    }

    pub fn find_item(&self, id: ItemId) -> Option<(TrackId, &Item)> {
        self.tracks.iter().find_map(|t| t.item(id).map(|i| (t.id, i)))
    }

    /// Sets the automatic fade lengths on a track from item overlaps: overlapping neighbours
    /// crossfade over the overlap, other items lose their automatic fades.
    pub fn auto_crossfade(&mut self, track: TrackId) {
        let Some(t) = self.track_mut(track) else { return };
        let mut order: Vec<usize> = (0..t.items.len()).collect();
        order.sort_by(|&a, &b| t.items[a].position.total_cmp(&t.items[b].position));
        for it in &mut t.items {
            it.fade_in.auto_length = 0.0;
            it.fade_out.auto_length = 0.0;
        }
        for w in order.windows(2) {
            let (a, b) = (w[0], w[1]);
            let overlap = t.items[a].end().min(t.items[b].end()) - t.items[b].position;
            if overlap > 0.0 && t.items[a].ypos == t.items[b].ypos {
                t.items[a].fade_out.auto_length = overlap;
                t.items[b].fade_in.auto_length = overlap;
            }
        }
    }

    /// Splits every item on the given tracks (all tracks when `None`) that crosses `at`.
    pub fn split_items_at(&mut self, at: f64, tracks: Option<&[TrackId]>) {
        for t in self.tracks.iter_mut().filter(|t| tracks.is_none_or(|ids| ids.contains(&t.id))) {
            let mut added = Vec::new();
            for it in &mut t.items {
                added.extend(it.split_at(at));
            }
            t.items.extend(added);
        }
    }

    /// Removes everything inside `range` on the given tracks (all when `None`). With `ripple`,
    /// later items on those tracks move left to close the gap; when every track ripples, so do
    /// markers and envelope points.
    pub fn remove_range(&mut self, range: TimeRange, tracks: Option<&[TrackId]>, ripple: bool) {
        if range.is_empty() {
            return;
        }
        self.split_items_at(range.start, tracks);
        self.split_items_at(range.end, tracks);
        let len = range.length();
        for t in self.tracks.iter_mut().filter(|t| tracks.is_none_or(|ids| ids.contains(&t.id))) {
            t.items.retain(|it| it.end() <= range.start + 1e-9 || it.position >= range.end - 1e-9);
            if ripple {
                for it in t.items.iter_mut().filter(|it| it.position >= range.end - 1e-9) {
                    it.position -= len;
                }
            }
        }
        if ripple && tracks.is_none() {
            self.markers.retain(|m| m.position < range.start || m.position >= range.end);
            self.shift_after(range.end, -len);
        }
    }

    /// Copies of the parts of items inside each area (razor edits), cropped to it, with fresh
    /// identities.
    pub fn pieces_in(&self, areas: &[(TrackId, TimeRange)]) -> Vec<(TrackId, Item)> {
        let mut out = Vec::new();
        for &(track, range) in areas {
            let Some(t) = self.track(track) else { continue };
            for it in t.items.iter().filter(|it| it.end() > range.start && it.position < range.end) {
                let mut piece = it.duplicate();
                let end = piece.end().min(range.end);
                if piece.position < range.start {
                    piece.trim_start(range.start);
                }
                piece.length = end - piece.position;
                if piece.length > MIN_ITEM_LENGTH {
                    out.push((track, piece));
                }
            }
        }
        out
    }

    /// Removes what lies inside each area (razor edits), splitting items at the edges.
    pub fn remove_areas(&mut self, areas: &[(TrackId, TimeRange)]) {
        for &(track, range) in areas {
            self.remove_range(range, Some(&[track]), false);
        }
    }

    /// Opens a gap of `range` at `range.start` on every track: items there are split and
    /// everything later, markers and envelope points included, moves right.
    pub fn insert_space(&mut self, range: TimeRange) {
        if range.is_empty() {
            return;
        }
        self.split_items_at(range.start, None);
        for it in self.tracks.iter_mut().flat_map(|t| t.items.iter_mut()).filter(|it| it.position >= range.start - 1e-9)
        {
            it.position += range.length();
        }
        self.shift_after(range.start, range.length());
    }

    /// Moves markers, regions and envelope points at or after `from` by `delta`.
    fn shift_after(&mut self, from: f64, delta: f64) {
        for m in self.markers.iter_mut().filter(|m| m.position >= from - 1e-9) {
            m.position += delta;
            if let Some(end) = m.region_end.as_mut() {
                *end += delta;
            }
        }
        for env in self.tracks.iter_mut().flat_map(|t| t.envelopes.iter_mut()) {
            for p in env.points.iter_mut().filter(|p| p.time >= from - 1e-9) {
                p.time = (p.time + delta).max(0.0);
            }
        }
    }

    /// Moves an item to another track, keeping its id.
    pub fn move_item_to_track(&mut self, item: ItemId, dest: TrackId) -> bool {
        let Some(src_idx) = self.tracks.iter().position(|t| t.item(item).is_some()) else {
            return false;
        };
        let Some(dest_idx) = self.track_index(dest) else {
            return false;
        };
        if src_idx == dest_idx {
            return true;
        }
        let pos = self.tracks[src_idx].items.iter().position(|i| i.id == item).unwrap();
        let it = self.tracks[src_idx].items.remove(pos);
        self.tracks[dest_idx].items.push(it);
        true
    }

    pub fn add_marker(&mut self, position: f64, name: impl Into<String>) {
        let index = self.markers.iter().map(|m| m.index).max().unwrap_or(0) + 1;
        self.markers.push(Marker {
            index,
            position,
            name: name.into(),
            color: None,
            region_end: None,
            lines: LineMemo::new(),
        });
        self.markers.sort_by(|a, b| a.position.total_cmp(&b.position));
    }
}

/// A REAPER-style `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}` GUID. Uniqueness, not
/// unpredictability, is what matters here, so std's randomly keyed hasher is enough.
pub fn new_guid() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    let mut words = [0u64; 2];
    for (i, w) in words.iter_mut().enumerate() {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(next_id());
        h.write_usize(i);
        *w = h.finish();
    }
    let b: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
    let hex = |r: std::ops::Range<usize>| b[r].iter().map(|x| format!("{x:02X}")).collect::<String>();
    format!("{{{}-{}-{}-{}-{}}}", hex(0..4), hex(4..6), hex(6..8), hex(8..10), hex(10..16))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solo_is_exclusive() {
        let mut p = Project::default();
        let a = p.add_track("a");
        let b = p.add_track("b");
        assert!(p.is_audible(p.track(a).unwrap()));
        p.track_mut(b).unwrap().solo = true;
        assert!(!p.is_audible(p.track(a).unwrap()));
        assert!(p.is_audible(p.track(b).unwrap()));
        p.track_mut(b).unwrap().muted = true;
        assert!(!p.is_audible(p.track(b).unwrap()));
    }

    #[test]
    fn removing_track_drops_sends_to_it() {
        let mut p = Project::default();
        let a = p.add_track("a");
        let bus = p.add_track("bus");
        p.track_mut(a).unwrap().sends.push(Send::new(bus));
        p.remove_track(bus);
        assert!(p.track(a).unwrap().sends.is_empty());
    }

    #[test]
    fn take_processing() {
        let mut take = Take::from_file("x.wav".into());
        assert_eq!(take.processing(7), None);
        take.playrate = 0.5;
        let p = take.processing(7).unwrap();
        assert_eq!((p.time, p.pitch, p.rate, p.mode, p.length_ratio(), p.shifts()), (2.0, 1.0, 1.0, 7, 2.0, true));
        take.preserve_pitch = false;
        take.pitch_mode = 3;
        let p = take.processing(7).unwrap();
        assert_eq!((p.time, p.rate, p.mode, p.length_ratio(), p.shifts()), (1.0, 0.5, 3, 2.0, false));
        take.pitch = 12.0;
        let p = take.processing(7).unwrap();
        assert_eq!((p.pitch, p.shifts()), (2.0, true));
    }

    #[test]
    fn move_item_between_tracks() {
        let mut p = Project::default();
        let a = p.add_track("a");
        let b = p.add_track("b");
        let item = Item::new(1.0, 2.0, Take::from_file("x.wav".into()));
        let id = item.id;
        p.track_mut(a).unwrap().items.push(item);
        assert!(p.move_item_to_track(id, b));
        assert_eq!(p.find_item(id).unwrap().0, b);
        assert_eq!(p.length(), 3.0);
    }

    #[test]
    fn folders_and_solo() {
        let mut p = Project::default();
        let bus = p.add_track("bus");
        let a = p.add_track("a");
        let b = p.add_track("b");
        let other = p.add_track("other");
        p.tracks[0].folder_depth = 1;
        p.tracks[2].folder_depth = -1;
        assert_eq!(p.folder_parents(), [None, Some(0), Some(0), None]);
        assert_eq!(p.folder_levels(), [0, 1, 1, 0]);

        p.track_mut(bus).unwrap().solo = true;
        assert!(p.is_audible(p.track(a).unwrap()));
        assert!(!p.is_audible(p.track(other).unwrap()));

        p.track_mut(bus).unwrap().solo = false;
        p.track_mut(b).unwrap().solo = true;
        assert!(p.is_audible(p.track(bus).unwrap()));
        assert!(!p.is_audible(p.track(a).unwrap()));

        p.track_mut(b).unwrap().solo = false;
        p.track_mut(bus).unwrap().muted = true;
        assert!(!p.is_audible(p.track(a).unwrap()));
    }

    #[test]
    fn trimming_keeps_audio_in_place() {
        let mut it = Item::new(10.0, 4.0, Take::from_file("x.wav".into()));
        it.takes[0].playrate = 2.0;
        it.trim_start(11.0);
        assert_eq!((it.position, it.length, it.takes[0].source_offset), (11.0, 3.0, 2.0));
        it.trim_start(9.0);
        assert_eq!((it.position, it.length, it.takes[0].source_offset), (9.0, 5.0, -2.0));
        it.trim_end(10.0);
        assert_eq!(it.length, 1.0);
        it.trim_end(0.0);
        assert_eq!(it.length, MIN_ITEM_LENGTH);
        let d = it.duplicate();
        assert!(d.id != it.id && d.guid != it.guid && d.position == it.position);
    }

    #[test]
    fn folder_editing() {
        let mut p = Project::default();
        let a = p.add_track("a");
        let b = p.add_track("b");
        let c = p.add_track("c");
        assert!(p.indent_track(b, true));
        assert_eq!(p.folder_parents(), [None, Some(0), None]);
        assert!(p.indent_track(c, true));
        assert_eq!(p.folder_parents(), [None, Some(0), Some(0)]);
        assert!(!p.indent_track(a, true), "the first track cannot be indented");
        assert!(p.move_track(c, true));
        assert_eq!(p.tracks.iter().map(|t| t.id).collect::<Vec<_>>(), [a, c, b]);
        assert_eq!(p.folder_parents(), [None, Some(0), Some(0)]);
        assert!(p.indent_track(b, false));
        assert_eq!(p.folder_parents(), [None, Some(0), None]);
        assert_eq!(p.tracks.iter().map(|t| t.folder_depth).sum::<i32>(), 0);
    }

    #[test]
    fn overlapping_items_crossfade() {
        let mut p = Project::default();
        let t = p.add_track("t");
        let tr = p.track_mut(t).unwrap();
        tr.items.push(Item::new(0.0, 2.0, Take::from_file("a.wav".into())));
        tr.items.push(Item::new(1.5, 2.0, Take::from_file("b.wav".into())));
        tr.items.push(Item::new(5.0, 1.0, Take::from_file("c.wav".into())));
        p.auto_crossfade(t);
        let items = &p.track(t).unwrap().items;
        assert_eq!((items[0].fade_out.auto_length, items[1].fade_in.auto_length), (0.5, 0.5));
        assert_eq!((items[1].fade_out.auto_length, items[2].fade_in.auto_length), (0.0, 0.0));
    }

    #[test]
    fn time_selection_editing() {
        let mut p = Project::default();
        let t = p.add_track("t");
        p.track_mut(t).unwrap().items.push(Item::new(0.0, 10.0, Take::from_file("a.wav".into())));
        p.add_marker(8.0, "late");
        p.remove_range(TimeRange::new(2.0, 5.0), None, true);
        let mut items: Vec<(f64, f64, f64)> =
            p.tracks[0].items.iter().map(|i| (i.position, i.length, i.takes[0].source_offset)).collect();
        items.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert_eq!(items, [(0.0, 2.0, 0.0), (2.0, 5.0, 5.0)]);
        assert_eq!(p.markers[0].position, 5.0);
        p.insert_space(TimeRange::new(1.0, 2.0));
        let mut starts: Vec<f64> = p.tracks[0].items.iter().map(|i| i.position).collect();
        starts.sort_by(f64::total_cmp);
        assert_eq!(starts, [0.0, 2.0, 3.0]);
        assert_eq!(p.markers[0].position, 6.0);
    }

    #[test]
    fn guid_shape() {
        let g = new_guid();
        assert_eq!(g.len(), 38);
        assert_ne!(g, new_guid());
    }
}

#[cfg(test)]
mod auto_mode_tests {
    use crate::rpp::{project_from_rpp, project_to_rpp};

    #[test]
    fn automation_mode_round_trips() {
        let mut p = project_from_rpp("<REAPER_PROJECT 0.1 \"7.73\" 0\n  <TRACK\n    AUTOMODE 2\n  >\n>\n").unwrap();
        assert_eq!(p.tracks[0].auto_mode, super::AutoMode::Touch);
        p.tracks[0].auto_mode = super::AutoMode::Latch;
        p.add_track("new");
        let text = project_to_rpp(&p);
        assert_eq!(text.matches("AUTOMODE 4").count(), 1, "{text}");
        assert!(!text.contains("AUTOMODE 0"), "the default is not written for new tracks");
    }
}

#[cfg(test)]
mod razor_tests {
    use super::*;

    #[test]
    fn areas_copy_and_remove_per_track() {
        let mut p = Project::default();
        let a = p.add_track("a");
        let b = p.add_track("b");
        p.track_mut(a).unwrap().items.push(Item::new(0.0, 10.0, Take::from_file("a.wav".into())));
        p.track_mut(b).unwrap().items.push(Item::new(2.0, 2.0, Take::from_file("b.wav".into())));
        let areas = [(a, TimeRange::new(3.0, 5.0)), (b, TimeRange::new(3.0, 8.0))];
        let pieces = p.pieces_in(&areas);
        assert_eq!(pieces.len(), 2);
        assert_eq!((pieces[0].1.position, pieces[0].1.length, pieces[0].1.takes[0].source_offset), (3.0, 2.0, 3.0));
        assert_eq!((pieces[1].1.position, pieces[1].1.length), (3.0, 1.0));
        assert_ne!(pieces[0].1.id, p.track(a).unwrap().items[0].id);
        p.remove_areas(&areas);
        let spans = |t: TrackId| p.track(t).unwrap().items.iter().map(|i| (i.position, i.end())).collect::<Vec<_>>();
        assert_eq!(spans(a), [(0.0, 3.0), (5.0, 10.0)]);
        assert_eq!(spans(b), [(2.0, 3.0)]);
    }
}
