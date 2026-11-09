//! Recording settings and fixed lanes: each track's record mode and input monitoring, the
//! project's record mode and the lane options of the preferences, and REAPER 7's fixed item
//! lanes with comping (comp lane, swiping comp areas, which lanes play).

use schemars::JsonSchema;
use scythe_core::prefs::{AUTOXFADE_LANES_EXCLUSIVE, AUTOXFADE_LOOP_TAKES, AUTOXFADE_RECORD_LANES};
use scythe_core::recording::{MonitorMode, ProjectRecordMode, RecordMode, RecordTap};
use scythe_core::{Track, TrackId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ApiError, TrackRef, json};
use crate::Session;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum RecordingCommand {
    /// What an armed track records, and where output recording taps it. Answers with the
    /// track's recording settings.
    #[serde(rename = "track.record_mode")]
    RecordMode {
        track: TrackRef,
        /// "input" (audio or MIDI as the input is), "input_mono", "input_stereo",
        /// "input_multichannel", "input_midi", "output_stereo", "output_stereo_compensated",
        /// "output_mono", "output_mono_compensated", "output_multichannel",
        /// "output_multichannel_compensated", "midi_output", "midi_overdub", "midi_replace",
        /// "midi_touch_replace", "midi_latch_replace" or "disabled" (monitor only).
        #[serde(default)]
        mode: Option<String>,
        /// Where output recording taps the track: "post_fader", "pre_fx" or "post_fx" (after the
        /// effects, before the fader).
        #[serde(default)]
        tap: Option<String>,
        /// Arm the track whenever it is selected.
        #[serde(default)]
        auto_arm: Option<bool>,
    },
    /// Input monitoring of an armed track: "off", "on" or "auto" (tape style: while stopped
    /// or recording, not while playing back). `items` plays the track's own items while it
    /// records (otherwise they fall silent). Answers with the track's recording settings.
    #[serde(rename = "track.monitor_mode")]
    MonitorMode {
        track: TrackRef,
        #[serde(default)]
        mode: Option<String>,
        #[serde(default)]
        items: Option<bool>,
    },
    /// The project's record mode and the recording preferences; what is left out stays.
    #[serde(rename = "record.options")]
    Options {
        /// "normal", "time_selection_punch" (keep what falls inside the time selection) or
        /// "item_punch" (record into the selected items as new takes).
        #[serde(default)]
        mode: Option<String>,
        /// Recording on a fixed-lane track puts each pass in a new lane (else passes become
        /// takes in the lane recorded into).
        #[serde(default)]
        new_lanes: Option<bool>,
        /// Loop recording makes takes even when new recording adds lanes.
        #[serde(default)]
        loop_takes: Option<bool>,
        /// New lanes play alone (else in layers with the lanes above).
        #[serde(default)]
        lanes_exclusive: Option<bool>,
    },
    /// The track's fixed lanes: names, which play, the comp lane and its comp areas.
    #[serde(rename = "lanes.list")]
    List { track: TrackRef },
    /// Turns fixed item lanes on or off for the track.
    #[serde(rename = "lanes.fixed")]
    Fixed { track: TrackRef, on: bool },
    /// Adds an empty lane at the bottom.
    #[serde(rename = "lanes.add")]
    Add { track: TrackRef },
    /// Plays only these lanes (counted from 0 at the top); all lanes when empty.
    #[serde(rename = "lanes.play")]
    Play { track: TrackRef, lanes: Vec<usize> },
    /// Comp into a new empty lane at the top: from now on swiping fills it.
    #[serde(rename = "lanes.comp")]
    Comp { track: TrackRef },
    /// Ends comping; the comp lane stays as an ordinary lane.
    #[serde(rename = "lanes.comp_off")]
    CompOff { track: TrackRef },
    /// Swipes `start`..`end` (seconds) of `lane` into the comp lane, as dragging across a
    /// lane does: the new comp area wins its range, neighbours cross-fade into it.
    #[serde(rename = "lanes.swipe")]
    Swipe { track: TrackRef, lane: usize, start: f64, end: f64 },
    /// Removes the comp area at `at` seconds, leaving a gap in the comp.
    #[serde(rename = "lanes.remove_area")]
    RemoveArea { track: TrackRef, at: f64 },
}

#[derive(Serialize)]
struct RecordInfo {
    track: u64,
    armed: bool,
    mode: &'static str,
    tap: &'static str,
    monitor: &'static str,
    monitor_items: bool,
    auto_arm: bool,
}

#[derive(Serialize)]
struct AreaInfo {
    start: f64,
    end: f64,
    lane: usize,
}

#[derive(Serialize)]
struct LanesInfo {
    track: u64,
    fixed: bool,
    names: Vec<String>,
    playing: Vec<usize>,
    comp_lane: Option<usize>,
    areas: Vec<AreaInfo>,
    /// Items per lane.
    items: Vec<usize>,
}

fn record_info(t: &Track) -> Value {
    json(RecordInfo {
        track: t.id.0,
        armed: t.armed,
        mode: t.rec_mode.name(),
        tap: t.rec_tap.name(),
        monitor: t.monitor_mode().name(),
        monitor_items: t.monitor_items,
        auto_arm: t.auto_arm,
    })
}

fn lanes_info(t: &Track) -> Value {
    let n = t.lane_count();
    json(LanesInfo {
        track: t.id.0,
        fixed: t.fixed_lanes,
        names: (0..n).map(|l| t.lane_names.get(l).cloned().unwrap_or_else(|| (l + 1).to_string())).collect(),
        playing: t.playing_lanes(),
        comp_lane: t.comp_lane(),
        areas: t.comp_areas.iter().map(|a| AreaInfo { start: a.start, end: a.end, lane: a.lane }).collect(),
        items: (0..n).map(|l| t.items.iter().filter(|i| !t.fixed_lanes || i.lane() == l).count()).collect(),
    })
}

fn options(session: &Session) -> Value {
    serde_json::json!({
        "mode": ProjectRecordMode::of(&session.project).name(),
        "new_lanes": session.prefs.autoxfade_bit(AUTOXFADE_RECORD_LANES),
        "loop_takes": session.prefs.autoxfade_bit(AUTOXFADE_LOOP_TAKES),
        "lanes_exclusive": session.prefs.autoxfade_bit(AUTOXFADE_LANES_EXCLUSIVE),
    })
}

fn edit_track(
    session: &mut Session,
    id: TrackId,
    label: &str,
    f: impl FnOnce(&mut Track) -> bool,
) -> Result<bool, ApiError> {
    let mut done = false;
    session.edit(label, |p| {
        if let Some(t) = p.track_mut(id) {
            done = f(t);
        }
    });
    Ok(done)
}

fn track(session: &Session, id: TrackId) -> &Track {
    session.project.track(id).expect("resolved above")
}

pub fn execute(session: &mut Session, command: RecordingCommand) -> Result<Value, ApiError> {
    match command {
        RecordingCommand::RecordMode { track: r, mode, tap, auto_arm } => {
            let id = r.track(&session.project)?;
            let mode = mode
                .map(|m| {
                    RecordMode::from_name(&m).ok_or_else(|| ApiError::invalid(format!("unknown record mode \"{m}\"")))
                })
                .transpose()?;
            let tap = tap
                .map(|m| {
                    RecordTap::from_name(&m).ok_or_else(|| ApiError::invalid(format!("unknown output tap \"{m}\"")))
                })
                .transpose()?;
            edit_track(session, id, "Set record mode", |t| {
                t.rec_mode = mode.unwrap_or(t.rec_mode);
                t.rec_tap = tap.unwrap_or(t.rec_tap);
                t.auto_arm = auto_arm.unwrap_or(t.auto_arm);
                true
            })?;
            Ok(record_info(track(session, id)))
        }
        RecordingCommand::MonitorMode { track: r, mode, items } => {
            let id = r.track(&session.project)?;
            let mode = mode
                .map(|m| {
                    MonitorMode::from_name(&m).ok_or_else(|| ApiError::invalid(format!("unknown monitor mode \"{m}\"")))
                })
                .transpose()?;
            edit_track(session, id, "Set input monitoring", |t| {
                if let Some(m) = mode {
                    t.set_monitor_mode(m);
                }
                t.monitor_items = items.unwrap_or(t.monitor_items);
                true
            })?;
            Ok(record_info(track(session, id)))
        }
        RecordingCommand::Options { mode, new_lanes, loop_takes, lanes_exclusive } => {
            if let Some(m) = mode {
                let m = ProjectRecordMode::from_name(&m)
                    .ok_or_else(|| ApiError::invalid(format!("unknown record mode \"{m}\"")))?;
                if m != ProjectRecordMode::of(&session.project) {
                    session.set_record_mode(m);
                }
            }
            session.change_preferences(|p| {
                for (bit, v) in [
                    (AUTOXFADE_RECORD_LANES, new_lanes),
                    (AUTOXFADE_LOOP_TAKES, loop_takes),
                    (AUTOXFADE_LANES_EXCLUSIVE, lanes_exclusive),
                ] {
                    if let Some(v) = v {
                        p.set_autoxfade_bit(bit, v);
                    }
                }
            });
            Ok(options(session))
        }
        RecordingCommand::List { track: r } => {
            let id = r.track(&session.project)?;
            Ok(lanes_info(track(session, id)))
        }
        RecordingCommand::Fixed { track: r, on } => {
            let id = r.track(&session.project)?;
            edit_track(session, id, if on { "Fixed item lanes" } else { "Free item lanes" }, |t| {
                t.set_fixed_lanes(on);
                true
            })?;
            Ok(lanes_info(track(session, id)))
        }
        RecordingCommand::Add { track: r } => {
            let id = r.track(&session.project)?;
            edit_track(session, id, "Add lane", |t| {
                t.add_lane();
                true
            })?;
            Ok(lanes_info(track(session, id)))
        }
        RecordingCommand::Play { track: r, lanes } => {
            let id = r.track(&session.project)?;
            let n = track(session, id).lane_count();
            if !track(session, id).fixed_lanes {
                return Err(ApiError::invalid("the track has no fixed lanes"));
            }
            if let Some(l) = lanes.iter().find(|&&l| l >= n) {
                return Err(ApiError::not_found(format!("the track has no lane {l}")));
            }
            edit_track(session, id, "Play lanes", |t| {
                t.lane_solo = (!lanes.is_empty()).then(|| scythe_core::lanes::solo_words(lanes.iter().copied()));
                true
            })?;
            Ok(lanes_info(track(session, id)))
        }
        RecordingCommand::Comp { track: r } => {
            let id = r.track(&session.project)?;
            edit_track(session, id, "Comp into new lane", |t| {
                t.create_comp_lane();
                true
            })?;
            Ok(lanes_info(track(session, id)))
        }
        RecordingCommand::CompOff { track: r } => {
            let id = r.track(&session.project)?;
            edit_track(session, id, "Turn off comping", |t| {
                t.stop_comping();
                true
            })?;
            Ok(lanes_info(track(session, id)))
        }
        RecordingCommand::Swipe { track: r, lane, start, end } => {
            let id = r.track(&session.project)?;
            let t = track(session, id);
            if t.comp_lane().is_none() {
                return Err(ApiError::invalid("the track has no comp lane: lanes.comp makes one"));
            }
            if lane >= t.lane_count() || Some(lane) == t.comp_lane() {
                return Err(ApiError::invalid(format!("lane {lane} is not a source lane")));
            }
            if !(start.is_finite() && end.is_finite() && start >= 0.0 && end > start) {
                return Err(ApiError::invalid("the comp area needs 0 <= start < end"));
            }
            edit_track(session, id, "Swipe comp area", |t| t.swipe(lane, start, end))?;
            Ok(lanes_info(track(session, id)))
        }
        RecordingCommand::RemoveArea { track: r, at } => {
            let id = r.track(&session.project)?;
            if !edit_track(session, id, "Delete comp area", |t| t.remove_comp_area(at))? {
                return Err(ApiError::not_found(format!("no comp area at {at} s")));
            }
            Ok(lanes_info(track(session, id)))
        }
    }
}
