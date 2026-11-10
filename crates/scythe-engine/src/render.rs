use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Instant;

use scythe_core::recording::{RecordMode, RecordTap};
use scythe_core::tempo::Timeline;
use scythe_core::units::PanLaw;
use scythe_core::{ChannelMode, Envelope, EnvelopeKind, Fade, FxSlot, HwOut, Project, SendMode, Source, TrackId};

use crate::chain::{BufferedChain, ChainInput, ChainIo, Channels, FxChain, MidiState};
use crate::fx::{FxOwner, FxRegistry, ProcessContext};
use crate::media::{AudioData, MediaPool};
use rayon::prelude::*;

use crate::loudness::Loudness;
use crate::meter::Meter;
use crate::midi_io::{LiveMidi, OutMessage, RecInput};
use crate::midi_sync::{LtcWriter, TimecodeRate};
use crate::mix::{Automation, FaderStage, Matrix, PreFxGain, VcaRamp};
use crate::synth::{ControlEvent, NoteList, SynthNote};
use scythe_core::midi_route::SendMidi;

/// One item, pre-resolved to frames at the engine rate.
#[derive(Debug, Clone)]
pub struct RenderItem {
    pub start: i64,
    pub length: i64,
    /// Source position (in source frames) at `start`.
    pub source_offset: f64,
    pub playrate: f64,
    pub gain_l: f32,
    pub gain_r: f32,
    pub fade_in: (i64, Fade),
    pub fade_out: (i64, Fade),
    pub looped: bool,
    /// Range of the source the item plays, in source frames; a zero length means up to the end.
    pub section_start: f64,
    pub section_len: f64,
    pub reverse: bool,
    /// Which source channels play.
    pub channel_mode: ChannelMode,
    pub audio: ItemAudio,
    /// Take envelopes, in seconds from the item start; they replace `gain_l` and `gain_r`.
    pub automation: Option<Arc<GainAutomation>>,
    /// The take's own effects, when it has any that run.
    pub take_fx: Option<Arc<BufferedChain>>,
}

/// Volume, pan and mute envelopes around a fixed volume and pan, as sends and takes have them.
/// REAPER follows them sample by sample.
#[derive(Debug)]
pub struct GainAutomation {
    volume: f64,
    pan: f64,
    pan_law: PanLaw,
    vol_env: Option<Envelope>,
    pan_env: Option<Envelope>,
    mute_env: Option<Envelope>,
}

/// A mute envelope's gain at `t`: REAPER does not switch at a point but crossfades over 4 ms
/// with a raised cosine.
pub(crate) fn mute_gain(env: &Envelope, t: f64) -> f64 {
    const RAMP: f64 = 0.004;
    let plays = |v: f64| v >= 0.5;
    let i = env.points.partition_point(|p| p.time <= t);
    let Some(at) = i.checked_sub(1) else {
        return env.points.first().map_or(1.0, |p| plays(p.value) as u8 as f64);
    };
    let now = plays(env.points[at].value);
    let before = at.checked_sub(1).map_or(now, |b| plays(env.points[b].value));
    let x = (t - env.points[at].time) / RAMP;
    if before == now || x >= 1.0 {
        return now as u8 as f64;
    }
    let fading = 0.5 * (1.0 + (std::f64::consts::PI * x).cos());
    if now { 1.0 - fading } else { fading }
}

impl GainAutomation {
    /// `None` when no envelope is active.
    pub fn new(volume: f64, pan: f64, pan_law: PanLaw, envelopes: &[Envelope]) -> Option<Self> {
        let find =
            |kind: EnvelopeKind| envelopes.iter().find(|e| e.kind == kind && e.active && !e.points.is_empty()).cloned();
        let (vol_env, pan_env, mute_env) =
            (find(EnvelopeKind::Volume), find(EnvelopeKind::Pan), find(EnvelopeKind::Mute));
        (vol_env.is_some() || pan_env.is_some() || mute_env.is_some()).then_some(Self {
            volume,
            pan,
            pan_law,
            vol_env,
            pan_env,
            mute_env,
        })
    }

    /// Channel gains at `t` seconds. Pan envelopes store positive values for left, as REAPER
    /// does for tracks.
    pub fn gains_at(&self, t: f64) -> (f32, f32) {
        let mute = self.mute_env.as_ref().map_or(1.0, |e| mute_gain(e, t));
        if mute == 0.0 {
            return (0.0, 0.0);
        }
        let vol = mute * self.volume * self.vol_env.as_ref().and_then(|e| e.value_at(t)).unwrap_or(1.0);
        let pan = self.pan - self.pan_env.as_ref().and_then(|e| e.value_at(t)).unwrap_or(0.0);
        let (l, r) = self.pan_law.gains(pan.clamp(-1.0, 1.0));
        ((vol * l) as f32, (vol * r) as f32)
    }
}

#[derive(Debug, Clone)]
pub enum ItemAudio {
    Media(Arc<AudioData>),
    /// A metronome source (RPP `<SOURCE CLICK`): clicks on every beat of the tempo map.
    Click,
    /// A MIDI source played by the built-in preview instrument.
    Midi(Arc<NoteList>),
    /// The timecode generator (RPP `<SOURCE LTC`): LTC audio, or MTC to the track's MIDI
    /// hardware output.
    Timecode(Arc<TimecodeItem>),
}

/// A timecode generator item as it plays.
#[derive(Debug)]
pub struct TimecodeItem {
    /// Timecode (seconds) at the start of the source.
    pub start: f64,
    pub rate: TimecodeRate,
    /// `SEND` bit 2: MTC instead of LTC audio.
    pub mtc: bool,
    writer: std::sync::Mutex<LtcWriter>,
    /// Where the last block sent MTC for ended, and the frame and timecode frame of the
    /// quarter-frame grid it follows.
    mtc_state: std::sync::Mutex<Option<(i64, i64, i64)>>,
}

impl TimecodeItem {
    pub fn parse(chunk: &scythe_core::rpp::Chunk) -> Self {
        let mut start = 0.0;
        let mut rate = TimecodeRate::from_rpp(30.0, 0);
        let mut send = 0;
        for node in &chunk.children {
            if let scythe_core::rpp::Node::Line(l) = node {
                let t = scythe_core::rpp::tokenize(l);
                let num = |i: usize| t.get(i).and_then(|v| v.parse::<f64>().ok());
                match t.first().map(String::as_str) {
                    Some("STARTTIME") => start = num(1).unwrap_or(0.0),
                    Some("FRAMERATE") => {
                        rate = TimecodeRate::from_rpp(num(1).unwrap_or(30.0), num(2).unwrap_or(0.0) as i64)
                    }
                    Some("SEND") => send = num(1).unwrap_or(0.0) as i64,
                    _ => {}
                }
            }
        }
        Self {
            start,
            rate,
            mtc: send & 2 != 0,
            writer: std::sync::Mutex::new(LtcWriter::new(rate)),
            mtc_state: std::sync::Mutex::new(None),
        }
    }
}

/// Gain of a monitor mix's click at 0 dB: what the metronome plays at by default.
const CLICK_LEVEL: f64 = 0.5;

/// Synthesised metronome clicks at the engine rate.
#[derive(Debug)]
pub struct ClickSounds {
    pub accent: Vec<f32>,
    pub beat: Vec<f32>,
}

impl ClickSounds {
    pub fn new(sample_rate: u32) -> Self {
        let tone = |freq: f64, gain: f64| {
            let sr = sample_rate as f64;
            let n = (sr * 0.03) as usize;
            (0..n)
                .map(|i| {
                    let t = i as f64 / sr;
                    let env = (-t * 160.0).exp() * (i as f64 / 24.0).min(1.0);
                    (gain * env * (std::f64::consts::TAU * freq * t).sin()) as f32
                })
                .collect()
        };
        Self { accent: tone(1760.0, 0.6), beat: tone(1320.0, 0.45) }
    }
}

/// A count-in as the audio thread plays it, in frames: see [`scythe_core::metronome::CountIn`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CountInClicks {
    /// Frames until the playhead moves.
    pub frames: i64,
    pub beat: f64,
    /// Frames into it of the first click.
    pub first: f64,
    /// Which beat of its bar the first click is, from 0.
    pub first_in_bar: u32,
    pub beats_per_bar: u32,
    /// Clicks sound only before this frame.
    pub audible: i64,
}

impl CountInClicks {
    pub fn from_seconds(c: &scythe_core::metronome::CountIn, sample_rate: u32) -> Self {
        let sr = sample_rate as f64;
        Self {
            frames: (c.length * sr).round() as i64,
            beat: c.beat * sr,
            first: c.first * sr,
            first_in_bar: c.first_in_bar,
            beats_per_bar: c.beats_per_bar.max(1),
            audible: (c.audible * sr).round() as i64,
        }
    }

    /// How many clicks fall inside it.
    pub fn beats(&self) -> u32 {
        if self.beat <= 0.0 {
            return 0;
        }
        let mut n = ((self.frames as f64 - self.first) / self.beat).ceil().max(0.0) as u32;
        while n > 0 && self.click_at(n - 1) >= self.frames {
            n -= 1;
        }
        n
    }

    pub fn click_at(&self, beat: u32) -> i64 {
        (self.first + beat as f64 * self.beat).round() as i64
    }

    fn accented(&self, beat: u32) -> bool {
        (self.first_in_bar + beat).is_multiple_of(self.beats_per_bar)
    }
}

/// What the metronome plays in the block being rendered.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum Clicks {
    #[default]
    Off,
    /// On the beats of the tempo map, at the block's project frames.
    Beats,
    /// A count-in, `at` frames into it where the block starts.
    CountIn { clicks: CountInClicks, at: i64 },
    /// Playback just after a count-in, `at` frames into the count-in where the block starts:
    /// its last clicks ring out (as REAPER 7.73's do), over the beat clicks with `beats`.
    CountInTail { clicks: CountInClicks, at: i64, beats: bool },
}

/// Where the transport metronome plays, from the project's metronome settings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MetronomeRouting {
    /// It clicks while playing, and while recording.
    pub on_play: bool,
    pub on_record: bool,
    /// Gain at its outputs.
    pub gain: f32,
    /// Device outputs: first channel from 0 and whether mono. Empty plays it with the master.
    pub outputs: Vec<(usize, bool)>,
}

impl MetronomeRouting {
    /// `default_output` is the preferences' `metronome_defout`, for a project naming none.
    pub fn from_project(project: &Project, default_output: i64) -> Self {
        let m = &project.metronome;
        Self {
            on_play: m.clicks(false),
            on_record: m.clicks(true),
            // REAPER's default click volume (0.25) plays at the level clicks have always had here.
            gain: (m.volume * 2.0) as f32,
            outputs: m.outputs_or(default_output),
        }
    }

    fn end(&self) -> Option<usize> {
        self.outputs.iter().map(|&(first, mono)| first + if mono { 1 } else { 2 }).max()
    }
}

#[derive(Debug, Clone)]
pub struct RenderTrack {
    pub id: TrackId,
    /// The fader stage's gains on the first pair, without envelopes (see `crate::mix`).
    pub matrix: Matrix,
    /// The fader alone, polarity included, for channels 3 and up: REAPER pans only the first
    /// pair.
    pub volume: f32,
    /// Whether the track plays at all: it is heard, or passes on or feeds what is heard while
    /// others are soloed (see `scythe_core::solo`).
    pub audible: bool,
    /// Whether its own material (items, live input, click) plays.
    pub content: bool,
    /// Whether its output to the parent or master is heard.
    pub main_out: bool,
    /// Whether its hardware outputs play.
    pub hw_out: bool,
    /// Levels of VCA leads it follows pre-FX.
    pub pre_fx_gain: Option<Arc<PreFxGain>>,
    pub items: Vec<RenderItem>,
    pub meter: Arc<Meter>,
    pub sends: Vec<RenderSend>,
    /// Hardware outputs, routed into `RenderGraph::hw_bus`.
    pub hw_outs: Vec<RenderSend>,
    pub taps: Taps,
    /// Audio channels; 1 and 2 live in the render scratch, the rest in `aux`.
    pub channels: usize,
    pub aux: Option<Arc<AuxChannels>>,
    /// First channel of the parent (or master) the output goes to.
    pub main_offset: usize,
    /// Envelopes of each send, when it has any.
    pub send_automation: Vec<Option<Arc<GainAutomation>>>,
    /// Where the post-fader output goes: the parent folder, or the master when `None`.
    pub parent: Option<usize>,
    pub main_send: bool,
    pub automation: Option<Arc<Automation>>,
    /// A VCA follower's pan glide at playback start, with the first frame this graph played.
    pub vca_ramp: Option<Arc<(VcaRamp, AtomicI64)>>,
    pub chain: Arc<FxChain>,
    /// Buffers the pins of effects with a pin map are gathered into, one per pin of the widest.
    pub pin_bufs: Option<Arc<AuxChannels>>,
    /// MIDI effects pass down the chain within a block.
    pub chain_midi: Arc<ChainMidi>,
    /// MIDI sends: destination, what passes, and whether it leaves before the effects.
    pub midi_sends: Vec<(usize, SendMidi, bool)>,
    /// MIDI other tracks send here, gathered during the block; only on send destinations.
    pub midi_inbox: Option<Arc<MidiInbox>>,
    /// MIDI of the track's items when an instrument in the chain plays it.
    pub notes: Option<Arc<NoteList>>,
    /// Live input channels (left, optional right) for an armed, monitored track.
    pub monitor: Option<(usize, Option<usize>)>,
    /// The track's input effects, which process the live input before it joins the track.
    pub input_fx: Option<Arc<BufferedChain>>,
    /// Tape-style monitoring: the input is not heard while playing back without recording.
    pub monitor_auto: bool,
    /// The track records and does not monitor its items: they fall silent while recording.
    pub mute_items_recording: bool,
    /// Output recording of this track, while one runs.
    pub record_tap: Option<Arc<crate::record::OutputTap>>,
    pub midi_record: Option<Arc<MidiRecordTap>>,
    /// Live MIDI for an armed, monitored track with a MIDI input: the device and channel it
    /// listens to.
    pub midi_input: Option<RecInput>,
    /// The live messages from the track's device, when it listens to one device only.
    pub live_buf: Arc<std::sync::Mutex<Vec<LiveMidi>>>,
    /// Hardware MIDI output: port and channel (0 keeps each message's own channel).
    pub midi_out: Option<(u8, u8)>,
    /// Frames between this track's input and the master output, including plugin latency
    /// downstream. Items are read this far ahead so they come out on time.
    pub read_ahead: i64,
    /// Delay lines that line this track up with slower paths into each send destination and
    /// into its parent or the master, when any are needed.
    pub send_delays: Vec<Option<Arc<DelayLine>>>,
    pub main_delay: Option<Arc<DelayLine>>,
    pub hw_delays: Vec<Option<Arc<DelayLine>>>,
    /// Gain of the metronome in this track (a monitor mix with a click), zero for none.
    pub click: f32,
}

/// One send or hardware output as the graph plays it.
#[derive(Debug, Clone)]
pub struct RenderSend {
    /// Index into `RenderGraph::tracks`; unused for hardware outputs.
    pub dest: usize,
    /// Gains of the pair, polarity included.
    pub gain_l: f32,
    pub gain_r: f32,
    /// Source channels (first, count) and the first destination channel (or device output),
    /// 0-based; a single source channel feeds a pair, or one channel with `mono`.
    pub src: (usize, usize),
    pub dst: usize,
    pub mono: bool,
    /// Mix a source pair down to mono before panning (`B_MONO`).
    pub downmix: bool,
    /// -1 when the polarity is inverted, for the envelope gains.
    pub sign: f32,
    pub tap: SendMode,
}

impl RenderSend {
    /// Source and destination channel of every routed channel, with whether it takes the
    /// right gain of the pair and a factor on top of it.
    fn routes(&self) -> impl Iterator<Item = (usize, usize, bool, f32)> + '_ {
        let (first, count) = self.src;
        let dst = self.dst;
        let pairs: Box<dyn Iterator<Item = (usize, usize, bool, f32)>> = match (count, self.mono) {
            // REAPER pans the pair first, then averages it into the one channel.
            (1, true) => Box::new([(first, dst, false, 1.0)].into_iter()),
            (_, true) => Box::new((0..count).map(move |k| (first + k, dst, k % 2 == 1, 1.0 / count as f32))),
            (1, false) => Box::new([(first, dst, false, 1.0), (first, dst + 1, true, 1.0)].into_iter()),
            (2, false) if self.downmix => Box::new(
                [(first, dst, false), (first + 1, dst, false), (first, dst + 1, true), (first + 1, dst + 1, true)]
                    .into_iter()
                    .map(|(s, d, r)| (s, d, r, 0.5)),
            ),
            _ => Box::new((0..count).map(move |k| (first + k, dst + k, k % 2 == 1, 1.0))),
        };
        pairs
    }

    /// A send or hardware output with fixed gains; `None` when it plays nothing.
    #[allow(clippy::too_many_arguments)]
    fn new(
        dest: usize,
        volume: f64,
        pan: f64,
        pan_law: PanLaw,
        src_channels: i64,
        dst_channels: i64,
        downmix: bool,
        phase: bool,
        mode: SendMode,
    ) -> Option<Self> {
        let src = scythe_core::model::source_span(src_channels)?;
        let (l, r) = pan_law.gains(pan);
        let sign = if phase { -1.0 } else { 1.0 };
        Some(Self {
            dest,
            gain_l: (sign * volume * l) as f32,
            gain_r: (sign * volume * r) as f32,
            src,
            dst: (dst_channels & 511) as usize,
            mono: dst_channels & 1024 != 0,
            downmix,
            sign: sign as f32,
            tap: match mode {
                SendMode::PostFxLegacy => SendMode::PostFx,
                m => m,
            },
        })
    }

    fn hw(o: &HwOut, project_law: PanLaw) -> Option<Self> {
        if o.muted || o.is_rearoute() {
            return None;
        }
        let law = if o.pan_law > 0.0 { PanLaw { law: o.pan_law, ..project_law } } else { project_law };
        Self::new(0, o.volume, o.pan, law, o.src_channels, o.dst_channels, false, o.phase, o.mode)
    }

    /// One past the last destination channel.
    fn end(&self) -> usize {
        self.routes().map(|r| r.1 + 1).max().unwrap_or(0)
    }
}

/// Where `RenderGraph::route_track` mixes a route into.
#[derive(Clone, Copy)]
enum Dest {
    Track(usize),
    /// The parent folder, or the master.
    Parent,
    /// The device output bus.
    Device,
}

/// Copies of a track's (or the master's) channels where sends and hardware outputs that do
/// not play post-fader tap it.
#[derive(Debug, Default, Clone)]
pub struct Taps {
    pub pre_fx: Option<Arc<AuxChannels>>,
    pub post_fx: Option<Arc<AuxChannels>>,
    /// The pre-FX volume envelope, which acts before both taps (the main path applies it with
    /// the fader).
    pub pre_fx_env: Option<Envelope>,
}

impl Taps {
    fn new<'a>(channels: usize, modes: impl Iterator<Item = &'a RenderSend>, pre_fx_env: Option<Envelope>) -> Self {
        let modes: Vec<SendMode> = modes.map(|s| s.tap).collect();
        let tap = |m: SendMode| modes.contains(&m).then(|| Arc::new(AuxChannels::new(channels)));
        Self { pre_fx: tap(SendMode::PreFx), post_fx: tap(SendMode::PostFx), pre_fx_env }
    }

    /// Copies channels 1/2 and the rest into the tap, through the pre-FX volume envelope from
    /// project frame `frame` on.
    fn fill(
        &self,
        tap: Option<&Arc<AuxChannels>>,
        l: &[f32],
        r: &[f32],
        aux: Option<&[Vec<f32>]>,
        frame: i64,
        sr: f64,
    ) {
        let Some(mut bufs) = tap.and_then(|t| t.0.try_lock().ok()) else { return };
        let n = l.len();
        let rest = aux.unwrap_or(&[]);
        for (c, b) in bufs.iter_mut().enumerate() {
            let src = match c {
                0 => l,
                1 => r,
                c => rest.get(c - 2).map_or(&[][..], |a| &a[..n]),
            };
            if src.is_empty() {
                b[..n].fill(0.0);
                continue;
            }
            b[..n].copy_from_slice(src);
            if let Some(env) = &self.pre_fx_env {
                for (i, s) in b[..n].iter_mut().enumerate() {
                    *s *= env.value_at((frame + i as i64) as f64 / sr).unwrap_or(1.0) as f32;
                }
            }
        }
    }

    fn clear(&self, n: usize) {
        for tap in self.pre_fx.iter().chain(&self.post_fx) {
            if let Ok(mut bufs) = tap.0.try_lock() {
                for b in bufs.iter_mut() {
                    b[..n].fill(0.0);
                }
            }
        }
    }
}

/// Channels 3 and up of a track, allocated when the graph is built so rendering never
/// allocates. Only the track's own render and the routing after it touch them.
#[derive(Debug)]
pub struct AuxChannels(std::sync::Mutex<Vec<Vec<f32>>>);

/// The MIDI the chain carries in the current block, the next effect's output and, on tracks
/// that route MIDI, what enters the chain.
#[derive(Debug, Default)]
pub struct ChainMidi(std::sync::Mutex<[crate::fx::MidiBlock; 3]>);

/// MIDI sent to a track by others in the current block, as frame offsets from its start.
#[derive(Debug, Default)]
pub struct MidiInbox(std::sync::Mutex<crate::fx::MidiBlock>);

impl AuxChannels {
    fn new(channels: usize) -> Self {
        Self(std::sync::Mutex::new((0..channels).map(|_| vec![0.0; RenderScratch::MAX_BLOCK]).collect()))
    }
}

/// A delay of a fixed number of frames over a few channels, used for plugin delay
/// compensation.
#[derive(Debug)]
pub struct DelayLine {
    state: std::sync::Mutex<(Vec<Vec<f32>>, usize)>,
}

impl DelayLine {
    fn new(frames: usize, channels: usize) -> Self {
        Self { state: std::sync::Mutex::new(((0..channels.max(1)).map(|_| vec![0.0; frames]).collect(), 0)) }
    }
}

/// Live input for one render call: per hardware channel, the samples aligned with the output.
pub struct LiveInput<'a> {
    pub channels: [Option<&'a [f32]>; LiveInput::MAX_CHANNELS],
    /// MIDI from the inputs, due at the start of the block.
    pub midi: &'a [LiveMidi],
    pub play_items: bool,
    /// A recording runs: output recording taps write, tape-style monitoring listens and
    /// recording tracks mute their items unless they monitor them.
    pub recording: bool,
    pub midi_out: Option<MidiOutClock<'a>>,
    pub clicks: Clicks,
}

/// Where hardware MIDI goes, and when a project frame is heard.
#[derive(Clone, Copy)]
pub struct MidiOutClock<'a> {
    pub queue: &'a std::sync::Mutex<rtrb::Producer<OutMessage>>,
    /// The master frame `frame` leaves the speakers at `at`.
    pub at: std::time::Instant,
    pub frame: i64,
}

impl Default for LiveInput<'_> {
    fn default() -> Self {
        Self {
            channels: [None; Self::MAX_CHANNELS],
            midi: &[],
            play_items: true,
            recording: false,
            midi_out: None,
            clicks: Clicks::Off,
        }
    }
}

impl<'a> LiveInput<'a> {
    pub const MAX_CHANNELS: usize = 64;

    fn slice(&self, from: usize, len: usize) -> LiveInput<'a> {
        LiveInput {
            channels: self.channels.map(|c| c.and_then(|s| s.get(from..(from + len).min(s.len())))),
            midi: if from == 0 { self.midi } else { &[] },
            play_items: self.play_items,
            recording: self.recording,
            midi_out: self.midi_out,
            clicks: match self.clicks {
                Clicks::CountIn { clicks, at } => Clicks::CountIn { clicks, at: at + from as i64 },
                Clicks::CountInTail { clicks, at, beats } => {
                    Clicks::CountInTail { clicks, at: at + from as i64, beats }
                }
                c => c,
            },
        }
    }
}

/// Immutable snapshot of everything the audio thread needs. Rebuilt on the UI side whenever the
/// project changes and swapped in atomically.
#[derive(Debug, Clone)]
pub struct RenderGraph {
    pub sample_rate: u32,
    pub tracks: Vec<RenderTrack>,
    /// Tracks in an order where every send source precedes its destination.
    pub order: Vec<usize>,
    /// Dependency level of every track: tracks only receive audio from lower levels.
    pub level_of: Vec<usize>,
    pub level_sizes: Vec<usize>,
    pub level_count: usize,
    pub master_gain_l: f32,
    pub master_gain_r: f32,
    pub master_meter: Arc<Meter>,
    pub master_loudness: Option<Arc<Loudness>>,
    pub master_chain: Arc<FxChain>,
    /// The monitoring effects, on what the device plays only (never in renders).
    pub monitor_chain: Option<Arc<BufferedChain>>,
    /// Master channels; 3 and up live in `master_aux`.
    pub master_channels: usize,
    pub master_aux: Option<Arc<AuxChannels>>,
    pub master_volume: f32,
    /// An effect's parameter follows one of another effect in its chain: REAPER runs the
    /// whole chain block by block, so renders go 512 frames at a time.
    pub linked_fx: bool,
    pub master_pin_bufs: Option<Arc<AuxChannels>>,
    /// The master's hardware outputs; `None` sends every master channel to the device output
    /// of the same number (REAPER's default output, see `Project::master_default_output`).
    pub master_hw_outs: Option<Vec<RenderSend>>,
    pub master_taps: Taps,
    /// Device output channels, when hardware outputs other than the master's default play;
    /// otherwise the device gets the master's channels.
    pub hw_bus: Option<Arc<AuxChannels>>,
    /// Device outputs the project plays to.
    pub device_channels: usize,
    pub timeline: Arc<Timeline>,
    pub clicks: Arc<ClickSounds>,
    pub metronome: MetronomeRouting,
}

/// Scratch space owned by the audio callback so rendering never allocates.
#[derive(Debug)]
pub struct RenderScratch {
    track_bufs: Vec<(Vec<f32>, Vec<f32>)>,
    /// The tracks of the level being rendered, slowest first.
    level_order: Vec<usize>,
    capacity: usize,
}

impl RenderScratch {
    pub const MAX_BLOCK: usize = 2048;

    pub fn new(max_tracks: usize) -> Self {
        Self {
            track_bufs: (0..max_tracks).map(|_| (vec![0.0; Self::MAX_BLOCK], vec![0.0; Self::MAX_BLOCK])).collect(),
            level_order: Vec::with_capacity(max_tracks),
            capacity: Self::MAX_BLOCK,
        }
    }

    fn ensure(&mut self, tracks: usize) -> bool {
        self.track_bufs.len() >= tracks
    }
}

/// Meters live as long as the track does across graph rebuilds, so the UI can keep its handle.
/// The MIDI a track's effects put out while it records "Output (MIDI)": messages with the
/// project frame they left at.
pub type MidiRecordTap = std::sync::Mutex<Vec<(i64, [u8; 3])>>;

#[derive(Debug, Default)]
pub struct MeterRegistry {
    tracks: HashMap<TrackId, Arc<Meter>>,
    record_taps: HashMap<TrackId, Arc<crate::record::OutputTap>>,
    midi_record_taps: HashMap<TrackId, Arc<MidiRecordTap>>,
    pub master: Arc<Meter>,
    pub loudness: Option<Arc<Loudness>>,
}

impl MeterRegistry {
    pub fn track(&mut self, id: TrackId) -> Arc<Meter> {
        self.tracks.entry(id).or_default().clone()
    }

    /// The output recording tap of track `id`, while it records its output.
    pub fn record_tap(&self, id: TrackId) -> Option<Arc<crate::record::OutputTap>> {
        self.record_taps.get(&id).cloned()
    }

    pub fn midi_record_tap(&self, id: TrackId) -> Option<Arc<MidiRecordTap>> {
        self.midi_record_taps.get(&id).cloned()
    }

    pub fn set_midi_record_tap(&mut self, id: TrackId, tap: Option<Arc<MidiRecordTap>>) {
        match tap {
            Some(t) => self.midi_record_taps.insert(id, t),
            None => self.midi_record_taps.remove(&id),
        };
    }

    pub fn set_record_tap(&mut self, id: TrackId, tap: Option<Arc<crate::record::OutputTap>>) {
        match tap {
            Some(t) => self.record_taps.insert(id, t),
            None => self.record_taps.remove(&id),
        };
    }

    /// The master loudness meter for `sample_rate`, created on first use.
    pub fn loudness(&mut self, sample_rate: u32) -> Arc<Loudness> {
        self.loudness.get_or_insert_with(|| Arc::new(Loudness::new(sample_rate))).clone()
    }

    pub fn get(&self, id: TrackId) -> Option<&Arc<Meter>> {
        self.tracks.get(&id)
    }

    pub fn retain(&mut self, project: &Project) {
        self.tracks.retain(|id, _| project.track(*id).is_some());
    }
}

impl RenderGraph {
    pub fn empty(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            tracks: Vec::new(),
            order: Vec::new(),
            level_of: Vec::new(),
            level_sizes: Vec::new(),
            level_count: 0,
            master_gain_l: 1.0,
            master_gain_r: 1.0,
            master_meter: Arc::default(),
            master_loudness: None,
            master_chain: Arc::default(),
            monitor_chain: None,
            master_channels: 2,
            master_aux: None,
            master_volume: 1.0,
            linked_fx: false,
            master_pin_bufs: None,
            master_hw_outs: None,
            master_taps: Taps::default(),
            hw_bus: None,
            device_channels: 2,
            timeline: Arc::new(scythe_core::TempoMap::default().timeline()),
            clicks: Arc::new(ClickSounds::new(sample_rate)),
            metronome: MetronomeRouting::default(),
        }
    }

    /// Items whose media is not decoded yet are skipped; the caller rebuilds once it is.
    pub fn build(
        project: &Project,
        pool: &MediaPool,
        meters: &mut MeterRegistry,
        fx: &mut FxRegistry,
        sample_rate: u32,
    ) -> Self {
        Self::build_with_click(project, pool, meters, fx, sample_rate, -1)
    }

    /// [`RenderGraph::build`], with the metronome playing to `click_output` (a `METRONOMEOUT`
    /// value, -1 for the master) when the project names no output.
    pub fn build_with_click(
        project: &Project,
        pool: &MediaPool,
        meters: &mut MeterRegistry,
        fx: &mut FxRegistry,
        sample_rate: u32,
        click_output: i64,
    ) -> Self {
        fx.automation = project.automation_context();
        let sr = sample_rate as f64;
        let index: HashMap<TrackId, usize> = project.tracks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
        let timeline = Arc::new(project.tempo.timeline());
        let parents = project.folder_parents();
        let routing = project.solo_routing();
        let mut tracks = project
            .tracks
            .iter()
            .enumerate()
            .map(|(ti, t)| {
                let fader = FaderStage::new(project, ti);
                let items = t
                    .items
                    .iter()
                    .filter(|it| !it.muted && it.length > 0.0)
                    // Fixed lanes that are not soloed stay silent, like REAPER's comp lanes.
                    .filter(|it| t.lane_plays(it.lane()))
                    .filter_map(|it| {
                        let take = it.take()?;
                        let (mut section_start, mut section_len, mut reverse) = match &take.source {
                            Source::Section(s) => (s.start, s.length.max(0.0), s.reverse),
                            _ => (0.0, 0.0, false),
                        };
                        let (mut source_offset, mut playrate, mut looped) =
                            (take.source_pos(0.0), take.playrate, it.looped);
                        // A processed copy runs on its own time axis, `time` times the source's.
                        let mut time = 1.0;
                        let audio = match &take.source {
                            Source::Raw(c) if c.args().first().is_some_and(|k| k == "CLICK") => ItemAudio::Click,
                            Source::Raw(c) if c.args().first().is_some_and(|k| k == "MIDI") => {
                                ItemAudio::Midi(Arc::new(midi_notes(c, it, take, &timeline, sample_rate)?))
                            }
                            Source::Raw(c) if c.args().first().is_some_and(|k| k == "LTC") => {
                                ItemAudio::Timecode(Arc::new(TimecodeItem::parse(c)))
                            }
                            source => {
                                let path = scythe_core::rpp::resolve_media_path(project, source.file()?);
                                let base = pool.ready(&path)?;
                                // A warped take was rendered for the item and plays straight through.
                                let warped = crate::warp::Warp::of(it, take, project.default_pitch_mode)
                                    .and_then(|w| pool.warped(&path, &w));
                                if let Some(audio) = warped {
                                    (section_start, section_len, reverse) = (0.0, 0.0, false);
                                    (source_offset, playrate, looped) = (0.0, 1.0, false);
                                    ItemAudio::Media(audio)
                                } else {
                                    // Until the processed copy is ready the take plays resampled.
                                    let processed = take
                                        .processing(project.default_pitch_mode)
                                        .and_then(|p| Some((p.length_ratio(), pool.processed(&path, &p)?)));
                                    match processed {
                                        Some((t, copy)) => {
                                            time = t;
                                            ItemAudio::Media(copy)
                                        }
                                        None => ItemAudio::Media(base.audio.clone()),
                                    }
                                }
                            }
                        };
                        let take_slots = take.fx();
                        let take_fx = take_slots.iter().any(FxSlot::active).then(|| {
                            let source_channels = match &audio {
                                ItemAudio::Media(a) => a.channels.len(),
                                _ => 2,
                            };
                            let channels = take.fx_channels().map_or(source_channels, |c| c as usize).max(2);
                            let owner = FxOwner::Take(it.id, it.active_take);
                            Arc::new(BufferedChain::new(
                                crate::chain::build(fx, &owner, &take_slots, channels),
                                channels,
                            ))
                        });
                        // Take pan never attenuates the centre in REAPER renders, whatever the project law.
                        let (tl, tr) = scythe_core::units::PanLaw::default().gains(take.pan);
                        let src_sr = sr * time;
                        let g = (it.volume * take.volume) as f32;
                        Some(RenderItem {
                            start: (it.position * sr).round() as i64,
                            length: (it.length * sr).round() as i64,
                            source_offset: source_offset * src_sr,
                            playrate: playrate * time,
                            gain_l: g * tl as f32,
                            gain_r: g * tr as f32,
                            fade_in: ((it.fade_in.effective_length() * sr).round() as i64, it.fade_in),
                            fade_out: ((it.fade_out.effective_length() * sr).round() as i64, it.fade_out),
                            automation: GainAutomation::new(
                                it.volume * take.volume,
                                take.pan,
                                scythe_core::units::PanLaw::default(),
                                &take.envelopes,
                            )
                            .map(Arc::new),
                            looped,
                            channel_mode: take.channel_mode,
                            section_start: section_start * src_sr,
                            section_len: section_len * src_sr,
                            reverse,
                            audio,
                            take_fx,
                        })
                    })
                    .collect::<Vec<_>>();
                let chain =
                    Arc::new(crate::chain::build(fx, &FxOwner::Track(t.id), &t.fx, (t.channels as usize).max(2)));
                let pin_count = chain.pin_buffers();
                // MIDI items feed an instrument in the chain or a hardware output instead of the
                // preview synth.
                let midi_sends: Vec<(usize, SendMidi, bool)> = t
                    .sends
                    .iter()
                    .zip(&routing.sends[ti])
                    .filter(|(s, on)| **on && !s.muted && s.midi().enabled)
                    .map(|(s, _)| s)
                    .filter_map(|s| Some((*index.get(&s.dest)?, s.midi(), s.mode == SendMode::PreFx)))
                    .collect();
                let (items, notes) = if chain.has_instrument()
                    || chain.reads_midi()
                    || t.midi_out.is_some()
                    || !midi_sends.is_empty()
                    || meters.midi_record_tap(t.id).is_some()
                {
                    let (midi, audio): (Vec<_>, Vec<_>) =
                        items.into_iter().partition(|i| matches!(i.audio, ItemAudio::Midi(_)) && i.take_fx.is_none());
                    let lists: Vec<&NoteList> = midi
                        .iter()
                        .filter_map(|i| match &i.audio {
                            ItemAudio::Midi(list) => Some(list.as_ref()),
                            _ => None,
                        })
                        .collect();
                    let notes = lists.iter().flat_map(|l| l.notes.iter().copied()).collect();
                    let controls = lists.iter().flat_map(|l| l.controls().iter().copied()).collect();
                    let long = lists.iter().flat_map(|l| l.long().iter().cloned()).collect();
                    (audio, Some(Arc::new(NoteList::new(notes, sample_rate).with_controls(controls).with_long(long))))
                } else {
                    (items, None)
                };
                let (sends, send_automation): (Vec<_>, Vec<_>) = t
                    .sends
                    .iter()
                    .zip(&routing.sends[ti])
                    .filter(|(s, on)| !s.muted && **on)
                    .map(|(s, _)| s)
                    .filter_map(|s| {
                        let automation = GainAutomation::new(s.volume, s.pan, project.pan_law, &s.envelopes);
                        let send = RenderSend::new(
                            *index.get(&s.dest)?,
                            s.volume,
                            s.pan,
                            project.pan_law,
                            s.src_channels,
                            s.dst_channels,
                            s.mono,
                            s.phase,
                            s.mode,
                        )?;
                        Some((send, automation.map(Arc::new)))
                    })
                    .unzip();
                let hw_outs: Vec<RenderSend> =
                    t.hw_outs.iter().filter_map(|o| RenderSend::hw(o, project.pan_law)).collect();
                let channels = (t.channels as usize).max(2);
                let monitor = (t.armed && t.monitor && t.rec_input >= 0 && t.rec_input & (512 | 2048 | 4096) == 0)
                    .then(|| {
                        let first = (t.rec_input & 511) as usize;
                        (first, (t.rec_input & 1024 != 0).then_some(first + 1))
                    });
                let input_slots = t.input_fx();
                // Built while not monitored too, so the effects run (for their parameters and
                // editors) as REAPER's do; only a monitored input plays through them.
                let input_chain = input_slots
                    .iter()
                    .any(FxSlot::active)
                    .then(|| crate::chain::build(fx, &FxOwner::Input(t.id), &input_slots, 2));
                let input_fx = input_chain.filter(|_| monitor.is_some()).map(|c| Arc::new(BufferedChain::new(c, 2)));
                let pre_fx_env = t
                    .envelope(EnvelopeKind::VolumePreFx)
                    .filter(|e| e.active && !e.points.is_empty())
                    .map(|e| Envelope { raw: None, ..e.clone() });
                RenderTrack {
                    id: t.id,
                    matrix: fader.matrix,
                    volume: fader.volume as f32,
                    audible: routing.processes(ti),
                    content: routing.content[ti],
                    main_out: routing.main_out[ti],
                    hw_out: routing.hw_out[ti],
                    pre_fx_gain: fader.pre_fx.map(Arc::new),
                    items,
                    meter: meters.track(t.id),
                    taps: Taps::new(channels, sends.iter().chain(&hw_outs), pre_fx_env),
                    sends,
                    hw_outs,
                    channels,
                    aux: (channels > 2).then(|| Arc::new(AuxChannels::new(channels - 2))),
                    main_offset: t.main_send_offset as usize,
                    send_automation,
                    parent: parents[ti],
                    main_send: t.main_send,
                    automation: fader.automation.map(Arc::new),
                    vca_ramp: fader.vca_ramp.map(|r| Arc::new((r, AtomicI64::new(i64::MIN)))),
                    chain,
                    pin_bufs: pin_count.map(|c| Arc::new(AuxChannels::new(c.min(64)))),
                    chain_midi: Arc::default(),
                    midi_sends,
                    midi_inbox: None,
                    notes,
                    monitor,
                    input_fx,
                    midi_input: (t.armed && t.monitor).then(|| RecInput::from_rec_input(t.rec_input)).flatten(),
                    monitor_auto: t.monitor_auto,
                    mute_items_recording: t.armed
                        && !t.monitor_items
                        && !matches!(t.rec_mode, RecordMode::Disabled | RecordMode::MidiOutput),
                    record_tap: meters.record_tap(t.id),
                    midi_record: meters.midi_record_tap(t.id),
                    live_buf: Arc::new(std::sync::Mutex::new(Vec::with_capacity(512))),
                    midi_out: t.midi_out,
                    read_ahead: 0,
                    send_delays: Vec::new(),
                    main_delay: None,
                    hw_delays: Vec::new(),
                    click: (t.ext(scythe_core::monitor::EXT_MUSICIAN).is_some())
                        .then(|| t.ext(scythe_core::monitor::EXT_CLICK)?.trim().parse::<f64>().ok())
                        .flatten()
                        .map_or(0.0, |g| (g * CLICK_LEVEL) as f32),
                }
            })
            .collect::<Vec<_>>();
        let midi_dests: Vec<usize> = tracks.iter().flat_map(|t| t.midi_sends.iter().map(|s| s.0)).collect();
        for d in midi_dests {
            tracks[d].midi_inbox.get_or_insert_with(Arc::default);
        }
        let master_channels = (project.master.channels as usize).max(2);
        let master_chain = Arc::new(crate::chain::build(fx, &FxOwner::Master, &project.master.fx, master_channels));
        let master_pin_count = master_chain.pin_buffers();
        let order = send_order(&tracks);
        compensate_latency(&mut tracks, &order, master_chain.latency);
        let level_of = dependency_levels(&tracks, &order);
        let level_count = level_of.iter().max().map_or(0, |m| m + 1);
        let mut level_sizes = vec![0; level_count];
        for &l in &level_of {
            level_sizes[l] += 1;
        }
        // REAPER renders show no centre attenuation on the master, whatever the project law.
        let (ml, mr) = scythe_core::units::PanLaw::default().gains(project.master.pan);
        let mv = if project.master.muted { 0.0 } else { project.master.volume };
        let master_hw_outs = (!project.master_default_output()).then(|| {
            // Muting the master silences its outputs whatever they tap, as muting a track does.
            let outs = project.master.hw_outs.iter().filter(|_| !project.master.muted);
            outs.filter_map(|o| RenderSend::hw(o, project.pan_law)).collect::<Vec<_>>()
        });
        let master_taps = match &master_hw_outs {
            Some(outs) => Taps::new(
                master_channels,
                outs.iter(),
                project
                    .master
                    .envelope(EnvelopeKind::VolumePreFx)
                    .filter(|e| e.active && !e.points.is_empty())
                    .map(|e| Envelope { raw: None, ..e.clone() }),
            ),
            None => Taps::default(),
        };
        let track_outputs = tracks.iter().flat_map(|t| &t.hw_outs).map(RenderSend::end).max();
        let master_outputs = match &master_hw_outs {
            Some(outs) => outs.iter().map(RenderSend::end).max().unwrap_or(0),
            None => master_channels,
        };
        let metronome = MetronomeRouting::from_project(project, click_output);
        let insert_outputs = tracks
            .iter()
            .map(|t| &t.chain)
            .chain([&master_chain])
            .flat_map(|c| c.effects())
            .filter_map(|e| e.fx.with(|e| e.device_outputs()))
            .max()
            .unwrap_or(0);
        let device_channels =
            track_outputs.unwrap_or(0).max(master_outputs).max(metronome.end().unwrap_or(0)).max(insert_outputs).max(2);
        let monitor_slots = fx.monitor_fx.clone();
        let monitor_chain = monitor_slots.iter().any(FxSlot::active).then(|| {
            let chain = crate::chain::build(fx, &FxOwner::Monitor, &monitor_slots, device_channels);
            Arc::new(BufferedChain::new(chain, device_channels))
        });
        let hw_bus = (track_outputs.is_some() || master_hw_outs.is_some() || metronome.end().is_some())
            .then(|| Arc::new(AuxChannels::new(device_channels)));
        let linked_fx = tracks.iter().any(|t| t.chain.links_effects());
        Self {
            master_hw_outs,
            master_taps,
            hw_bus,
            device_channels,
            sample_rate,
            order,
            level_of,
            level_sizes,
            level_count,
            tracks,
            master_gain_l: (mv * ml) as f32,
            master_gain_r: (mv * mr) as f32,
            master_meter: meters.master.clone(),
            master_loudness: Some(meters.loudness(sample_rate)),
            master_chain,
            monitor_chain,
            master_channels,
            master_aux: (master_channels > 2).then(|| Arc::new(AuxChannels::new(master_channels - 2))),
            master_volume: mv as f32,
            linked_fx,
            master_pin_bufs: master_pin_count.map(|c| Arc::new(AuxChannels::new(c.min(64)))),
            timeline,
            clicks: Arc::new(ClickSounds::new(sample_rate)),
            metronome,
        }
    }

    /// Mixes `out_l.len()` frames starting at project frame `start` into `out_l`/`out_r`
    /// (overwriting them). Blocks longer than `RenderScratch::MAX_BLOCK` are split.
    pub fn render(&self, start: i64, out_l: &mut [f32], out_r: &mut [f32], scratch: &mut RenderScratch) {
        self.render_with(start, out_l, out_r, scratch, &LiveInput::default());
    }

    /// Master channels 3 and up of the block just rendered (at most `RenderScratch::MAX_BLOCK`
    /// frames), for multichannel renders and outputs.
    pub fn master_extra(&self, n: usize, out: &mut [Vec<f32>]) {
        self.master_extra_at(0, n, out);
    }

    /// Like `master_extra`, written from frame `at` of each buffer in `out`.
    pub fn master_extra_at(&self, at: usize, n: usize, out: &mut [Vec<f32>]) {
        let Some(aux) = self.master_aux.as_ref() else { return };
        let Ok(bufs) = aux.0.try_lock() else { return };
        for (dst, src) in out.iter_mut().zip(bufs.iter()) {
            dst[at..at + n].copy_from_slice(&src[..n]);
        }
    }

    /// Whether any track listens to the live input.
    /// Frames of plugin latency the graph compensates by reading ahead; a render starting
    /// anywhere should first run this many frames so the effects have heard them.
    /// The frames to render from `frame` on in one call: up to the next 512-frame block when
    /// effects link across the chain, else `n`.
    pub fn block_len(&self, frame: i64, n: usize) -> usize {
        if self.linked_fx { n.min((512 - frame.rem_euclid(512)) as usize) } else { n }
    }

    pub fn latency(&self) -> i64 {
        let master: i64 = self.master_chain.latency;
        self.tracks.iter().map(|t| t.read_ahead).max().unwrap_or(0).max(master)
    }

    pub fn monitoring(&self) -> bool {
        self.tracks.iter().any(|t| t.monitor.is_some() || t.midi_input.is_some())
    }

    /// Like `render`, with live input for monitored tracks. With `play_items` false only the
    /// live input and effect tails sound (the transport is stopped).
    pub fn render_with(
        &self,
        start: i64,
        out_l: &mut [f32],
        out_r: &mut [f32],
        scratch: &mut RenderScratch,
        input: &LiveInput,
    ) {
        let n = out_l.len().min(out_r.len());
        let mut done = 0;
        while done < n {
            let len = (n - done).min(scratch.capacity);
            let block = input.slice(done, len);
            self.render_block(
                start + done as i64,
                &mut out_l[done..done + len],
                &mut out_r[done..done + len],
                scratch,
                &block,
            );
            done += len;
        }
    }

    fn render_block(
        &self,
        start: i64,
        out_l: &mut [f32],
        out_r: &mut [f32],
        scratch: &mut RenderScratch,
        input: &LiveInput,
    ) {
        out_l.fill(0.0);
        out_r.fill(0.0);
        let n = out_l.len();
        if !scratch.ensure(self.tracks.len()) {
            // Graph grew beyond what the callback preallocated; stay silent rather than allocate.
            return;
        }
        for (l, r) in scratch.track_bufs.iter_mut().take(self.tracks.len()) {
            l[..n].fill(0.0);
            r[..n].fill(0.0);
        }
        for aux in self.tracks.iter().filter_map(|t| t.aux.as_ref()).chain(&self.master_aux).chain(&self.hw_bus) {
            if let Ok(mut bufs) = aux.0.try_lock() {
                for b in bufs.iter_mut() {
                    b[..n].fill(0.0);
                }
            }
        }
        for inbox in self.tracks.iter().filter_map(|t| t.midi_inbox.as_ref()) {
            if let Ok(mut m) = inbox.0.try_lock() {
                m.clear();
            }
        }
        let tracks = self.tracks.len();
        for level in 0..self.level_count {
            // Tracks within a level do not feed each other, so they render in parallel; routing
            // into parents, send destinations and the master happens afterwards, in order.
            let RenderScratch { track_bufs, level_order, .. } = &mut *scratch;
            let bufs = &mut track_bufs[..tracks];
            let in_level = |i: usize| self.level_of[i] == level;
            if self.level_sizes[level] > 1 {
                // Each thread takes the slowest track left, so a heavy track does not start
                // last behind light ones that happened to sit before it.
                level_order.clear();
                level_order.extend((0..tracks).filter(|&i| in_level(i)));
                level_order.sort_unstable_by_key(|&i| std::cmp::Reverse(self.tracks[i].meter.cpu.last()));
                let order = &level_order[..];
                let next = std::sync::atomic::AtomicUsize::new(0);
                let bufs = TrackBufs(bufs.as_mut_ptr());
                let pool = track_threads();
                pool.install(|| {
                    (0..order.len().min(pool.current_num_threads())).into_par_iter().with_max_len(1).for_each(|_| {
                        while let Some(&i) = order.get(next.fetch_add(1, Ordering::Relaxed)) {
                            // SAFETY: `order` names each track once and every index is taken
                            // by one thread only, so no two threads reach the same buffers.
                            let (l, r) = unsafe { bufs.get(i) };
                            let started = Instant::now();
                            self.process_track(i, start, &mut l[..n], &mut r[..n], input);
                            self.tracks[i].meter.cpu.add(started);
                        }
                    });
                });
            } else if let Some(i) = (0..tracks).find(|&i| in_level(i)) {
                let (l, r) = &mut bufs[i];
                let started = Instant::now();
                self.process_track(i, start, &mut l[..n], &mut r[..n], input);
                self.tracks[i].meter.cpu.add(started);
            }
            for &ti in self.order.iter().filter(|&&i| in_level(i)) {
                self.route_track(ti, start, n, scratch, out_l, out_r);
            }
        }
        let mut master_aux = self.master_aux.as_ref().and_then(|a| a.0.try_lock().ok());
        let sr = self.sample_rate as f64;
        let taps = &self.master_taps;
        taps.fill(taps.pre_fx.as_ref(), out_l, out_r, master_aux.as_deref().map(|a| &a[..]), start, sr);
        if !self.master_chain.is_empty() {
            // The master's input is the master effects' latency ahead of the output.
            let position = start + self.master_chain.latency;
            let mut pin_bufs = self.master_pin_bufs.as_ref().and_then(|p| p.0.try_lock().ok());
            let sr = self.sample_rate as f64;
            let tempo = |f: i64| self.timeline.bpm_at(f as f64 / sr);
            let chain_input = ChainInput {
                sample_rate: self.sample_rate,
                playing: input.play_items,
                notes: None,
                live: &[],
                live_channel: None,
                tempo: &tempo,
                timeline: &self.timeline,
            };
            let aux: &mut [Vec<f32>] = match master_aux.as_deref_mut() {
                Some(a) => a,
                None => &mut [],
            };
            let mut ch = Channels { l: out_l, r: out_r, aux, n };
            let mut midi = MidiState { bufs: None, has: false };
            self.master_chain.process(
                &mut ch,
                &chain_input,
                position,
                &mut midi,
                pin_bufs.as_deref_mut().map(|b| &mut b[..]),
            );
        }
        taps.fill(taps.post_fx.as_ref(), out_l, out_r, master_aux.as_deref().map(|a| &a[..]), start, sr);
        // The master fader acts on every channel; its pan on the first pair only.
        if let Some(aux) = master_aux.as_deref_mut() {
            for b in aux.iter_mut() {
                for s in &mut b[..n] {
                    *s *= self.master_volume;
                }
            }
        }
        drop(master_aux);
        for s in out_l.iter_mut() {
            *s *= self.master_gain_l;
        }
        for s in out_r.iter_mut() {
            *s *= self.master_gain_r;
        }
        self.master_meter.record(out_l, out_r);
        if let Some(l) = &self.master_loudness {
            l.record(out_l, out_r);
        }
        let gain = self.metronome.gain;
        if self.metronome.outputs.is_empty() && gain > 0.0 {
            self.each_click(input.clicks, start, n, |o, s| {
                out_l[o] += s * gain;
                out_r[o] += s * gain;
            });
        }
        self.master_to_device(n, out_l, out_r);
        if !self.metronome.outputs.is_empty()
            && gain > 0.0
            && input.clicks != Clicks::Off
            && let Some(mut bus) = self.hw_bus.as_ref().and_then(|b| b.0.try_lock().ok())
        {
            for &(first, mono) in &self.metronome.outputs {
                let pair = if mono { first..first + 1 } else { first..first + 2 };
                for c in pair {
                    let Some(out) = bus.get_mut(c) else { continue };
                    self.each_click(input.clicks, start, n, |o, s| out[o] += s * gain);
                }
            }
        }
    }

    /// Adds the master's hardware outputs to the device bus, when there is one.
    fn master_to_device(&self, n: usize, out_l: &[f32], out_r: &[f32]) {
        let Some(mut bus) = self.hw_bus.as_ref().and_then(|b| b.0.try_lock().ok()) else { return };
        let master_aux = self.master_aux.as_ref().and_then(|a| a.0.try_lock().ok());
        let post = |c: usize| -> Option<&[f32]> {
            match c {
                0 => Some(&out_l[..n]),
                1 => Some(&out_r[..n]),
                c => master_aux.as_ref()?.get(c - 2).map(|b| &b[..n]),
            }
        };
        let Some(outs) = &self.master_hw_outs else {
            for (c, b) in bus.iter_mut().enumerate().take(self.master_channels) {
                if let Some(src) = post(c) {
                    b[..n].iter_mut().zip(src).for_each(|(o, s)| *o += s);
                }
            }
            return;
        };
        let pre = self.master_taps.pre_fx.as_ref().and_then(|t| t.0.try_lock().ok());
        let post_fx = self.master_taps.post_fx.as_ref().and_then(|t| t.0.try_lock().ok());
        for o in outs {
            for (src, dst, right, scale) in o.routes() {
                let input = match o.tap {
                    SendMode::PreFx => pre.as_ref().and_then(|b| b.get(src)).map(|b| &b[..n]),
                    SendMode::PostFx | SendMode::PostFxLegacy => {
                        post_fx.as_ref().and_then(|b| b.get(src)).map(|b| &b[..n])
                    }
                    SendMode::PostFader => post(src),
                };
                let (Some(input), Some(target)) = (input, bus.get_mut(dst)) else { continue };
                let g = scale * if right { o.gain_r } else { o.gain_l };
                target[..n].iter_mut().zip(input).for_each(|(t, s)| *t += s * g);
            }
        }
    }

    /// Device output channels of the block just rendered, when hardware outputs other than
    /// the master's default play: written from frame `at` of `left`, `right` and `extra` (the
    /// outputs past the first pair), replacing the master's channels there. Returns false (and
    /// leaves them alone) when the device simply gets the master.
    pub fn device_out_at(
        &self,
        at: usize,
        n: usize,
        left: &mut [f32],
        right: &mut [f32],
        extra: &mut [Vec<f32>],
    ) -> bool {
        let Some(bus) = self.hw_bus.as_ref() else { return false };
        let Ok(bufs) = bus.0.try_lock() else { return false };
        left[at..at + n].copy_from_slice(&bufs[0][..n]);
        right[at..at + n].copy_from_slice(&bufs[1][..n]);
        for (c, dst) in extra.iter_mut().enumerate() {
            match bufs.get(c + 2) {
                Some(src) => dst[at..at + n].copy_from_slice(&src[..n]),
                None => dst[at..at + n].fill(0.0),
            }
        }
        true
    }

    /// Runs the monitoring effects over the device output of the block just rendered (frames
    /// `at..at + n` of `left`, `right` and `extra`), the block starting at project frame
    /// `position`.
    pub fn monitor_fx_at(
        &self,
        position: i64,
        at: usize,
        n: usize,
        left: &mut [f32],
        right: &mut [f32],
        extra: &mut [Vec<f32>],
    ) {
        let Some(chain) = &self.monitor_chain else { return };
        let sr = self.sample_rate as f64;
        let tempo = |f: i64| self.timeline.bpm_at(f as f64 / sr);
        let input = ChainInput {
            sample_rate: self.sample_rate,
            playing: true,
            notes: None,
            live: &[],
            live_channel: None,
            tempo: &tempo,
            timeline: &self.timeline,
        };
        chain.run(n, position, &input, |io, bufs| {
            let (l, rest) = bufs.split_at_mut(1);
            let (r, more) = rest.split_at_mut(1);
            let pairs = [(&mut l[0], &mut left[at..at + n]), (&mut r[0], &mut right[at..at + n])]
                .into_iter()
                .chain(more.iter_mut().zip(extra.iter_mut().map(|e| &mut e[at..at + n])));
            for (b, dev) in pairs {
                match io {
                    ChainIo::Fill(_) => b[..n].copy_from_slice(dev),
                    ChainIo::Drain => dev.copy_from_slice(&b[..n]),
                }
            }
        });
    }

    /// Items, FX and fader of one track, in its own buffer.
    fn process_track(&self, ti: usize, start: i64, tl: &mut [f32], tr: &mut [f32], input: &LiveInput) {
        let track = &self.tracks[ti];
        let n = tl.len();
        let mut live_buf = track.live_buf.try_lock().ok();
        let live: &[LiveMidi] = match (track.midi_input, live_buf.as_deref_mut()) {
            (None, _) => &[],
            _ if !track.content || (track.monitor_auto && input.play_items && !input.recording) => &[],
            (Some(i), _) if i.device == scythe_core::midi_route::MidiDevice::All => input.midi,
            (Some(i), Some(buf)) => {
                buf.clear();
                buf.extend(input.midi.iter().filter(|m| i.device.hears(m.device)).take(buf.capacity()));
                buf
            }
            (Some(_), None) => &[],
        };
        let live_channel = track.midi_input.and_then(|i| i.channel);
        let records_midi = track.midi_record.is_some() && input.recording && input.play_items;
        let routes_midi = track.audible && (track.midi_inbox.is_some() || !track.midi_sends.is_empty() || records_midi);
        if !routes_midi && let (Some(out), Some(clock)) = (track.midi_out, input.midi_out) {
            self.send_midi(track, out, clock, start, n as i64, input, live);
        }
        let hears_input = !track.monitor_auto || !input.play_items || input.recording;
        if let Some((cl, cr)) = track.monitor.filter(|_| track.content && hears_input)
            && let Some(left) = input.channels.get(cl).copied().flatten()
        {
            let right = cr.and_then(|c| input.channels.get(c).copied().flatten()).unwrap_or(left);
            let m = n.min(left.len()).min(right.len());
            match &track.input_fx {
                Some(chain) => {
                    let sr = self.sample_rate as f64;
                    let tempo = |f: i64| self.timeline.bpm_at(f as f64 / sr);
                    let chain_input = ChainInput {
                        sample_rate: self.sample_rate,
                        playing: input.play_items,
                        notes: None,
                        live,
                        live_channel: track.midi_input.and_then(|i| i.channel),
                        tempo: &tempo,
                        timeline: &self.timeline,
                    };
                    chain.run(m, start, &chain_input, |io, bufs| match io {
                        ChainIo::Fill(_) => {
                            bufs[0][..m].copy_from_slice(&left[..m]);
                            bufs[1][..m].copy_from_slice(&right[..m]);
                        }
                        ChainIo::Drain => {
                            tl[..m].iter_mut().zip(&bufs[0][..m]).for_each(|(o, s)| *o += s);
                            tr[..m].iter_mut().zip(&bufs[1][..m]).for_each(|(o, s)| *o += s);
                        }
                    });
                }
                None => {
                    for i in 0..m {
                        tl[i] += left[i];
                        tr[i] += right[i];
                    }
                }
            }
        }
        // Plugin latency downstream is compensated by reading this track's material early.
        let read = start + track.read_ahead;
        let items_play = input.play_items && track.content && !(input.recording && track.mute_items_recording);
        let items: &[RenderItem] = if items_play { &track.items } else { &[] };
        let mut aux = track.aux.as_ref().and_then(|a| a.0.try_lock().ok());
        for item in items {
            if let Some(take_fx) = &item.take_fx {
                self.play_take_fx(item, take_fx, read, n, input, (tl, tr), aux.as_deref_mut());
                continue;
            }
            match &item.audio {
                ItemAudio::Media(audio) => {
                    mix_item(item, audio, read, self.sample_rate as f64, tl, tr);
                    // Source channels beyond the first pair play into the track's further channels.
                    if audio.channels.len() > 2
                        && item.channel_mode == ChannelMode::Normal
                        && let Some(aux) = aux.as_deref_mut()
                    {
                        for (c, out) in aux.iter_mut().enumerate().take(audio.channels.len() - 2) {
                            mix_item_channel(item, audio, c + 2, read, self.sample_rate as f64, &mut out[..n]);
                        }
                    }
                }
                ItemAudio::Midi(notes) => notes.render(read, self.sample_rate, (item.gain_l, item.gain_r), tl, tr),
                ItemAudio::Timecode(tc) if !tc.mtc => {
                    let from = read.max(item.start);
                    let to = (read + n as i64).min(item.start + item.length);
                    if let Ok(mut w) = tc.writer.try_lock() {
                        let sr = self.sample_rate as f64;
                        for f in from..to {
                            let t = tc.start + (item.source_offset + (f - item.start) as f64 * item.playrate) / sr;
                            let v = w.sample(t);
                            tl[(f - read) as usize] += v * item.gain_l;
                            tr[(f - read) as usize] += v * item.gain_r;
                        }
                    }
                }
                ItemAudio::Timecode(_) => {}
                ItemAudio::Click => {
                    let from = read.max(item.start);
                    let to = (read + n as i64).min(item.start + item.length);
                    self.mix_clicks(read, from..to, item.gain_l, item.gain_r, tl, tr);
                }
            }
        }
        if track.click > 0.0 && track.content {
            self.each_click(input.clicks, read, n, |o, s| {
                tl[o] += s * track.click;
                tr[o] += s * track.click;
            });
        }
        let sr_f = self.sample_rate as f64;
        if let Some(g) = &track.pre_fx_gain {
            for i in 0..n {
                let gain = g.at((read + i as i64) as f64 / sr_f);
                tl[i] *= gain;
                tr[i] *= gain;
                for b in aux.iter_mut().flat_map(|a| a.iter_mut()) {
                    b[i] *= gain;
                }
            }
        }
        let recording_tap = track.record_tap.as_deref().filter(|_| input.play_items && input.recording);
        if let Some(tap) = recording_tap.filter(|t| t.point == RecordTap::PreFx) {
            tap.write(read, tl, tr, aux.as_deref().map(|a| &a[..]));
        }
        // A muted track's sends fall silent whatever they tap.
        if track.audible {
            track.taps.fill(track.taps.pre_fx.as_ref(), tl, tr, aux.as_deref().map(|a| &a[..]), read, sr_f);
        } else {
            track.taps.clear(n);
        }
        let mut position = read;
        let mut midi_bufs = track.chain_midi.0.try_lock().ok();
        // Set once an effect has passed MIDI on; until then effects read the track's own.
        let mut chain_has_midi = false;
        if routes_midi && let Some([held, _, entering]) = midi_bufs.as_deref_mut() {
            // A track that sends or receives MIDI gathers its items, live input and what it
            // receives into one block the chain starts from.
            let ctx = ProcessContext {
                sample_rate: self.sample_rate,
                position: read,
                playing: input.play_items,
                aux: None,
                notes: if input.play_items && track.content { track.notes.as_deref() } else { None },
                live,
                live_channel,
                bpm: 0.0,
                midi: None,
                midi_from: 0,
            };
            entering.clear();
            ctx.block_midi(n, &mut entering.short);
            ctx.block_long(n, &mut entering.long);
            if let Some(inbox) = &track.midi_inbox
                && let Ok(mut received) = inbox.0.lock()
            {
                entering.short.append(&mut received.short);
                entering.long.append(&mut received.long);
                entering.bused.append(&mut received.bused);
                entering.sort();
            }
            held.copy_from(entering);
            chain_has_midi = true;
        }
        if track.audible && !track.chain.is_empty() {
            let mut pin_bufs = track.pin_bufs.as_ref().and_then(|p| p.0.try_lock().ok());
            let tempo = |f: i64| self.timeline.bpm_at(f as f64 / sr_f);
            let chain_input = ChainInput {
                sample_rate: self.sample_rate,
                playing: input.play_items,
                // While stopped the position stands still; feeding notes would retrigger them.
                notes: if input.play_items && track.content { track.notes.as_deref() } else { None },
                live,
                live_channel,
                tempo: &tempo,
                timeline: &self.timeline,
            };
            let aux: &mut [Vec<f32>] = match aux.as_deref_mut() {
                Some(a) => a,
                None => &mut [],
            };
            let mut ch = Channels { l: &mut *tl, r: &mut *tr, aux, n };
            let mut midi = MidiState { bufs: midi_bufs.as_deref_mut(), has: chain_has_midi };
            track.chain.process(
                &mut ch,
                &chain_input,
                position,
                &mut midi,
                pin_bufs.as_deref_mut().map(|b| &mut b[..]),
            );
        }
        position -= track.chain.latency;
        if routes_midi && let Some([held, _, entering]) = midi_bufs.as_deref() {
            self.route_midi(track, held, entering);
            if let (Some(out), Some(clock)) = (track.midi_out, input.midi_out) {
                self.send_block_midi(held, out, clock, start);
            }
            if records_midi
                && let Some(tap) = &track.midi_record
                && let Ok(mut events) = tap.try_lock()
            {
                events.extend(held.short.iter().map(|&(at, bytes)| (start + at as i64, bytes)));
            }
        }
        drop(midi_bufs);
        if track.audible {
            track.taps.fill(track.taps.post_fx.as_ref(), tl, tr, aux.as_deref().map(|a| &a[..]), read, sr_f);
        }
        // Compensated output recording places the audio where the chain's latency put it.
        let tap_frame = |tap: &crate::record::OutputTap| if tap.compensated { position } else { read };
        if let Some(tap) = recording_tap.filter(|t| t.point == RecordTap::PostFxPreFader) {
            tap.write(tap_frame(tap), tl, tr, aux.as_deref().map(|a| &a[..]));
        }
        if let Some(aux) = aux.as_deref_mut() {
            for b in aux.iter_mut() {
                for (i, s) in b[..n].iter_mut().enumerate() {
                    *s *= match (&track.automation, track.audible) {
                        (_, false) => 0.0,
                        (Some(auto), true) => auto.volume_at((position + i as i64) as f64 / sr_f),
                        (None, true) => track.volume,
                    };
                }
            }
        }
        drop(aux);
        let ramp = track.vca_ramp.as_deref().filter(|_| track.audible).and_then(|(ramp, first)| {
            let _ = first.compare_exchange(i64::MIN, position, Ordering::Relaxed, Ordering::Relaxed);
            let into = position - first.load(Ordering::Relaxed);
            (into < VcaRamp::FRAMES).then_some((ramp, into))
        });
        match (&track.automation, track.audible) {
            (_, false) => {
                tl.fill(0.0);
                tr.fill(0.0);
            }
            (None, true) if let Some((ramp, into)) = ramp => {
                for (i, (sl, sr)) in tl.iter_mut().zip(tr.iter_mut()).enumerate() {
                    let m = if into + (i as i64) < VcaRamp::FRAMES {
                        ramp.matrix_at(into + i as i64)
                    } else {
                        track.matrix
                    };
                    crate::mix::apply(&m, sl, sr);
                }
            }
            (None, true) if crate::mix::is_diagonal(&track.matrix) => {
                let [gl, _, _, gr] = track.matrix;
                for (sl, sr) in tl.iter_mut().zip(tr.iter_mut()) {
                    *sl *= gl;
                    *sr *= gr;
                }
            }
            (None, true) => {
                for (sl, sr) in tl.iter_mut().zip(tr.iter_mut()) {
                    crate::mix::apply(&track.matrix, sl, sr);
                }
            }
            (Some(auto), true) => {
                // The fader acts where the signal leaves the track's effects.
                let start = position;
                let sr_f = self.sample_rate as f64;
                let mut at = 0;
                let end = start + n as i64;
                let mut g0 = auto.gains_at(start, end, sr_f);
                while at < n {
                    // Steps stay on a fixed grid, so they never straddle a pan grid boundary.
                    let frame = start + at as i64;
                    let len = ((Automation::STEP - frame.rem_euclid(Automation::STEP)) as usize).min(n - at);
                    let t0 = frame as f64 / sr_f;
                    let g1 = auto.gains_at(frame + len as i64, end, sr_f);
                    if auto.exact_between(t0, t0 + len as f64 / sr_f) {
                        for i in 0..len {
                            let g = auto.gains_at(frame + i as i64, end, sr_f);
                            crate::mix::apply(&g, &mut tl[at + i], &mut tr[at + i]);
                        }
                        g0 = g1;
                        at += len;
                        continue;
                    }
                    for i in 0..len {
                        let g = crate::mix::lerp(&g0, &g1, i as f32 / len as f32);
                        crate::mix::apply(&g, &mut tl[at + i], &mut tr[at + i]);
                    }
                    g0 = g1;
                    at += len;
                }
            }
        }
        if let Some(tap) = recording_tap.filter(|t| t.point == RecordTap::PostFader) {
            let aux = track.aux.as_ref().and_then(|a| a.0.try_lock().ok());
            tap.write(tap_frame(tap), tl, tr, aux.as_deref().map(|a| &a[..]));
        }
        track.meter.record(tl, tr);
    }

    /// Plays an item through its take's effects into the track's channels.
    #[allow(clippy::too_many_arguments)]
    fn play_take_fx(
        &self,
        item: &RenderItem,
        take_fx: &BufferedChain,
        read: i64,
        n: usize,
        input: &LiveInput,
        (tl, tr): (&mut [f32], &mut [f32]),
        mut aux: Option<&mut Vec<Vec<f32>>>,
    ) {
        let span = crate::chain::take_fx_span(item.start, item.length, self.sample_rate);
        if read + n as i64 <= span.start || read >= span.end {
            return;
        }
        let sr = self.sample_rate as f64;
        let tempo = |f: i64| self.timeline.bpm_at(f as f64 / sr);
        let notes = match &item.audio {
            ItemAudio::Midi(list) if input.play_items => Some(list.as_ref()),
            _ => None,
        };
        let chain_input = ChainInput {
            sample_rate: self.sample_rate,
            playing: input.play_items,
            notes,
            live: &[],
            live_channel: None,
            tempo: &tempo,
            timeline: &self.timeline,
        };
        // The effects run on the frames of their span only, as in REAPER's 512-frame blocks.
        let lo = (span.start - read).clamp(0, n as i64) as usize;
        let hi = (span.end - read).clamp(0, n as i64) as usize;
        let m = hi - lo;
        // Nothing is heard before the item starts, whatever the effects make of the silence.
        let from = (item.start - read).clamp(lo as i64, hi as i64) as usize - lo;
        take_fx.run(m, read + lo as i64, &chain_input, |io, bufs| match io {
            ChainIo::Fill(at) => {
                let ItemAudio::Media(audio) = &item.audio else { return };
                let (l, rest) = bufs.split_at_mut(1);
                mix_item(item, audio, at, sr, &mut l[0][..m], &mut rest[0][..m]);
                let extra =
                    if item.channel_mode == ChannelMode::Normal { audio.channels.len().saturating_sub(2) } else { 0 };
                for (c, out) in rest.iter_mut().enumerate().skip(1).take(extra) {
                    mix_item_channel(item, audio, c + 1, at, sr, &mut out[..m]);
                }
            }
            ChainIo::Drain => {
                for (c, b) in bufs.iter().enumerate() {
                    let out: &mut [f32] = match c {
                        0 => &mut *tl,
                        1 => &mut *tr,
                        c => match aux.as_deref_mut().and_then(|a| a.get_mut(c - 2)) {
                            Some(a) => &mut a[..n],
                            None => continue,
                        },
                    };
                    for (o, s) in out[lo + from..hi].iter_mut().zip(&b[from..m]) {
                        *o += s;
                    }
                }
            }
        });
    }

    /// Hands the MIDI leaving `track`'s chain (`post`), or entering it (`pre`), to the
    /// destinations of its MIDI sends.
    fn route_midi(&self, track: &RenderTrack, post: &crate::fx::MidiBlock, pre: &crate::fx::MidiBlock) {
        for &(dest, midi, pre_fx) in &track.midi_sends {
            let Some(inbox) = self.tracks.get(dest).and_then(|t| t.midi_inbox.as_ref()) else { continue };
            let Ok(mut inbox) = inbox.0.lock() else { continue };
            let src = if pre_fx { pre } else { post };
            for &(at, b) in &src.short {
                if let Some((status, bus)) = midi.route(b[0], 0) {
                    inbox.push(at, bus, [status, b[1], b[2]]);
                }
            }
            for &(at, from, b) in &src.bused {
                if let Some((status, bus)) = midi.route(b[0], from) {
                    inbox.push(at, bus, [status, b[1], b[2]]);
                }
            }
            if midi.route(0xf0, 0) == Some((0xf0, 0)) {
                inbox.long.extend(src.long.iter().cloned());
            }
        }
    }

    /// Queues a track's routed MIDI (its chain's output) for its hardware output.
    fn send_block_midi(
        &self,
        block: &crate::fx::MidiBlock,
        (port, channel): (u8, u8),
        clock: MidiOutClock,
        start: i64,
    ) {
        let Ok(mut queue) = clock.queue.try_lock() else { return };
        let sr = self.sample_rate as f64;
        for &(at, mut bytes) in &block.short {
            if channel > 0 {
                bytes[0] = bytes[0] & 0xf0 | (channel - 1);
            }
            let len = crate::midi_io::message_len(bytes[0]);
            let ahead = ((start + at as i64 - clock.frame) as f64 / sr).max(0.0);
            let due = clock.at + std::time::Duration::from_secs_f64(ahead);
            let _ = queue.push(OutMessage::new(due, port, &bytes[..len]));
        }
    }

    /// Adds a rendered track to its send destinations and its parent folder or the master.
    /// Queues the MIDI a track sends to its hardware output in this block: its items' messages
    /// (read as far ahead as its audio, so they sound with it) and live input it monitors.
    #[allow(clippy::too_many_arguments)]
    fn send_midi(
        &self,
        track: &RenderTrack,
        (port, channel): (u8, u8),
        clock: MidiOutClock,
        start: i64,
        n: i64,
        input: &LiveInput,
        live: &[LiveMidi],
    ) {
        let Ok(mut queue) = clock.queue.try_lock() else { return };
        let sr = self.sample_rate as f64;
        let mut push_long = |frame: i64, msg: &[u8]| {
            let ahead = ((frame - clock.frame) as f64 / sr).max(0.0);
            let due = clock.at + std::time::Duration::from_secs_f64(ahead);
            let _ = queue.push(OutMessage::new(due, port, msg));
        };
        if input.play_items {
            self.send_mtc(track, start, n, &mut push_long);
        }
        let mut push = |frame: i64, mut bytes: [u8; 3], len: u8| {
            if channel > 0 && bytes[0] < 0xf0 {
                bytes[0] = bytes[0] & 0xf0 | (channel - 1);
            }
            push_long(frame, &bytes[..len as usize]);
        };
        let read = start + track.read_ahead;
        if input.play_items
            && let Some(notes) = &track.notes
        {
            for note in notes.starting_in(read - notes.max_span()..read + n) {
                if (read..read + n).contains(&note.start) {
                    push(note.start - track.read_ahead, [0x90 | note.channel, note.key, note.velocity], 3);
                }
                let off = note.end.min(note.cutoff);
                if (read..read + n).contains(&off) {
                    push(off - track.read_ahead, [0x80 | note.channel, note.key, 0], 3);
                }
            }
            for c in notes.controls_in(read..read + n) {
                push(c.frame - track.read_ahead, c.bytes, c.len);
            }
        }
        if let Some(listen) = track.midi_input {
            for m in live.iter().filter(|m| listen.channel.is_none_or(|c| m.channel() == c)) {
                push(start, m.bytes, m.len);
            }
        }
    }

    /// MTC from the track's timecode generator items in this block: a full frame where
    /// playback enters an item or jumps, then quarter frames on a grid from there.
    fn send_mtc(&self, track: &RenderTrack, start: i64, n: i64, push: &mut dyn FnMut(i64, &[u8])) {
        let sr = self.sample_rate as f64;
        let read = start + track.read_ahead;
        for item in &track.items {
            let ItemAudio::Timecode(tc) = &item.audio else { continue };
            if !tc.mtc {
                continue;
            }
            let (from, to) = (read.max(item.start), (read + n).min(item.start + item.length));
            let Ok(mut state) = tc.mtc_state.try_lock() else { continue };
            if from >= to {
                *state = None;
                continue;
            }
            let tc_at = |f: i64| tc.start + (item.source_offset + (f - item.start) as f64 * item.playrate) / sr;
            let step = sr / (4.0 * tc.rate.fps);
            let (anchor, frame0) = match *state {
                Some((end, anchor, frame0)) if end == from => (anchor, frame0),
                _ => {
                    let frame0 = (tc_at(from) * tc.rate.fps + 1e-9).floor() as i64;
                    push(from - track.read_ahead, &crate::midi_sync::mtc_full_frame(frame0, tc.rate));
                    (from, frame0)
                }
            };
            let mut k = (((from - anchor) as f64 / step) - 1e-9).ceil().max(0.0) as i64;
            loop {
                let at = anchor + (k as f64 * step).round() as i64;
                if at >= to {
                    break;
                }
                let qf = crate::midi_sync::mtc_quarter_frame((k % 8) as u8, frame0 + 2 * (k / 8), tc.rate);
                push(at - track.read_ahead, &qf);
                k += 1;
            }
            *state = Some((to, anchor, frame0));
        }
    }

    /// Mixes a track's output into its send destinations and its parent or the master.
    /// `start` is the block's project frame at the master.
    fn route_track(
        &self,
        ti: usize,
        start: i64,
        n: usize,
        scratch: &mut RenderScratch,
        out_l: &mut [f32],
        out_r: &mut [f32],
    ) {
        let track = &self.tracks[ti];
        let (l, r) = std::mem::take(&mut scratch.track_bufs[ti]);
        let own_aux = track.aux.as_ref().and_then(|a| a.0.try_lock().ok());
        let pre_fx = track.taps.pre_fx.as_ref().and_then(|a| a.0.try_lock().ok());
        let post_fx = track.taps.post_fx.as_ref().and_then(|a| a.0.try_lock().ok());
        let source = |tap: SendMode, c: usize| -> Option<&[f32]> {
            match (tap, c) {
                (SendMode::PreFx, c) => pre_fx.as_ref()?.get(c).map(|b| &b[..n]),
                (SendMode::PostFx | SendMode::PostFxLegacy, c) => post_fx.as_ref()?.get(c).map(|b| &b[..n]),
                (SendMode::PostFader, 0) => Some(&l[..n]),
                (SendMode::PostFader, 1) => Some(&r[..n]),
                (SendMode::PostFader, c) => own_aux.as_ref()?.get(c - 2).map(|b| &b[..n]),
            }
        };
        fn mix(
            target: &mut [f32],
            input: &[f32],
            delay: Option<(&mut Vec<Vec<f32>>, usize, usize)>,
            gain: &dyn Fn(usize) -> f32,
        ) {
            match delay {
                Some((lines, k, at)) => {
                    let Some(line) = lines.get_mut(k) else { return };
                    let len = line.len();
                    for i in 0..target.len() {
                        let p = (at + i) % len;
                        target[i] += line[p] * gain(i);
                        line[p] = input[i];
                    }
                }
                None => {
                    for i in 0..target.len() {
                        target[i] += input[i] * gain(i);
                    }
                }
            }
        }
        // Adds source channel `src` (tapped where `tap` says) to channel `dst` of `dest`,
        // through channel `k` of `delay` when there is one.
        let add = |scratch: &mut RenderScratch,
                   out: (&mut [f32], &mut [f32]),
                   dest: Dest,
                   (tap, src, dst): (SendMode, usize, usize),
                   delay: Option<(&mut Vec<Vec<f32>>, usize, usize)>,
                   gain: &dyn Fn(usize) -> f32| {
            let Some(input) = source(tap, src) else { return };
            let dest = match dest {
                Dest::Device => {
                    let Some(bus) = self.hw_bus.as_ref() else { return };
                    let Ok(mut guard) = bus.0.try_lock() else { return };
                    if let Some(b) = guard.get_mut(dst) {
                        mix(&mut b[..n], input, delay, gain);
                    }
                    return;
                }
                Dest::Track(d) => Some(d),
                Dest::Parent => track.parent,
            };
            match (dest, dst) {
                (None, 0) => mix(&mut out.0[..n], input, delay, gain),
                (None, 1) => mix(&mut out.1[..n], input, delay, gain),
                (None, c) => {
                    let Some(aux) = self.master_aux.as_ref() else { return };
                    let Ok(mut guard) = aux.0.try_lock() else { return };
                    if let Some(b) = guard.get_mut(c - 2) {
                        mix(&mut b[..n], input, delay, gain);
                    }
                }
                (Some(d), 0) => mix(&mut scratch.track_bufs[d].0[..n], input, delay, gain),
                (Some(d), 1) => mix(&mut scratch.track_bufs[d].1[..n], input, delay, gain),
                (Some(d), c) => {
                    let Some(aux) = self.tracks[d].aux.as_ref() else { return };
                    let Ok(mut guard) = aux.0.try_lock() else { return };
                    if let Some(b) = guard.get_mut(c - 2) {
                        mix(&mut b[..n], input, delay, gain);
                    }
                }
            }
        };
        // The track's output is this far ahead of the master: what its effects did not use up.
        let out_frame = start + track.read_ahead - track.chain.latency;
        let sr = self.sample_rate as f64;
        let sends = track.sends.iter().enumerate().map(|(si, s)| (s, Dest::Track(s.dest), si, &track.send_delays));
        // Hardware outputs follow the track's own output: off while it is only heard through a
        // send into a soloed track.
        let hw = track.hw_outs.iter().enumerate().filter(|_| track.hw_out);
        for (send, dest, si, delays) in sends.chain(hw.map(|(i, o)| (o, Dest::Device, i, &track.hw_delays))) {
            let auto = match dest {
                Dest::Track(_) => track.send_automation.get(si).and_then(Option::as_ref),
                _ => None,
            };
            let mut delay = delays.get(si).and_then(Option::as_ref).and_then(|d| d.state.try_lock().ok());
            for (k, (src, dst, right, scale)) in send.routes().enumerate() {
                let gain = |i: usize| {
                    scale
                        * match auto {
                            Some(a) => {
                                let g = a.gains_at((out_frame + i as i64) as f64 / sr);
                                send.sign * if right { g.1 } else { g.0 }
                            }
                            None if right => send.gain_r,
                            None => send.gain_l,
                        }
                };
                let line = delay.as_deref_mut().map(|(lines, at)| (lines, k, *at));
                add(scratch, (&mut *out_l, &mut *out_r), dest, (send.tap, src, dst), line, &gain);
            }
            if let Some((lines, at)) = delay.as_deref_mut() {
                *at = (*at + n) % lines[0].len();
            }
        }
        if track.main_send && track.main_out {
            let mut delay = track.main_delay.as_ref().and_then(|d| d.state.try_lock().ok());
            for c in 0..track.channels {
                let line = delay.as_deref_mut().map(|(lines, at)| (lines, c, *at));
                let route = (SendMode::PostFader, c, c + track.main_offset);
                add(scratch, (&mut *out_l, &mut *out_r), Dest::Parent, route, line, &|_| 1.0);
            }
            if let Some((lines, at)) = delay.as_deref_mut() {
                *at = (*at + n) % lines[0].len();
            }
        }
        drop((own_aux, pre_fx, post_fx));
        scratch.track_bufs[ti] = (l, r);
    }

    /// Adds the clicks sounding in project frames `range` to buffers that start at `block_start`.
    fn mix_clicks(
        &self,
        block_start: i64,
        range: std::ops::Range<i64>,
        gain_l: f32,
        gain_r: f32,
        out_l: &mut [f32],
        out_r: &mut [f32],
    ) {
        self.beat_clicks(block_start, range, |o, s| {
            out_l[o] += s * gain_l;
            out_r[o] += s * gain_r;
        });
    }

    /// Calls `f(offset, sample)` for the metronome samples `clicks` plays in the `n` frames of a
    /// block starting at project frame `start`.
    fn each_click(&self, clicks: Clicks, start: i64, n: usize, mut f: impl FnMut(usize, f32)) {
        match clicks {
            Clicks::Off => {}
            Clicks::Beats => self.beat_clicks(start, start..start + n as i64, f),
            Clicks::CountIn { clicks, at } => self.count_in_clicks(&clicks, at, n, f),
            Clicks::CountInTail { clicks, at, beats } => {
                if beats {
                    self.beat_clicks(start, start..start + n as i64, &mut f);
                }
                self.count_in_clicks(&clicks, at, n, f);
            }
        }
    }

    /// The count-in clicks sounding in its frames `at..at + n`: those starting while it is
    /// audible, each played to its end.
    fn count_in_clicks(&self, clicks: &CountInClicks, at: i64, n: usize, mut f: impl FnMut(usize, f32)) {
        let end = at + n as i64;
        let last = clicks.audible.min(clicks.frames);
        for k in 0..clicks.beats() {
            let pos = clicks.click_at(k);
            if pos >= end || pos >= last {
                break;
            }
            let sound = if clicks.accented(k) { &self.clicks.accent } else { &self.clicks.beat };
            for frame in pos.max(at)..(pos + sound.len() as i64).min(end) {
                f((frame - at) as usize, sound[(frame - pos) as usize]);
            }
        }
    }

    /// How long the longest click sound lasts, in frames.
    pub fn click_len(&self) -> i64 {
        self.clicks.accent.len().max(self.clicks.beat.len()) as i64
    }

    /// Calls `f(offset, sample)` for the beat clicks sounding in project frames `range` of a
    /// block that starts at `block_start`.
    fn beat_clicks(&self, block_start: i64, range: std::ops::Range<i64>, mut f: impl FnMut(usize, f32)) {
        if range.is_empty() {
            return;
        }
        let sr = self.sample_rate as f64;
        let tail = self.clicks.accent.len().max(self.clicks.beat.len()) as i64;
        let t0 = (range.start - tail) as f64 / sr;
        let t1 = range.end as f64 / sr;
        self.timeline.for_each_beat(t0.max(0.0), t1, |time, downbeat| {
            let sound = if downbeat { &self.clicks.accent } else { &self.clicks.beat };
            let at = (time * sr).round() as i64;
            let from = range.start.max(at);
            let to = range.end.min(at + sound.len() as i64);
            for frame in from..to {
                f((frame - block_start) as usize, sound[(frame - at) as usize]);
            }
        });
    }
}

/// The track buffers of a level, handed to the track threads.
struct TrackBufs(*mut (Vec<f32>, Vec<f32>));

// SAFETY: the threads reach disjoint buffers through it (see `render_block`).
unsafe impl Sync for TrackBufs {}

impl TrackBufs {
    /// # Safety
    /// No other thread may use buffer `i` while the result lives, and `i` must be in bounds.
    #[allow(clippy::mut_from_ref)]
    unsafe fn get(&self, i: usize) -> &mut (Vec<f32>, Vec<f32>) {
        unsafe { &mut *self.0.add(i) }
    }
}

/// Starts the track render threads, so the first block played does not wait for them (and
/// they are promoted to real-time priority when the audio thread is).
pub fn start_track_threads() {
    track_threads();
}

/// How many threads render tracks: `SCYTHE_TRACK_THREADS`, else one per fast physical core.
/// More threads than that make blocks slower, not faster: every block wakes them all, and
/// a heavy track landing on a hyperthread sibling or an efficiency core finishes late (on a
/// 6+8-core laptop CPU, 128-frame blocks of a 48-track session took 0.72 ms with 6 threads
/// and 1.25 ms with one per logical CPU).
fn track_thread_count() -> usize {
    if let Some(n) = std::env::var("SCYTHE_TRACK_THREADS").ok().and_then(|v| v.parse().ok()) {
        return n;
    }
    let logical = std::thread::available_parallelism().map_or(4, |n| n.get());
    fast_cores().unwrap_or(logical / 2).clamp(2, 8)
}

/// Physical cores among the CPUs that clock within 15 % of the fastest, from sysfs.
fn fast_cores() -> Option<usize> {
    let read = |path: String| std::fs::read_to_string(path).ok()?.trim().parse::<u64>().ok();
    let cpus: Vec<(u64, u64, u64)> = (0..1024)
        .map_while(|c| {
            let base = format!("/sys/devices/system/cpu/cpu{c}");
            std::path::Path::new(&base).exists().then(|| {
                let freq = read(format!("{base}/cpufreq/cpuinfo_max_freq")).unwrap_or(0);
                let core = read(format!("{base}/topology/core_id")).unwrap_or(c);
                let package = read(format!("{base}/topology/physical_package_id")).unwrap_or(0);
                (freq, package, core)
            })
        })
        .collect();
    let top = cpus.iter().map(|c| c.0).max().filter(|&f| f > 0)?;
    let mut cores: Vec<(u64, u64)> = cpus.iter().filter(|c| c.0 * 100 >= top * 85).map(|c| (c.1, c.2)).collect();
    cores.sort_unstable();
    cores.dedup();
    Some(cores.len())
}

/// The threads tracks render on. Not rayon's global pool: whatever else queues work there,
/// such as converting media while a set loads, would hold up the audio callback behind it.
fn track_threads() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .thread_name(|i| format!("scythe-tracks-{i}"))
            .num_threads(track_thread_count())
            .start_handler(|_| crate::realtime::request_helper())
            .build()
            .expect("spawn track render threads")
    })
}

/// Plugin delay compensation. Working back from the master, each track's `read_ahead` is its
/// own effects' latency plus the slowest path from its output to the master; faster paths get
/// a delay line so every path into a destination arrives together.
fn compensate_latency(tracks: &mut [RenderTrack], order: &[usize], master_latency: i64) {
    let mut read_ahead = vec![0i64; tracks.len()];
    for &ti in order.iter().rev() {
        let t = &tracks[ti];
        let main = t.main_send.then(|| t.parent.map_or(master_latency, |p| read_ahead[p]));
        let sends: Vec<(i64, usize)> = t.sends.iter().map(|s| (read_ahead[s.dest], s.routes().count())).collect();
        // A hardware output leaves at once, so it waits for the slowest path to the master.
        let hw: Vec<(i64, usize)> = t.hw_outs.iter().map(|s| (0, s.routes().count())).collect();
        let out = sends.iter().chain(&hw).map(|s| s.0).chain(main).max().unwrap_or(master_latency);
        let delay = |dest: i64, channels: usize| {
            (out > dest).then(|| Arc::new(DelayLine::new((out - dest) as usize, channels)))
        };
        let channels = t.channels;
        let t = &mut tracks[ti];
        t.send_delays = sends.iter().map(|&(d, c)| delay(d, c)).collect();
        t.hw_delays = hw.iter().map(|&(d, c)| delay(d, c)).collect();
        t.main_delay = main.and_then(|d| delay(d, channels));
        t.read_ahead = out + t.chain.latency;
        read_ahead[ti] = t.read_ahead;
    }
}

/// Topological order over send and folder edges, so a destination is mixed after all of its
/// sources.
/// Tracks caught in a send cycle are appended in project order; the send that closes the cycle
/// then lands in an already-mixed buffer and is effectively ignored.
/// Level 0 for tracks nothing feeds; otherwise one more than the highest level feeding it.
fn dependency_levels(tracks: &[RenderTrack], order: &[usize]) -> Vec<usize> {
    let mut level = vec![0usize; tracks.len()];
    for &i in order {
        let t = &tracks[i];
        for dest in t.sends.iter().map(|s| s.dest).chain(t.midi_sends.iter().map(|s| s.0)).chain(t.parent) {
            level[dest] = level[dest].max(level[i] + 1);
        }
    }
    level
}

fn send_order(tracks: &[RenderTrack]) -> Vec<usize> {
    let n = tracks.len();
    let edges = |t: &RenderTrack| {
        t.sends.iter().map(|s| s.dest).chain(t.midi_sends.iter().map(|s| s.0)).chain(t.parent).collect::<Vec<_>>()
    };
    let mut indegree = vec![0usize; n];
    for t in tracks {
        for d in edges(t) {
            indegree[d] += 1;
        }
    }
    let mut ready: Vec<usize> = (0..n).filter(|&i| indegree[i] == 0).rev().collect();
    let mut order = Vec::with_capacity(n);
    while let Some(i) = ready.pop() {
        order.push(i);
        for d in edges(&tracks[i]) {
            indegree[d] -= 1;
            if indegree[d] == 0 {
                ready.push(d);
            }
        }
    }
    for i in 0..n {
        if !order.contains(&i) {
            order.push(i);
        }
    }
    order
}

/// Resolves a MIDI item's notes to project frames, following the tempo map and repeating the
/// source when the item loops.
fn midi_notes(
    chunk: &scythe_core::rpp::Chunk,
    item: &scythe_core::Item,
    take: &scythe_core::Take,
    timeline: &Timeline,
    sample_rate: u32,
) -> Option<NoteList> {
    let source = scythe_core::midi::MidiSource::parse(chunk)?;
    let sr = sample_rate as f64;
    let cutoff = (item.end() * sr).round() as i64;
    let notes = source
        .place(item, take, timeline)
        .into_iter()
        .map(|n| SynthNote {
            start: (n.start * sr).round() as i64,
            // A note still held where the item ends is let go on its last frame (REAPER 7.73).
            end: ((n.end * sr).round() as i64).min(cutoff - 1).max((n.start * sr).round() as i64),
            cutoff,
            channel: n.channel,
            key: n.key,
            velocity: n.velocity,
        })
        .collect();
    let controls = source
        .place_events(item, take, timeline)
        .into_iter()
        .map(|e| ControlEvent { frame: (e.time * sr).round() as i64, bytes: e.bytes, len: e.len })
        .collect();
    let long = source.place_long(item, take, timeline).into_iter().map(|(t, d)| ((t * sr).round() as i64, d)).collect();
    Some(NoteList::new(notes, sample_rate).with_controls(controls).with_long(long))
}

fn mix_item(
    item: &RenderItem,
    audio: &AudioData,
    block_start: i64,
    sample_rate: f64,
    out_l: &mut [f32],
    out_r: &mut [f32],
) {
    let n = out_l.len() as i64;
    let from = block_start.max(item.start);
    let to = (block_start + n).min(item.start + item.length);
    if from >= to {
        return;
    }
    let src_frames = audio.frames() as f64;
    if src_frames == 0.0 {
        return;
    }
    let sec_start = item.section_start.clamp(0.0, src_frames);
    let available = src_frames - sec_start;
    let sec_len = if item.section_len > 0.0 { item.section_len.min(available) } else { available };
    if sec_len <= 0.0 {
        return;
    }
    // A mono file counts as two equal channels.
    let channel = |c: usize| {
        let mono = audio.channels.len() == 1 && c == 1;
        audio.channels.get(if mono { 0 } else { c }).map(Vec::as_slice)
    };
    let (left, right, mix) = match item.channel_mode {
        ChannelMode::Normal => (channel(0), channel(1), false),
        ChannelMode::ReverseStereo => (channel(1), channel(0), false),
        ChannelMode::Downmix => (channel(0), channel(1), true),
        ChannelMode::Mono(c) => (channel(c as usize), channel(c as usize), false),
        ChannelMode::Stereo(c) => (channel(c as usize), channel(c as usize + 1), false),
    };
    if left.is_none() && right.is_none() {
        return;
    }
    let read = |ch: Option<&[f32]>, i: usize, next: usize, frac: f32| {
        ch.map_or(0.0, |c| if frac == 0.0 { c[i] } else { c[i] + (c[next] - c[i]) * frac })
    };
    for t in from..to {
        let rel = t - item.start;
        let mut pos = item.source_offset + rel as f64 * item.playrate;
        if item.looped {
            pos = pos.rem_euclid(sec_len);
        }
        if pos < 0.0 || pos >= sec_len {
            continue;
        }
        let pos = if item.reverse { sec_start + (sec_len - 1.0 - pos).max(0.0) } else { sec_start + pos };
        let i = pos as usize;
        let frac = (pos - i as f64) as f32;
        let next = (i + 1).min(audio.frames() - 1);
        let (mut sl, mut sr) = (read(left, i, next, frac), read(right, i, next, frac));
        if mix {
            let m = 0.5 * (sl + sr);
            (sl, sr) = (m, m);
        }
        let fade = fade_gain(rel, item.length, &item.fade_in, &item.fade_out);
        let (gl, gr) = match &item.automation {
            Some(auto) => auto.gains_at(rel as f64 / sample_rate),
            None => (item.gain_l, item.gain_r),
        };
        let o = (t - block_start) as usize;
        out_l[o] += sl * gl * fade;
        out_r[o] += sr * gr * fade;
    }
}

/// One further source channel (3 and up) of an item into its track channel, with the item's
/// volume and fades but no pan.
fn mix_item_channel(
    item: &RenderItem,
    audio: &AudioData,
    channel: usize,
    block_start: i64,
    sample_rate: f64,
    out: &mut [f32],
) {
    let Some(src) = audio.channels.get(channel) else { return };
    let n = out.len() as i64;
    let (from, to) = (block_start.max(item.start), (block_start + n).min(item.start + item.length));
    let src_frames = audio.frames() as f64;
    let sec_start = item.section_start.clamp(0.0, src_frames);
    let available = src_frames - sec_start;
    let sec_len = if item.section_len > 0.0 { item.section_len.min(available) } else { available };
    if from >= to || sec_len <= 0.0 {
        return;
    }
    for t in from..to {
        let rel = t - item.start;
        let mut pos = item.source_offset + rel as f64 * item.playrate;
        if item.looped {
            pos = pos.rem_euclid(sec_len);
        }
        if pos < 0.0 || pos >= sec_len {
            continue;
        }
        let pos = if item.reverse { sec_start + (sec_len - 1.0 - pos).max(0.0) } else { sec_start + pos };
        let i = pos as usize;
        let frac = (pos - i as f64) as f32;
        let next = (i + 1).min(audio.frames() - 1);
        let s = src[i] + (src[next] - src[i]) * frac;
        let g = match &item.automation {
            Some(auto) => auto.gains_at(rel as f64 / sample_rate).0,
            None => item.gain_l.max(item.gain_r),
        };
        out[(t - block_start) as usize] += s * g * fade_gain(rel, item.length, &item.fade_in, &item.fade_out);
    }
}

fn fade_gain(rel: i64, length: i64, fade_in: &(i64, Fade), fade_out: &(i64, Fade)) -> f32 {
    let mut g = 1.0;
    let (len, shape) = fade_in;
    if *len > 0 && rel < *len {
        g *= shape.gain(rel as f64 / *len as f64);
    }
    let to_end = length - rel;
    let (len, shape) = fade_out;
    if *len > 0 && to_end < *len {
        g *= shape.out_gain(to_end as f64 / *len as f64);
    }
    g as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use scythe_core::{Item, Send, Take};

    fn graph_with(project: &Project, audio: AudioData) -> RenderGraph {
        let audio = Arc::new(audio);
        let mut meters = MeterRegistry::default();
        let pool = MediaPool::default();
        let mut g = RenderGraph::build(project, &pool, &mut meters, &mut FxRegistry::new(100), 100);
        // The pool is empty, so inject media directly; the pool path is covered by engine tests.
        for (gt, pt) in g.tracks.iter_mut().zip(&project.tracks) {
            gt.items = pt
                .items
                .iter()
                .map(|it| RenderItem {
                    start: (it.position * 100.0) as i64,
                    length: (it.length * 100.0) as i64,
                    source_offset: it.takes[0].source_offset * 100.0,
                    playrate: 1.0,
                    gain_l: 1.0,
                    gain_r: 1.0,
                    fade_in: (0, Fade::default()),
                    fade_out: (0, Fade::default()),
                    looped: it.looped,
                    section_start: 0.0,
                    section_len: 0.0,
                    reverse: false,
                    channel_mode: ChannelMode::Normal,
                    audio: ItemAudio::Media(audio.clone()),
                    automation: None,
                    take_fx: None,
                })
                .collect();
        }
        g
    }

    fn ones(frames: usize) -> AudioData {
        AudioData { sample_rate: 100, channels: vec![vec![1.0; frames]] }
    }

    fn project_with_item(pos: f64, len: f64) -> Project {
        let mut p = Project::default();
        let t = p.add_track("t");
        p.track_mut(t).unwrap().items.push(Item::new(pos, len, Take::from_file("a.wav".into())));
        p
    }

    fn render(g: &RenderGraph, start: i64, n: usize) -> (Vec<f32>, Vec<f32>) {
        let mut l = vec![9.0; n];
        let mut r = vec![9.0; n];
        let mut s = RenderScratch::new(8);
        g.render(start, &mut l, &mut r, &mut s);
        (l, r)
    }

    #[test]
    fn item_plays_only_inside_its_range() {
        let p = project_with_item(0.1, 0.2);
        let g = graph_with(&p, ones(1000));
        let (l, _) = render(&g, 0, 40);
        let expect = 1.0;
        assert_eq!(l[9], 0.0);
        assert!((l[10] - expect).abs() < 1e-6);
        assert!((l[29] - expect).abs() < 1e-6);
        assert_eq!(l[30], 0.0);
    }

    #[test]
    fn mute_and_solo_silence_tracks() {
        let mut p = project_with_item(0.0, 1.0);
        let other = p.add_track("other");
        p.track_mut(other).unwrap().solo = true;
        let g = graph_with(&p, ones(100));
        let (l, _) = render(&g, 0, 10);
        assert!(l.iter().all(|&s| s == 0.0));
        p.track_mut(other).unwrap().solo = false;
        p.tracks[0].muted = true;
        let (l, _) = render(&graph_with(&p, ones(100)), 0, 10);
        assert!(l.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn hard_pan_moves_signal_to_one_side() {
        let mut p = project_with_item(0.0, 1.0);
        p.tracks[0].pan = 1.0;
        let g = graph_with(&p, ones(100));
        let (l, r) = render(&g, 0, 10);
        assert!(l[0].abs() < 1e-6);
        assert!((r[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn source_offset_and_loop() {
        let audio = AudioData { sample_rate: 100, channels: vec![(0..10).map(|i| i as f32).collect()] };
        let mut p = project_with_item(0.0, 0.25);
        p.tracks[0].items[0].takes[0].source_offset = 0.05;
        p.tracks[0].items[0].looped = true;
        p.tracks[0].pan = -1.0;
        let g = graph_with(&p, audio);
        let (l, _) = render(&g, 0, 8);
        assert_eq!(&l[..7], &[5.0, 6.0, 7.0, 8.0, 9.0, 0.0, 1.0]);
    }

    #[test]
    fn sends_feed_destination_track() {
        let mut p = project_with_item(0.0, 1.0);
        let bus = p.add_track("bus");
        let src = p.tracks[0].id;
        p.tracks[0].volume = 0.0;
        p.track_mut(src).unwrap().sends.push(Send { volume: 0.5, ..Send::new(bus) });
        let g = graph_with(&p, ones(100));
        assert_eq!(g.order, [0, 1]);
        let (l, _) = render(&g, 0, 4);
        // Sends are post-fader: the source fader at zero silences the send too.
        assert_eq!(l[0], 0.0);
    }

    #[test]
    fn send_order_respects_edges() {
        let mut p = Project::default();
        let a = p.add_track("a");
        let b = p.add_track("b");
        p.track_mut(b).unwrap().sends.push(Send { volume: 1.0, ..Send::new(a) });
        let g = graph_with(&p, ones(10));
        assert_eq!(g.order, [1, 0]);
    }

    #[test]
    fn folder_children_mix_through_parent() {
        let mut p = project_with_item(0.0, 1.0);
        p.tracks[0].pan = -1.0;
        let bus = p.add_track("bus");
        p.tracks.swap(0, 1);
        p.tracks[0].folder_depth = 1;
        p.tracks[1].folder_depth = -1;
        p.track_mut(bus).unwrap().volume = 0.5;
        let g = graph_with(&p, ones(100));
        assert_eq!(g.tracks[1].parent, Some(0));
        assert_eq!(g.order, [1, 0]);
        let (l, _) = render(&g, 0, 4);
        let expect = 0.5;
        assert!((l[0] - expect).abs() < 1e-6, "{}", l[0]);

        p.tracks[1].main_send = false;
        let (l, _) = render(&graph_with(&p, ones(100)), 0, 4);
        assert_eq!(l[0], 0.0);
    }

    #[test]
    fn fades_ramp() {
        let lin = (10, Fade { shape: 0, ..Fade::new(0.1) });
        assert_eq!(fade_gain(0, 100, &lin, &lin), 0.0);
        assert_eq!(fade_gain(5, 100, &lin, &lin), 0.5);
        assert_eq!(fade_gain(50, 100, &lin, &lin), 1.0);
        assert_eq!(fade_gain(95, 100, &lin, &lin), 0.5);
        let fast = (10, Fade { shape: 1, ..Fade::new(0.1) });
        assert!(fade_gain(5, 100, &fast, &fast) > 0.5);
    }

    #[test]
    fn metronome_clicks_on_beats() {
        let p = Project { tempo: scythe_core::TempoMap::constant(60.0, 4, 4), ..Project::default() };
        let mut g = RenderGraph::build(
            &p,
            &MediaPool::default(),
            &mut MeterRegistry::default(),
            &mut FxRegistry::new(1000),
            1000,
        );
        g.metronome.gain = 1.0;
        let clicks = |g: &RenderGraph, start: i64, n: usize| {
            let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
            let input = LiveInput { clicks: Clicks::Beats, ..LiveInput::default() };
            g.render_with(start, &mut l, &mut r, &mut RenderScratch::new(4), &input);
            (l, r)
        };
        assert!(render(&g, 990, 30).0.iter().all(|&s| s == 0.0), "offline renders have no metronome");
        let (l, _) = clicks(&g, 990, 30);
        assert!(l[..10].iter().all(|&s| s == 0.0));
        assert!(l[10..].iter().any(|&s| s != 0.0));
        let (quiet, _) = clicks(&g, 500, 400);
        assert!(quiet.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn monitored_tracks_hear_the_input() {
        let mut p = project_with_item(0.0, 1.0);
        let live_track = p.add_track("guitar");
        let t = p.track_mut(live_track).unwrap();
        t.armed = true;
        t.rec_input = 1;
        let g = graph_with(&p, ones(100));
        assert!(g.monitoring());
        let input_ch1 = [0.25f32; 10];
        let mut channels = [None; LiveInput::MAX_CHANNELS];
        channels[1] = Some(&input_ch1[..]);
        let (mut l, mut r) = (vec![0.0; 10], vec![0.0; 10]);
        let mut s = RenderScratch::new(8);
        g.render_with(0, &mut l, &mut r, &mut s, &LiveInput { channels, play_items: false, ..LiveInput::default() });
        // Stopped: only the input, on both sides; the item on the other track stays quiet.
        assert_eq!((l[3], r[3]), (0.25, 0.25));
        g.render_with(0, &mut l, &mut r, &mut s, &LiveInput { channels, ..LiveInput::default() });
        assert_eq!(l[3], 1.25);
        p.track_mut(live_track).unwrap().monitor = false;
        assert!(!graph_with(&p, ones(100)).monitoring());
    }

    #[test]
    fn tape_monitoring_and_items_while_recording() {
        let mut p = project_with_item(0.0, 1.0);
        let item_track = p.tracks[0].id;
        let live_track = p.add_track("guitar");
        let t = p.track_mut(live_track).unwrap();
        (t.armed, t.rec_input, t.monitor_auto) = (true, 1, true);
        let input_ch1 = [0.25f32; 10];
        let mut channels = [None; LiveInput::MAX_CHANNELS];
        channels[1] = Some(&input_ch1[..]);
        let (mut l, mut r) = (vec![0.0; 10], vec![0.0; 10]);
        let mut s = RenderScratch::new(8);
        let mut at = |g: &RenderGraph, play_items, recording| {
            g.render_with(
                0,
                &mut l,
                &mut r,
                &mut s,
                &LiveInput { channels, play_items, recording, ..LiveInput::default() },
            );
            l[3]
        };
        let g = graph_with(&p, ones(100));
        // Tape style: the input while stopped and while recording, not while playing back.
        assert_eq!(at(&g, false, false), 0.25);
        assert_eq!(at(&g, true, false), 1.0);
        assert_eq!(at(&g, true, true), 1.25);
        // An armed track recording mutes its own items unless it monitors them.
        p.track_mut(item_track).unwrap().armed = true;
        let g = graph_with(&p, ones(100));
        assert_eq!(at(&g, true, true), 0.25);
        assert_eq!(at(&g, true, false), 1.0);
        p.track_mut(item_track).unwrap().monitor_items = true;
        assert_eq!(at(&graph_with(&p, ones(100)), true, true), 1.25);
    }

    #[test]
    fn reversed_section_plays_backwards() {
        let audio = AudioData { sample_rate: 100, channels: vec![(0..10).map(|i| i as f32).collect()] };
        let mut p = project_with_item(0.0, 0.04);
        p.tracks[0].pan = -1.0;
        let mut g = graph_with(&p, audio);
        let item = &mut g.tracks[0].items[0];
        item.section_start = 2.0;
        item.section_len = 4.0;
        item.reverse = true;
        let (l, _) = render(&g, 0, 5);
        assert_eq!(&l[..5], &[5.0, 4.0, 3.0, 2.0, 0.0]);
    }

    #[test]
    fn multichannel_sends_and_folders() {
        let quad = || AudioData { sample_rate: 100, channels: (1..=4).map(|c| vec![c as f32 / 10.0; 100]).collect() };
        // A 4-channel item; channels 3/4 sent to a stereo track, nothing else reaches the master.
        let mut p = project_with_item(0.0, 1.0);
        let (a, b) = (p.tracks[0].id, p.add_track("b"));
        let t = p.track_mut(a).unwrap();
        (t.channels, t.main_send) = (4, false);
        t.sends.push(Send { src_channels: 2, ..Send::new(b) });
        let (l, r) = render(&graph_with(&p, quad()), 0, 10);
        assert!((l[5] - 0.3).abs() < 1e-6 && (r[5] - 0.4).abs() < 1e-6, "{} {}", l[5], r[5]);

        // Channels 3/4 of a child reach the folder's 3/4, which only the send takes on.
        let mut p = Project::default();
        let parent = p.add_track("folder");
        let child = p.add_track("child");
        let out = p.add_track("out");
        p.track_mut(child).unwrap().items.push(Item::new(0.0, 1.0, Take::from_file("q.wav".into())));
        let f = p.track_mut(parent).unwrap();
        (f.channels, f.folder_depth, f.main_send) = (4, 1, false);
        f.sends.push(Send { src_channels: 2, ..Send::new(out) });
        let c = p.track_mut(child).unwrap();
        (c.channels, c.folder_depth) = (4, -1);
        let (l, r) = render(&graph_with(&p, quad()), 0, 10);
        assert!((l[5] - 0.3).abs() < 1e-6 && (r[5] - 0.4).abs() < 1e-6, "{} {}", l[5], r[5]);

        // A stereo child sent into the folder's channels 3/4.
        p.track_mut(child).unwrap().channels = 2;
        p.track_mut(child).unwrap().main_send_offset = 2;
        let (l, r) = render(&graph_with(&p, quad()), 0, 10);
        assert!((l[5] - 0.1).abs() < 1e-6 && (r[5] - 0.2).abs() < 1e-6, "{} {}", l[5], r[5]);
    }

    fn stereo() -> AudioData {
        AudioData { sample_rate: 100, channels: vec![vec![0.4; 100], vec![0.2; 100]] }
    }

    /// A source at volume 0.5, pan 0.4 sending into a bus (the only thing the master hears).
    fn source_and_bus(mode: SendMode) -> (Project, TrackId, TrackId) {
        let mut p = project_with_item(0.0, 1.0);
        let src = p.tracks[0].id;
        let bus = p.add_track("bus");
        let t = p.track_mut(src).unwrap();
        (t.volume, t.pan, t.main_send) = (0.5, 0.4, false);
        t.sends.push(Send { volume: 0.7, pan: -0.3, mode, ..Send::new(bus) });
        (p, src, bus)
    }

    fn close(a: f32, b: f64) -> bool {
        (a as f64 - b).abs() < 1e-6
    }

    #[test]
    fn send_modes_tap_before_the_fader() {
        let law = Project::default().pan_law;
        let (sl, sr) = law.gains(-0.3);
        let (tl, tr) = law.gains(0.4);
        let (p, ..) = source_and_bus(SendMode::PostFader);
        let (l, r) = render(&graph_with(&p, stereo()), 0, 10);
        assert!(close(l[5], 0.4 * 0.5 * tl * 0.7 * sl) && close(r[5], 0.2 * 0.5 * tr * 0.7 * sr), "{l:?} {r:?}");
        // Pre-fader and pre-FX taps skip the source's fader, pan and volume envelope.
        for mode in [SendMode::PostFx, SendMode::PreFx, SendMode::PostFxLegacy] {
            let (mut p, src, _) = source_and_bus(mode);
            let env = p.track_mut(src).unwrap().envelope_mut(EnvelopeKind::Volume);
            env.points.push(scythe_core::EnvPoint::new(0.0, 0.25));
            let (l, r) = render(&graph_with(&p, stereo()), 0, 10);
            assert!(close(l[5], 0.4 * 0.7 * sl) && close(r[5], 0.2 * 0.7 * sr), "{mode:?} {l:?} {r:?}");
        }
        // The pre-FX volume envelope acts before every tap, as in REAPER.
        let (mut p, src, _) = source_and_bus(SendMode::PreFx);
        let env = p.track_mut(src).unwrap().envelope_mut(EnvelopeKind::VolumePreFx);
        env.points.push(scythe_core::EnvPoint::new(0.0, 0.5));
        let (l, _) = render(&graph_with(&p, stereo()), 0, 10);
        assert!(close(l[5], 0.5 * 0.4 * 0.7 * sl), "{l:?}");
    }

    #[test]
    fn muting_and_soloing_pre_fader_sends() {
        // Muting the source silences its pre-fader sends too.
        let (mut p, src, bus) = source_and_bus(SendMode::PostFx);
        p.track_mut(src).unwrap().muted = true;
        let (l, _) = render(&graph_with(&p, stereo()), 0, 10);
        assert!(l.iter().all(|&s| s == 0.0));
        // Soloing the bus keeps its source playing into it, but not into the master.
        p.track_mut(src).unwrap().muted = false;
        p.track_mut(src).unwrap().main_send = true;
        p.track_mut(bus).unwrap().solo = true;
        let (soloed, _) = render(&graph_with(&p, stereo()), 0, 10);
        p.track_mut(src).unwrap().main_send = false;
        p.track_mut(bus).unwrap().solo = false;
        let (plain, _) = render(&graph_with(&p, stereo()), 0, 10);
        assert!(soloed[5] != 0.0 && close(soloed[5], plain[5] as f64), "{soloed:?} {plain:?}");
        // Soloing something else silences both.
        let other = p.add_track("other");
        p.track_mut(other).unwrap().solo = true;
        let (l, _) = render(&graph_with(&p, stereo()), 0, 10);
        assert!(l.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn send_polarity_and_mono() {
        let law = Project::default().pan_law;
        let (sl, sr) = law.gains(-0.3);
        let (mut p, src, _) = source_and_bus(SendMode::PostFx);
        let s = &mut p.track_mut(src).unwrap().sends[0];
        (s.phase, s.mono) = (true, true);
        let (l, r) = render(&graph_with(&p, stereo()), 0, 10);
        let mono = (0.4 + 0.2) / 2.0;
        assert!(close(l[5], -mono * 0.7 * sl) && close(r[5], -mono * 0.7 * sr), "{l:?} {r:?}");
    }

    fn device(g: &RenderGraph, n: usize) -> Vec<Vec<f32>> {
        let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
        let mut s = RenderScratch::new(8);
        g.render(0, &mut l, &mut r, &mut s);
        let mut extra = vec![vec![9.0; n]; g.device_channels - 2];
        g.master_extra(n, &mut extra);
        g.device_out_at(0, n, &mut l, &mut r, &mut extra);
        [l, r].into_iter().chain(extra).collect()
    }

    #[test]
    fn hardware_outputs_reach_device_channels() {
        // A monitor bus off the master, to outputs 3/4, and a mono output to 5.
        let (mut p, src, bus) = source_and_bus(SendMode::PostFx);
        p.track_mut(src).unwrap().main_send = true;
        let b = p.track_mut(bus).unwrap();
        b.main_send = false;
        b.volume = 0.5;
        b.hw_outs = vec![HwOut::stereo(2), HwOut { volume: 0.5, ..HwOut::mono(4) }];
        let g = graph_with(&p, stereo());
        assert_eq!(g.device_channels, 5);
        let law = p.pan_law;
        let (sl, sr) = law.gains(-0.3);
        let (tl, tr) = law.gains(0.4);
        let out = device(&g, 10);
        // Outputs 1/2 carry the master (the source's own output), 3/4 the bus after its fader.
        assert!(close(out[0][5], 0.4 * 0.5 * tl) && close(out[1][5], 0.2 * 0.5 * tr), "{out:?}");
        let (bl, br) = (0.4 * 0.7 * sl * 0.5, 0.2 * 0.7 * sr * 0.5);
        assert!(close(out[2][5], bl) && close(out[3][5], br), "{out:?}");
        let (cl, cr) = law.gains(0.0);
        assert!(close(out[4][5], 0.5 * (bl * cl + br * cr) / 2.0), "{out:?}");

        // A muted output plays nothing; a pre-fader output skips the bus fader.
        let b = p.track_mut(bus).unwrap();
        b.hw_outs[1].muted = true;
        b.hw_outs[0].mode = SendMode::PostFx;
        let out = device(&graph_with(&p, stereo()), 10);
        assert_eq!(out.len(), 4);
        assert!(close(out[2][5], bl * 2.0) && close(out[3][5], br * 2.0), "{out:?}");

        // The master's own outputs, when set, replace the default 1/2.
        p.master.hw_outs = vec![HwOut { volume: 0.5, ..HwOut::stereo(4) }];
        let g = graph_with(&p, stereo());
        assert_eq!(g.device_channels, 6);
        let out = device(&g, 10);
        assert!(out[0].iter().chain(&out[1]).all(|&s| s == 0.0), "{out:?}");
        assert!(close(out[4][5], 0.5 * 0.4 * 0.5 * tl), "{out:?}");
        p.master.hw_outs.clear();
        let out = device(&graph_with(&p, stereo()), 10);
        assert!(out[0].iter().all(|&s| s == 0.0), "a master without outputs is not heard");

        // Without hardware outputs the device just gets the master.
        let (p, ..) = source_and_bus(SendMode::PostFader);
        let g = graph_with(&p, stereo());
        assert!(g.hw_bus.is_none() && g.device_channels == 2);
    }
}
