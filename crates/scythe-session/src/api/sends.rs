use schemars::JsonSchema;
use scythe_core::{HwOut, Send, SendMode, Track, TrackId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::snapshot::Tap;
use super::{ApiError, Target, TrackRef, json, level, pan, snapshot};
use crate::Session;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum SendCommand {
    /// The sends of `track`.
    #[serde(rename = "send.list")]
    List { track: TrackRef },
    /// Sends `track` to `dest` and answers with the new send.
    #[serde(rename = "send.add")]
    Add {
        track: TrackRef,
        /// The track receiving the send.
        dest: TrackRef,
    },
    /// Removes send number `send` (its index in `send.list`).
    #[serde(rename = "send.remove")]
    Remove {
        track: TrackRef,
        /// The send's index in `send.list`, from 0.
        send: usize,
    },
    /// Sets the send level, as gain (`volume`) or in dB (`db`).
    #[serde(rename = "send.volume")]
    Volume {
        track: TrackRef,
        /// The send's index in `send.list`, from 0.
        send: usize,
        /// Gain: 1.0 is 0 dB.
        #[serde(default)]
        volume: Option<f64>,
        /// Level in dB, instead of `volume`.
        #[serde(default)]
        db: Option<f64>,
    },
    #[serde(rename = "send.pan")]
    Pan {
        track: TrackRef,
        /// The send's index in `send.list`, from 0.
        send: usize,
        /// From -1 (left) to 1 (right).
        pan: f64,
    },
    /// Mutes the send, or toggles it when `muted` is left out.
    #[serde(rename = "send.mute")]
    Mute {
        track: TrackRef,
        /// The send's index in `send.list`, from 0.
        send: usize,
        #[serde(default)]
        muted: Option<bool>,
    },
    /// Where the send taps the track (`post_fader`, `pre_fx`, `post_fx`), and optionally its
    /// mono downmix and polarity.
    #[serde(rename = "send.mode")]
    Mode {
        track: TrackRef,
        /// The send's index in `send.list`, from 0.
        send: usize,
        #[serde(default)]
        mode: Option<Tap>,
        /// Mix down to mono before panning.
        #[serde(default)]
        mono: Option<bool>,
        /// Invert the polarity.
        #[serde(default)]
        phase: Option<bool>,
    },
    /// What MIDI the send carries (REAPER's `I_MIDIFLAGS`); fields left out stay. Channels and
    /// buses count from 1; 0 means all (for sources) or unchanged (for destinations).
    #[serde(rename = "send.midi")]
    Midi {
        track: TrackRef,
        /// The send's index in `send.list`, from 0.
        send: usize,
        /// Carry MIDI at all.
        #[serde(default)]
        enabled: Option<bool>,
        /// Audio too: false makes it a MIDI-only send, true restores channels 1/2.
        #[serde(default)]
        audio: Option<bool>,
        /// Only this channel (1-16); 0 for all.
        #[serde(default)]
        src_channel: Option<u8>,
        /// Move every message to this channel (1-16); 0 keeps each message's own.
        #[serde(default)]
        dst_channel: Option<u8>,
        /// Only this MIDI bus (1-16); 0 for all.
        #[serde(default)]
        src_bus: Option<u8>,
        /// Arrive on this MIDI bus (1-16); 0 keeps each message's own.
        #[serde(default)]
        dst_bus: Option<u8>,
        /// Send fader moves as MIDI volume and pan.
        #[serde(default)]
        faders: Option<bool>,
    },
    /// The audio hardware outputs of a track or `"master"`.
    #[serde(rename = "send.hw_list")]
    HwList { track: TrackRef },
    /// Adds a hardware output from device output `output` (counted from 0; the next free pair
    /// or channel when left out), mono when `mono`, and answers with it.
    #[serde(rename = "send.hw_add")]
    HwAdd {
        track: TrackRef,
        /// The first device output, from 0.
        #[serde(default)]
        output: Option<usize>,
        #[serde(default)]
        mono: bool,
    },
    /// Removes hardware output number `hw`.
    #[serde(rename = "send.hw_remove")]
    HwRemove {
        track: TrackRef,
        /// The hardware output's index in `send.hw_list`, from 0.
        hw: usize,
    },
    /// Changes hardware output number `hw`; fields left out stay.
    #[serde(rename = "send.hw_set")]
    HwSet {
        track: TrackRef,
        /// The hardware output's index in `send.hw_list`, from 0.
        hw: usize,
        /// The first device output, from 0.
        #[serde(default)]
        output: Option<usize>,
        #[serde(default)]
        mono: Option<bool>,
        #[serde(default)]
        mode: Option<Tap>,
        /// Gain: 1.0 is 0 dB.
        #[serde(default)]
        volume: Option<f64>,
        /// Level in dB, instead of `volume`.
        #[serde(default)]
        db: Option<f64>,
        /// From -1 (left) to 1 (right).
        #[serde(default)]
        pan: Option<f64>,
        #[serde(default)]
        muted: Option<bool>,
        #[serde(default)]
        phase: Option<bool>,
    },
}

/// Device outputs a hardware output may start at.
const MAX_OUTPUT: usize = 511;

fn hw_outs(session: &Session, target: Target) -> Value {
    target.get(&session.project).map_or(Value::Null, |t| json(snapshot::hw_out_list(t)))
}

/// The track (or master) and a hardware output index that exists on it.
fn existing_hw(session: &Session, track: &TrackRef, hw: usize) -> Result<Target, ApiError> {
    let target = track.resolve(&session.project)?;
    let count = target.get(&session.project).map_or(0, |t| t.hw_outs.len());
    if hw >= count {
        return Err(ApiError::not_found(format!("the track has {count} hardware output(s); there is no output {hw}")));
    }
    Ok(target)
}

fn output_flags(output: usize, mono: bool) -> Result<i64, ApiError> {
    if output > MAX_OUTPUT {
        return Err(ApiError::invalid(format!("device outputs run from 0 to {MAX_OUTPUT}")));
    }
    Ok(output as i64 | if mono { HwOut::MONO } else { 0 })
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
        SendCommand::Mode { track, send, mode, mono, phase } => {
            let id = existing(session, &track, send)?;
            session.edit("Set send mode", |p| {
                with_send(p.track_mut(id), send, |s| {
                    s.mode = mode.map_or(s.mode, SendMode::from);
                    s.mono = mono.unwrap_or(s.mono);
                    s.phase = phase.unwrap_or(s.phase);
                });
            });
            Ok(send_info(session, id, send))
        }
        SendCommand::Midi { track, send, enabled, audio, src_channel, dst_channel, src_bus, dst_bus, faders } => {
            let id = existing(session, &track, send)?;
            for v in [src_channel, dst_channel, src_bus, dst_bus].into_iter().flatten() {
                if v > 16 {
                    return Err(ApiError::invalid("channels and buses run from 1 to 16, 0 for all"));
                }
            }
            let pick = |v: Option<u8>, old: Option<u8>| v.map_or(old, |v| v.checked_sub(1));
            session.edit("Set send MIDI", |p| {
                with_send(p.track_mut(id), send, |s| {
                    let old = s.midi();
                    s.set_midi(scythe_core::midi_route::SendMidi {
                        enabled: enabled.unwrap_or(old.enabled),
                        src_channel: pick(src_channel, old.src_channel),
                        dst_channel: pick(dst_channel, old.dst_channel),
                        src_bus: pick(src_bus, old.src_bus),
                        dst_bus: pick(dst_bus, old.dst_bus),
                        faders: faders.unwrap_or(old.faders),
                    });
                    match audio {
                        Some(false) => s.src_channels = -1,
                        Some(true) if s.src_channels < 0 => s.src_channels = 0,
                        _ => {}
                    }
                });
            });
            Ok(send_info(session, id, send))
        }
        SendCommand::HwList { track } => {
            let target = track.resolve(&session.project)?;
            Ok(hw_outs(session, target))
        }
        SendCommand::HwAdd { track, output, mono } => {
            let target = track.resolve(&session.project)?;
            let width = if mono { 1 } else { 2 };
            let output = output.unwrap_or_else(|| session.project.next_free_output(width) as usize);
            let dst = output_flags(output, mono)?;
            session.edit("Add hardware output", |p| {
                if let Some(t) = target.get_mut(p) {
                    t.hw_outs.push(HwOut { dst_channels: dst, ..HwOut::default() });
                }
            });
            let t = target.get(&session.project).unwrap();
            Ok(json(snapshot::hw_out(t.hw_outs.len() - 1, &t.hw_outs[t.hw_outs.len() - 1])))
        }
        SendCommand::HwRemove { track, hw } => {
            let target = existing_hw(session, &track, hw)?;
            session.edit("Remove hardware output", |p| {
                if let Some(t) = target.get_mut(p) {
                    t.hw_outs.remove(hw);
                }
            });
            Ok(hw_outs(session, target))
        }
        SendCommand::HwSet { track, hw, output, mono, mode, volume, db, pan: pan_value, muted, phase } => {
            let target = existing_hw(session, &track, hw)?;
            let gain = if volume.is_some() || db.is_some() { Some(level(volume, db)?) } else { None };
            let pan_value = pan_value.map(pan).transpose()?;
            let old = target.get(&session.project).unwrap().hw_outs[hw].clone();
            let dst = match (output, mono) {
                (None, None) => None,
                (o, m) => {
                    let flags = old.dst_channels & !(HwOut::MONO | 511);
                    Some(flags | output_flags(o.unwrap_or(old.first_output()), m.unwrap_or(old.is_mono()))?)
                }
            };
            let continuous = gain.is_some() || pan_value.is_some();
            let edit = |p: &mut scythe_core::Project| {
                if let Some(o) = target.get_mut(p).and_then(|t| t.hw_outs.get_mut(hw)) {
                    o.dst_channels = dst.unwrap_or(o.dst_channels);
                    o.mode = mode.map_or(o.mode, SendMode::from);
                    o.volume = gain.unwrap_or(o.volume);
                    o.pan = pan_value.unwrap_or(o.pan);
                    o.muted = muted.unwrap_or(o.muted);
                    o.phase = phase.unwrap_or(o.phase);
                }
            };
            if continuous && dst.is_none() && mode.is_none() && muted.is_none() && phase.is_none() {
                let key = format!("hw:{}:{hw}", target.key());
                session.edit_merging("Hardware output level", &key, edit);
            } else {
                session.edit("Set hardware output", edit);
            }
            let t = target.get(&session.project).unwrap();
            Ok(json(snapshot::hw_out(hw, &t.hw_outs[hw])))
        }
    }
}
