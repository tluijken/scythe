use std::path::PathBuf;

use crate::midi::{MidiEvent, MidiSource};
use crate::model::{Item, ItemId, MIN_ITEM_LENGTH, Source, Take, TimeRange, Track};
use crate::tempo::Timeline;

/// What an armed track records (`REC` field 4, REAPER's `I_RECMODE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecordMode {
    /// The record input, audio or MIDI as the input is.
    #[default]
    Input,
    /// The input, written in a fixed format whatever the input is.
    InputForced(InputFormat),
    /// The track's own signal, where `RecordTap` says. Without latency compensation the file
    /// comes out late by the track's plugin latency.
    Output { format: OutputFormat, compensated: bool },
    /// The MIDI the track's effects put out.
    MidiOutput,
    /// MIDI played into an existing item under the playhead, added to what is there.
    MidiOverdub,
    /// MIDI that replaces what an existing item has in the recorded range.
    MidiReplace,
    /// Like `MidiReplace`, only while something is played (and the CC timeout after).
    MidiTouchReplace,
    /// Like `MidiReplace`, from the first message played until recording stops.
    MidiLatchReplace,
    /// Nothing is recorded; the track only monitors its input.
    Disabled,
    /// A code Scythe does not know, kept for REAPER.
    Other(i64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFormat {
    Mono,
    Stereo,
    Multichannel,
    Midi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Stereo,
    Mono,
    /// Every channel of the track.
    Multichannel,
}

impl RecordMode {
    /// Every mode Scythe knows, in the order of REAPER's record mode menu.
    pub const ALL: [RecordMode; 17] = [
        Self::Input,
        Self::InputForced(InputFormat::Mono),
        Self::InputForced(InputFormat::Stereo),
        Self::InputForced(InputFormat::Multichannel),
        Self::InputForced(InputFormat::Midi),
        Self::Output { format: OutputFormat::Stereo, compensated: false },
        Self::Output { format: OutputFormat::Stereo, compensated: true },
        Self::Output { format: OutputFormat::Mono, compensated: false },
        Self::Output { format: OutputFormat::Mono, compensated: true },
        Self::Output { format: OutputFormat::Multichannel, compensated: false },
        Self::Output { format: OutputFormat::Multichannel, compensated: true },
        Self::MidiOutput,
        Self::MidiOverdub,
        Self::MidiReplace,
        Self::MidiTouchReplace,
        Self::MidiLatchReplace,
        Self::Disabled,
    ];

    pub fn from_code(code: i64) -> Self {
        use OutputFormat::*;
        let out = |format, compensated| Self::Output { format, compensated };
        match code {
            0 => Self::Input,
            1 => out(Stereo, false),
            2 => Self::Disabled,
            3 => out(Stereo, true),
            4 => Self::MidiOutput,
            5 => out(Mono, false),
            6 => out(Mono, true),
            7 => Self::MidiOverdub,
            8 => Self::MidiReplace,
            9 => Self::MidiTouchReplace,
            10 => out(Multichannel, false),
            11 => out(Multichannel, true),
            12 => Self::InputForced(InputFormat::Mono),
            13 => Self::InputForced(InputFormat::Stereo),
            14 => Self::InputForced(InputFormat::Multichannel),
            15 => Self::InputForced(InputFormat::Midi),
            16 => Self::MidiLatchReplace,
            c => Self::Other(c),
        }
    }

    pub fn code(self) -> i64 {
        use OutputFormat::*;
        match self {
            Self::Input => 0,
            Self::Output { format: Stereo, compensated: false } => 1,
            Self::Disabled => 2,
            Self::Output { format: Stereo, compensated: true } => 3,
            Self::MidiOutput => 4,
            Self::Output { format: Mono, compensated: false } => 5,
            Self::Output { format: Mono, compensated: true } => 6,
            Self::MidiOverdub => 7,
            Self::MidiReplace => 8,
            Self::MidiTouchReplace => 9,
            Self::Output { format: Multichannel, compensated: false } => 10,
            Self::Output { format: Multichannel, compensated: true } => 11,
            Self::InputForced(InputFormat::Mono) => 12,
            Self::InputForced(InputFormat::Stereo) => 13,
            Self::InputForced(InputFormat::Multichannel) => 14,
            Self::InputForced(InputFormat::Midi) => 15,
            Self::MidiLatchReplace => 16,
            Self::Other(c) => c,
        }
    }

    /// The name the control API uses.
    pub fn name(self) -> &'static str {
        use OutputFormat::*;
        match self {
            Self::Input => "input",
            Self::InputForced(InputFormat::Mono) => "input_mono",
            Self::InputForced(InputFormat::Stereo) => "input_stereo",
            Self::InputForced(InputFormat::Multichannel) => "input_multichannel",
            Self::InputForced(InputFormat::Midi) => "input_midi",
            Self::Output { format: Stereo, compensated: false } => "output_stereo",
            Self::Output { format: Stereo, compensated: true } => "output_stereo_compensated",
            Self::Output { format: Mono, compensated: false } => "output_mono",
            Self::Output { format: Mono, compensated: true } => "output_mono_compensated",
            Self::Output { format: Multichannel, compensated: false } => "output_multichannel",
            Self::Output { format: Multichannel, compensated: true } => "output_multichannel_compensated",
            Self::MidiOutput => "midi_output",
            Self::MidiOverdub => "midi_overdub",
            Self::MidiReplace => "midi_replace",
            Self::MidiTouchReplace => "midi_touch_replace",
            Self::MidiLatchReplace => "midi_latch_replace",
            Self::Disabled => "disabled",
            Self::Other(_) => "other",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == name)
    }

    /// The label of REAPER's record mode menu.
    pub fn label(self) -> &'static str {
        use OutputFormat::*;
        match self {
            Self::Input => "Input (audio or MIDI)",
            Self::InputForced(InputFormat::Mono) => "Input (force mono)",
            Self::InputForced(InputFormat::Stereo) => "Input (force stereo)",
            Self::InputForced(InputFormat::Multichannel) => "Input (force multichannel)",
            Self::InputForced(InputFormat::Midi) => "Input (force MIDI)",
            Self::Output { format: Stereo, compensated: false } => "Output (stereo)",
            Self::Output { format: Stereo, compensated: true } => "Output (stereo, latency compensated)",
            Self::Output { format: Mono, compensated: false } => "Output (mono)",
            Self::Output { format: Mono, compensated: true } => "Output (mono, latency compensated)",
            Self::Output { format: Multichannel, compensated: false } => "Output (multichannel)",
            Self::Output { format: Multichannel, compensated: true } => "Output (multichannel, latency compensated)",
            Self::MidiOutput => "Output (MIDI)",
            Self::MidiOverdub => "MIDI overdub",
            Self::MidiReplace => "MIDI replace",
            Self::MidiTouchReplace => "MIDI touch-replace",
            Self::MidiLatchReplace => "MIDI latch-replace",
            Self::Disabled => "Disable (input monitoring only)",
            Self::Other(_) => "Other",
        }
    }

    /// Records the track's own audio rather than its input.
    pub fn is_output(self) -> bool {
        matches!(self, Self::Output { .. })
    }

    /// Records MIDI into existing items rather than a new one.
    pub fn is_midi_overdub(self) -> bool {
        matches!(self, Self::MidiOverdub | Self::MidiReplace | Self::MidiTouchReplace | Self::MidiLatchReplace)
    }
}

/// Where output recording taps the track (`REC` field 8, `I_RECMODE_FLAGS & 3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecordTap {
    #[default]
    PostFader,
    PreFx,
    PostFxPreFader,
}

impl RecordTap {
    pub const ALL: [RecordTap; 3] = [Self::PostFader, Self::PreFx, Self::PostFxPreFader];

    pub fn from_code(code: i64) -> Self {
        match code & 3 {
            1 => Self::PreFx,
            2 => Self::PostFxPreFader,
            _ => Self::PostFader,
        }
    }

    pub fn code(self) -> i64 {
        match self {
            Self::PostFader => 0,
            Self::PreFx => 1,
            Self::PostFxPreFader => 2,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::PostFader => "post_fader",
            Self::PreFx => "pre_fx",
            Self::PostFxPreFader => "post_fx",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::PostFader => "Post-fader (post-FX)",
            Self::PreFx => "Pre-FX",
            Self::PostFxPreFader => "Post-FX, pre-fader",
        }
    }
}

/// Input monitoring (`REC` field 3, `I_RECMON`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MonitorMode {
    Off,
    #[default]
    On,
    /// Tape style: not while playing, only while stopped or recording.
    Auto,
}

impl MonitorMode {
    pub const ALL: [MonitorMode; 3] = [Self::Off, Self::On, Self::Auto];

    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
            Self::Auto => "auto",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Monitor input: off",
            Self::On => "Monitor input",
            Self::Auto => "Monitor input (tape auto style)",
        }
    }

    /// Whether the input is heard, while playing or not and recording or not.
    pub fn hears(self, playing: bool, recording: bool) -> bool {
        match self {
            Self::Off => false,
            Self::On => true,
            Self::Auto => !playing || recording,
        }
    }
}

/// The project's record mode (`RECMODE`): what part of a recording is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProjectRecordMode {
    #[default]
    Normal,
    /// Only what falls inside the selected items, as new takes of them.
    ItemPunch,
    /// Only what falls inside the time selection.
    TimeSelectionPunch,
}

impl ProjectRecordMode {
    pub const ALL: [ProjectRecordMode; 3] = [Self::Normal, Self::TimeSelectionPunch, Self::ItemPunch];

    pub fn from_code(code: i64) -> Self {
        match code {
            0 => Self::ItemPunch,
            2 => Self::TimeSelectionPunch,
            _ => Self::Normal,
        }
    }

    pub fn code(self) -> i64 {
        match self {
            Self::ItemPunch => 0,
            Self::Normal => 1,
            Self::TimeSelectionPunch => 2,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::ItemPunch => "item_punch",
            Self::TimeSelectionPunch => "time_selection_punch",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::ItemPunch => "Selected item auto-punch",
            Self::TimeSelectionPunch => "Time selection auto-punch",
        }
    }

    pub fn of(project: &crate::Project) -> Self {
        project_line(project, "RECMODE").and_then(|t| t.get(1)?.parse().ok()).map_or(Self::Normal, Self::from_code)
    }

    pub fn set(self, project: &mut crate::Project) {
        set_project_line(project, "RECMODE", &["RECMODE".to_owned(), self.code().to_string()]);
    }
}

/// Where recordings go (`RECORD_PATH`, first field): relative to the project's folder, empty
/// for the folder itself.
pub fn record_path(project: &crate::Project) -> String {
    project_line(project, "RECORD_PATH").and_then(|t| t.get(1).cloned()).unwrap_or_default()
}

fn project_line(project: &crate::Project, key: &str) -> Option<Vec<String>> {
    project.extra.iter().find_map(|n| match n {
        crate::rpp::Node::Line(l) => {
            let t = crate::rpp::tokenize(l);
            (t.first().map(String::as_str) == Some(key)).then_some(t)
        }
        _ => None,
    })
}

fn set_project_line(project: &mut crate::Project, key: &str, toks: &[String]) {
    let line = crate::rpp::Node::Line(crate::rpp::join_tokens(toks.iter().map(String::as_str)));
    let at = project.extra.iter().position(
        |n| matches!(n, crate::rpp::Node::Line(l) if crate::rpp::tokenize(l).first().map(String::as_str) == Some(key)),
    );
    match at {
        Some(i) => project.extra[i] = line,
        None => project.extra.insert(0, line),
    }
}

/// Where the transport went while one file was recorded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecordPass {
    /// Project time recording started at.
    pub start: f64,
    /// Loop range the transport wrapped around, when repeat was on.
    pub looped: Option<TimeRange>,
    /// Only keep what falls inside this range (time-selection auto-punch).
    pub punch: Option<TimeRange>,
}

/// A stretch of project time a recording covers, with the passes heard there: one for a
/// linear recording, one per time around the loop for loop recording.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    /// Pass number (from 0) and the file time heard at `start`.
    pub takes: Vec<(usize, f64)>,
}

/// Lays a recorded file out on the timeline as REAPER 7.73 does. `file_len` is the file's
/// length and `offset` how far into it project time `pass.start` was heard (input plus output
/// latency), both in seconds. Returns the segments and the number of passes.
///
/// Around a loop every pass spans the whole loop, also an incomplete first pass (which then
/// starts in silence) and an incomplete last one; what was recorded before the loop start is
/// a segment of its own. `punch` keeps only what falls inside those ranges (auto-punch).
pub fn record_segments(file_len: f64, offset: f64, pass: RecordPass, punch: &[TimeRange]) -> (Vec<Segment>, usize) {
    let elapsed = file_len - offset;
    if elapsed <= MIN_ITEM_LENGTH {
        return (Vec::new(), 0);
    }
    let start = pass.start;
    let wrap = pass.looped.filter(|r| !r.is_empty() && start < r.end && elapsed > r.end - start);
    let (mut segments, passes) = match wrap {
        Some(r) => {
            let len = r.length();
            let lead = r.start - start;
            let passes = (((elapsed - lead.max(0.0)) / len) - 1e-9).ceil().max(1.0) as usize;
            let mut segs = Vec::new();
            if lead > MIN_ITEM_LENGTH {
                segs.push(Segment { start, end: r.start, takes: vec![(0, offset)] });
            }
            let takes = (0..passes).map(|k| (k, offset + lead + k as f64 * len)).collect();
            segs.push(Segment { start: r.start, end: r.end, takes });
            (segs, passes)
        }
        None => (vec![Segment { start, end: start + elapsed, takes: vec![(0, offset)] }], 1),
    };
    let punch: Vec<TimeRange> = punch.iter().copied().filter(|p| !p.is_empty()).collect();
    if !punch.is_empty() {
        segments = segments
            .into_iter()
            .flat_map(|s| {
                punch
                    .iter()
                    .filter_map(|p| {
                        let (from, to) = (s.start.max(p.start), s.end.min(p.end));
                        (to - from > MIN_ITEM_LENGTH).then(|| Segment {
                            start: from,
                            end: to,
                            takes: s.takes.iter().map(|&(k, o)| (k, o + from - s.start)).collect(),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
    }
    if segments.is_empty() {
        return (Vec::new(), 0);
    }
    (segments, passes)
}

/// Where a recording goes on a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordInto {
    /// Into `lane` (0 without fixed lanes): items the recording overlaps are split at its
    /// edges and get the passes as new takes; where there is no item, new items do.
    Takes { lane: usize },
    /// Every pass into a lane of its own (REAPER 7's "new recording adds lanes").
    NewLanes,
}

/// An empty take, as REAPER fills take lanes that a pass did not reach (`TAKE NULL`).
pub fn empty_take() -> Take {
    let mut t = Take::from_file(PathBuf::new());
    t.name.clear();
    t.source = Source::Empty;
    t.lines.insert("TAKE".into(), vec!["TAKE".into(), "NULL".into()]);
    t
}

/// The highest `RECPASS` of any take in the project, so new passes number on from it.
pub fn last_record_pass(project: &crate::Project) -> u32 {
    project.tracks.iter().flat_map(|t| &t.items).flat_map(|i| &i.takes).filter_map(record_pass).max().unwrap_or(0)
}

/// A take's `RECPASS`: which record pass made it.
pub fn record_pass(take: &Take) -> Option<u32> {
    take.extra.iter().find_map(|n| match n {
        crate::rpp::Node::Line(l) => {
            let t = crate::rpp::tokenize(l);
            (t.first().map(String::as_str) == Some("RECPASS")).then(|| t.get(1)?.parse().ok()).flatten()
        }
        _ => None,
    })
}

/// Adds a recording laid out by [`record_segments`] to `track`. `take` makes the take of a
/// pass from the file time heard at the item's start; `first_pass` is the `RECPASS` of pass 0
/// and `file_len` the file's length in seconds.
/// Returns the items that got a recorded take.
pub fn add_recording(
    track: &mut Track,
    segments: &[Segment],
    passes: usize,
    take: &dyn Fn(f64) -> Take,
    into: RecordInto,
    first_pass: u32,
    file_len: f64,
) -> Vec<ItemId> {
    let make = |k: usize, offset: f64| {
        let mut t = take(offset);
        t.extra.push(crate::rpp::Node::Line(format!("RECPASS {}", first_pass + k as u32)));
        t
    };
    match into {
        RecordInto::Takes { lane } => add_as_takes(track, segments, passes, &make, lane),
        RecordInto::NewLanes => add_as_lanes(track, segments, &make, file_len),
    }
}

fn lane_ypos(track: &Track, lane: usize) -> Option<(f64, f64)> {
    let n = track.lane_count() as f64;
    track.fixed_lanes.then(|| (lane as f64 / n, 1.0 / n))
}

fn add_as_takes(
    track: &mut Track,
    segments: &[Segment],
    passes: usize,
    make: &dyn Fn(usize, f64) -> Take,
    lane: usize,
) -> Vec<ItemId> {
    let in_lane = |it: &Item| !track_fixed(it, lane);
    let (from, to) = segments.iter().fold((f64::MAX, f64::MIN), |(a, b), s| (a.min(s.start), b.max(s.end)));
    let base = track
        .items
        .iter()
        .filter(|it| in_lane(it) && it.position < to - 1e-9 && it.end() > from + 1e-9)
        .map(|it| it.takes.len())
        .max()
        .unwrap_or(0);
    for s in segments {
        for at in [s.start, s.end] {
            let mut right = Vec::new();
            for it in track.items.iter_mut().filter(|it| in_lane(it)) {
                right.extend(it.split_at(at));
            }
            track.items.extend(right);
        }
    }
    let ypos = lane_ypos(track, lane);
    let mut touched = Vec::new();
    for s in segments {
        let fill = |it: &mut Item, at: f64| {
            it.takes.resize_with(base.max(it.takes.len()), empty_take);
            for k in 0..passes {
                match s.takes.iter().find(|t| t.0 == k) {
                    Some(&(_, offset)) => it.takes.push(make(k, offset + at - s.start)),
                    None => it.takes.push(empty_take()),
                }
            }
            let last = s.takes.iter().map(|t| t.0).max().unwrap_or(0);
            it.active_take = it.takes.len() - passes + last;
        };
        let mut covered: Vec<(f64, f64)> = Vec::new();
        for it in track
            .items
            .iter_mut()
            .filter(|it| !track_fixed(it, lane) && it.position >= s.start - 1e-9 && it.end() <= s.end + 1e-9)
        {
            let at = it.position;
            fill(it, at);
            covered.push((it.position, it.end()));
            touched.push(it.id);
        }
        covered.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut gaps = Vec::new();
        let mut at = s.start;
        for (a, b) in covered {
            if a - at > MIN_ITEM_LENGTH {
                gaps.push((at, a));
            }
            at = at.max(b);
        }
        if s.end - at > MIN_ITEM_LENGTH {
            gaps.push((at, s.end));
        }
        for (a, b) in gaps {
            let mut it = Item::new(a, b - a, empty_take());
            it.takes.clear();
            fill(&mut it, a);
            if it.takes.first().is_some_and(|t| t.source == Source::Empty) {
                it.takes[0].lines.remove("TAKE");
            }
            it.ypos = ypos;
            touched.push(it.id);
            track.items.push(it);
        }
    }
    track.items.sort_by(|a, b| a.position.total_cmp(&b.position));
    touched
}

/// Whether `it` sits outside `lane` of a fixed-lane track (named for the filter it serves).
fn track_fixed(it: &Item, lane: usize) -> bool {
    it.ypos.is_some() && it.lane() != lane
}

fn add_as_lanes(
    track: &mut Track,
    segments: &[Segment],
    make: &dyn Fn(usize, f64) -> Take,
    file_len: f64,
) -> Vec<ItemId> {
    if !track.fixed_lanes {
        track.set_fixed_lanes(true);
    }
    let comp = track.comp_lane();
    let at_bottom = track.lane_settings() & 16 != 0;
    let mut lane_of_pass: Vec<Option<usize>> = Vec::new();
    let mut touched = Vec::new();
    let mut pieces: Vec<(usize, f64, f64, f64)> = Vec::new();
    for s in segments {
        for &(k, offset) in &s.takes {
            // Only what was recorded: an incomplete pass ends (or begins) with the file.
            let from = s.start.max(s.start - offset);
            let to = s.end.min(s.start + file_len - offset);
            if to - from > MIN_ITEM_LENGTH {
                pieces.push((k, from, to, offset + from - s.start));
            }
        }
    }
    for &(k, from, to, offset) in &pieces {
        if lane_of_pass.len() <= k {
            lane_of_pass.resize(k + 1, None);
        }
        let lane = match lane_of_pass[k] {
            Some(l) => l,
            None => {
                let n = track.lane_count();
                let free = (0..n)
                    .filter(|&l| !at_bottom && Some(l) != comp && !lane_of_pass.contains(&Some(l)))
                    .find(|&l| !track.lane_busy(l, from, to));
                let lane = free.unwrap_or_else(|| track.add_lane());
                lane_of_pass[k] = Some(lane);
                lane
            }
        };
        let mut it = Item::new(from, to - from, make(k, offset));
        it.ypos = lane_ypos(track, lane);
        touched.push(it.id);
        track.items.push(it);
    }
    let n = track.lane_count();
    track.layout_lanes(n);
    if let Some(last) = lane_of_pass.iter().rev().flatten().next().copied() {
        // The newest pass plays, as do the lanes below it (REAPER sets every bit from it up).
        let mut words = crate::lanes::solo_words(last..256);
        if let Some(c) = comp {
            words = crate::lanes::solo_words([c]);
        }
        track.lane_solo = Some(words);
        let mut rec = track.lane_rec.unwrap_or([-1; 4]);
        rec[3] = last as i64;
        track.lane_rec = Some(rec);
        if comp.is_some()
            && track.lane_settings() & 2 == 0
            && let Some(&(_, from, to, _)) = pieces.iter().rev().find(|p| lane_of_pass.get(p.0) == Some(&Some(last)))
        {
            track.swipe(last, from, to);
        }
    }
    track.items.sort_by(|a, b| a.position.total_cmp(&b.position));
    touched
}

/// Ticks per quarter note of recorded MIDI, as REAPER records it.
const RECORD_PPQ: u32 = 960;

/// Turns MIDI played during a recording into an in-project MIDI item. `events` are project
/// times with their message; `stop` is where the transport was when recording stopped.
///
/// Messages stay where they were played, so passes around a loop layer into one take (as
/// REAPER's MIDI overdub); the item then spans the loop. Notes still held end with the item and
/// note-offs without their note-on are dropped.
pub fn place_midi_recording(
    events: &[(f64, [u8; 3], u8)],
    pass: RecordPass,
    stop: f64,
    timeline: &Timeline,
) -> Option<Item> {
    let wrapped = events.iter().any(|e| e.0 < pass.start) || stop < pass.start;
    let (mut from, mut to) = match pass.looped.filter(|_| wrapped) {
        Some(r) => (pass.start.min(r.start), r.end),
        None => (pass.start, stop.max(events.iter().map(|e| e.0).fold(pass.start, f64::max))),
    };
    if let Some(p) = pass.punch.filter(|p| !p.is_empty()) {
        (from, to) = (from.max(p.start), to.min(p.end));
    }
    if to - from <= MIN_ITEM_LENGTH {
        return None;
    }
    let start_qn = timeline.qn_at(from);
    let length = ((timeline.qn_at(to) - start_qn) * RECORD_PPQ as f64).round() as u64;
    let tick = |t: f64| (((timeline.qn_at(t) - start_qn) * RECORD_PPQ as f64).round() as u64).min(length);
    let mut sorted: Vec<&(f64, [u8; 3], u8)> = events.iter().filter(|e| (from..to).contains(&e.0)).collect();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut held: Vec<(u8, u8)> = Vec::new();
    let mut out = Vec::new();
    for &&(t, bytes, len) in &sorted {
        let (status, key) = (bytes[0] & 0xf0, (bytes[0] & 0x0f, bytes[1]));
        let on = status == 0x90 && bytes[2] > 0;
        let off = status == 0x80 || (status == 0x90 && bytes[2] == 0);
        if off {
            let Some(i) = held.iter().position(|h| *h == key) else { continue };
            held.remove(i);
        } else if on {
            held.push(key);
        }
        out.push(MidiEvent { tick: tick(t), bytes, len, flags: 0, shape: None });
    }
    if out.is_empty() {
        return None;
    }
    for (ch, k) in held {
        out.push(MidiEvent { tick: length, bytes: [0x80 | ch, k, 0], len: 3, flags: 0, shape: None });
    }
    let source =
        MidiSource { ticks_per_qn: RECORD_PPQ, events: out, long: Vec::new(), length_ticks: length, cc_interp: 32 };
    let mut take = Take::from_file(PathBuf::new());
    take.source = Source::Raw(source.to_chunk());
    Some(Item::new(from, to - from, take))
}

/// REAPER's default name for a recorded file (`$tracknumber-$track-$yymmdd_$hhmm`), without
/// the extension; the time is UTC.
pub fn record_file_name(track_number: usize, track_name: &str, unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let secs = unix_secs % 86_400;
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let stamp = format!("{:02}{month:02}{day:02}_{:02}{:02}", year % 100, secs / 3600, secs / 60 % 60);
    let name: String =
        track_name.chars().map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '_' } else { c }).collect();
    match name.trim() {
        "" => format!("{track_number:02}-{stamp}"),
        n => format!("{track_number:02}-{n}-{stamp}"),
    }
}

/// Puts a MIDI recording made by [`place_midi_recording`] on `track` as its record mode says.
/// The overdub modes record into the MIDI item under the recording's start: overdub adds the
/// new events, replace first clears the recorded range, latch-replace clears from the first
/// event played to the end, touch-replace notes from the first note played to the last and
/// each controller (with pitch bend and pressure) only while it is played: from its first
/// message until `cc_timeout` seconds pass without one (REAPER's `midicctouchtimeout`).
/// Without such an item (or for other modes) the recording becomes an item of its own.
/// Returns the item.
pub fn merge_midi_recording(
    track: &mut Track,
    item: Item,
    mode: RecordMode,
    timeline: &Timeline,
    cc_timeout: f64,
) -> ItemId {
    let new_id = item.id;
    if !mode.is_midi_overdub() {
        track.items.push(item);
        return new_id;
    }
    let raw = |it: &Item| match &it.take()?.source {
        Source::Raw(c) if c.args().first().is_some_and(|k| k == "MIDI") => Some(c.clone()),
        _ => None,
    };
    let target = track.items.iter().position(|it| {
        it.position <= item.position + 1e-9
            && it.end() > item.position + 1e-9
            && !it.looped
            && it.take().is_some_and(|t| t.source_offset == 0.0 && t.playrate == 1.0)
            && raw(it).is_some()
    });
    let (Some(ti), Some(rec_chunk)) = (target, raw(&item)) else {
        track.items.push(item);
        return new_id;
    };
    let Some(recorded) = MidiSource::parse(&rec_chunk) else { return new_id };
    let target = &mut track.items[ti];
    let chunk = raw(target).expect("checked above");
    let Some(mut src) = MidiSource::parse(&chunk) else { return new_id };
    let (qn0, rec_qn0) = (timeline.qn_at(target.position), timeline.qn_at(item.position));
    let ppq = src.ticks_per_qn.max(1) as f64;
    let to_tick = |rec_tick: u64| {
        let qn = rec_qn0 + rec_tick as f64 / recorded.ticks_per_qn.max(1) as f64;
        ((qn - qn0) * ppq).round().max(0.0) as u64
    };
    // The recording's end-of-source marker stays behind.
    let played: Vec<MidiEvent> = recorded
        .events
        .iter()
        .filter(|e| !(e.tick == recorded.length_ticks && e.bytes[0] & 0xf0 == 0xb0))
        .map(|e| MidiEvent { tick: to_tick(e.tick), ..e.clone() })
        .collect();
    let first = played.first().map(|e| e.tick);
    let (start, end) = (to_tick(0), to_tick(recorded.length_ticks));
    let is_note = |e: &MidiEvent| matches!(e.bytes[0] & 0xf0, 0x80 | 0x90);
    let touch = mode == RecordMode::MidiTouchReplace;
    let range = match mode {
        RecordMode::MidiReplace => Some((start, end)),
        RecordMode::MidiLatchReplace => first.map(|f| (f, end)),
        RecordMode::MidiTouchReplace => {
            let mut notes = played.iter().filter(|e| is_note(e)).map(|e| e.tick);
            notes.next().map(|f| (f, notes.next_back().unwrap_or(f)))
        }
        _ => None,
    };
    if touch {
        // Each controller lane (status, channel and controller number) is replaced while it is
        // played: from a message until the timeout passes without another.
        let lane = |e: &MidiEvent| {
            let status = e.bytes[0] & 0xf0;
            (e.bytes[0], if status == 0xb0 { e.bytes[1] } else { 0 })
        };
        let timeout_end = |tick: u64| {
            let qn = qn0 + tick as f64 / ppq;
            let t = timeline.time_at_qn(qn) + cc_timeout;
            ((timeline.qn_at(t) - qn0) * ppq).round().max(0.0) as u64
        };
        let mut windows: Vec<((u8, u8), u64, u64)> = Vec::new();
        for e in played.iter().filter(|e| matches!(e.bytes[0] & 0xf0, 0xa0..=0xe0)) {
            let key = lane(e);
            match windows.iter_mut().rev().find(|w| w.0 == key) {
                Some(w) if e.tick <= w.2 => w.2 = timeout_end(e.tick),
                _ => windows.push((key, e.tick, timeout_end(e.tick))),
            }
        }
        src.events.retain(|e| {
            e.tick == src.length_ticks
                || !matches!(e.bytes[0] & 0xf0, 0xa0..=0xe0)
                || !windows.iter().any(|w| w.0 == lane(e) && (w.1..w.2).contains(&e.tick))
        });
    }
    if let Some((from, to)) = range {
        let mut cleared: Vec<(u8, u8)> = Vec::new();
        src.events.retain(|e| {
            let status = e.bytes[0] & 0xf0;
            let key = (e.bytes[0] & 0x0f, e.bytes[1]);
            let off = status == 0x80 || (status == 0x90 && e.bytes[2] == 0);
            if off {
                if let Some(i) = cleared.iter().position(|k| *k == key) {
                    cleared.remove(i);
                    return false;
                }
                return true;
            }
            let inside = (from..to).contains(&e.tick) && e.tick != src.length_ticks;
            if inside && status == 0x90 {
                cleared.push(key);
            }
            let kept_lane = touch && !matches!(status, 0x80 | 0x90);
            !(inside && status != 0xf0 && !kept_lane)
        });
    }
    let end_tick = src.length_ticks.max(end);
    // The end-of-source marker moves with the end.
    for e in src.events.iter_mut().filter(|e| e.tick == src.length_ticks) {
        e.tick = end_tick;
    }
    src.events.extend(played);
    src.events.sort_by_key(|e| (e.tick, e.bytes[0] & 0xf0 == 0x90));
    src.length_ticks = end_tick;
    let new_end = item.end().max(target.end());
    target.length = new_end - target.position;
    if let Some(take) = target.takes.get_mut(target.active_take) {
        take.source = Source::Raw(src.write_chunk(&chunk));
    }
    target.id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(v: f64) -> f64 {
        (v * 1e6).round() / 1e6
    }

    fn segs(len: f64, start: f64, looped: Option<(f64, f64)>, punch: &[(f64, f64)]) -> (Vec<Segment>, usize) {
        let tr = |(a, b): (f64, f64)| TimeRange::new(a, b);
        let punch: Vec<TimeRange> = punch.iter().copied().map(tr).collect();
        record_segments(len, 0.0, RecordPass { start, looped: looped.map(tr), punch: None }, &punch)
    }

    fn shape(s: &[Segment]) -> Vec<(f64, f64, Vec<f64>)> {
        s.iter().map(|s| (r(s.start), r(s.end), s.takes.iter().map(|t| r(t.1)).collect())).collect()
    }

    fn record(
        track: &mut Track,
        len: f64,
        start: f64,
        looped: Option<(f64, f64)>,
        punch: &[(f64, f64)],
        into: RecordInto,
    ) {
        let (s, passes) = segs(len, start, looped, punch);
        let take = |offset: f64| {
            let mut t = Take::from_file("rec.wav".into());
            t.source_offset = offset;
            t
        };
        add_recording(track, &s, passes, &take, into, 1, len);
    }

    fn sine(pos: f64, len: f64) -> Item {
        Item::new(pos, len, Take::from_file("sine.wav".into()))
    }

    type Laid = (f64, f64, Vec<(String, f64)>, usize);

    /// (position, length, [(file, offset)], active) per item, empty takes as "-".
    fn items(t: &Track) -> Vec<Laid> {
        t.items
            .iter()
            .map(|i| {
                let takes = i
                    .takes
                    .iter()
                    .map(|k| match &k.source {
                        Source::Empty => ("-".to_owned(), 0.0),
                        s => (s.file().unwrap().display().to_string(), r(k.source_offset)),
                    })
                    .collect();
                (r(i.position), r(i.length), takes, i.active_take)
            })
            .collect()
    }

    fn rec(o: f64) -> (String, f64) {
        ("rec.wav".into(), o)
    }

    // The cases below are REAPER 7.73 recordings on a dummy JACK device (Lua probes saving the
    // project), with the loop from 1 to 3 s.

    #[test]
    fn loop_passes() {
        assert_eq!(shape(&segs(1.376, 1.0, None, &[]).0), [(1.0, 2.376, vec![0.0])]);
        // From the loop start: every pass a take over the loop, the incomplete last one too.
        let (s, n) = segs(5.216, 1.0, Some((1.0, 3.0)), &[]);
        assert_eq!((shape(&s), n), (vec![(1.0, 3.0, vec![0.0, 2.0, 4.0])], 3));
        assert_eq!(segs(4.288, 1.0, Some((1.0, 3.0)), &[]).1, 3);
        // From before the loop: the lead-in is a segment of its own.
        let (s, n) = segs(4.96, 0.5, Some((1.0, 3.0)), &[]);
        assert_eq!((shape(&s), n), (vec![(0.5, 1.0, vec![0.0]), (1.0, 3.0, vec![0.5, 2.5, 4.5])], 3));
        // From inside the loop: the first pass starts in silence.
        let (s, n) = segs(5.0, 2.0, Some((1.0, 3.0)), &[]);
        assert_eq!((shape(&s), n), (vec![(1.0, 3.0, vec![-1.0, 1.0, 3.0])], 3));
        // A loop never reached.
        assert_eq!(shape(&segs(1.0, 4.0, Some((1.0, 3.0)), &[]).0), [(4.0, 5.0, vec![0.0])]);
        assert!(segs(0.0005, 1.0, None, &[]).0.is_empty());
    }

    #[test]
    fn takes_on_an_empty_track() {
        let mut t = Track::new("t");
        record(&mut t, 4.96, 0.5, Some((1.0, 3.0)), &[], RecordInto::Takes { lane: 0 });
        let e = || ("-".to_owned(), 0.0);
        assert_eq!(
            items(&t),
            [(0.5, 0.5, vec![rec(0.0), e(), e()], 0), (1.0, 2.0, vec![rec(0.5), rec(2.5), rec(4.5)], 2),]
        );
        assert_eq!(t.items[1].takes.iter().filter_map(record_pass).collect::<Vec<_>>(), [1, 2, 3]);
    }

    #[test]
    fn recording_over_an_item_adds_takes() {
        let mut t = Track::new("t");
        t.items.push(sine(0.5, 1.0));
        record(&mut t, 1.461333, 1.0, None, &[], RecordInto::Takes { lane: 0 });
        let s = |o: f64| ("sine.wav".to_owned(), o);
        assert_eq!(
            items(&t),
            [
                (0.5, 0.5, vec![s(0.0)], 0),
                (1.0, 0.5, vec![s(0.5), rec(0.0)], 1),
                (1.5, 0.961333, vec![("-".into(), 0.0), rec(0.5)], 1),
            ]
        );
    }

    #[test]
    fn item_and_time_selection_punch() {
        // Item auto-punch: the selected item gets the take.
        let mut t = Track::new("t");
        t.items.push(sine(1.5, 1.0));
        record(&mut t, 2.4, 1.0, None, &[(1.5, 2.5)], RecordInto::Takes { lane: 0 });
        assert_eq!(items(&t), [(1.5, 1.0, vec![("sine.wav".into(), 0.0), rec(0.5)], 1)]);
        // Time selection auto-punch on a fixed-lane track that adds no lanes: the item is split.
        let mut t = Track::new("t");
        t.fixed_lanes = true;
        let mut it = sine(0.5, 3.0);
        it.ypos = Some((0.0, 1.0));
        t.items.push(it);
        record(&mut t, 2.9, 1.0, None, &[(1.5, 2.5)], RecordInto::Takes { lane: 0 });
        let got: Vec<_> = items(&t).into_iter().map(|i| (i.0, i.1, i.2.len())).collect();
        assert_eq!(got, [(0.5, 1.0, 1), (1.5, 1.0, 2), (2.5, 1.0, 1)]);
        assert_eq!(items(&t)[1].2[1], rec(0.5));
    }

    #[test]
    fn loop_passes_into_new_lanes() {
        let mut t = Track::new("t");
        t.fixed_lanes = true;
        record(&mut t, 5.194667, 1.0, Some((1.0, 3.0)), &[], RecordInto::NewLanes);
        let got: Vec<_> =
            t.items.iter().map(|i| (i.lane(), r(i.position), r(i.length), r(i.takes[0].source_offset))).collect();
        assert_eq!(got, [(0, 1.0, 2.0, 0.0), (1, 1.0, 2.0, 2.0), (2, 1.0, 1.194667, 4.0)]);
        assert!(t.items.iter().all(|i| r(i.ypos.unwrap().1) == r(1.0 / 3.0)));
        assert_eq!(t.lane_names, ["1", "2", "3"]);
        assert_eq!(t.lane_solo.as_ref().unwrap()[..2], [0xFFFF_FFFC, 0xFFFF_FFFF]);
        assert_eq!(t.lane_rec, Some([-1, -1, -1, 2]));
        assert_eq!(t.playing_lanes(), [2]);
    }

    #[test]
    fn new_lanes_auto_comp() {
        let mut t = Track::new("t");
        t.fixed_lanes = true;
        let mut it = sine(0.0, 4.0);
        it.ypos = Some((0.0, 1.0));
        t.items.push(it);
        t.create_comp_lane();
        t.swipe(1, 0.0, 4.0);
        record(&mut t, 1.0, 1.0, None, &[], RecordInto::NewLanes);
        assert_eq!(t.lane_count(), 3);
        let areas: Vec<_> = t.comp_areas.iter().map(|a| (r(a.start), r(a.end), a.lane)).collect();
        assert_eq!(areas, [(0.0, 1.01, 1), (1.0, 2.0, 2), (1.99, 4.0, 1)]);
        assert_eq!(t.playing_lanes(), [0]);
    }

    #[test]
    fn midi_overdub_modes() {
        let tempo = crate::TempoMap::default();
        let tl = tempo.timeline();
        let pass = |start: f64| RecordPass { start, looped: None, punch: None };
        // 120 bpm: a quarter note is half a second. Notes at 0.5 s and 2.5 s in a 4 s item.
        let old =
            [(0.5, [0x90, 60, 100], 3), (0.75, [0x80, 60, 0], 3), (2.5, [0x90, 64, 100], 3), (2.75, [0x80, 64, 0], 3)];
        let played = [(2.25, [0x90, 67, 90], 3), (2.4, [0x80, 67, 0], 3)];
        let keys = |mode: RecordMode| {
            let mut t = Track::new("m");
            t.items.push(place_midi_recording(&old, pass(0.0), 4.0, &tl).unwrap());
            let new = place_midi_recording(&played, pass(2.0), 3.0, &tl).unwrap();
            let id = merge_midi_recording(&mut t, new, mode, &tl, 1.0);
            let Source::Raw(c) = &t.item(id).unwrap().take().unwrap().source else { panic!() };
            let mut keys: Vec<(f64, u8)> =
                MidiSource::parse(c).unwrap().notes().iter().map(|n| (n.start_qn, n.key)).collect();
            keys.sort_by(|a, b| a.0.total_cmp(&b.0));
            (t.items.len(), keys)
        };
        assert_eq!(keys(RecordMode::MidiOverdub), (1, vec![(1.0, 60), (4.5, 67), (5.0, 64)]));
        // Replace clears the recorded range (2 to 3 s), latch from the first note to the end.
        assert_eq!(keys(RecordMode::MidiReplace), (1, vec![(1.0, 60), (4.5, 67)]));
        assert_eq!(keys(RecordMode::MidiLatchReplace), (1, vec![(1.0, 60), (4.5, 67)]));
        // Touch-replace only while playing: the note after it stays.
        assert_eq!(keys(RecordMode::MidiTouchReplace), (1, vec![(1.0, 60), (4.5, 67), (5.0, 64)]));
        assert_eq!(keys(RecordMode::Input).0, 2);
    }

    #[test]
    fn touch_replace_holds_a_controller_until_its_timeout() {
        let tempo = crate::TempoMap::default();
        let tl = tempo.timeline();
        let pass = |start: f64| RecordPass { start, looped: None, punch: None };
        let old = [
            (0.5, [0x90, 60, 100], 3),
            (0.75, [0x80, 60, 0], 3),
            (1.0, [0xb0, 1, 10], 3),
            (2.2, [0xb0, 7, 90], 3),
            (2.5, [0xb0, 1, 20], 3),
            (3.5, [0xb0, 1, 30], 3),
        ];
        let played = [(2.0, [0xb0, 1, 64], 3), (2.3, [0xb0, 1, 70], 3)];
        let mut t = Track::new("m");
        t.items.push(place_midi_recording(&old, pass(0.0), 4.0, &tl).unwrap());
        let new = place_midi_recording(&played, pass(2.0), 3.0, &tl).unwrap();
        let id = merge_midi_recording(&mut t, new, RecordMode::MidiTouchReplace, &tl, 1.0);
        let Source::Raw(c) = &t.item(id).unwrap().take().unwrap().source else { panic!() };
        let src = MidiSource::parse(c).unwrap();
        let ppq = src.ticks_per_qn as f64;
        let ccs: Vec<(f64, u8, u8)> = src
            .events
            .iter()
            .filter(|e| e.bytes[0] == 0xb0 && e.tick != src.length_ticks)
            .map(|e| (e.tick as f64 / ppq / 2.0, e.bytes[1], e.bytes[2]))
            .collect();
        // CC1 is replaced from 2.0 s until a second after its last message (3.3 s): its 2.5 s
        // value goes, the 3.5 s one stays, and CC7 and the note are left alone.
        assert_eq!(ccs, [(1.0, 1, 10), (2.0, 1, 64), (2.2, 7, 90), (2.3, 1, 70), (3.5, 1, 30)]);
        assert_eq!(src.notes().len(), 1);
    }

    #[test]
    fn midi_recording() {
        let tempo = crate::TempoMap::default();
        let tl = tempo.timeline();
        let pass = |looped: Option<(f64, f64)>, punch: Option<(f64, f64)>| RecordPass {
            start: 2.0,
            looped: looped.map(|(a, b)| TimeRange::new(a, b)),
            punch: punch.map(|(a, b)| TimeRange::new(a, b)),
        };
        // 120 bpm: a quarter note is half a second.
        let events = [
            (2.5, [0x90, 60, 100], 3),
            (2.75, [0x80, 64, 0], 3),
            (3.0, [0xb0, 1, 50], 3),
            (3.5, [0x90, 60, 0], 3),
            (3.75, [0x90, 62, 90], 3),
        ];
        let it = place_midi_recording(&events, pass(None, None), 4.0, &tl).unwrap();
        assert_eq!((it.position, it.length), (2.0, 2.0));
        let Source::Raw(c) = &it.take().unwrap().source else { panic!() };
        let src = MidiSource::parse(c).unwrap();
        assert_eq!(src.length_qn(), 4.0);
        let notes = src.notes();
        assert_eq!(notes.len(), 2);
        assert_eq!((notes[0].start_qn, notes[0].end_qn, notes[0].key), (1.0, 3.0, 60));
        assert_eq!((notes[1].start_qn, notes[1].end_qn, notes[1].key), (3.5, 4.0, 62));
        assert!(!src.events.iter().any(|e| e.bytes[1] == 64), "orphan note-off dropped");
        assert!(place_midi_recording(&[], pass(None, None), 4.0, &tl).is_none());

        // Wrapped around a loop from 1 to 3 s: the item spans the loop, events stay in place.
        let looped = [(2.5, [0x90, 60, 100], 3), (1.5, [0x80, 60, 0], 3)];
        let it = place_midi_recording(&looped, pass(Some((1.0, 3.0)), None), 1.75, &tl).unwrap();
        assert_eq!((it.position, it.length), (1.0, 2.0));

        // Punch from 3 to 3.6 s keeps only what was played there.
        let it = place_midi_recording(&events, pass(None, Some((3.0, 3.6))), 4.0, &tl).unwrap();
        assert!((it.position - 3.0).abs() < 1e-9 && (it.length - 0.6).abs() < 1e-9);
        let Source::Raw(c) = &it.take().unwrap().source else { panic!() };
        let src = MidiSource::parse(c).unwrap();
        assert!(src.notes().is_empty());
        assert_eq!(src.events[0].bytes, [0xb0, 1, 50]);
    }
}
