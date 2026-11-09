//! How takes play their source over time: stretch markers, channel modes, the item and project
//! timebase and the project play rate.

use schemars::JsonSchema;
use scythe_core::stretch::{MAX_SLOPE, StretchMarker};
use scythe_core::timebase::{PlayRate, Timebase};
use scythe_core::{ChannelMode, Item, ItemId, Take};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ApiError, json, time};
use crate::Session;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum TimingCommand {
    /// The stretch markers of the item's active take.
    #[serde(rename = "item.stretch_markers")]
    StretchMarkers {
        /// The item's id.
        item: u64,
    },
    /// Adds a stretch marker at `position` and answers with the markers.
    #[serde(rename = "item.stretch_markers.add")]
    AddStretchMarker {
        /// The item's id.
        item: u64,
        /// Seconds from the item's start.
        position: f64,
        /// Seconds into the source the marker pins there; where the take plays at `position`
        /// when left out.
        #[serde(default)]
        source: Option<f64>,
    },
    /// Moves a stretch marker: `position` moves it along the item keeping its source position
    /// (the audio around it stretches), `source` changes which source position it pins (as
    /// Shift-dragging a marker does in REAPER).
    #[serde(rename = "item.stretch_markers.move")]
    MoveStretchMarker {
        /// The item's id.
        item: u64,
        /// The marker's number from `item.stretch_markers`, from 0.
        index: usize,
        /// Seconds from the item's start.
        #[serde(default)]
        position: Option<f64>,
        /// Seconds into the source.
        #[serde(default)]
        source: Option<f64>,
    },
    /// Sets how the rate ramps from a marker to the next: -0.99 to 0.99, 0 for a steady rate.
    #[serde(rename = "item.stretch_markers.slope")]
    StretchMarkerSlope {
        /// The item's id.
        item: u64,
        /// The marker's number from `item.stretch_markers`, from 0.
        index: usize,
        slope: f64,
    },
    /// Deletes a stretch marker, or all of them when `index` is left out.
    #[serde(rename = "item.stretch_markers.delete")]
    DeleteStretchMarker {
        /// The item's id.
        item: u64,
        /// The marker's number from `item.stretch_markers`, from 0.
        #[serde(default)]
        index: Option<usize>,
    },
    /// What the item follows when the tempo changes; `project` uses the track's or project's
    /// setting. Answers with the item's and the effective timebase.
    #[serde(rename = "item.timebase")]
    ItemTimebase {
        /// The item's id.
        item: u64,
        #[serde(default)]
        timebase: Option<TimebaseChoice>,
    },
    /// Which source channels the item's active take plays.
    #[serde(rename = "take.channel_mode")]
    ChannelMode {
        /// The item's id.
        item: u64,
        /// The take's number in the item, from 0; the active take when left out.
        #[serde(default)]
        take: Option<usize>,
        /// Left out to just read it.
        #[serde(default)]
        mode: Option<ChannelModeName>,
        /// For `mono` and `stereo`: the (first) source channel, from 1; 1 when left out.
        #[serde(default)]
        channel: Option<u8>,
    },
    /// What items, envelopes and markers follow when the tempo changes, and whether tempo
    /// markers keep their beat when an earlier one changes. Answers with both.
    #[serde(rename = "project.timebase")]
    ProjectTimebase {
        #[serde(default)]
        timebase: Option<TimebaseName>,
        #[serde(default)]
        tempo_follows_beats: Option<bool>,
    },
    /// The project play rate: playback and renders run this many times as fast. Answers with
    /// the rate and whether it keeps the pitch.
    #[serde(rename = "project.playrate")]
    PlayRate {
        /// 0.25 to 4; 1 is normal speed.
        #[serde(default)]
        rate: Option<f64>,
        /// Time-stretch instead of resampling.
        #[serde(default)]
        preserve_pitch: Option<bool>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TimebaseName {
    /// Positions stay in seconds.
    Time,
    /// Position, length and rate follow the beat.
    Beats,
    /// Only the position follows the beat.
    BeatsPosition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TimebaseChoice {
    /// The track's timebase, or the project's.
    Project,
    Time,
    Beats,
    BeatsPosition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChannelModeName {
    Normal,
    ReverseStereo,
    /// The first two channels averaged.
    Downmix,
    /// One channel on both sides.
    Mono,
    /// A pair of channels.
    Stereo,
}

impl From<TimebaseName> for Timebase {
    fn from(t: TimebaseName) -> Self {
        match t {
            TimebaseName::Time => Self::Time,
            TimebaseName::Beats => Self::Beats,
            TimebaseName::BeatsPosition => Self::BeatsPosition,
        }
    }
}

impl From<Timebase> for TimebaseName {
    fn from(t: Timebase) -> Self {
        match t {
            Timebase::Time => Self::Time,
            Timebase::Beats => Self::Beats,
            Timebase::BeatsPosition => Self::BeatsPosition,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StretchMarkerInfo {
    pub index: usize,
    /// Seconds from the item's start.
    pub position: f64,
    /// Seconds into the source.
    pub source: f64,
    pub slope: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelModeInfo {
    pub item: u64,
    pub take: usize,
    /// REAPER's `I_CHANMODE` code.
    pub code: i64,
    pub label: String,
}

fn find(session: &Session, item: u64) -> Result<&Item, ApiError> {
    session
        .project
        .find_item(ItemId(item))
        .map(|(_, it)| it)
        .ok_or_else(|| ApiError::not_found(format!("no item {item}")))
}

fn active_take(session: &Session, item: u64) -> Result<&Take, ApiError> {
    find(session, item)?.take().ok_or_else(|| ApiError::invalid(format!("item {item} has no take")))
}

/// Edits the active take of `item`.
fn edit_take(session: &mut Session, label: &str, item: u64, f: impl FnOnce(&mut Take)) {
    session.edit(label, |p| {
        if let Some(take) = p.tracks.iter_mut().find_map(|t| t.item_mut(ItemId(item))).and_then(|it| {
            let i = it.active_take;
            it.takes.get_mut(i)
        }) {
            f(take);
        }
    });
}

fn rate(take: &Take) -> f64 {
    if take.playrate > 0.0 { take.playrate } else { 1.0 }
}

fn markers(session: &Session, item: u64) -> Result<Value, ApiError> {
    let take = active_take(session, item)?;
    let rate = rate(take);
    Ok(json(
        take.stretch_markers
            .iter()
            .enumerate()
            .map(|(index, m)| StretchMarkerInfo { index, position: m.pos / rate, source: m.src, slope: m.slope })
            .collect::<Vec<_>>(),
    ))
}

fn marker_index(take: &Take, index: usize) -> Result<(), ApiError> {
    if index < take.stretch_markers.len() {
        Ok(())
    } else {
        Err(ApiError::not_found(format!("no stretch marker {index}")))
    }
}

fn timebase_info(session: &Session) -> Value {
    let p = &session.project;
    serde_json::json!({
        "timebase": TimebaseName::from(p.timebase),
        "tempo_follows_beats": p.tempo_follows_beats,
    })
}

fn playrate_info(session: &Session) -> Value {
    let r = session.project.playrate;
    serde_json::json!({"rate": r.rate, "preserve_pitch": r.preserve_pitch})
}

pub fn execute(session: &mut Session, command: TimingCommand) -> Result<Value, ApiError> {
    match command {
        TimingCommand::StretchMarkers { item } => markers(session, item),
        TimingCommand::AddStretchMarker { item, position, source } => {
            let take = active_take(session, item)?;
            let position = time(position, "position")?;
            let source = source.map(|s| time(s, "source")).transpose()?.unwrap_or_else(|| take.source_pos(position));
            let tau = position * rate(take);
            if take.stretch_markers.iter().any(|m| (m.pos - tau).abs() < 1e-6) {
                return Err(ApiError::invalid("there is a stretch marker there already"));
            }
            edit_take(session, "Add stretch marker", item, |t| {
                t.stretch_markers.push(StretchMarker::new(tau, source));
                t.stretch_markers.sort_by(|a, b| a.pos.total_cmp(&b.pos));
            });
            markers(session, item)
        }
        TimingCommand::MoveStretchMarker { item, index, position, source } => {
            let take = active_take(session, item)?;
            marker_index(take, index)?;
            let tau = position.map(|p| time(p, "position")).transpose()?.map(|p| p * rate(take));
            let source = source.map(|s| time(s, "source")).transpose()?;
            let ms = &take.stretch_markers;
            if let Some(tau) = tau {
                let lo = index.checked_sub(1).map_or(f64::NEG_INFINITY, |i| ms[i].pos);
                let hi = ms.get(index + 1).map_or(f64::INFINITY, |m| m.pos);
                if tau <= lo || tau >= hi {
                    return Err(ApiError::invalid("a stretch marker stays between its neighbours"));
                }
            }
            session.edit_merging("Move stretch marker", &format!("sm:{item}:{index}"), |p| {
                let Some(it) = p.tracks.iter_mut().find_map(|t| t.item_mut(ItemId(item))) else { return };
                let i = it.active_take;
                let Some(m) = it.takes.get_mut(i).and_then(|t| t.stretch_markers.get_mut(index)) else { return };
                if let Some(tau) = tau {
                    m.pos = tau;
                }
                if let Some(src) = source {
                    m.src = src;
                }
            });
            markers(session, item)
        }
        TimingCommand::StretchMarkerSlope { item, index, slope } => {
            marker_index(active_take(session, item)?, index)?;
            if !(-MAX_SLOPE..=MAX_SLOPE).contains(&slope) {
                return Err(ApiError::invalid(format!("a slope runs from -{MAX_SLOPE} to {MAX_SLOPE}")));
            }
            edit_take(session, "Stretch marker slope", item, |t| t.stretch_markers[index].slope = slope);
            markers(session, item)
        }
        TimingCommand::DeleteStretchMarker { item, index } => {
            let take = active_take(session, item)?;
            if let Some(i) = index {
                marker_index(take, i)?;
            }
            edit_take(session, "Delete stretch marker", item, |t| match index {
                Some(i) => {
                    t.stretch_markers.remove(i);
                }
                None => t.stretch_markers.clear(),
            });
            markers(session, item)
        }
        TimingCommand::ItemTimebase { item, timebase } => {
            find(session, item)?;
            if let Some(choice) = timebase {
                let tb = match choice {
                    TimebaseChoice::Project => None,
                    TimebaseChoice::Time => Some(Timebase::Time),
                    TimebaseChoice::Beats => Some(Timebase::Beats),
                    TimebaseChoice::BeatsPosition => Some(Timebase::BeatsPosition),
                };
                session.edit("Item timebase", |p| {
                    if let Some(it) = p.tracks.iter_mut().find_map(|t| t.item_mut(ItemId(item))) {
                        it.timebase = tb;
                    }
                });
            }
            let p = &session.project;
            let (track, it) = p.find_item(ItemId(item)).ok_or_else(|| ApiError::not_found("no such item"))?;
            let index = p.track_index(track).unwrap_or(0);
            Ok(serde_json::json!({
                "item": item,
                "timebase": it.timebase.map(TimebaseName::from),
                "effective": TimebaseName::from(p.item_timebase(index, it)),
            }))
        }
        TimingCommand::ChannelMode { item, take, mode, channel } => {
            let it = find(session, item)?;
            let index = take.unwrap_or(it.active_take);
            if index >= it.takes.len() {
                return Err(ApiError::not_found(format!("item {item} has no take {index}")));
            }
            if let Some(mode) = mode {
                let first = channel.unwrap_or(1);
                if first == 0 || first > 64 {
                    return Err(ApiError::invalid("channels run from 1 to 64"));
                }
                let mode = match mode {
                    ChannelModeName::Normal => ChannelMode::Normal,
                    ChannelModeName::ReverseStereo => ChannelMode::ReverseStereo,
                    ChannelModeName::Downmix => ChannelMode::Downmix,
                    ChannelModeName::Mono => ChannelMode::Mono(first - 1),
                    ChannelModeName::Stereo => ChannelMode::Stereo(first - 1),
                };
                session.edit("Take channel mode", |p| {
                    if let Some(t) = p
                        .tracks
                        .iter_mut()
                        .find_map(|t| t.item_mut(ItemId(item)))
                        .and_then(|it| it.takes.get_mut(index))
                    {
                        t.channel_mode = mode;
                    }
                });
            }
            let mode = find(session, item)?.takes[index].channel_mode;
            Ok(json(ChannelModeInfo { item, take: index, code: mode.code(), label: mode.label() }))
        }
        TimingCommand::ProjectTimebase { timebase, tempo_follows_beats } => {
            session.edit("Project timebase", |p| {
                if let Some(t) = timebase {
                    p.timebase = t.into();
                }
                if let Some(f) = tempo_follows_beats {
                    p.tempo_follows_beats = f;
                }
            });
            Ok(timebase_info(session))
        }
        TimingCommand::PlayRate { rate, preserve_pitch } => {
            if let Some(r) = rate
                && !(PlayRate::MIN..=PlayRate::MAX).contains(&r)
            {
                return Err(ApiError::invalid(format!(
                    "the play rate runs from {} to {}",
                    PlayRate::MIN,
                    PlayRate::MAX
                )));
            }
            session.edit_merging("Project play rate", "playrate", |p| {
                if let Some(r) = rate {
                    p.playrate.rate = r;
                }
                if let Some(k) = preserve_pitch {
                    p.playrate.preserve_pitch = k;
                }
            });
            Ok(playrate_info(session))
        }
    }
}
