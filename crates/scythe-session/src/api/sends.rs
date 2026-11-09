use scythe_core::{Send, Track, TrackId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ApiError, TrackRef, json, level, pan, snapshot};
use crate::Session;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd")]
pub enum SendCommand {
    /// The sends of `track`.
    #[serde(rename = "send.list")]
    List { track: TrackRef },
    /// Sends `track` to `dest` and answers with the new send.
    #[serde(rename = "send.add")]
    Add { track: TrackRef, dest: TrackRef },
    /// Removes send number `send` (its index in `send.list`).
    #[serde(rename = "send.remove")]
    Remove { track: TrackRef, send: usize },
    #[serde(rename = "send.volume")]
    Volume {
        track: TrackRef,
        send: usize,
        #[serde(default)]
        volume: Option<f64>,
        #[serde(default)]
        db: Option<f64>,
    },
    #[serde(rename = "send.pan")]
    Pan { track: TrackRef, send: usize, pan: f64 },
    #[serde(rename = "send.mute")]
    Mute {
        track: TrackRef,
        send: usize,
        #[serde(default)]
        muted: Option<bool>,
    },
}

fn sends(session: &Session, id: TrackId) -> Value {
    session.project.track(id).map_or(Value::Null, |t| json(snapshot::send_list(t)))
}

fn send_info(session: &Session, id: TrackId, index: usize) -> Value {
    let sends = session.project.track(id).map(snapshot::send_list).unwrap_or_default();
    sends.into_iter().nth(index).map_or(Value::Null, json)
}

/// The track and a send index that exists on it.
fn existing(session: &Session, track: &TrackRef, send: usize) -> Result<TrackId, ApiError> {
    let id = track.track(&session.project)?;
    let count = session.project.track(id).map_or(0, |t| t.sends.len());
    if send >= count {
        return Err(ApiError::not_found(format!("the track has {count} send(s); there is no send {send}")));
    }
    Ok(id)
}

fn with_send(t: Option<&mut Track>, send: usize, f: impl FnOnce(&mut Send)) {
    if let Some(s) = t.and_then(|t| t.sends.get_mut(send)) {
        f(s);
    }
}

pub fn execute(session: &mut Session, command: SendCommand) -> Result<Value, ApiError> {
    match command {
        SendCommand::List { track } => {
            let id = track.track(&session.project)?;
            Ok(sends(session, id))
        }
        SendCommand::Add { track, dest } => {
            let id = track.track(&session.project)?;
            let dest = dest.track(&session.project)?;
            if id == dest {
                return Err(ApiError::invalid("a track cannot send to itself"));
            }
            session.edit("Add send", |p| {
                if let Some(t) = p.track_mut(id) {
                    t.sends.push(Send::new(dest));
                }
            });
            let index = session.project.track(id).map_or(0, |t| t.sends.len() - 1);
            Ok(send_info(session, id, index))
        }
        SendCommand::Remove { track, send } => {
            let id = existing(session, &track, send)?;
            session.edit("Remove send", |p| {
                if let Some(t) = p.track_mut(id) {
                    t.sends.remove(send);
                }
            });
            Ok(sends(session, id))
        }
        SendCommand::Volume { track, send, volume, db } => {
            let gain = level(volume, db)?;
            let id = existing(session, &track, send)?;
            session.edit_merging("Send volume", &format!("send:{}:{send}", id.0), |p| {
                with_send(p.track_mut(id), send, |s| s.volume = gain);
            });
            Ok(send_info(session, id, send))
        }
        SendCommand::Pan { track, send, pan: value } => {
            let value = pan(value)?;
            let id = existing(session, &track, send)?;
            session.edit_merging("Send pan", &format!("send-pan:{}:{send}", id.0), |p| {
                with_send(p.track_mut(id), send, |s| s.pan = value);
            });
            Ok(send_info(session, id, send))
        }
        SendCommand::Mute { track, send, muted } => {
            let id = existing(session, &track, send)?;
            session.edit("Toggle send mute", |p| {
                with_send(p.track_mut(id), send, |s| s.muted = muted.unwrap_or(!s.muted));
            });
            Ok(send_info(session, id, send))
        }
    }
}
