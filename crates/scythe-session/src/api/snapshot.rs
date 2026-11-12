//! The project as clients see it: plain JSON-friendly copies of what the model holds.

use scythe_core::metronome::{PREROLL_MID_MEASURE, PREROLL_PLAY, PREROLL_RECORD};
use scythe_core::model::source_span;
use scythe_core::units::gain_to_db;
use scythe_core::{Color, HwOut, Item, Marker, Project, SendMode, Track};
use scythe_engine::fx::FxStatus;
use serde::{Deserialize, Serialize};

use super::Target;
use crate::Session;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectSnapshot {
    pub revision: u64,
    pub name: String,
    pub path: Option<String>,
    pub dirty: bool,
    pub sample_rate: u32,
    /// End of the last item, in seconds.
    pub length: f64,
    pub bpm: f64,
    pub time_signature: (u32, u32),
    /// Where each bar starts, from bar 1, in seconds: to a little past the end of the project.
    pub bars: Vec<f64>,
    pub undo: Option<String>,
    pub redo: Option<String>,
    pub transport: TransportState,
    pub master: TrackInfo,
    pub tracks: Vec<TrackInfo>,
    pub markers: Vec<MarkerInfo>,
    /// The monitor mixes (`mix.list`).
    pub mixes: Vec<super::mix::MixInfo>,
    /// The tempo and time signature markers (`tempo.list`).
    #[serde(default)]
    pub tempo: Vec<super::tempo::TempoMarker>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransportState {
    pub playing: bool,
    pub recording: bool,
    /// The play position while playing, else the edit cursor, in seconds.
    pub position: f64,
    /// `position` as REAPER shows it: `bar.beat.hundredths`.
    pub bar_beat: String,
    pub cursor: f64,
    pub loop_enabled: bool,
    /// The time selection the loop repeats, `[start, end]` in seconds.
    pub loop_range: Option<(f64, f64)>,
    pub metronome: bool,
    /// The metronome's level at its outputs as a gain (REAPER's default is 0.25).
    #[serde(default)]
    pub metronome_volume: f64,
    /// Device outputs the metronome plays to; empty plays it with the master.
    #[serde(default)]
    pub metronome_outputs: Vec<ClickOutput>,
    #[serde(default)]
    pub count_in: CountInState,
    /// Where the metronome plays when a project names no output (a preference); `None` is
    /// with the master.
    #[serde(default)]
    pub metronome_default_output: Option<ClickOutput>,
    #[serde(default)]
    pub preroll: PrerollState,
}

/// The pre-roll preferences.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PrerollState {
    /// Pre-roll before playback.
    pub playback: bool,
    /// Pre-roll before recording.
    pub record: bool,
    /// Length in measures.
    pub measures: f64,
    /// Starts on a bar line.
    pub measure_start: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClickOutput {
    /// First device output, from 0.
    pub output: u32,
    pub mono: bool,
}

/// The count-in settings, and where a running count-in is.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CountInState {
    /// Count in before playback starts.
    pub playback: bool,
    /// Count in before recording starts.
    pub record: bool,
    /// Count-in length in measures.
    pub measures: f64,
    /// Counts in from the bar line before the start (a preference).
    #[serde(default)]
    pub measure_start: bool,
    /// While counting in (or pre-rolling before the project start): seconds until the playhead
    /// moves, negative (-2.0 is two seconds before).
    pub position: Option<f64>,
    /// While counting in: the beat clicking, from 1.
    pub beat: Option<u32>,
    /// While counting in: how many beats it has.
    pub beats: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackInfo {
    /// Stable for the session; `0` for the master.
    pub id: u64,
    /// Position from the top, `None` for the master.
    pub index: Option<usize>,
    pub guid: String,
    pub name: String,
    /// `#rrggbb`.
    pub color: Option<String>,
    /// Gain, 1.0 = 0 dB.
    pub volume: f64,
    pub volume_db: f64,
    /// -1 (left) to 1 (right).
    pub pan: f64,
    pub muted: bool,
    pub solo: bool,
    /// How it solos: "in_place" (heard through its sends too) or "ignore_routing".
    #[serde(default)]
    pub solo_mode: String,
    /// Solo defeat: stays heard while other tracks are soloed (monitor mixes have it).
    #[serde(default)]
    pub solo_defeat: bool,
    /// Polarity inverted.
    #[serde(default)]
    pub phase: bool,
    /// Pan mode: "balance", "classic", "stereo" or "dual"; `None` follows the project.
    #[serde(default)]
    pub pan_mode: Option<String>,
    /// Stereo width, -1..1 (1 = unchanged).
    #[serde(default)]
    pub width: f64,
    /// Left and right channel positions in dual pan.
    #[serde(default)]
    pub dual_pan: [f64; 2],
    /// Track groups the track belongs to, with its roles in each.
    #[serde(default)]
    pub groups: Vec<GroupRole>,
    pub armed: bool,
    pub monitor: bool,
    /// What recording writes (`track.record_mode` names), "input" by default.
    #[serde(default)]
    pub record_mode: String,
    /// Input monitoring: "off", "on" or "auto" (tape style).
    #[serde(default)]
    pub monitor_mode: String,
    /// REAPER's record input (`I_RECINPUT`), inputs from 0: mono input n is n, stereo pair from n is
    /// 1024 + n, MIDI is 4096 + device * 32 + channel; -1 is none.
    pub input: i64,
    /// REAPER's folder depth change (`I_FOLDERDEPTH`): 1 opens a folder, -n closes n.
    pub folder_depth: i32,
    /// How deep in folders the track sits, 0 at the top.
    pub level: usize,
    /// How a folder shows its children in the track view (`I_FOLDERCOMPACT`): 0 normal, 1 small,
    /// 2 collapsed.
    #[serde(default)]
    pub folder_compact: u8,
    /// Whether the mixer hides the folder's children.
    #[serde(default)]
    pub mixer_collapsed: bool,
    pub channels: u32,
    pub fx: Vec<FxInfo>,
    pub sends: Vec<SendInfo>,
    /// Whether the track feeds its parent or the master (`B_MAINSEND`).
    pub main_send: bool,
    /// Audio hardware outputs (the master's included).
    pub hw_outs: Vec<HwOutInfo>,
    pub items: Vec<ItemInfo>,
}

/// A track's part in one group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupRole {
    /// Group number, 1-64.
    pub group: usize,
    /// REAPER's names of the grouped parameters (`VOLUME_LEAD`, `VOLUME_VCA_FOLLOW`, ...).
    pub params: Vec<String>,
}

/// A track's group roles, by group.
pub fn group_roles(t: &Track) -> Vec<GroupRole> {
    use scythe_core::groups::{GROUP_COUNT, GroupParam};
    (0..GROUP_COUNT)
        .filter_map(|g| {
            let params: Vec<String> =
                GroupParam::ALL.iter().filter(|p| t.groups.has(**p, g)).map(|p| p.name().to_owned()).collect();
            (!params.is_empty()).then_some(GroupRole { group: g + 1, params })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FxInfo {
    pub index: usize,
    pub name: String,
    pub enabled: bool,
    /// Whether it processes audio (built in or hosted) rather than passing it through.
    pub running: bool,
    /// Unloaded, passing audio through.
    pub offline: bool,
    /// Wet share, 0-1.
    pub wet: f64,
    /// Plays only the difference it makes.
    pub delta: bool,
    /// `serial`, `parallel` (with the effect before it) or `merge_midi`.
    pub parallel: String,
    /// For an FX container: its channels and pins and the effects inside.
    pub container: Option<ContainerInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContainerInfo {
    pub channels: u32,
    pub inputs: u32,
    pub outputs: u32,
    pub effects: Vec<FxInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendInfo {
    pub index: usize,
    /// The receiving track's id.
    pub dest: u64,
    pub volume: f64,
    pub volume_db: f64,
    pub pan: f64,
    pub muted: bool,
    pub mode: Tap,
    /// Mixed down to mono before panning.
    pub mono: bool,
    /// Polarity inverted.
    pub phase: bool,
    /// Carries audio (false for a MIDI-only send).
    pub audio: bool,
    /// The first source channel, from 1; `None` for a MIDI-only send.
    #[serde(default)]
    pub src: Option<usize>,
    /// How many source channels it carries: 1, 2 or an even number.
    #[serde(default)]
    pub src_count: usize,
    /// The first channel it arrives on at the receiving track, from 1 (3 for the sidechain
    /// input on 3/4).
    #[serde(default)]
    pub dst: usize,
    /// Mixed down to the one destination channel `dst`.
    #[serde(default)]
    pub dst_mono: bool,
    /// The MIDI it carries, when any.
    pub midi: Option<SendMidiInfo>,
}

/// The MIDI half of a send. Channels and buses count from 1; 0 means all (sources) or
/// unchanged (destinations).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SendMidiInfo {
    pub src_channel: u8,
    pub dst_channel: u8,
    pub src_bus: u8,
    pub dst_bus: u8,
    pub faders: bool,
}

/// Where a send or hardware output takes its track's signal (REAPER's `I_SENDMODE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tap {
    /// After the fader and pan.
    PostFader,
    /// Before the effects.
    PreFx,
    /// After the effects, before the fader: what monitor mixes use.
    PostFx,
}

impl From<SendMode> for Tap {
    fn from(m: SendMode) -> Self {
        match m {
            SendMode::PostFader => Self::PostFader,
            SendMode::PreFx => Self::PreFx,
            SendMode::PostFx | SendMode::PostFxLegacy => Self::PostFx,
        }
    }
}

impl From<Tap> for SendMode {
    fn from(t: Tap) -> Self {
        match t {
            Tap::PostFader => Self::PostFader,
            Tap::PreFx => Self::PreFx,
            Tap::PostFx => Self::PostFx,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HwOutInfo {
    pub index: usize,
    /// First device output, from 0.
    pub output: usize,
    /// The track's channels summed into that one output.
    pub mono: bool,
    /// "Out 3/4", "Out 5 (mono)".
    pub label: String,
    pub mode: Tap,
    pub volume: f64,
    pub volume_db: f64,
    pub pan: f64,
    pub muted: bool,
    pub phase: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemInfo {
    pub id: u64,
    pub track: u64,
    pub name: String,
    pub position: f64,
    pub length: f64,
    pub volume: f64,
    pub muted: bool,
    pub takes: usize,
    pub active_take: usize,
    /// The active take's media file, when it plays one.
    pub file: Option<String>,
    /// Locked against moves, splits and deletes.
    pub locked: bool,
    /// Its item group; 0 for none.
    pub group: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarkerInfo {
    /// REAPER's marker or region number.
    pub number: u32,
    pub region: bool,
    pub name: String,
    pub position: f64,
    /// Where a region ends.
    pub end: Option<f64>,
    pub color: Option<String>,
}

pub fn color_hex(c: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

pub fn parse_color(text: &str) -> Option<Color> {
    let hex = text.strip_prefix('#').unwrap_or(text);
    (hex.len() == 6).then(|| u32::from_str_radix(hex, 16).ok().map(Color::from_hex)).flatten()
}

pub fn snapshot(session: &Session) -> ProjectSnapshot {
    let p = &session.project;
    let levels = p.folder_levels();
    ProjectSnapshot {
        revision: session.revision(),
        name: p.name(),
        path: p.path.as_ref().map(|p| p.display().to_string()),
        dirty: session.dirty,
        sample_rate: session.engine.sample_rate(),
        length: p.length(),
        bpm: p.tempo.bpm_at(0.0),
        time_signature: p.tempo.time_sig_at(0.0),
        bars: bars(p),
        undo: session.history.undo_label().map(Into::into),
        redo: session.history.redo_label().map(Into::into),
        transport: transport(session),
        master: track(session, &p.master, None, 0),
        tracks: p.tracks.iter().enumerate().map(|(i, t)| track(session, t, Some(i), levels[i])).collect(),
        markers: p.markers.iter().map(marker).collect(),
        mixes: super::mix::list(p),
        tempo: super::tempo::markers(p),
    }
}

/// Bar starts up to a minute past the project end, so an empty project still has a grid.
fn bars(p: &Project) -> Vec<f64> {
    const MAX_BARS: i64 = 20_000;
    let end = p.length() + 60.0;
    let timeline = p.tempo.timeline();
    (1..=MAX_BARS).map(|bar| timeline.bar_start(bar)).take_while(|&t| t <= end).collect()
}

pub fn transport(session: &Session) -> TransportState {
    let p = &session.project;
    TransportState {
        playing: session.is_playing(),
        recording: session.is_recording(),
        position: session.position(),
        bar_beat: p.tempo.format_bar_beat(session.position()),
        cursor: p.cursor,
        loop_enabled: p.loop_enabled,
        loop_range: p.time_selection.filter(|r| !r.is_empty()).map(|r| (r.start, r.end)),
        metronome: session.metronome(),
        metronome_volume: p.metronome.volume,
        metronome_outputs: p
            .metronome
            .device_outputs()
            .into_iter()
            .map(|(first, mono)| ClickOutput { output: first as u32, mono })
            .collect(),
        count_in: count_in(session),
        metronome_default_output: scythe_core::metronome::Metronome::default()
            .outputs_or(session.prefs.metronome_default_output)
            .first()
            .map(|&(first, mono)| ClickOutput { output: first as u32, mono }),
        preroll: PrerollState {
            playback: session.prefs.preroll & PREROLL_PLAY != 0,
            record: session.prefs.preroll & PREROLL_RECORD != 0,
            measures: session.prefs.preroll_measures,
            measure_start: session.prefs.preroll & PREROLL_MID_MEASURE == 0,
        },
    }
}

pub fn count_in(session: &Session) -> CountInState {
    let m = &session.project.metronome;
    let running = session.counting_in();
    CountInState {
        playback: m.has(scythe_core::metronome::COUNT_IN_PLAY),
        record: m.has(scythe_core::metronome::COUNT_IN_RECORD),
        measures: m.count_in_measures,
        measure_start: session.prefs.count_in_at_measure(),
        position: running.map(|(at, _, _)| at),
        beat: running.map(|(_, beat, _)| beat),
        beats: running.map(|(_, _, beats)| beats),
    }
}

pub fn track(session: &Session, t: &Track, index: Option<usize>, level: usize) -> TrackInfo {
    TrackInfo {
        id: if index.is_some() { t.id.0 } else { 0 },
        index,
        guid: t.guid.clone(),
        name: t.name.clone(),
        color: t.color.map(color_hex),
        volume: t.volume,
        volume_db: gain_to_db(t.volume),
        pan: t.pan,
        muted: t.muted,
        solo: t.solo,
        solo_mode: match t.solo_mode {
            scythe_core::SoloMode::InPlace => "in_place",
            scythe_core::SoloMode::IgnoreRouting => "ignore_routing",
        }
        .into(),
        solo_defeat: t.solo_defeat,
        phase: t.phase,
        pan_mode: t.pan_mode.map(|m| m.name().to_owned()),
        width: t.width,
        dual_pan: t.dual_pan,
        groups: group_roles(t),
        armed: t.armed,
        monitor: t.monitor,
        record_mode: t.rec_mode.name().to_owned(),
        monitor_mode: t.monitor_mode().name().to_owned(),
        input: t.rec_input,
        folder_depth: t.folder_depth,
        level,
        folder_compact: t.folder_compact,
        mixer_collapsed: t.mixer_collapsed,
        channels: t.channels,
        fx: fx_list(session, if index.is_some() { Target::Track(t.id) } else { Target::Master }, t),
        sends: send_list(t),
        main_send: t.main_send,
        hw_outs: hw_out_list(t),
        items: t.items.iter().map(|it| item(t, it)).collect(),
    }
}

pub fn fx_list(session: &Session, target: Target, t: &Track) -> Vec<FxInfo> {
    let owner = match target {
        Target::Master => scythe_engine::fx::FxOwner::Master,
        Target::Track(id) => scythe_engine::fx::FxOwner::Track(id),
    };
    chain_info(session, &owner, &t.fx)
}

/// The effects of a chain, those inside containers included.
pub fn chain_info(session: &Session, owner: &scythe_engine::fx::FxOwner, slots: &[scythe_core::FxSlot]) -> Vec<FxInfo> {
    fn walk(
        session: &Session,
        owner: &scythe_engine::fx::FxOwner,
        top: &[scythe_core::FxSlot],
        slots: &[scythe_core::FxSlot],
        path: &mut Vec<usize>,
    ) -> Vec<FxInfo> {
        slots
            .iter()
            .enumerate()
            .map(|(i, slot)| {
                path.push(i);
                let status = session.engine.fx().status_at(owner, top, path);
                let (wet, delta) = slot.wet();
                let container = slot.container_config().map(|cfg| ContainerInfo {
                    channels: cfg.channels,
                    inputs: cfg.inputs,
                    outputs: cfg.outputs,
                    effects: walk(session, owner, top, &slot.container_children(), path),
                });
                path.pop();
                FxInfo {
                    index: i,
                    name: slot.name.clone(),
                    enabled: slot.enabled,
                    running: container.is_some() || !matches!(status, None | Some(FxStatus::Unavailable(_))),
                    offline: slot.offline(),
                    wet,
                    delta,
                    parallel: match slot.parallel() {
                        scythe_core::fx_chain::Parallel::Serial => "serial",
                        scythe_core::fx_chain::Parallel::Audio => "parallel",
                        scythe_core::fx_chain::Parallel::MergeMidi => "merge_midi",
                    }
                    .into(),
                    container,
                }
            })
            .collect()
    }
    walk(session, owner, slots, slots, &mut Vec::new())
}

pub fn send_list(t: &Track) -> Vec<SendInfo> {
    t.sends
        .iter()
        .enumerate()
        .map(|(i, s)| SendInfo {
            index: i,
            dest: s.dest.0,
            volume: s.volume,
            volume_db: gain_to_db(s.volume),
            pan: s.pan,
            muted: s.muted,
            mode: s.mode.into(),
            mono: s.mono,
            phase: s.phase,
            audio: s.src_channels >= 0,
            src: source_span(s.src_channels).map(|(first, _)| first + 1),
            src_count: source_span(s.src_channels).map_or(0, |(_, count)| count),
            dst: (s.dst_channels & 1023) as usize + 1,
            dst_mono: s.dst_channels & 1024 != 0,
            midi: Some(s.midi()).filter(|m| m.enabled).map(|m| {
                let n = |v: Option<u8>| v.map_or(0, |v| v + 1);
                SendMidiInfo {
                    src_channel: n(m.src_channel),
                    dst_channel: n(m.dst_channel),
                    src_bus: n(m.src_bus),
                    dst_bus: n(m.dst_bus),
                    faders: m.faders,
                }
            }),
        })
        .collect()
}

pub fn hw_out(index: usize, o: &HwOut) -> HwOutInfo {
    HwOutInfo {
        index,
        output: o.first_output(),
        mono: o.is_mono(),
        label: o.label(),
        mode: o.mode.into(),
        volume: o.volume,
        volume_db: gain_to_db(o.volume),
        pan: o.pan,
        muted: o.muted,
        phase: o.phase,
    }
}

pub fn hw_out_list(t: &Track) -> Vec<HwOutInfo> {
    t.hw_outs.iter().enumerate().map(|(i, o)| hw_out(i, o)).collect()
}

pub fn item(t: &Track, it: &Item) -> ItemInfo {
    ItemInfo {
        id: it.id.0,
        track: t.id.0,
        name: it.name().to_owned(),
        position: it.position,
        length: it.length,
        volume: it.volume,
        muted: it.muted,
        takes: it.takes.len(),
        active_take: it.active_take,
        file: it.take().and_then(|tk| tk.source.file()).map(|f| f.display().to_string()),
        locked: it.locked,
        group: it.group,
    }
}

pub fn marker(m: &Marker) -> MarkerInfo {
    MarkerInfo {
        number: m.index,
        region: m.region_end.is_some(),
        name: m.name.clone(),
        position: m.position,
        end: m.region_end,
        color: m.color.map(color_hex),
    }
}
