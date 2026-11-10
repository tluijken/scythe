//! Editing tools: ripple editing, the snap grid, locking, item groups, takes (explode,
//! implode, crop), fades and crossfades, nudges, dynamic split and razor edits. They follow
//! the project's editing settings as REAPER's actions do (`scythe_core::edit`).

use schemars::JsonSchema;
use scythe_core::edit::nudge::{Nudge, NudgeContext, NudgeUnit, NudgeWhat};
use scythe_core::edit::ops::{ExplodeMode, FadeTarget, RazorArea};
use scythe_core::edit::transients::{self, TransientSettings};
use scythe_core::edit::{GridKind, LOCK_ENABLED, LOCK_MODES, RippleMode};
use scythe_core::{EnvelopeKind, ItemId, Project, TimeRange};
use scythe_engine::MediaState;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{ApiError, TrackRef, json as to_json, snapshot, time};
use crate::Session;

/// The commands of other domains this module answers.
pub const COMMANDS: &[&str] = &[
    "project.grid",
    "project.snap",
    "project.lock",
    "project.editing",
    "item.lock",
    "item.group",
    "item.ungroup",
    "item.select_groups",
    "item.nudge",
    "item.fade",
    "item.crossfade",
    "item.transients",
    "item.dynamic_split",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Ripple {
    Off,
    PerTrack,
    AllTracks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Grid {
    Normal,
    Swing,
    /// Grid lines at bar starts.
    Measure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Explode {
    /// One new track per take under the item's track; the item stays.
    AcrossTracks,
    /// The takes stacked in the item's place.
    InPlace,
    /// The takes one after the other.
    InOrder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Implode {
    /// Items on different tracks that overlap become one item on the topmost track, each
    /// take keeping its timing.
    AcrossTracks,
    /// Items on one track become the takes of the earliest one.
    SameTrack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Which {
    In,
    Out,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NudgeTarget {
    Position,
    /// The left edge, the content staying in place.
    LeftTrim,
    /// The left edge with the content.
    LeftEdge,
    RightEdge,
    /// The content inside the items.
    Contents,
    /// Copies of the items, one nudge apart.
    Duplicate,
    EditCursor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RazorOp {
    /// Removes what lies inside (closing the gap as the ripple mode says).
    Delete,
    /// Splits items at the area edges.
    Split,
}

/// A razor edit area.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Area {
    /// The track, by id or name.
    pub track: TrackRef,
    /// Start in seconds.
    pub start: f64,
    /// End in seconds.
    pub end: f64,
    /// An envelope lane of the track instead of its items: "volume", "pan", "mute", "width"…
    #[serde(default)]
    pub envelope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum EditingCommand {
    /// The ripple editing mode, or sets it: "off", "per_track" (later items and envelope
    /// points on the edited tracks follow every edit) or "all_tracks" (on every track, with
    /// markers and regions).
    #[serde(rename = "edit.ripple")]
    Ripple {
        #[serde(default)]
        mode: Option<Ripple>,
    },
    /// Razor edits: removes or splits what lies inside the areas.
    #[serde(rename = "edit.razor")]
    Razor {
        areas: Vec<Area>,
        /// "delete" or "split".
        op: RazorOp,
    },
    /// The snap grid, or changes it (`GetSetProjectGrid`).
    #[serde(rename = "project.grid")]
    Grid {
        /// Grid spacing in whole notes, as `GetSetProjectGrid` takes it: 0.25 is a quarter
        /// note, 1/24 a sixteenth-note triplet.
        #[serde(default)]
        division: Option<f64>,
        /// "normal", "swing" or "measure".
        #[serde(default)]
        kind: Option<Grid>,
        /// Swing amount, -1 to 1.
        #[serde(default)]
        swing: Option<f64>,
        /// Snapping on or off.
        #[serde(default)]
        snap: Option<bool>,
        /// Grid lines shown.
        #[serde(default)]
        lines: Option<bool>,
    },
    /// `time` snapped to the grid (`SnapToGrid`): unchanged while snapping is off.
    #[serde(rename = "project.snap")]
    Snap {
        /// Seconds.
        time: f64,
    },
    /// The locking settings, or changes them: whether locking is on and which of its modes
    /// apply ("time selection", "full item", "left/right item", "up/down item", "track
    /// envelope", "region", "marker", "time signature marker", "item edges", "item
    /// fade/volume handles", "loop points").
    #[serde(rename = "project.lock")]
    Lock {
        #[serde(default)]
        enabled: Option<bool>,
        #[serde(default)]
        modes: Option<Vec<String>>,
    },
    /// Auto-crossfade, item grouping and envelope points following items, or switches them.
    #[serde(rename = "project.editing")]
    Options {
        /// Items that come to overlap crossfade automatically.
        #[serde(default)]
        auto_crossfade: Option<bool>,
        /// Grouped items move, split and select together.
        #[serde(default)]
        grouping: Option<bool>,
        /// Moving items takes the track envelope points under them along (REAPER's "Move
        /// envelope points with media items", on in a new project).
        #[serde(default)]
        envelopes_follow_items: Option<bool>,
    },
    /// Locks or unlocks items (toggles each when `locked` is left out). Locked items do not
    /// move, split or delete.
    #[serde(rename = "item.lock")]
    ItemLock {
        /// Item ids.
        items: Vec<u64>,
        #[serde(default)]
        locked: Option<bool>,
    },
    /// Puts the items in a new group and answers its number.
    #[serde(rename = "item.group")]
    Group {
        /// Item ids.
        items: Vec<u64>,
    },
    /// Takes the items out of their groups.
    #[serde(rename = "item.ungroup")]
    Ungroup {
        /// Item ids.
        items: Vec<u64>,
    },
    /// The items with every other member of their groups.
    #[serde(rename = "item.select_groups")]
    SelectGroups {
        /// Item ids.
        items: Vec<u64>,
    },
    /// Nudges or sets the items as REAPER's nudge/set dialog and `ApplyNudge` do.
    #[serde(rename = "item.nudge")]
    Nudge {
        /// Item ids.
        items: Vec<u64>,
        /// What moves: "position" (default), "left_trim", "left_edge", "right_edge",
        /// "contents", "duplicate" or "edit_cursor".
        #[serde(default)]
        what: Option<NudgeTarget>,
        /// REAPER's nudge unit: 0 ms (default), 1 seconds, 2 grid, 3 to 15 notes from 1/256
        /// to whole (with triplets), 16 measures.beats, 17 samples, 18 frames, 19 pixels, 20
        /// item lengths, 21 item selections.
        #[serde(default)]
        unit: Option<u8>,
        /// How far, or the value to set.
        value: f64,
        /// Set to `value` instead of nudging by it.
        #[serde(default)]
        set: bool,
        /// Snap the result to the grid.
        #[serde(default)]
        snap: bool,
        /// Nudge left instead of right.
        #[serde(default)]
        reverse: bool,
        /// Copies, for "duplicate".
        #[serde(default)]
        copies: Option<u32>,
    },
    /// Changes an item's fade-in or fade-out.
    #[serde(rename = "item.fade")]
    Fade {
        /// The item's id.
        item: u64,
        /// "in" or "out".
        fade: Which,
        /// Length in seconds.
        #[serde(default)]
        length: Option<f64>,
        /// REAPER's shape number (`C_FADEINSHAPE`): 0 linear, 1 fast start, 2 fast end, 3
        /// fast start steep, 4 fast end steep, 5 slow start/end, 6 slow start/end steep, 7
        /// equal power.
        #[serde(default)]
        shape: Option<u8>,
        /// Curvature, -1 to 1 (`D_FADEINDIR`).
        #[serde(default)]
        curve: Option<f64>,
        /// One of REAPER's fade types 1 to 7 instead of `shape` and `curve`, as the "Set
        /// fade shape" actions set them.
        #[serde(default)]
        kind: Option<usize>,
    },
    /// Crossfades items: those that overlap over their overlap, or neighbours over the time
    /// range `start`..`end` (extending them to reach it). `kind` (1 to 7) sets the crossfade
    /// shape of items that already crossfade.
    #[serde(rename = "item.crossfade")]
    Crossfade {
        /// Item ids.
        items: Vec<u64>,
        #[serde(default)]
        start: Option<f64>,
        #[serde(default)]
        end: Option<f64>,
        #[serde(default)]
        kind: Option<usize>,
    },
    /// The item's transients in project time, as tab-to-transient and dynamic split find
    /// them.
    #[serde(rename = "item.transients")]
    Transients {
        /// The item's id.
        item: u64,
        /// 0 to 1, default 0.5: higher finds softer transients.
        #[serde(default)]
        sensitivity: Option<f64>,
        /// Level a transient must reach in dB, default -24.
        #[serde(default)]
        threshold_db: Option<f64>,
    },
    /// Splits the items at their transients (dynamic split) and answers the pieces.
    #[serde(rename = "item.dynamic_split")]
    DynamicSplit {
        /// Item ids.
        items: Vec<u64>,
        #[serde(default)]
        sensitivity: Option<f64>,
        #[serde(default)]
        threshold_db: Option<f64>,
        /// Shortest piece in seconds (default 0.01).
        #[serde(default)]
        min_length: Option<f64>,
    },
    /// Lays the takes of the items out as separate items.
    #[serde(rename = "take.explode")]
    Explode {
        /// Item ids.
        items: Vec<u64>,
        /// "across_tracks" (default), "in_place" or "in_order".
        #[serde(default)]
        mode: Option<Explode>,
    },
    /// Turns items into the takes of one item.
    #[serde(rename = "take.implode")]
    Implode {
        /// Item ids.
        items: Vec<u64>,
        /// "across_tracks" (default) or "same_track".
        #[serde(default)]
        mode: Option<Implode>,
    },
    /// Keeps only the active take of each item.
    #[serde(rename = "take.crop")]
    Crop {
        /// Item ids.
        items: Vec<u64>,
    },
    /// Deletes the active take of each item.
    #[serde(rename = "take.delete")]
    DeleteTake {
        /// Item ids.
        items: Vec<u64>,
    },
    /// Makes take `take` (0-based) the item's active take.
    #[serde(rename = "take.active")]
    ActiveTake {
        /// The item's id.
        item: u64,
        take: usize,
    },
}

fn ids(session: &Session, items: &[u64]) -> Result<Vec<ItemId>, ApiError> {
    items
        .iter()
        .map(|&i| {
            let id = ItemId(i);
            session.project.find_item(id).map(|_| id).ok_or_else(|| ApiError::not_found(format!("no item {i}")))
        })
        .collect()
}

fn infos(p: &Project, items: &[ItemId]) -> Value {
    let list: Vec<_> = p
        .tracks
        .iter()
        .flat_map(|t| t.items.iter().filter(|it| items.contains(&it.id)).map(move |it| snapshot::item(t, it)))
        .collect();
    to_json(list)
}

fn ripple_name(m: RippleMode) -> Ripple {
    match m {
        RippleMode::Off => Ripple::Off,
        RippleMode::PerTrack => Ripple::PerTrack,
        RippleMode::AllTracks => Ripple::AllTracks,
    }
}

fn grid_state(p: &Project) -> Value {
    let g = &p.editing.grid;
    let (division, mode, swing) = g.api();
    json!({
        "division": division,
        "kind": match g.kind { GridKind::Swing => "swing", GridKind::Measure => "measure", _ => "normal" },
        "swing_mode": mode,
        "swing": swing,
        "snap": g.snap,
        "lines": g.lines,
        "label": g.label(),
    })
}

fn lock_state(p: &Project) -> Value {
    let l = p.editing.lock;
    let modes: Vec<&str> = LOCK_MODES.iter().filter(|m| l & m.0 != 0).map(|m| m.1).collect();
    json!({ "enabled": l & LOCK_ENABLED != 0, "modes": modes, "flags": l })
}

fn options_state(p: &Project) -> Value {
    json!({
        "ripple": ripple_name(p.editing.ripple),
        "auto_crossfade": p.editing.auto_crossfade,
        "grouping": p.editing.grouping,
        "envelopes_follow_items": p.editing.envelopes_follow_items,
    })
}

/// The item's transients in project time, from its active take's media as the take plays
/// it; empty while the media is not loaded.
pub fn item_transients(session: &Session, item: ItemId, settings: TransientSettings) -> Vec<f64> {
    let Some((_, it)) = session.project.find_item(item) else { return Vec::new() };
    let Some(take) = it.take() else { return Vec::new() };
    let Some(MediaState::Ready(media)) = session.media(take) else { return Vec::new() };
    let audio = &media.audio;
    let rate = audio.sample_rate;
    let media_len = audio.duration();
    let frames = (it.length * rate as f64) as usize;
    let rendered: Vec<Vec<f32>> = audio
        .channels
        .iter()
        .map(|c| {
            (0..frames)
                .map(|i| {
                    take.source_time(i as f64 / rate as f64, it.looped, media_len)
                        .and_then(|s| c.get((s * rate as f64) as usize).copied())
                        .unwrap_or(0.0)
                })
                .collect()
        })
        .collect();
    transients::detect(&rendered, rate, settings).into_iter().map(|t| it.position + t).collect()
}

fn settings(sensitivity: Option<f64>, threshold_db: Option<f64>) -> TransientSettings {
    let d = TransientSettings::default();
    TransientSettings {
        sensitivity: sensitivity.unwrap_or(d.sensitivity).clamp(0.0, 1.0),
        threshold_db: threshold_db.unwrap_or(d.threshold_db),
    }
}

fn envelope_kind(name: &str) -> Result<EnvelopeKind, ApiError> {
    EnvelopeKind::ALL
        .iter()
        .copied()
        .find(|k| k.label().eq_ignore_ascii_case(name) || format!("{k:?}").eq_ignore_ascii_case(name))
        .ok_or_else(|| ApiError::invalid(format!("no envelope \"{name}\"")))
}

pub fn execute(session: &mut Session, command: EditingCommand) -> Result<Value, ApiError> {
    match command {
        EditingCommand::Ripple { mode } => {
            if let Some(mode) = mode {
                let m = match mode {
                    Ripple::Off => RippleMode::Off,
                    Ripple::PerTrack => RippleMode::PerTrack,
                    Ripple::AllTracks => RippleMode::AllTracks,
                };
                session.edit("Set ripple editing mode", |p| p.editing.ripple = m);
            }
            Ok(options_state(&session.project))
        }
        EditingCommand::Razor { areas, op } => {
            let mut list = Vec::new();
            for a in &areas {
                let track = a.track.track(&session.project)?;
                let (start, end) = (time(a.start, "start")?, time(a.end, "end")?);
                if end <= start {
                    return Err(ApiError::invalid("an area's end must come after its start"));
                }
                let envelope = a.envelope.as_deref().map(envelope_kind).transpose()?;
                list.push(RazorArea { track, range: TimeRange::new(start, end), envelope });
            }
            match op {
                RazorOp::Delete => session.edit("Remove razor edit contents", |p| p.remove_razor(&list)),
                RazorOp::Split => session.edit("Split at razor edit", |p| {
                    for a in list.iter().filter(|a| a.envelope.is_none()) {
                        p.split_items_at(a.range.start, Some(&[a.track]));
                        p.split_items_at(a.range.end, Some(&[a.track]));
                    }
                }),
            }
            Ok(super::done())
        }
        EditingCommand::Grid { division, kind, swing, snap, lines } => {
            if division.is_some_and(|d| !(d > 0.0 && d <= 16.0)) {
                return Err(ApiError::invalid("division runs from just above 0 to 16 whole notes"));
            }
            if swing.is_some_and(|s| !(-1.0..=1.0).contains(&s)) {
                return Err(ApiError::invalid("swing runs from -1 to 1"));
            }
            session.edit("Set grid", |p| {
                let g = &mut p.editing.grid;
                if let Some(d) = division {
                    g.qn = d * 4.0;
                }
                if let Some(k) = kind {
                    g.kind = match k {
                        Grid::Normal => GridKind::Normal,
                        Grid::Swing => GridKind::Swing,
                        Grid::Measure => GridKind::Measure,
                    };
                }
                g.swing = swing.unwrap_or(g.swing);
                g.snap = snap.unwrap_or(g.snap);
                g.lines = lines.unwrap_or(g.lines);
            });
            Ok(grid_state(&session.project))
        }
        EditingCommand::Snap { time: t } => {
            let t = time(t, "time")?;
            let p = &session.project;
            Ok(json!({ "time": p.editing.grid.snap(&p.tempo, t) }))
        }
        EditingCommand::Lock { enabled, modes } => {
            let flags = modes
                .map(|list| {
                    list.iter().try_fold(0u32, |acc, name| {
                        LOCK_MODES
                            .iter()
                            .find(|m| m.1.eq_ignore_ascii_case(name.trim()))
                            .map(|m| acc | m.0)
                            .ok_or_else(|| ApiError::invalid(format!("no lock mode \"{name}\"")))
                    })
                })
                .transpose()?;
            if enabled.is_some() || flags.is_some() {
                session.edit("Locking", |p| {
                    let l = &mut p.editing.lock;
                    if let Some(f) = flags {
                        *l = (*l & LOCK_ENABLED) | f;
                    }
                    if let Some(on) = enabled {
                        *l = if on { *l | LOCK_ENABLED } else { *l & !LOCK_ENABLED };
                    }
                });
            }
            Ok(lock_state(&session.project))
        }
        EditingCommand::Options { auto_crossfade, grouping, envelopes_follow_items } => {
            if auto_crossfade.is_some() || grouping.is_some() || envelopes_follow_items.is_some() {
                session.edit("Editing options", |p| {
                    p.editing.auto_crossfade = auto_crossfade.unwrap_or(p.editing.auto_crossfade);
                    p.editing.grouping = grouping.unwrap_or(p.editing.grouping);
                    p.editing.envelopes_follow_items =
                        envelopes_follow_items.unwrap_or(p.editing.envelopes_follow_items);
                });
            }
            Ok(options_state(&session.project))
        }
        EditingCommand::ItemLock { items, locked } => {
            let ids = ids(session, &items)?;
            session.edit("Item lock", |p| p.set_items_locked(&ids, locked));
            Ok(infos(&session.project, &ids))
        }
        EditingCommand::Group { items } => {
            let ids = ids(session, &items)?;
            let mut group = 0;
            session.edit("Group items", |p| group = p.group_items(&ids));
            Ok(json!({ "group": group }))
        }
        EditingCommand::Ungroup { items } => {
            let ids = ids(session, &items)?;
            session.edit("Remove items from group", |p| p.ungroup_items(&ids));
            Ok(infos(&session.project, &ids))
        }
        EditingCommand::SelectGroups { items } => {
            let ids = ids(session, &items)?;
            let p = &session.project;
            let mut all = p.with_groups(&ids);
            if !p.editing.grouping {
                let mut q = p.clone();
                q.editing.grouping = true;
                all = q.with_groups(&ids);
            }
            Ok(to_json(all.iter().map(|i| i.0).collect::<Vec<_>>()))
        }
        EditingCommand::Nudge { items, what, unit, value, set, snap, reverse, copies } => {
            let ids = ids(session, &items)?;
            if !value.is_finite() {
                return Err(ApiError::invalid("value must be a number"));
            }
            let unit = unit.unwrap_or(0);
            if unit >= NudgeUnit::COUNT {
                return Err(ApiError::invalid("unit runs from 0 to 21"));
            }
            let what = match what.unwrap_or(NudgeTarget::Position) {
                NudgeTarget::Position => NudgeWhat::Position,
                NudgeTarget::LeftTrim => NudgeWhat::LeftTrim,
                NudgeTarget::LeftEdge => NudgeWhat::LeftEdge,
                NudgeTarget::RightEdge => NudgeWhat::RightEdge,
                NudgeTarget::Contents => NudgeWhat::Contents,
                NudgeTarget::Duplicate => NudgeWhat::Duplicate,
                NudgeTarget::EditCursor => NudgeWhat::EditCursor,
            };
            let nudge = Nudge { set, snap, what, unit: NudgeUnit(unit), value, reverse, copies: copies.unwrap_or(1) };
            let mut ctx = NudgeContext { cursor: session.project.cursor, pixel: 0.01 };
            let mut selected = Vec::new();
            session.edit("Nudge items", |p| selected = nudge.apply(p, &ids, &mut ctx));
            if what == NudgeWhat::EditCursor {
                session.set_cursor(ctx.cursor);
                return Ok(json!({ "cursor": ctx.cursor }));
            }
            Ok(infos(&session.project, &selected))
        }
        EditingCommand::Fade { item, fade, length, shape, curve, kind } => {
            let ids = ids(session, &[item])?;
            if shape.is_some_and(|s| s > 7) {
                return Err(ApiError::invalid("shape runs from 0 to 7"));
            }
            if kind.is_some_and(|k| !(1..=7).contains(&k)) {
                return Err(ApiError::invalid("kind runs from 1 to 7"));
            }
            let length = length.map(|l| time(l, "length")).transpose()?;
            session.edit("Item fade", |p| {
                let Some(it) = p.item_mut_anywhere(ids[0]) else { return };
                let item_len = it.length;
                let out = fade == Which::Out;
                let f = if out { &mut it.fade_out } else { &mut it.fade_in };
                if let Some(l) = length {
                    f.length = l.min(item_len);
                }
                if let Some(k) = kind {
                    f.set_type(k - 1, out, false);
                }
                if let Some(s) = shape {
                    f.set_api_shape(s);
                }
                if let Some(c) = curve {
                    f.curve = c.clamp(-1.0, 1.0);
                }
            });
            let p = &session.project;
            let (_, it) = p.find_item(ids[0]).ok_or_else(|| ApiError::not_found("no such item"))?;
            let f = if fade == Which::Out { &it.fade_out } else { &it.fade_in };
            Ok(json!({
                "length": f.length,
                "auto_length": f.auto_length,
                "shape": f.api_shape(),
                "curve": f.curve,
                "kind": f.kind(fade == Which::Out).map(|k| k + 1),
            }))
        }
        EditingCommand::Crossfade { items, start, end, kind } => {
            let ids = ids(session, &items)?;
            if kind.is_some_and(|k| !(1..=7).contains(&k)) {
                return Err(ApiError::invalid("kind runs from 1 to 7"));
            }
            let range = match (start, end) {
                (Some(s), Some(e)) if e > s => Some(TimeRange::new(time(s, "start")?, e)),
                (None, None) => None,
                _ => return Err(ApiError::invalid("give both start and end, end after start")),
            };
            session.edit("Crossfade items", |p| {
                match range {
                    Some(r) => p.crossfade_in_range(&ids, r),
                    None if kind.is_none() => p.crossfade_overlapping(&ids),
                    None => {}
                }
                if let Some(k) = kind {
                    p.set_fade_type(&ids, FadeTarget::Crossfade, k - 1);
                }
            });
            Ok(infos(&session.project, &ids))
        }
        EditingCommand::Transients { item, sensitivity, threshold_db } => {
            let ids = ids(session, &[item])?;
            Ok(json!({ "transients": item_transients(session, ids[0], settings(sensitivity, threshold_db)) }))
        }
        EditingCommand::DynamicSplit { items, sensitivity, threshold_db, min_length } => {
            let ids = ids(session, &items)?;
            let s = settings(sensitivity, threshold_db);
            let min = min_length.unwrap_or(0.01).max(0.0);
            let cuts: Vec<(ItemId, Vec<f64>)> = ids.iter().map(|&i| (i, item_transients(session, i, s))).collect();
            let mut pieces = Vec::new();
            session.edit("Dynamic split items", |p| {
                for (item, times) in &cuts {
                    pieces.extend(scythe_core::edit::actions::split_at_times(p, *item, times, min));
                }
            });
            Ok(infos(&session.project, &pieces))
        }
        EditingCommand::Explode { items, mode } => {
            let ids = ids(session, &items)?;
            let mode = match mode.unwrap_or(Explode::AcrossTracks) {
                Explode::AcrossTracks => ExplodeMode::AcrossTracks,
                Explode::InPlace => ExplodeMode::InPlace,
                Explode::InOrder => ExplodeMode::InOrder,
            };
            let mut new = Vec::new();
            session.edit("Explode takes", |p| new = p.explode_takes(&ids, mode));
            Ok(infos(&session.project, &new))
        }
        EditingCommand::Implode { items, mode } => {
            let ids = ids(session, &items)?;
            let mut out = Vec::new();
            session.edit("Implode items into takes", |p| {
                out = match mode.unwrap_or(Implode::AcrossTracks) {
                    Implode::AcrossTracks => p.implode_across_tracks(&ids),
                    Implode::SameTrack => p.implode_on_track(&ids),
                }
            });
            Ok(infos(&session.project, &out))
        }
        EditingCommand::Crop { items } => {
            let ids = ids(session, &items)?;
            session.edit("Crop to active take", |p| p.crop_to_active_take(&ids));
            Ok(infos(&session.project, &ids))
        }
        EditingCommand::DeleteTake { items } => {
            let ids = ids(session, &items)?;
            session.edit("Delete active take", |p| p.delete_active_take(&ids));
            Ok(infos(&session.project, &ids))
        }
        EditingCommand::ActiveTake { item, take } => {
            let ids = ids(session, &[item])?;
            let count = session.project.find_item(ids[0]).map_or(0, |(_, it)| it.takes.len());
            if take >= count {
                return Err(ApiError::invalid(format!("the item has {count} takes")));
            }
            session.edit("Set active take", |p| {
                if let Some(it) = p.item_mut_anywhere(ids[0]) {
                    it.active_take = take;
                }
            });
            Ok(infos(&session.project, &ids))
        }
    }
}
