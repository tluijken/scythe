use schemars::JsonSchema;
use scythe_core::{ItemId, TrackId};
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
    }
}
