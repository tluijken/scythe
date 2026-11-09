use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ApiError, Target, TrackRef, json, level, pan, snapshot};
use crate::Session;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd")]
pub enum TrackCommand {
    #[serde(rename = "track.list")]
    List,
    #[serde(rename = "track.get")]
    Get { track: TrackRef },
    /// Adds a track at `index` (at the end when left out) and answers with it.
    #[serde(rename = "track.add")]
    Add {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        index: Option<usize>,
    },
    #[serde(rename = "track.delete")]
    Delete { track: TrackRef },
    #[serde(rename = "track.rename")]
    Rename { track: TrackRef, name: String },
    /// Moves the track to position `index` from the top.
    #[serde(rename = "track.move")]
    Move { track: TrackRef, index: usize },
    #[serde(rename = "track.volume")]
    Volume {
        track: TrackRef,
        #[serde(default)]
        volume: Option<f64>,
        #[serde(default)]
        db: Option<f64>,
    },
    #[serde(rename = "track.pan")]
    Pan { track: TrackRef, pan: f64 },
    /// Sets mute, or toggles it when `muted` is left out.
    #[serde(rename = "track.mute")]
    Mute {
        track: TrackRef,
        #[serde(default)]
        muted: Option<bool>,
    },
    #[serde(rename = "track.solo")]
    Solo {
        track: TrackRef,
        #[serde(default)]
        solo: Option<bool>,
    },
    #[serde(rename = "track.arm")]
    Arm {
        track: TrackRef,
        #[serde(default)]
        armed: Option<bool>,
    },
    /// Input monitoring: an armed track with monitoring on plays its input through its FX.
    #[serde(rename = "track.monitor")]
    Monitor {
        track: TrackRef,
        #[serde(default)]
        monitor: Option<bool>,
    },
    /// The record input, in REAPER's `I_RECINPUT` numbering (see `TrackInfo::input`).
    #[serde(rename = "track.input")]
    Input { track: TrackRef, input: i64 },
    /// `#rrggbb`, or `null` for no colour.
    #[serde(rename = "track.color", alias = "track.colour")]
    Color { track: TrackRef, color: Option<String> },
    /// REAPER's folder depth change (`I_FOLDERDEPTH`): 1 makes the track a folder holding the
    /// tracks below it, -n makes it the last track of n folders, 0 a plain track.
    #[serde(rename = "track.folder")]
    Folder { track: TrackRef, depth: i32 },
    /// Whether the track feeds its parent or the master; off for a bus that only plays through
    /// its hardware outputs. Toggles when `on` is left out.
    #[serde(rename = "track.main_send")]
    MainSend {
        track: TrackRef,
        #[serde(default)]
        on: Option<bool>,
    },
}

fn info(session: &Session, target: Target) -> Value {
    let p = &session.project;
    match target {
        Target::Master => json(snapshot::track(session, &p.master, None, 0)),
        Target::Track(id) => {
            let index = p.track_index(id).unwrap_or(0);
            let level = p.folder_levels().get(index).copied().unwrap_or(0);
            p.track(id).map_or(Value::Null, |t| json(snapshot::track(session, t, Some(index), level)))
        }
    }
}

/// Flips `current` when no value is given.
fn flag(value: Option<bool>, current: bool) -> bool {
    value.unwrap_or(!current)
}

fn set(
    session: &mut Session,
    track: &TrackRef,
    label: &str,
    f: impl FnOnce(&mut scythe_core::Track),
) -> Result<Value, ApiError> {
    let target = track.resolve(&session.project)?;
    session.edit(label, |p| {
        if let Some(t) = target.get_mut(p) {
            f(t);
        }
    });
    Ok(info(session, target))
}

pub fn execute(session: &mut Session, command: TrackCommand) -> Result<Value, ApiError> {
    match command {
        TrackCommand::List => {
            let p = &session.project;
            let levels = p.folder_levels();
            let tracks: Vec<_> =
                p.tracks.iter().enumerate().map(|(i, t)| snapshot::track(session, t, Some(i), levels[i])).collect();
            Ok(json(tracks))
        }
        TrackCommand::Get { track } => {
            let target = track.resolve(&session.project)?;
            Ok(info(session, target))
        }
        TrackCommand::Add { name, index } => {
            let n = session.project.tracks.len();
            if index.is_some_and(|i| i > n) {
                return Err(ApiError::invalid(format!("index runs from 0 to {n}")));
            }
            let name = name.unwrap_or_else(|| format!("Track {}", n + 1));
            let mut id = scythe_core::TrackId(0);
            session.edit("Add track", |p| {
                id = p.add_track(name);
                if let Some(at) = index {
                    let t = p.tracks.pop().expect("the track just added");
                    p.tracks.insert(at, t);
                }
            });
            Ok(info(session, Target::Track(id)))
        }
        TrackCommand::Delete { track } => {
            let id = track.track(&session.project)?;
            session.edit("Delete track", |p| {
                p.remove_track(id);
            });
            Ok(super::done())
        }
        TrackCommand::Rename { track, name } => set(session, &track, "Rename track", |t| t.name = name),
        TrackCommand::Move { track, index } => {
            let id = track.track(&session.project)?;
            let n = session.project.tracks.len();
            if index >= n {
                return Err(ApiError::invalid(format!("index runs from 0 to {}", n - 1)));
            }
            session.edit("Move track", |p| {
                if let Some(from) = p.track_index(id) {
                    let t = p.tracks.remove(from);
                    p.tracks.insert(index, t);
                }
            });
            Ok(info(session, Target::Track(id)))
        }
        TrackCommand::Volume { track, volume, db } => {
            let gain = level(volume, db)?;
            let target = track.resolve(&session.project)?;
            session.edit_merging("Track volume", &format!("volume:{}", target.key()), |p| {
                if let Some(t) = target.get_mut(p) {
                    t.volume = gain;
                }
            });
            Ok(info(session, target))
        }
        TrackCommand::Pan { track, pan: value } => {
            let value = pan(value)?;
            let target = track.resolve(&session.project)?;
            session.edit_merging("Track pan", &format!("pan:{}", target.key()), |p| {
                if let Some(t) = target.get_mut(p) {
                    t.pan = value;
                }
            });
            Ok(info(session, target))
        }
        TrackCommand::Mute { track, muted } => set(session, &track, "Toggle mute", |t| t.muted = flag(muted, t.muted)),
        TrackCommand::Solo { track, solo } => {
            track.track(&session.project)?;
            set(session, &track, "Toggle solo", |t| t.solo = flag(solo, t.solo))
        }
        TrackCommand::Arm { track, armed } => {
            track.track(&session.project)?;
            set(session, &track, "Toggle record arm", |t| t.armed = flag(armed, t.armed))
        }
        TrackCommand::Monitor { track, monitor } => {
            track.track(&session.project)?;
            set(session, &track, "Toggle input monitoring", |t| t.monitor = flag(monitor, t.monitor))
        }
        TrackCommand::Input { track, input } => {
            track.track(&session.project)?;
            set(session, &track, "Set record input", |t| t.rec_input = input)
        }
        TrackCommand::Color { track, color } => {
            let color = match color.as_deref() {
                None => None,
                Some(text) => {
                    Some(snapshot::parse_color(text).ok_or_else(|| ApiError::invalid("colours are written #rrggbb"))?)
                }
            };
            set(session, &track, "Track colour", |t| t.color = color)
        }
        TrackCommand::Folder { track, depth } => {
            track.track(&session.project)?;
            set(session, &track, "Set folder depth", |t| t.folder_depth = depth)
        }
        TrackCommand::MainSend { track, on } => {
            track.track(&session.project)?;
            set(session, &track, "Toggle master/parent send", |t| t.main_send = on.unwrap_or(!t.main_send))
        }
    }
}
