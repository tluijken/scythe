use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ApiError, Target, TrackRef, json, level, pan, snapshot};
use crate::Session;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum TrackCommand {
    /// Every track with its level, pan, flags, effects, sends and items.
    #[serde(rename = "track.list")]
    List,
    /// One track, or the master.
    #[serde(rename = "track.get")]
    Get { track: TrackRef },
    /// Adds a track at `index` (at the end when left out) and answers with it.
    #[serde(rename = "track.add")]
    Add {
        /// "Track N" when left out.
        #[serde(default)]
        name: Option<String>,
        /// Position from the top, counted from 0.
        #[serde(default)]
        index: Option<usize>,
    },
    /// Deletes the track with its items, effects and sends.
    #[serde(rename = "track.delete")]
    Delete { track: TrackRef },
    #[serde(rename = "track.rename")]
    Rename { track: TrackRef, name: String },
    /// Moves the track to position `index` from the top.
    #[serde(rename = "track.move")]
    Move {
        track: TrackRef,
        /// The new position, counted from 0.
        index: usize,
    },
    /// Sets the fader, as gain (`volume`) or in dB (`db`); capped at +24 dB.
    #[serde(rename = "track.volume")]
    Volume {
        track: TrackRef,
        /// Gain: 1.0 is 0 dB, 0.5 about -6 dB.
        #[serde(default)]
        volume: Option<f64>,
        /// Level in dB, instead of `volume`.
        #[serde(default)]
        db: Option<f64>,
    },
    #[serde(rename = "track.pan")]
    Pan {
        track: TrackRef,
        /// From -1 (left) through 0 (centre) to 1 (right).
        pan: f64,
    },
    /// Sets mute, or toggles it when `muted` is left out.
    #[serde(rename = "track.mute")]
    Mute {
        track: TrackRef,
        #[serde(default)]
        muted: Option<bool>,
    },
    /// Sets solo, or toggles it when `solo` is left out. Followers in the track's solo groups
    /// follow.
    #[serde(rename = "track.solo")]
    Solo {
        track: TrackRef,
        #[serde(default)]
        solo: Option<bool>,
        /// "in_place" (default: the track is heard through its sends and the tracks they reach)
        /// or "ignore_routing" (only the track itself); kept when left out.
        #[serde(default)]
        mode: Option<String>,
    },
    /// Solo defeat: while other tracks are soloed the track stays heard (unless muted), so
    /// soloing never silences a monitor mix. Toggles when `on` is left out.
    #[serde(rename = "track.solo_defeat")]
    SoloDefeat {
        track: TrackRef,
        #[serde(default)]
        on: Option<bool>,
    },
    /// Inverts the track's polarity at its fader (pre-fader sends keep the original). Toggles
    /// when `phase` is left out.
    #[serde(rename = "track.phase")]
    Phase {
        track: TrackRef,
        #[serde(default)]
        phase: Option<bool>,
    },
    /// The track's pan mode.
    #[serde(rename = "track.pan_mode")]
    PanMode {
        track: TrackRef,
        /// "balance" (stereo balance / mono pan), "classic" (REAPER 3.x balance), "stereo"
        /// (stereo pan with width) or "dual" (each channel panned on its own); `null` follows
        /// the project default.
        mode: Option<String>,
    },
    /// Stereo width (every pan mode but dual pan applies it). Width groups follow.
    #[serde(rename = "track.width")]
    Width {
        track: TrackRef,
        /// From -1 (swapped) through 0 (mono) to 1 (unchanged).
        width: f64,
    },
    /// Positions of the left and right channel in dual pan.
    #[serde(rename = "track.dual_pan")]
    DualPan {
        track: TrackRef,
        /// -1 (left) to 1 (right).
        left: f64,
        /// -1 (left) to 1 (right).
        right: f64,
    },
    /// Arms the track for recording, or toggles it when `armed` is left out.
    #[serde(rename = "track.arm")]
    Arm {
        track: TrackRef,
        #[serde(default)]
        armed: Option<bool>,
    },
    /// Input monitoring: an armed track with monitoring on plays its input through its FX.
    /// Toggles when `monitor` is left out.
    #[serde(rename = "track.monitor")]
    Monitor {
        track: TrackRef,
        #[serde(default)]
        monitor: Option<bool>,
    },
    /// The record input, in REAPER's `I_RECINPUT` numbering (see `TrackInfo::input`).
    #[serde(rename = "track.input")]
    Input {
        track: TrackRef,
        /// Inputs counted from 0: mono input n is n, the stereo pair from n is 1024 + n, MIDI is
        /// 4096 + device * 32 + channel (device 63 = all, channel 0 = all), -1 is none.
        input: i64,
    },
    /// Sets the track colour.
    #[serde(rename = "track.color", alias = "track.colour")]
    Color {
        track: TrackRef,
        /// `#rrggbb`, or `null` for no colour.
        color: Option<String>,
    },
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

/// An edit of a track (not the master) that may carry over to its group followers.
fn grouped(
    session: &mut Session,
    track: &TrackRef,
    label: &str,
    f: impl FnOnce(&mut scythe_core::Project, usize),
) -> Result<Value, ApiError> {
    let id = track.track(&session.project)?;
    session.edit(label, |p| {
        if let Some(i) = p.track_index(id) {
            f(p, i);
        }
    });
    Ok(info(session, Target::Track(id)))
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
            session.edit_merging("Track volume", &format!("volume:{}", target.key()), |p| match target {
                Target::Track(id) => {
                    if let Some(i) = p.track_index(id) {
                        p.set_volume_grouped(i, gain);
                    }
                }
                Target::Master => p.master.volume = gain,
            });
            Ok(info(session, target))
        }
        TrackCommand::Pan { track, pan: value } => {
            let value = pan(value)?;
            let target = track.resolve(&session.project)?;
            session.edit_merging("Track pan", &format!("pan:{}", target.key()), |p| match target {
                Target::Track(id) => {
                    if let Some(i) = p.track_index(id) {
                        p.set_pan_grouped(i, value);
                    }
                }
                Target::Master => p.master.pan = value,
            });
            Ok(info(session, target))
        }
        TrackCommand::Mute { track, muted } => match track.resolve(&session.project)? {
            Target::Master => set(session, &track, "Toggle mute", |t| t.muted = flag(muted, t.muted)),
            Target::Track(_) => grouped(session, &track, "Toggle mute", |p, i| {
                let on = flag(muted, p.tracks[i].muted);
                p.set_mute_grouped(i, on);
            }),
        },
        TrackCommand::Solo { track, solo, mode } => {
            let mode = match mode.as_deref() {
                None => None,
                Some("in_place") => Some(scythe_core::SoloMode::InPlace),
                Some("ignore_routing") => Some(scythe_core::SoloMode::IgnoreRouting),
                Some(m) => {
                    return Err(ApiError::invalid(format!("solo mode \"{m}\" is not in_place or ignore_routing")));
                }
            };
            grouped(session, &track, "Toggle solo", |p, i| {
                let on = flag(solo, p.tracks[i].solo);
                if let Some(m) = mode {
                    p.tracks[i].solo_mode = m;
                }
                p.set_solo_grouped(i, on);
            })
        }
        TrackCommand::SoloDefeat { track, on } => {
            track.track(&session.project)?;
            set(session, &track, "Toggle solo defeat", |t| t.solo_defeat = flag(on, t.solo_defeat))
        }
        TrackCommand::Phase { track, phase } => grouped(session, &track, "Toggle polarity", |p, i| {
            let on = flag(phase, p.tracks[i].phase);
            p.set_phase_grouped(i, on);
        }),
        TrackCommand::PanMode { track, mode } => {
            let mode = match mode.as_deref() {
                None => None,
                Some(name) => Some(scythe_core::fader::PanMode::from_name(name).ok_or_else(|| {
                    ApiError::invalid(format!("pan mode \"{name}\" is not balance, classic, stereo or dual"))
                })?),
            };
            track.track(&session.project)?;
            set(session, &track, "Set pan mode", |t| t.pan_mode = mode)
        }
        TrackCommand::Width { track, width } => {
            if !(-1.0..=1.0).contains(&width) {
                return Err(ApiError::invalid("width runs from -1 to 1"));
            }
            let target = track.resolve(&session.project)?;
            let id = track.track(&session.project)?;
            session.edit_merging("Track width", &format!("width:{}", target.key()), |p| {
                if let Some(i) = p.track_index(id) {
                    p.set_width_grouped(i, width);
                }
            });
            Ok(info(session, target))
        }
        TrackCommand::DualPan { track, left, right } => {
            let (left, right) = (pan(left)?, pan(right)?);
            track.track(&session.project)?;
            set(session, &track, "Set dual pan", |t| t.dual_pan = [left, right])
        }
        TrackCommand::Arm { track, armed } => grouped(session, &track, "Toggle record arm", |p, i| {
            let on = flag(armed, p.tracks[i].armed);
            p.set_arm_grouped(i, on);
        }),
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
