use schemars::JsonSchema;
use scythe_core::{ItemId, TrackId};
use scythe_engine::MediaState;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ApiError, TrackRef, json, level, snapshot, time};
use crate::Session;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum ItemCommand {
    /// The items of `track`, or of every track.
    #[serde(rename = "item.list")]
    List {
        #[serde(default)]
        track: Option<TrackRef>,
    },
    /// Moves the item to `position` (seconds) and/or onto `track`.
    #[serde(rename = "item.move")]
    Move {
        /// The item's id.
        item: u64,
        #[serde(default)]
        position: Option<f64>,
        #[serde(default)]
        track: Option<TrackRef>,
    },
    /// Splits the item at `at` (seconds) and answers with both halves.
    #[serde(rename = "item.split")]
    Split {
        /// The item's id.
        item: u64,
        /// Project time in seconds, inside the item.
        at: f64,
    },
    /// Deletes the item.
    #[serde(rename = "item.delete")]
    Delete {
        /// The item's id.
        item: u64,
    },
    /// Sets the item's gain, as gain (`volume`) or in dB (`db`).
    #[serde(rename = "item.volume")]
    Volume {
        /// The item's id.
        item: u64,
        /// Gain: 1.0 is 0 dB.
        #[serde(default)]
        volume: Option<f64>,
        /// Level in dB, instead of `volume`.
        #[serde(default)]
        db: Option<f64>,
    },
    /// Mutes the item, or toggles it when `muted` is left out.
    #[serde(rename = "item.mute")]
    Mute {
        /// The item's id.
        item: u64,
        #[serde(default)]
        muted: Option<bool>,
    },
    /// The item's waveform for drawing: `width` columns from item time `from` to `to`, each
    /// the lowest and highest sample (channels summed, item and take gain applied).
    #[serde(rename = "item.peaks")]
    Peaks {
        /// The item's id.
        item: u64,
        /// Seconds from the item's start; 0 when left out.
        #[serde(default)]
        from: Option<f64>,
        /// Seconds from the item's start; its length when left out.
        #[serde(default)]
        to: Option<f64>,
        /// Columns, 1 to 8192.
        width: usize,
    },
}

const MAX_PEAK_COLUMNS: usize = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeaksState {
    Ready,
    /// The file is still being read; ask again a moment later.
    Loading,
    /// The file is missing or could not be read.
    Offline,
    /// The take has no audio file (MIDI, a click source, an empty item).
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemPeaks {
    pub item: u64,
    pub from: f64,
    pub to: f64,
    pub state: PeaksState,
    /// One per column, empty unless `state` is `ready`; gain from -1 to 1.
    pub min: Vec<f32>,
    pub max: Vec<f32>,
}

fn find(session: &Session, item: u64) -> Result<(TrackId, ItemId), ApiError> {
    let id = ItemId(item);
    session.project.find_item(id).map(|(t, _)| (t, id)).ok_or_else(|| ApiError::not_found(format!("no item {item}")))
}

fn info(session: &Session, id: ItemId) -> Value {
    let p = &session.project;
    let found = p.tracks.iter().find_map(|t| t.item(id).map(|it| snapshot::item(t, it)));
    found.map_or(Value::Null, json)
}

/// Three decimals are finer than any screen draws and keep the answer small.
fn rounded(v: f32) -> f32 {
    (v * 1000.0).round() / 1000.0
}

fn peaks(session: &Session, item: u64, from: Option<f64>, to: Option<f64>, width: usize) -> Result<Value, ApiError> {
    if !(1..=MAX_PEAK_COLUMNS).contains(&width) {
        return Err(ApiError::invalid(format!("width runs from 1 to {MAX_PEAK_COLUMNS} columns")));
    }
    let (track, id) = find(session, item)?;
    let it =
        session.project.track(track).and_then(|t| t.item(id)).ok_or_else(|| ApiError::not_found("no such item"))?;
    let from = time(from.unwrap_or(0.0), "from")?;
    let to = to.map(|t| time(t, "to")).transpose()?.unwrap_or(it.length);
    if to <= from {
        return Err(ApiError::invalid("\"to\" must come after \"from\""));
    }
    let mut answer = ItemPeaks { item, from, to, state: PeaksState::None, min: Vec::new(), max: Vec::new() };
    let Some(take) = it.take() else { return Ok(json(answer)) };
    answer.state = match session.media(take) {
        Some(MediaState::Ready(media)) => {
            let sr = media.audio.sample_rate as f64;
            let media_len = media.audio.frames() as f64 / sr;
            let gain = (it.volume * take.volume) as f32;
            let step = (to - from) / width as f64;
            for column in 0..width {
                let t0 = from + column as f64 * step;
                let t1 = (t0 + step).min(it.length - 1e-9);
                let span = (take.source_time(t0, it.looped, media_len), take.source_time(t1, it.looped, media_len));
                let (lo, hi) = match span {
                    (Some(s0), Some(s1)) => {
                        let (f0, f1) = (s0.min(s1) * sr, s0.max(s1) * sr);
                        media.peaks.range(f0, f1.max(f0 + 1.0))
                    }
                    _ => (0.0, 0.0),
                };
                answer.min.push(rounded((lo * gain).clamp(-1.0, 1.0)));
                answer.max.push(rounded((hi * gain).clamp(-1.0, 1.0)));
            }
            PeaksState::Ready
        }
        Some(MediaState::Failed(_)) => PeaksState::Offline,
        None if take.source.file().is_some() => PeaksState::Loading,
        None => PeaksState::None,
    };
    Ok(json(answer))
}

pub fn execute(session: &mut Session, command: ItemCommand) -> Result<Value, ApiError> {
    match command {
        ItemCommand::List { track } => {
            let only = track.map(|t| t.track(&session.project)).transpose()?;
            let items: Vec<_> = session
                .project
                .tracks
                .iter()
                .filter(|t| only.is_none_or(|id| id == t.id))
                .flat_map(|t| t.items.iter().map(move |it| snapshot::item(t, it)))
                .collect();
            Ok(json(items))
        }
        ItemCommand::Move { item, position, track } => {
            let (_, id) = find(session, item)?;
            let dest = track.map(|t| t.track(&session.project)).transpose()?;
            let position = position.map(|p| time(p, "position")).transpose()?;
            session.edit("Move item", |p| {
                if let Some(dest) = dest {
                    p.move_item_to_track(id, dest);
                }
                if let (Some(pos), Some(it)) = (position, p.tracks.iter_mut().find_map(|t| t.item_mut(id))) {
                    it.position = pos;
                }
                if let Some((t, _)) = p.find_item(id) {
                    p.auto_crossfade(t);
                }
            });
            Ok(info(session, id))
        }
        ItemCommand::Split { item, at } => {
            let (_, id) = find(session, item)?;
            let mut right = None;
            session.edit("Split item", |p| {
                for t in &mut p.tracks {
                    let Some(idx) = t.items.iter().position(|i| i.id == id) else { continue };
                    if let Some(r) = t.items[idx].split_at(at) {
                        right = Some(r.id);
                        t.items.insert(idx + 1, r);
                    }
                    return;
                }
            });
            let Some(right) = right else {
                return Err(ApiError::invalid("the split point must lie inside the item"));
            };
            Ok(json([info(session, id), info(session, right)]))
        }
        ItemCommand::Delete { item } => {
            let (track, id) = find(session, item)?;
            session.edit("Delete items", |p| {
                if let Some(t) = p.track_mut(track) {
                    t.items.retain(|i| i.id != id);
                }
            });
            Ok(super::done())
        }
        ItemCommand::Volume { item, volume, db } => {
            let gain = level(volume, db)?;
            let (_, id) = find(session, item)?;
            session.edit_merging("Item volume", &format!("item:{item}"), |p| {
                if let Some(it) = p.tracks.iter_mut().find_map(|t| t.item_mut(id)) {
                    it.volume = gain;
                }
            });
            Ok(info(session, id))
        }
        ItemCommand::Mute { item, muted } => {
            let (_, id) = find(session, item)?;
            session.edit("Toggle item mute", |p| {
                if let Some(it) = p.tracks.iter_mut().find_map(|t| t.item_mut(id)) {
                    it.muted = muted.unwrap_or(!it.muted);
                }
            });
            Ok(info(session, id))
        }
        ItemCommand::Peaks { item, from, to, width } => peaks(session, item, from, to, width),
    }
}
