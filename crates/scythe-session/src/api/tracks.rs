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
    /// Freezes the track: renders its items through its effects (before the fader) to a file
    /// beside the project and plays that instead, keeping the effects and items to restore.
    /// Freezing again stacks another level. The project must have been saved.
    #[serde(rename = "track.freeze")]
    Freeze { track: TrackRef },
    /// Restores the effects and items of the track's last freeze level.
    #[serde(rename = "track.unfreeze")]
    Unfreeze { track: TrackRef },
    /// Moves the track, a folder with its children, keeping every folder closed: to position
    /// `index` in the track list, or with `parent` into a folder (the master for the top level).
    #[serde(rename = "track.move")]
    Move {
        track: TrackRef,
        /// Without `parent`: the new position in the track list, counted from 0; the track keeps
        /// its nesting where the tracks around it allow. With `parent`: the position among the
        /// folder's other children, the end when left out.
        #[serde(default)]
        index: Option<usize>,
        /// The folder to move the track into, which becomes a folder if it is a plain track;
        /// "master" for the top level.
        #[serde(default)]
        parent: Option<TrackRef>,
        /// More tracks moved along, as REAPER drags every selected track: they keep their
        /// order and go after `track`.
        #[serde(default)]
        also: Vec<TrackRef>,
    },
    /// REAPER's "Move tracks to new folder": a new folder track takes the place of the first
    /// track, and the tracks (folders with their children) become its children. Answers the
    /// folder.
    #[serde(rename = "track.new_folder")]
    NewFolder {
        tracks: Vec<TrackRef>,
        /// "Track N" when left out.
        #[serde(default)]
        name: Option<String>,
    },
    /// How a folder shows its children: `compact` in the track view (0 normal, 1 small, 2
    /// collapsed, as `I_FOLDERCOMPACT`), `mixer` hides them in the mixer. With neither, the
    /// track view cycles normal, small, collapsed (REAPER's "Cycle folder collapsed state").
    /// Not an undo step, as in REAPER.
    #[serde(rename = "track.collapse")]
    Collapse {
        track: TrackRef,
        #[serde(default)]
        compact: Option<u8>,
        #[serde(default)]
        mixer: Option<bool>,
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
    /// Makes the record input MIDI: a device (`"all"`, `"keyboard"` for the virtual MIDI
    /// keyboard, or a device number from `midi.devices`) and a channel.
    #[serde(rename = "track.midi_input")]
    MidiInput {
        track: TrackRef,
        /// `"all"`, `"keyboard"` or a device number; all when left out.
        #[serde(default)]
        device: Option<MidiDeviceRef>,
        /// 1-16 listens to one channel; 0 or left out to all.
        #[serde(default)]
        channel: Option<u8>,
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
    /// The track's (or the master's) audio channels (REAPER's `NCHAN`): an even number from 2
    /// to 128. Channels 3/4 are the sidechain input of its effects.
    #[serde(rename = "track.channels")]
    Channels { track: TrackRef, channels: u32 },
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
        TrackCommand::Freeze { track } => {
            let id = track.track(&session.project)?;
            let path = session.freeze_track(id).map_err(ApiError::failed)?;
            Ok(
                serde_json::json!({ "file": path, "levels": session.project.track(id).map_or(0, scythe_core::freeze::levels) }),
            )
        }
        TrackCommand::Unfreeze { track } => {
            let id = track.track(&session.project)?;
            if !session.unfreeze_track(id) {
                return Err(ApiError::invalid("the track is not frozen"));
            }
            Ok(serde_json::json!({ "levels": session.project.track(id).map_or(0, scythe_core::freeze::levels) }))
        }
        TrackCommand::Move { track, index, parent, also } => {
            let id = track.track(&session.project)?;
            let mut ids = vec![id];
            for t in &also {
                let t = t.track(&session.project)?;
                if !ids.contains(&t) {
                    ids.push(t);
                }
            }
            let label = if ids.len() > 1 { "Move tracks" } else { "Move track" };
            match parent {
                Some(parent) => {
                    let parent = match parent.resolve(&session.project)? {
                        Target::Master => None,
                        Target::Track(p) => Some(p),
                    };
                    let mut fits = None;
                    session.edit(label, |p| fits = p.move_into(&ids, parent, index));
                    if fits.is_none() {
                        return Err(ApiError::invalid("a folder cannot move into itself"));
                    }
                }
                None => {
                    let n = session.project.tracks.len();
                    let Some(index) = index.filter(|&i| i < n) else {
                        return Err(ApiError::invalid(format!("index runs from 0 to {}", n - 1)));
                    };
                    session.edit(label, |p| {
                        p.move_to_index(&ids, index);
                    });
                }
            }
            Ok(info(session, Target::Track(id)))
        }
        TrackCommand::NewFolder { tracks, name } => {
            let ids = tracks.iter().map(|t| t.track(&session.project)).collect::<Result<Vec<_>, _>>()?;
            let Some(&first) = session.project.with_children(&ids).first() else {
                return Err(ApiError::invalid("name at least one track"));
            };
            let name = name.unwrap_or_else(|| format!("Track {}", first + 1));
            let mut folder = None;
            session.edit("Change track order", |p| {
                p.add_track(name);
                let t = p.tracks.pop().expect("the track just added");
                folder = p.move_to_new_folder(&ids, t);
            });
            Ok(folder.map_or(Value::Null, |f| info(session, Target::Track(f))))
        }
        TrackCommand::Collapse { track, compact, mixer } => {
            let id = track.track(&session.project)?;
            if compact.is_some_and(|c| c > 2) {
                return Err(ApiError::invalid("compact is 0 (normal), 1 (small) or 2 (collapsed)"));
            }
            session.change_view(|p| {
                if compact.is_none() && mixer.is_none() {
                    p.cycle_folder_compact(id);
                }
                if let Some(t) = p.track_mut(id) {
                    t.folder_compact = compact.unwrap_or(t.folder_compact);
                    t.mixer_collapsed = mixer.unwrap_or(t.mixer_collapsed);
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
        TrackCommand::MidiInput { track, device, channel } => {
            let channel = channel.unwrap_or(0);
            if channel > 16 {
                return Err(ApiError::invalid("channels run from 1 to 16, 0 for all"));
            }
            let device = match device {
                None | Some(MidiDeviceRef::Name(MidiDeviceName::All)) => scythe_core::midi_route::MidiDevice::All,
                Some(MidiDeviceRef::Name(MidiDeviceName::Keyboard)) => {
                    scythe_core::midi_route::MidiDevice::VirtualKeyboard
                }
                Some(MidiDeviceRef::Number(n)) if n < 62 || (64..=127).contains(&n) => {
                    scythe_core::midi_route::MidiDevice::Port(n)
                }
                Some(MidiDeviceRef::Number(_)) => {
                    return Err(ApiError::invalid(
                        "device numbers run from 0 to 127; 62 and 63 are \"keyboard\" and \"all\"",
                    ));
                }
            };
            let input = scythe_core::midi_route::MidiInput { device, channel: channel.checked_sub(1) };
            set(session, &track, "Set record input", |t| t.rec_input = input.rec_input())
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
        TrackCommand::Channels { track, channels } => {
            if channels % 2 != 0 || !(2..=128).contains(&channels) {
                return Err(ApiError::invalid("channels is an even number from 2 to 128"));
            }
            set(session, &track, "Set track channels", |t| t.channels = channels)
        }
        TrackCommand::MainSend { track, on } => {
            track.track(&session.project)?;
            set(session, &track, "Toggle master/parent send", |t| t.main_send = on.unwrap_or(!t.main_send))
        }
    }
}

/// A MIDI input device: `"all"`, `"keyboard"` or a device number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum MidiDeviceRef {
    Name(MidiDeviceName),
    Number(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MidiDeviceName {
    All,
    Keyboard,
}
