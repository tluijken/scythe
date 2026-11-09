use schemars::JsonSchema;
use scythe_core::TimeRange;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ApiError, json, snapshot, time};
use crate::Session;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum TransportCommand {
    /// Starts playback from the edit cursor; nothing happens while playing.
    #[serde(rename = "transport.play")]
    Play,
    /// Stops and leaves the edit cursor where playback was.
    #[serde(rename = "transport.pause")]
    Pause,
    /// Stops (ending a recording) and returns to the edit cursor.
    #[serde(rename = "transport.stop")]
    Stop,
    /// Starts recording the armed tracks; `transport.stop` ends it.
    #[serde(rename = "transport.record")]
    Record,
    /// Moves the edit cursor, and the playhead while playing.
    #[serde(rename = "transport.seek")]
    Seek {
        /// Seconds from the start of the project.
        position: f64,
    },
    /// Turns repeat on or off and/or sets the range it repeats.
    #[serde(rename = "transport.loop")]
    Loop {
        #[serde(default)]
        enabled: Option<bool>,
        /// Seconds; give `end` with it.
        #[serde(default)]
        start: Option<f64>,
        /// Seconds; give `start` with it.
        #[serde(default)]
        end: Option<f64>,
    },
    /// Turns the metronome (click) on or off.
    #[serde(rename = "transport.metronome")]
    Metronome { enabled: bool },
    /// Play state, playhead and edit cursor, loop and metronome.
    #[serde(rename = "transport.state")]
    State,
}

pub fn execute(session: &mut Session, command: TransportCommand) -> Result<Value, ApiError> {
    match command {
        TransportCommand::Play => {
            if !session.is_playing() {
                session.toggle_play();
            }
        }
        TransportCommand::Pause => {
            if session.is_recording() {
                session.toggle_record();
            } else if session.is_playing() {
                session.toggle_play();
            }
        }
        TransportCommand::Stop => session.stop(),
        TransportCommand::Record => {
            if !session.is_recording() {
                session.toggle_record();
                if !session.is_recording() {
                    return Err(ApiError::failed("recording did not start: arm a track with an input"));
                }
            }
        }
        TransportCommand::Seek { position } => session.set_cursor(time(position, "position")?),
        TransportCommand::Loop { enabled, start, end } => {
            match (start, end) {
                (Some(s), Some(e)) => {
                    let range = TimeRange::new(time(s, "start")?, time(e, "end")?);
                    session.edit("Set loop points", |p| p.time_selection = Some(range));
                }
                (None, None) => {}
                _ => return Err(ApiError::invalid("give both \"start\" and \"end\" for the loop range")),
            }
            if let Some(on) = enabled {
                session.edit(if on { "Enable repeat" } else { "Disable repeat" }, |p| p.loop_enabled = on);
            }
        }
        TransportCommand::Metronome { enabled } => session.set_metronome(enabled),
        TransportCommand::State => {}
    }
    Ok(json(snapshot::transport(session)))
}
