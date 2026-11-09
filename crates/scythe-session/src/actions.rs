//! Actions a surface button, an OSC `/action` message or a footswitch can run: REAPER's
//! command ids where Scythe has the same action, and Scythe's own named commands
//! (`_SCYTHE_SETLIST_NEXT`) for live use.

use crate::Session;
use crate::api::{self, ApiError};
use scythe_core::marker_action as ids;
use serde_json::{Value, json};

/// Scythe's named commands, with what they do.
pub const NAMED: [(&str, &str); 6] = [
    ("_SCYTHE_SETLIST_PLAY", "Setlist: play the current song"),
    ("_SCYTHE_SETLIST_STOP", "Setlist: stop"),
    ("_SCYTHE_SETLIST_PLAY_STOP", "Setlist: play the current song, or stop when playing"),
    ("_SCYTHE_SETLIST_NEXT", "Setlist: next song (cued when stopped, played when playing)"),
    ("_SCYTHE_SETLIST_PREVIOUS", "Setlist: previous song"),
    ("_SCYTHE_SETLIST_RESTART", "Setlist: play the current song again from its start"),
];

/// REAPER command ids a surface can run, with REAPER's names for them.
pub const COMMANDS: [(u32, &str); 21] = [
    (ids::PLAY, "Transport: Play"),
    (ids::PAUSE, "Transport: Pause"),
    (ids::RECORD, "Transport: Record"),
    (ids::STOP, "Transport: Stop"),
    (ids::TOGGLE_REPEAT, "Transport: Toggle repeat"),
    (ids::GO_TO_START, "Transport: Go to start of project"),
    (ids::GO_TO_END, "Transport: Go to end of project"),
    (ids::PLAY_STOP, "Transport: Play/stop"),
    (ids::PLAY_PAUSE, "Transport: Play/pause"),
    (ids::PLAY_STOP_MOVE_CURSOR, "Transport: Play/stop (move edit cursor on stop)"),
    (ids::GO_TO_LOOP_START, "Transport: Go to start of loop"),
    (40029, "Edit: Undo"),
    (40030, "Edit: Redo"),
    (40026, "File: Save project"),
    (40172, "Markers: Go to previous marker/project start"),
    (40173, "Markers: Go to next marker/project end"),
    (40339, "Track: Unmute all tracks"),
    (40340, "Track: Unsolo all tracks"),
    (40364, "Options: Toggle metronome"),
    (40161, "Markers: Go to marker 01 (40161-40169 for 1-9)"),
    (65535, "No-op (no action)"),
];

/// Runs `action`: a REAPER command id (`1007`) or a named command (`_SCYTHE_SETLIST_NEXT`).
pub fn run(session: &mut Session, action: &str) -> Result<(), ApiError> {
    let action = action.trim();
    if let Ok(id) = action.parse::<u32>() {
        return run_id(session, id);
    }
    let cmd = |c: &str| json!({ "cmd": c });
    match action.to_ascii_uppercase().as_str() {
        "_SCYTHE_SETLIST_PLAY" => exec(session, cmd("setlist.play")),
        "_SCYTHE_SETLIST_STOP" => exec(session, cmd("setlist.stop")),
        "_SCYTHE_SETLIST_PLAY_STOP" => {
            let c = if session.is_playing() { "setlist.stop" } else { "setlist.play" };
            exec(session, cmd(c))
        }
        "_SCYTHE_SETLIST_NEXT" => exec(session, cmd("setlist.next")),
        "_SCYTHE_SETLIST_PREVIOUS" => exec(session, cmd("setlist.previous")),
        "_SCYTHE_SETLIST_RESTART" => {
            if session.is_playing() {
                exec(session, cmd("setlist.stop"))?;
            }
            exec(session, cmd("setlist.play"))
        }
        _ => Err(ApiError::not_found(format!("no action \"{action}\""))),
    }
}

fn exec(session: &mut Session, command: Value) -> Result<(), ApiError> {
    api::execute_json(session, command).map(|_| ())
}

fn run_id(session: &mut Session, id: u32) -> Result<(), ApiError> {
    let cmd = |c: &str| json!({ "cmd": c });
    let playing = session.is_playing() || session.is_recording();
    match id {
        ids::PLAY => exec(session, cmd("transport.play")),
        ids::PAUSE => exec(session, cmd("transport.pause")),
        ids::RECORD => exec(session, cmd("transport.record")),
        ids::STOP => exec(session, cmd("transport.stop")),
        ids::PLAY_STOP | ids::PLAY_STOP_MOVE_CURSOR => {
            if playing {
                let position = session.position();
                exec(session, cmd("transport.stop"))?;
                if id == ids::PLAY_STOP_MOVE_CURSOR {
                    exec(session, json!({ "cmd": "transport.seek", "position": position }))?;
                }
                Ok(())
            } else {
                exec(session, cmd("transport.play"))
            }
        }
        ids::PLAY_PAUSE => exec(session, cmd(if playing { "transport.pause" } else { "transport.play" })),
        ids::TOGGLE_REPEAT => {
            let on = !session.project.loop_enabled;
            exec(session, json!({ "cmd": "transport.loop", "enabled": on }))
        }
        ids::GO_TO_START => exec(session, json!({ "cmd": "transport.seek", "position": 0.0 })),
        ids::GO_TO_END => {
            let end = session.project.length();
            exec(session, json!({ "cmd": "transport.seek", "position": end }))
        }
        ids::GO_TO_LOOP_START => match session.project.time_selection {
            Some(range) => exec(session, json!({ "cmd": "transport.seek", "position": range.start })),
            None => Ok(()),
        },
        40029 => exec(session, cmd("project.undo")),
        40030 => exec(session, cmd("project.redo")),
        40026 => exec(session, cmd("project.save")),
        40172 | 40173 => {
            let here = session.position();
            let target = marker_step(session, here, id == 40173);
            exec(session, json!({ "cmd": "transport.seek", "position": target }))
        }
        40161..=40169 => {
            let number = id - 40160;
            exec(session, json!({ "cmd": "marker.goto", "marker": number, "region": false }))
        }
        40339 | 40340 => {
            let label = if id == 40339 { "Unmute all tracks" } else { "Unsolo all tracks" };
            session.edit(label, |p| {
                for t in &mut p.tracks {
                    if id == 40339 {
                        t.muted = false;
                    } else {
                        t.solo = false;
                    }
                }
            });
            Ok(())
        }
        40364 => {
            session.toggle_metronome();
            Ok(())
        }
        65535 => Ok(()),
        _ => Err(ApiError::not_found(format!("action {id} is not available in Scythe"))),
    }
}

/// The marker (or region start) before or after `at`, else the project start or end, as
/// REAPER's previous/next marker actions go.
pub fn marker_step(session: &Session, at: f64, forward: bool) -> f64 {
    const EPS: f64 = 1e-6;
    let points = session.project.markers.iter().map(|m| m.position);
    if forward {
        points
            .filter(|p| *p > at + EPS)
            .fold(None, |a: Option<f64>, p| Some(a.map_or(p, |a| a.min(p))))
            .unwrap_or_else(|| session.project.length())
    } else {
        points.filter(|p| *p < at - EPS).fold(None, |a: Option<f64>, p| Some(a.map_or(p, |a| a.max(p)))).unwrap_or(0.0)
    }
}
