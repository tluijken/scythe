//! Editing MIDI items: notes, controller lanes with REAPER 7's CC shapes, system exclusive and
//! text events, quantize/humanize/transpose and the other MIDI editor commands, grooves, and
//! the editor settings and note names REAPER keeps with each take and track. Times are in
//! quarter notes from the start of the take's source.

use schemars::JsonSchema;
use scythe_core::ItemId;
use scythe_core::midi::actions;
use scythe_core::midi::edit::{self, EditNote, Grid, Humanize, Quantize, QuantizeWhat};
use scythe_core::midi::groove::{self, Groove};
use scythe_core::midi::lanes::LanePoint;
use scythe_core::midi::names::NoteNames;
use scythe_core::midi::scale::{self, KeySnap};
use scythe_core::midi::view::{EditorMode, EditorView, LaneKind};
use scythe_core::midi::{CcShape, LongEvent, MidiSource, ShapeKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ApiError, TrackRef, json};
use crate::Session;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum MidiEditCommand {
    /// The notes of a MIDI item's active take, in start order, with their numbers for the other
    /// note commands.
    #[serde(rename = "midi.notes.list")]
    Notes {
        /// The item's id.
        item: u64,
    },
    /// Adds notes and answers with all notes.
    #[serde(rename = "midi.notes.add")]
    AddNotes {
        /// The item's id.
        item: u64,
        notes: Vec<NoteSpec>,
    },
    /// Changes notes; fields left out stay. Answers with all notes.
    #[serde(rename = "midi.notes.set")]
    SetNotes {
        /// The item's id.
        item: u64,
        /// The notes' numbers from `midi.notes.list`.
        indexes: Vec<usize>,
        /// New start in quarter notes (the length stays).
        #[serde(default)]
        start: Option<f64>,
        /// New end in quarter notes.
        #[serde(default)]
        end: Option<f64>,
        /// MIDI note number, 0 to 127.
        #[serde(default)]
        key: Option<u8>,
        /// 1 to 127.
        #[serde(default)]
        velocity: Option<u8>,
        /// MIDI channel, 1 to 16.
        #[serde(default)]
        channel: Option<u8>,
        #[serde(default)]
        muted: Option<bool>,
        #[serde(default)]
        selected: Option<bool>,
    },
    /// Deletes notes and answers with the rest.
    #[serde(rename = "midi.notes.delete")]
    DeleteNotes {
        /// The item's id.
        item: u64,
        /// The notes' numbers from `midi.notes.list`.
        indexes: Vec<usize>,
    },
    /// Selects exactly these notes (none when empty). The editing commands work on the
    /// selected notes, or on all notes when none is selected.
    #[serde(rename = "midi.notes.select")]
    SelectNotes {
        /// The item's id.
        item: u64,
        indexes: Vec<usize>,
    },
    /// Quantizes the selected notes (all when none is selected) as REAPER's quantize dialog.
    #[serde(rename = "midi.quantize")]
    Quantize {
        /// The item's id.
        item: u64,
        /// Grid in quarter notes: 0.25 for 1/16, 1/3 for 1/8 triplets. The editor's grid of
        /// the take when left out.
        #[serde(default)]
        grid: Option<f64>,
        /// Swing, -1 to 1: every second grid line moves by half of it times the grid. The
        /// take's swing when left out.
        #[serde(default)]
        swing: Option<f64>,
        /// 0 to 100 percent (100 when left out).
        #[serde(default)]
        strength: Option<f64>,
        /// `position` (default; keeps lengths), `position_and_end`, `end` or `length`.
        #[serde(default)]
        what: Option<QuantizeTarget>,
        /// Only notes at least this far from a grid line, percent of the grid (0 when left
        /// out).
        #[serde(default)]
        window_min: Option<f64>,
        /// Only notes at most this far from a grid line, percent of the grid (100 when left
        /// out).
        #[serde(default)]
        window_max: Option<f64>,
    },
    /// Moves the selected notes (all when none) by random amounts.
    #[serde(rename = "midi.humanize")]
    Humanize {
        /// The item's id.
        item: u64,
        /// Largest shift of a start, percent of the grid.
        #[serde(default)]
        timing: Option<f64>,
        /// Largest velocity change, percent of the full range.
        #[serde(default)]
        velocity: Option<f64>,
        /// Grid in quarter notes; the take's grid when left out.
        #[serde(default)]
        grid: Option<f64>,
        /// Seed for repeatable results.
        #[serde(default)]
        seed: Option<u64>,
    },
    /// Transposes the selected notes (all when none).
    #[serde(rename = "midi.transpose")]
    Transpose {
        /// The item's id.
        item: u64,
        /// Semitones, or scale steps with `scale`.
        semitones: i32,
        /// Move in scale steps of this scale (`midi.scales` lists them).
        #[serde(default)]
        scale: Option<String>,
        /// The scale's root, 0 (C) to 11.
        #[serde(default)]
        root: Option<u8>,
    },
    /// One of the MIDI editor's note commands on the selected notes (all when none).
    #[serde(rename = "midi.edit")]
    Edit {
        /// The item's id.
        item: u64,
        op: NoteOp,
        /// `length`, `lengthen` and `move`: quarter notes; `velocity`: the value (or change
        /// with `relative`); `channel`: 1-16; `split`: quarter notes from the source start;
        /// `duplicate`: semitones (12 when left out).
        #[serde(default)]
        value: Option<f64>,
        /// `velocity`: add `value` instead of setting it.
        #[serde(default)]
        relative: Option<bool>,
        /// `split_grid`: the grid in quarter notes (the take's grid when left out);
        /// `force_key`: the scale root 0-11.
        #[serde(default)]
        grid: Option<f64>,
        /// `force_key`: the scale.
        #[serde(default)]
        scale: Option<String>,
    },
    /// Runs a MIDI editor action on the item, by its command id in REAPER's MIDI editor section
    /// (32060): 40469 quantizes note positions to the take's grid, 40765 makes notes legato,
    /// 42471 toggles key snap and so on (`midi.actions` lists them).
    #[serde(rename = "midi.action")]
    Action {
        /// The item's id.
        item: u64,
        command: u32,
        /// Key snap's scale for the actions that follow the key.
        #[serde(default)]
        scale: Option<String>,
        /// Key snap's root, 0 (C) to 11.
        #[serde(default)]
        root: Option<u8>,
    },
    /// The MIDI editor actions `midi.action` runs, with REAPER's names.
    #[serde(rename = "midi.actions")]
    Actions,
    /// The scales key snap and `midi.transpose` know: Scythe's own and those of `.reascale`
    /// files given by path.
    #[serde(rename = "midi.scales")]
    Scales {
        #[serde(default)]
        files: Vec<String>,
    },
    /// The events of a controller lane, with their numbers for `midi.cc.delete`.
    #[serde(rename = "midi.cc.list")]
    CcList {
        /// The item's id.
        item: u64,
        /// `cc1`…`cc127`, `cc14:1` (14-bit MSB 1 with LSB 33), `pitch`, `program`,
        /// `pressure` (channel aftertouch) or `poly` (poly aftertouch). Every lane that holds
        /// events is listed when left out.
        #[serde(default)]
        lane: Option<String>,
    },
    /// Sets lane values: replaces the lane's events between `from` and `to` (or the points'
    /// span) by `points`.
    #[serde(rename = "midi.cc.set")]
    CcSet {
        /// The item's id.
        item: u64,
        lane: String,
        points: Vec<CcSpec>,
        /// Quarter notes; the first point when left out.
        #[serde(default)]
        from: Option<f64>,
        /// Quarter notes; the last point when left out.
        #[serde(default)]
        to: Option<f64>,
    },
    /// Draws a line or curve: two events at `from` and `to` joined by a CC shape (as REAPER 7
    /// does), replacing the lane's events in between.
    #[serde(rename = "midi.cc.line")]
    CcLine {
        /// The item's id.
        item: u64,
        lane: String,
        from: f64,
        to: f64,
        start_value: u16,
        end_value: u16,
        /// `linear` (default), `slow_start_end`, `fast_start`, `fast_end`, `bezier` or `square`.
        #[serde(default)]
        shape: Option<String>,
        /// Bezier tension, -1 to 1.
        #[serde(default)]
        tension: Option<f64>,
        /// MIDI channel 1-16 (1 when left out).
        #[serde(default)]
        channel: Option<u8>,
    },
    /// Sets the CC shape of lane events (by their numbers from `midi.cc.list`).
    #[serde(rename = "midi.cc.shape")]
    CcShape {
        /// The item's id.
        item: u64,
        lane: String,
        indexes: Vec<usize>,
        shape: String,
        #[serde(default)]
        tension: Option<f64>,
    },
    /// Deletes lane events by their numbers from `midi.cc.list`.
    #[serde(rename = "midi.cc.delete")]
    CcDelete {
        /// The item's id.
        item: u64,
        lane: String,
        indexes: Vec<usize>,
    },
    /// The take's system exclusive, text and notation events.
    #[serde(rename = "midi.sysex.list")]
    SysexList {
        /// The item's id.
        item: u64,
    },
    /// Adds a system exclusive message (`hex`, with or without F0/F7) or a text event (`text`
    /// with its meta `kind`: 1 text, 3 track name, 5 lyric, 6 marker, 7 cue point).
    #[serde(rename = "midi.sysex.add")]
    SysexAdd {
        /// The item's id.
        item: u64,
        at: f64,
        #[serde(default)]
        hex: Option<String>,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        kind: Option<u8>,
    },
    /// Deletes system exclusive and text events by their numbers from `midi.sysex.list`.
    #[serde(rename = "midi.sysex.delete")]
    SysexDelete {
        /// The item's id.
        item: u64,
        indexes: Vec<usize>,
    },
    /// Moves notes (`target` `notes`, of the item) or item positions (`items`) towards a groove
    /// template (an `.rgt` file).
    #[serde(rename = "midi.groove")]
    Groove {
        /// The items: one MIDI item for `notes`, any items for `items`.
        items: Vec<u64>,
        /// Path of the `.rgt` file.
        #[serde(default)]
        file: Option<String>,
        /// The groove file's text, instead of `file`.
        #[serde(default)]
        groove: Option<String>,
        /// 0 to 100 percent (100 when left out).
        #[serde(default)]
        strength: Option<f64>,
        /// Notes: how far velocities follow the groove's levels, 0 to 100 percent (0 when left
        /// out).
        #[serde(default)]
        velocity_strength: Option<f64>,
        /// `notes` (default) or `items`.
        #[serde(default)]
        target: Option<String>,
    },
    /// Makes a groove template from the item's notes (`beats` long) and answers with its
    /// `.rgt` text; saves it when `file` is given.
    #[serde(rename = "midi.groove.extract")]
    GrooveExtract {
        /// The item's id.
        item: u64,
        /// The pattern's length in quarter notes (4 when left out).
        #[serde(default)]
        beats: Option<f64>,
        #[serde(default)]
        file: Option<String>,
    },
    /// The MIDI editor settings REAPER keeps with the take.
    #[serde(rename = "midi.editor.get")]
    EditorGet {
        /// The item's id.
        item: u64,
    },
    /// Changes the take's MIDI editor settings; fields left out stay.
    #[serde(rename = "midi.editor.set")]
    EditorSet {
        /// The item's id.
        item: u64,
        /// `piano_roll`, `named_notes` (drum map) or `event_list`.
        #[serde(default)]
        mode: Option<String>,
        /// Grid in quarter notes.
        #[serde(default)]
        grid: Option<f64>,
        /// Swing -1 to 1; 0 for a straight grid.
        #[serde(default)]
        swing: Option<f64>,
        #[serde(default)]
        key_snap: Option<bool>,
        /// Channel for new events, 1-16.
        #[serde(default)]
        channel: Option<u8>,
        /// Channels shown (1-16); empty shows all.
        #[serde(default)]
        show_channels: Option<Vec<u8>>,
        /// The controller lanes, by the names `midi.cc.list` takes plus `velocity`,
        /// `off_velocity`, `sysex` and `text`.
        #[serde(default)]
        lanes: Option<Vec<String>>,
    },
    /// The track's note names (as REAPER keeps them in `MIDINOTENAMES`).
    #[serde(rename = "midi.note_names.get")]
    NoteNamesGet { track: TrackRef },
    /// Names notes of a track, or loads a note name file (`60 Kick`, `CC7 Volume`); an empty
    /// name clears one.
    #[serde(rename = "midi.note_names.set")]
    NoteNamesSet {
        track: TrackRef,
        #[serde(default)]
        names: Vec<NoteName>,
        /// A note name file to load (replacing the track's names).
        #[serde(default)]
        file: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NoteSpec {
    /// Quarter notes from the source start.
    pub start: f64,
    /// Quarter notes; `start + length` when left out.
    #[serde(default)]
    pub end: Option<f64>,
    /// Quarter notes (the editor's grid when neither `end` nor `length` is given).
    #[serde(default)]
    pub length: Option<f64>,
    pub key: u8,
    /// 1 to 127 (100 when left out).
    #[serde(default)]
    pub velocity: Option<u8>,
    /// 1 to 16 (the editor's channel for new events when left out).
    #[serde(default)]
    pub channel: Option<u8>,
    #[serde(default)]
    pub muted: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CcSpec {
    /// Quarter notes from the source start.
    pub at: f64,
    /// 0-127, or 0-16383 for pitch bend and 14-bit lanes.
    pub value: u16,
    /// 1 to 16 (1 when left out).
    #[serde(default)]
    pub channel: Option<u8>,
    /// `poly` lane: the note number.
    #[serde(default)]
    pub key: Option<u8>,
    /// CC shape towards the next point.
    #[serde(default)]
    pub shape: Option<String>,
    #[serde(default)]
    pub tension: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NoteName {
    pub key: u8,
    pub name: String,
    /// 1 to 16; all channels when left out.
    #[serde(default)]
    pub channel: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuantizeTarget {
    Position,
    PositionAndEnd,
    End,
    Length,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NoteOp {
    /// Ends each note where the next begins.
    Legato,
    /// Splits notes at every grid line.
    SplitGrid,
    /// Splits notes at `value`.
    Split,
    /// Joins notes of the same key into one.
    Join,
    /// Sets lengths to `value` quarter notes.
    Length,
    /// Lengthens (negative: shortens) by `value` quarter notes.
    Lengthen,
    /// Moves by `value` quarter notes.
    Move,
    Velocity,
    Channel,
    Mute,
    Unmute,
    ToggleMute,
    /// Copies the notes `value` semitones away (an octave up when left out).
    Duplicate,
    /// Moves notes onto the nearest key of `scale` with root `grid`.
    ForceKey,
    SelectAll,
    SelectNone,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NoteInfo {
    pub index: usize,
    pub start: f64,
    pub end: f64,
    pub key: u8,
    pub velocity: u8,
    /// 1 to 16.
    pub channel: u8,
    pub selected: bool,
    pub muted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CcInfo {
    pub index: usize,
    pub lane: String,
    pub at: f64,
    pub value: u16,
    pub channel: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<u8>,
    pub shape: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tension: Option<f64>,
    pub selected: bool,
    pub muted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SysexInfo {
    pub index: usize,
    pub at: f64,
    /// `sysex`, `text`, `notation` or `meta`.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The bytes in hex.
    pub hex: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EditorInfo {
    pub mode: String,
    pub grid: f64,
    pub swing: Option<f64>,
    pub key_snap: bool,
    pub channel: u8,
    pub show_channels: Vec<u8>,
    pub lanes: Vec<String>,
}

pub fn lane_name(kind: LaneKind) -> String {
    match kind {
        LaneKind::Velocity => "velocity".into(),
        LaneKind::OffVelocity => "off_velocity".into(),
        LaneKind::Cc(n) => format!("cc{n}"),
        LaneKind::Cc14(n) => format!("cc14:{n}"),
        LaneKind::PitchBend => "pitch".into(),
        LaneKind::Program => "program".into(),
        LaneKind::ChannelPressure => "pressure".into(),
        LaneKind::BankProgram => "bank_program".into(),
        LaneKind::Text => "text".into(),
        LaneKind::Sysex => "sysex".into(),
        LaneKind::PolyAftertouch => "poly".into(),
        LaneKind::Notation => "notation".into(),
    }
}

pub fn parse_lane(name: &str) -> Result<LaneKind, ApiError> {
    let bad = || {
        ApiError::invalid(format!(
            "unknown lane \"{name}\"; use cc0-cc127, cc14:0-cc14:31, pitch, program, pressure or poly"
        ))
    };
    let name = name.trim().to_ascii_lowercase();
    Ok(match name.as_str() {
        "velocity" => LaneKind::Velocity,
        "off_velocity" => LaneKind::OffVelocity,
        "pitch" | "pitchbend" | "pitch_bend" => LaneKind::PitchBend,
        "program" => LaneKind::Program,
        "pressure" | "channel_pressure" | "aftertouch" => LaneKind::ChannelPressure,
        "poly" | "poly_aftertouch" => LaneKind::PolyAftertouch,
        "bank_program" => LaneKind::BankProgram,
        "text" => LaneKind::Text,
        "sysex" => LaneKind::Sysex,
        "notation" => LaneKind::Notation,
        _ => {
            if let Some(n) = name.strip_prefix("cc14:") {
                let n: u8 = n.parse().map_err(|_| bad())?;
                if n > 31 {
                    return Err(bad());
                }
                LaneKind::Cc14(n)
            } else if let Some(n) = name.strip_prefix("cc") {
                let n: u8 = n.parse().map_err(|_| bad())?;
                if n > 127 {
                    return Err(bad());
                }
                LaneKind::Cc(n)
            } else {
                return Err(bad());
            }
        }
    })
}

fn value_lane(name: &str) -> Result<LaneKind, ApiError> {
    let kind = parse_lane(name)?;
    if !kind.has_values() {
        return Err(ApiError::invalid(format!("\"{name}\" is not a lane of controller values")));
    }
    Ok(kind)
}

fn shape(name: &str, tension: Option<f64>) -> Result<Option<CcShape>, ApiError> {
    let kind = ShapeKind::from_name(name).ok_or_else(|| {
        ApiError::invalid(format!(
            "unknown shape \"{name}\"; use square, linear, slow_start_end, fast_start, fast_end or bezier"
        ))
    })?;
    Ok((kind != ShapeKind::Square || tension.is_some()).then(|| CcShape::new(kind, tension.unwrap_or(0.0))))
}

fn channel(ch: Option<u8>) -> Result<u8, ApiError> {
    match ch {
        None => Ok(0),
        Some(c @ 1..=16) => Ok(c - 1),
        Some(_) => Err(ApiError::invalid("channels run from 1 to 16")),
    }
}

fn qn(value: f64, what: &str) -> Result<f64, ApiError> {
    if !value.is_finite() || value < 0.0 {
        return Err(ApiError::invalid(format!("{what} must be 0 quarter notes or more")));
    }
    Ok(value)
}

fn percent(value: Option<f64>, default: f64) -> Result<f64, ApiError> {
    let v = value.unwrap_or(default);
    if !(0.0..=100.0).contains(&v) {
        return Err(ApiError::invalid("percentages run from 0 to 100"));
    }
    Ok(v / 100.0)
}

fn source(session: &Session, item: u64) -> Result<(MidiSource, EditorView), ApiError> {
    let id = ItemId(item);
    if session.project.find_item(id).is_none() {
        return Err(ApiError::not_found(format!("no item {item}")));
    }
    let chunk =
        session.project.midi_chunk(id).ok_or_else(|| ApiError::invalid(format!("item {item} is not a MIDI item")))?;
    let src = MidiSource::parse(chunk).ok_or_else(|| ApiError::failed("the MIDI source could not be read"))?;
    Ok((src, EditorView::read(chunk)))
}

fn notes_info(session: &Session, item: u64) -> Result<Value, ApiError> {
    let (src, _) = source(session, item)?;
    let names =
        session.project.find_item(ItemId(item)).and_then(|(t, _)| session.project.track(t)).map(|t| t.note_names());
    let ppq = src.ticks_per_qn as f64;
    let notes: Vec<NoteInfo> = src
        .edit_notes()
        .iter()
        .enumerate()
        .map(|(index, n)| NoteInfo {
            index,
            start: n.start as f64 / ppq,
            end: n.end as f64 / ppq,
            key: n.key,
            velocity: n.velocity,
            channel: n.channel + 1,
            selected: n.selected,
            muted: n.muted,
            name: names.as_ref().and_then(|m| m.note(n.channel, n.key)).map(str::to_owned),
            notation: n.notation.clone(),
        })
        .collect();
    Ok(json(notes))
}

/// Runs `f` on the take's notes as one undoable edit and answers with the notes.
fn edit_notes(
    session: &mut Session,
    item: u64,
    label: &str,
    f: impl FnOnce(&mut Vec<EditNote>, &MidiSource, &EditorView) -> Result<(), ApiError>,
) -> Result<Value, ApiError> {
    let (src, view) = source(session, item)?;
    let mut notes = src.edit_notes();
    f(&mut notes, &src, &view)?;
    session.edit(label, |p| {
        p.edit_midi(ItemId(item), |s| s.with_edit_notes(&notes));
    });
    notes_info(session, item)
}

fn check_indexes(indexes: &[usize], len: usize) -> Result<(), ApiError> {
    match indexes.iter().find(|&&i| i >= len) {
        Some(i) => Err(ApiError::not_found(format!("no note {i}; the take has {len}"))),
        None => Ok(()),
    }
}

fn key_snap(scale_name: Option<&str>, root: Option<u8>) -> Result<Option<KeySnap>, ApiError> {
    let Some(name) = scale_name else { return Ok(None) };
    let scale = scale::builtin()
        .into_iter()
        .find(|s| s.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| ApiError::invalid(format!("unknown scale \"{name}\"; midi.scales lists them")))?;
    Ok(Some(KeySnap { root: root.unwrap_or(0) % 12, scale }))
}

fn cc_info(src: &MidiSource, kind: LaneKind) -> Vec<CcInfo> {
    let ppq = src.ticks_per_qn as f64;
    src.lane(kind)
        .iter()
        .enumerate()
        .map(|(index, p)| CcInfo {
            index,
            lane: lane_name(kind),
            at: p.tick as f64 / ppq,
            value: p.value,
            channel: p.channel + 1,
            key: (kind == LaneKind::PolyAftertouch).then_some(p.key),
            shape: p.shape.map_or(ShapeKind::Square, |s| s.kind).name().into(),
            tension: p.shape.filter(|s| s.kind == ShapeKind::Bezier).map(|s| s.tension),
            selected: p.selected,
            muted: p.muted,
        })
        .collect()
}

fn edit_lane(
    session: &mut Session,
    item: u64,
    kind: LaneKind,
    label: &str,
    f: impl FnOnce(&mut Vec<LanePoint>, u32) -> Result<(), ApiError>,
) -> Result<Value, ApiError> {
    let (src, _) = source(session, item)?;
    let mut points = src.lane(kind);
    f(&mut points, src.ticks_per_qn)?;
    points.sort_by_key(|p| p.tick);
    session.edit(label, |p| {
        p.edit_midi(ItemId(item), |s| s.with_lane(kind, &points));
    });
    let (src, _) = source(session, item)?;
    Ok(json(cc_info(&src, kind)))
}

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
}

fn sysex_info(src: &MidiSource) -> Vec<SysexInfo> {
    let ppq = src.ticks_per_qn as f64;
    src.long
        .iter()
        .enumerate()
        .map(|(index, x)| {
            let (kind, text) = match x.data.as_slice() {
                [0xf0, ..] => ("sysex", None),
                [0xff, 0x0f, rest @ ..] => ("notation", Some(String::from_utf8_lossy(rest).into_owned())),
                [0xff, 1..=7, rest @ ..] => ("text", Some(String::from_utf8_lossy(rest).into_owned())),
                _ => ("meta", None),
            };
            SysexInfo { index, at: x.tick as f64 / ppq, kind: kind.into(), text, hex: hex(&x.data) }
        })
        .collect()
}

fn editor_info(v: &EditorView) -> EditorInfo {
    EditorInfo {
        mode: match v.mode {
            EditorMode::PianoRoll => "piano_roll",
            EditorMode::NamedNotes => "named_notes",
            EditorMode::EventList => "event_list",
        }
        .into(),
        grid: v.grid_qn,
        swing: v.swing,
        key_snap: v.key_snap,
        channel: v.channel + 1,
        show_channels: (0..16).filter(|c| v.channel_filter & (1 << c) != 0).map(|c| c + 1).collect(),
        lanes: v.lanes.iter().map(|(k, _)| lane_name(*k)).collect(),
    }
}

fn read_file(path: &str) -> Result<String, ApiError> {
    std::fs::read_to_string(path).map_err(|e| ApiError::failed(format!("could not read {path}: {e}")))
}

pub fn execute(session: &mut Session, command: MidiEditCommand) -> Result<Value, ApiError> {
    use MidiEditCommand as C;
    match command {
        C::Notes { item } => notes_info(session, item),
        C::AddNotes { item, notes } => edit_notes(session, item, "Insert notes", |all, src, view| {
            let ppq = src.ticks_per_qn as f64;
            for n in &notes {
                let start = qn(n.start, "start")?;
                let end = match (n.end, n.length) {
                    (Some(e), _) => qn(e, "end")?,
                    (None, Some(l)) => start + qn(l, "length")?,
                    (None, None) => start + view.grid_qn,
                };
                if end <= start {
                    return Err(ApiError::invalid("a note must end after it starts"));
                }
                if n.key > 127 {
                    return Err(ApiError::invalid("note numbers run from 0 to 127"));
                }
                let ch = match n.channel {
                    Some(_) => channel(n.channel)?,
                    None => view.channel,
                };
                let mut note = EditNote::new(
                    (start * ppq).round() as u64,
                    (end * ppq).round() as u64,
                    ch,
                    n.key,
                    n.velocity.unwrap_or(100),
                );
                note.muted = n.muted.unwrap_or(false);
                all.push(note);
            }
            all.sort_by_key(|n| (n.start, n.key, n.channel));
            Ok(())
        }),
        C::SetNotes { item, indexes, start, end, key, velocity, channel: ch, muted, selected } => {
            edit_notes(session, item, "Edit notes", |all, src, _| {
                check_indexes(&indexes, all.len())?;
                let ppq = src.ticks_per_qn as f64;
                let ch = ch.map(|c| channel(Some(c))).transpose()?;
                for &i in &indexes {
                    let n = &mut all[i];
                    if let Some(s) = start {
                        let len = n.length();
                        n.start = (qn(s, "start")? * ppq).round() as u64;
                        n.end = n.start + len;
                    }
                    if let Some(e) = end {
                        let e = (qn(e, "end")? * ppq).round() as u64;
                        if e <= n.start {
                            return Err(ApiError::invalid("a note must end after it starts"));
                        }
                        n.end = e;
                    }
                    if let Some(k) = key {
                        n.key = k.min(127);
                    }
                    if let Some(v) = velocity {
                        n.velocity = v.clamp(1, 127);
                    }
                    if let Some(c) = ch {
                        n.channel = c;
                    }
                    if let Some(m) = muted {
                        n.muted = m;
                    }
                    if let Some(s) = selected {
                        n.selected = s;
                    }
                }
                Ok(())
            })
        }
        C::DeleteNotes { item, indexes } => edit_notes(session, item, "Delete notes", |all, _, _| {
            check_indexes(&indexes, all.len())?;
            let mut i = 0;
            all.retain(|_| {
                i += 1;
                !indexes.contains(&(i - 1))
            });
            Ok(())
        }),
        C::SelectNotes { item, indexes } => edit_notes(session, item, "Select notes", |all, _, _| {
            check_indexes(&indexes, all.len())?;
            for (i, n) in all.iter_mut().enumerate() {
                n.selected = indexes.contains(&i);
            }
            Ok(())
        }),
        C::Quantize { item, grid, swing, strength, what, window_min, window_max } => {
            edit_notes(session, item, "Quantize notes", |all, src, view| {
                let grid = grid.unwrap_or(view.grid_qn);
                if !(grid > 0.0 && grid.is_finite()) {
                    return Err(ApiError::invalid("the grid must be more than 0 quarter notes"));
                }
                let swing = swing.or(view.swing).unwrap_or(0.0);
                if !(-1.0..=1.0).contains(&swing) {
                    return Err(ApiError::invalid("swing runs from -1 to 1"));
                }
                let q = Quantize {
                    grid: Grid { qn: grid, swing },
                    strength: percent(strength, 100.0)?,
                    what: match what.unwrap_or(QuantizeTarget::Position) {
                        QuantizeTarget::Position => QuantizeWhat::Position,
                        QuantizeTarget::PositionAndEnd => QuantizeWhat::PositionAndEnd,
                        QuantizeTarget::End => QuantizeWhat::End,
                        QuantizeTarget::Length => QuantizeWhat::Length,
                    },
                    window: (percent(window_min, 0.0)?, percent(window_max, 100.0)?),
                };
                edit::quantize(all, src.ticks_per_qn, &q);
                Ok(())
            })
        }
        C::Humanize { item, timing, velocity, grid, seed } => {
            edit_notes(session, item, "Humanize notes", |all, src, view| {
                let h = Humanize {
                    timing: percent(timing, 0.0)?,
                    grid_qn: grid.unwrap_or(view.grid_qn),
                    velocity: percent(velocity, 0.0)?,
                    seed: seed.unwrap_or_else(|| {
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(1, |d| d.as_nanos() as u64)
                    }),
                };
                edit::humanize(all, src.ticks_per_qn, &h);
                Ok(())
            })
        }
        C::Transpose { item, semitones, scale, root } => {
            let snap = key_snap(scale.as_deref(), root)?;
            edit_notes(session, item, "Transpose notes", |all, _, _| {
                edit::transpose(all, semitones, snap.as_ref());
                Ok(())
            })
        }
        C::Edit { item, op, value, relative, grid, scale } => {
            let snap = key_snap(scale.as_deref(), grid.map(|g| g as u8))?;
            edit_notes(session, item, "Edit notes", |all, src, view| {
                let ppq = src.ticks_per_qn;
                let ticks = |v: f64| (v * ppq as f64).round() as i64;
                let need = || value.ok_or_else(|| ApiError::invalid("this edit needs a value"));
                match op {
                    NoteOp::Legato => edit::legato(all),
                    NoteOp::SplitGrid => {
                        let g = grid.unwrap_or(view.grid_qn);
                        edit::split_on_grid(all, ppq, Grid { qn: g, swing: view.swing.unwrap_or(0.0) });
                    }
                    NoteOp::Split => edit::split_at(all, ticks(qn(need()?, "value")?) as u64),
                    NoteOp::Join => edit::join(all),
                    NoteOp::Length => edit::set_length(all, ticks(qn(need()?, "value")?).max(1) as u64),
                    NoteOp::Lengthen => edit::lengthen(all, ticks(need()?)),
                    NoteOp::Move => edit::shift(all, ticks(need()?)),
                    NoteOp::Velocity => edit::velocity(all, need()?.round() as i32, !relative.unwrap_or(false)),
                    NoteOp::Channel => edit::set_channel(all, channel(Some(need()? as u8))?),
                    NoteOp::Mute => edit::mute(all, Some(true)),
                    NoteOp::Unmute => edit::mute(all, Some(false)),
                    NoteOp::ToggleMute => edit::mute(all, None),
                    NoteOp::Duplicate => edit::duplicate_transposed(all, value.unwrap_or(12.0).round() as i32),
                    NoteOp::ForceKey => {
                        let snap = snap.as_ref().ok_or_else(|| ApiError::invalid("force_key needs a scale"))?;
                        edit::force_into_key(all, snap);
                    }
                    NoteOp::SelectAll => all.iter_mut().for_each(|n| n.selected = true),
                    NoteOp::SelectNone => all.iter_mut().for_each(|n| n.selected = false),
                }
                Ok(())
            })
        }
        C::Action { item, command, scale, root } => {
            let (src, view) = source(session, item)?;
            let ctx = actions::Context { key: key_snap(scale.as_deref(), root)? };
            let (new_src, new_view) = actions::run(command, &src, &view, &ctx)
                .ok_or_else(|| ApiError::invalid(format!("Scythe has no MIDI editor action {command}")))?;
            let label = actions::name(command).unwrap_or_else(|| "MIDI editor action".into());
            session.edit(&label, |p| {
                p.edit_midi_chunk(ItemId(item), |c| {
                    *c = new_src.write_chunk(c);
                    new_view.write(c);
                });
            });
            notes_info(session, item)
        }
        C::Actions => Ok(json(
            actions::ids()
                .into_iter()
                .filter_map(|id| actions::name(id).map(|n| serde_json::json!({"command": id, "name": n})))
                .collect::<Vec<_>>(),
        )),
        C::Scales { files } => {
            let mut all = scale::builtin();
            for f in &files {
                all.extend(scale::parse_reascale(&read_file(f)?));
            }
            Ok(json(all.iter().map(|s| serde_json::json!({"name": s.name, "pattern": s.pattern})).collect::<Vec<_>>()))
        }
        C::CcList { item, lane } => {
            let (src, _) = source(session, item)?;
            let lanes = match lane {
                Some(l) => vec![value_lane(&l)?],
                None => src.used_lanes().into_iter().filter(|k| k.has_values()).collect(),
            };
            Ok(json(lanes.into_iter().flat_map(|k| cc_info(&src, k)).collect::<Vec<_>>()))
        }
        C::CcSet { item, lane, points, from, to } => {
            let kind = value_lane(&lane)?;
            edit_lane(session, item, kind, "Edit MIDI controllers", |all, ppq| {
                let ppq = ppq as f64;
                let mut new = Vec::new();
                for p in &points {
                    if p.value > kind.max() {
                        return Err(ApiError::invalid(format!("{lane} values run from 0 to {}", kind.max())));
                    }
                    let mut point =
                        LanePoint::new((qn(p.at, "at")? * ppq).round() as u64, p.value, channel(p.channel)?);
                    point.key = p.key.unwrap_or(0).min(127);
                    point.shape = match &p.shape {
                        Some(s) => shape(s, p.tension)?,
                        None => None,
                    };
                    new.push(point);
                }
                let lo = from.map(|f| (f * ppq).round() as u64).or(new.iter().map(|p| p.tick).min());
                let hi = to.map(|t| (t * ppq).round() as u64).or(new.iter().map(|p| p.tick).max());
                if let (Some(lo), Some(hi)) = (lo, hi) {
                    all.retain(|p| p.tick < lo || p.tick > hi);
                }
                all.extend(new);
                Ok(())
            })
        }
        C::CcLine { item, lane, from, to, start_value, end_value, shape: shape_name, tension, channel: ch } => {
            let kind = value_lane(&lane)?;
            if to <= from {
                return Err(ApiError::invalid("\"to\" must come after \"from\""));
            }
            if start_value.max(end_value) > kind.max() {
                return Err(ApiError::invalid(format!("{lane} values run from 0 to {}", kind.max())));
            }
            let s = shape(shape_name.as_deref().unwrap_or("linear"), tension)?;
            let ch = channel(ch)?;
            edit_lane(session, item, kind, "Draw MIDI controller line", |all, ppq| {
                let (a, b) = ((qn(from, "from")? * ppq as f64).round() as u64, (to * ppq as f64).round() as u64);
                all.retain(|p| p.channel != ch || p.tick < a || p.tick > b);
                all.push(LanePoint { shape: s, ..LanePoint::new(a, start_value, ch) });
                all.push(LanePoint::new(b, end_value, ch));
                Ok(())
            })
        }
        C::CcShape { item, lane, indexes, shape: shape_name, tension } => {
            let kind = value_lane(&lane)?;
            let s = shape(&shape_name, tension)?;
            edit_lane(session, item, kind, "Set CC shape", |all, _| {
                if let Some(i) = indexes.iter().find(|&&i| i >= all.len()) {
                    return Err(ApiError::not_found(format!("no event {i} in {lane}")));
                }
                for &i in &indexes {
                    all[i].shape = s;
                }
                Ok(())
            })
        }
        C::CcDelete { item, lane, indexes } => {
            let kind = value_lane(&lane)?;
            edit_lane(session, item, kind, "Delete MIDI controllers", |all, _| {
                let mut i = 0;
                all.retain(|_| {
                    i += 1;
                    !indexes.contains(&(i - 1))
                });
                Ok(())
            })
        }
        C::SysexList { item } => {
            let (src, _) = source(session, item)?;
            Ok(json(sysex_info(&src)))
        }
        C::SysexAdd { item, at, hex: bytes, text, kind } => {
            let (src, _) = source(session, item)?;
            let tick = (qn(at, "at")? * src.ticks_per_qn as f64).round() as u64;
            let event = match (bytes, text) {
                (Some(h), None) => {
                    let mut data = Vec::new();
                    for b in h.split_whitespace().flat_map(|w| w.as_bytes().chunks(2).map(|c| c.to_vec())) {
                        let s = String::from_utf8_lossy(&b).into_owned();
                        data.push(
                            u8::from_str_radix(&s, 16).map_err(|_| ApiError::invalid(format!("\"{s}\" is not hex")))?,
                        );
                    }
                    if data.first() != Some(&0xf0) {
                        data.insert(0, 0xf0);
                    }
                    if data.last() != Some(&0xf7) {
                        data.push(0xf7);
                    }
                    LongEvent { tick, args: vec!["0".into()], data }
                }
                (None, Some(t)) => {
                    let kind = kind.unwrap_or(1);
                    if !(1..=15).contains(&kind) {
                        return Err(ApiError::invalid("text kinds run from 1 to 15"));
                    }
                    let mut data = vec![0xff, kind];
                    data.extend(t.as_bytes());
                    let args = if kind == 0x0f {
                        vec!["0".into()]
                    } else {
                        vec!["0".into(), "0".into(), "0".into(), kind.to_string(), t.clone()]
                    };
                    LongEvent { tick, args, data }
                }
                _ => return Err(ApiError::invalid("give either hex or text")),
            };
            session.edit("Insert MIDI event", |p| {
                p.edit_midi(ItemId(item), |s| {
                    let mut s = s.clone();
                    s.long.push(event);
                    s.long.sort_by_key(|x| x.tick);
                    s.length_ticks = s.length_ticks.max(tick);
                    s
                });
            });
            let (src, _) = source(session, item)?;
            Ok(json(sysex_info(&src)))
        }
        C::SysexDelete { item, indexes } => {
            source(session, item)?;
            session.edit("Delete MIDI events", |p| {
                p.edit_midi(ItemId(item), |s| {
                    let mut s = s.clone();
                    let mut i = 0;
                    s.long.retain(|_| {
                        i += 1;
                        !indexes.contains(&(i - 1))
                    });
                    s
                });
            });
            let (src, _) = source(session, item)?;
            Ok(json(sysex_info(&src)))
        }
        C::Groove { items, file, groove: text, strength, velocity_strength, target } => {
            let text = match (file, text) {
                (Some(f), None) => read_file(&f)?,
                (None, Some(t)) => t,
                _ => return Err(ApiError::invalid("give either a groove file or its text")),
            };
            let g = Groove::parse(&text).ok_or_else(|| ApiError::invalid("not a groove template"))?;
            let strength = percent(strength, 100.0)?;
            let vel = percent(velocity_strength, 0.0)?;
            match target.as_deref().unwrap_or("notes") {
                "notes" => {
                    let &[item] = items.as_slice() else {
                        return Err(ApiError::invalid("grooving notes takes one MIDI item"));
                    };
                    let tl = session.project.tempo.timeline();
                    let pos = session.project.find_item(ItemId(item)).map_or(0.0, |(_, it)| it.position);
                    let offset = tl.qn_at(pos);
                    edit_notes(session, item, "Apply groove", |all, src, _| {
                        groove::apply_to_notes(all, src.ticks_per_qn, &g, offset, strength, vel);
                        Ok(())
                    })
                }
                "items" => {
                    for &i in &items {
                        if session.project.find_item(ItemId(i)).is_none() {
                            return Err(ApiError::not_found(format!("no item {i}")));
                        }
                    }
                    let tl = session.project.tempo.timeline();
                    session.edit("Apply groove to items", |p| {
                        for t in &mut p.tracks {
                            for it in t.items.iter_mut().filter(|it| items.contains(&it.id.0)) {
                                let (q, _) = g.apply(tl.qn_at(it.position), strength);
                                it.position = tl.time_at_qn(q).max(0.0);
                            }
                        }
                    });
                    let p = &session.project;
                    let out: Vec<Value> = items
                        .iter()
                        .filter_map(|&i| {
                            p.find_item(ItemId(i))
                                .map(|(_, it)| serde_json::json!({"item": i, "position": it.position}))
                        })
                        .collect();
                    Ok(json(out))
                }
                other => Err(ApiError::invalid(format!("unknown groove target \"{other}\"; use notes or items"))),
            }
        }
        C::GrooveExtract { item, beats, file } => {
            let (src, _) = source(session, item)?;
            let ppq = src.ticks_per_qn as f64;
            let hits: Vec<(f64, u8)> =
                src.edit_notes().iter().filter(|n| !n.muted).map(|n| (n.start as f64 / ppq, n.velocity)).collect();
            let g = Groove::from_hits(&hits, beats.unwrap_or(4.0))
                .ok_or_else(|| ApiError::invalid("the item has no notes"))?;
            let text = g.write();
            if let Some(f) = file {
                std::fs::write(&f, &text).map_err(|e| ApiError::failed(format!("could not write {f}: {e}")))?;
            }
            Ok(serde_json::json!({ "groove": text }))
        }
        C::EditorGet { item } => {
            let (_, view) = source(session, item)?;
            Ok(json(editor_info(&view)))
        }
        C::EditorSet { item, mode, grid, swing, key_snap, channel: ch, show_channels, lanes } => {
            let (_, mut view) = source(session, item)?;
            if let Some(m) = mode {
                view.mode = match m.as_str() {
                    "piano_roll" => EditorMode::PianoRoll,
                    "named_notes" => EditorMode::NamedNotes,
                    "event_list" => EditorMode::EventList,
                    _ => return Err(ApiError::invalid("modes are piano_roll, named_notes and event_list")),
                };
            }
            if let Some(g) = grid {
                if !(g > 0.0 && g.is_finite()) {
                    return Err(ApiError::invalid("the grid must be more than 0 quarter notes"));
                }
                view.grid_qn = g;
            }
            if let Some(s) = swing {
                if !(-1.0..=1.0).contains(&s) {
                    return Err(ApiError::invalid("swing runs from -1 to 1"));
                }
                view.swing = (s != 0.0).then_some(s);
            }
            if let Some(k) = key_snap {
                view.key_snap = k;
            }
            if let Some(c) = ch {
                view.channel = channel(Some(c))?;
            }
            if let Some(chs) = show_channels {
                let mut mask = 0u16;
                for c in chs {
                    mask |= 1 << channel(Some(c))?;
                }
                view.channel_filter = mask;
            }
            if let Some(ls) = lanes {
                view.lanes = ls.iter().map(|l| parse_lane(l).map(|k| (k, 100))).collect::<Result<_, _>>()?;
            }
            session.edit("MIDI editor settings", |p| {
                p.edit_midi_chunk(ItemId(item), |c| view.write(c));
            });
            Ok(json(editor_info(&view)))
        }
        C::NoteNamesGet { track } => {
            let id = track.track(&session.project)?;
            let names = session.project.track(id).map(|t| t.note_names()).unwrap_or_default();
            Ok(json(
                names
                    .notes
                    .iter()
                    .map(|(ch, key, name)| NoteName {
                        key: *key,
                        name: name.clone(),
                        channel: (*ch >= 0).then(|| *ch as u8 + 1),
                    })
                    .collect::<Vec<_>>(),
            ))
        }
        C::NoteNamesSet { track, names, file } => {
            let id = track.track(&session.project)?;
            let mut current = match &file {
                Some(f) => NoteNames::parse_file(&read_file(f)?),
                None => session.project.track(id).map(|t| t.note_names()).unwrap_or_default(),
            };
            for n in &names {
                if n.key > 127 {
                    return Err(ApiError::invalid("note numbers run from 0 to 127"));
                }
                let ch = match n.channel {
                    Some(_) => channel(n.channel)? as i8,
                    None => -1,
                };
                current.set_note(ch, n.key, &n.name);
            }
            session.edit("Note names", |p| {
                if let Some(t) = p.track_mut(id) {
                    t.set_note_names(&current);
                }
            });
            execute(session, C::NoteNamesGet { track })
        }
    }
}
