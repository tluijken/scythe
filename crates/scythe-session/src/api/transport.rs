use schemars::JsonSchema;
use scythe_core::{HwOut, TimeRange, metronome};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ApiError, json, level, snapshot, time};
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
    /// Turns the metronome (click) on or off and/or sets its level at its outputs (`volume` as
    /// gain, REAPER's default 0.25, or `db`). Saved with the project, like REAPER's.
    #[serde(rename = "transport.metronome")]
    Metronome {
        /// On or off; left out, it stays as it is.
        #[serde(default)]
        enabled: Option<bool>,
        /// Gain at the metronome's outputs.
        #[serde(default)]
        volume: Option<f64>,
        /// Level in dB, instead of `volume`.
        #[serde(default)]
        db: Option<f64>,
    },
    /// Where the metronome plays: device output `output` (from 0; a pair unless `mono`), or
    /// with the master when `null`. Pointed at a drummer's in-ear output, the click reaches
    /// only them. REAPER's metronome hardware output (`METRONOMEOUT`).
    #[serde(rename = "transport.metronome_output")]
    MetronomeOutput {
        /// First device output, from 0; `null` plays it with the master.
        output: Option<u32>,
        /// Play to that one output instead of the pair starting there.
        #[serde(default)]
        mono: bool,
    },
    /// Count-in: whether playback and/or recording start after `measures` measures of
    /// metronome at the tempo where they start. The count-in clicks even with the metronome off.
    #[serde(rename = "transport.count_in")]
    CountIn {
        /// Count in before playback (REAPER's "Count-in before playback").
        #[serde(default)]
        playback: Option<bool>,
        /// Count in before recording.
        #[serde(default)]
        record: Option<bool>,
        /// Length in measures, up to 16.
        #[serde(default)]
        measures: Option<f64>,
    },
    /// The audio device and how it holds up: xruns, DSP load, latency, a lost device.
    #[serde(rename = "transport.health")]
    Health,
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
        TransportCommand::Metronome { enabled, volume, db } => {
            let gain = if volume.is_some() || db.is_some() { Some(level(volume, db)?) } else { None };
            session.change_metronome(|m| {
                if let Some(on) = enabled {
                    m.set(metronome::ENABLED, on);
                }
                if let Some(gain) = gain {
                    m.volume = gain;
                }
            });
        }
        TransportCommand::MetronomeOutput { output, mono } => {
            let outputs = match output {
                Some(n) if n >= 512 => return Err(ApiError::invalid("output runs from 0 to 511")),
                Some(n) => vec![n as i64 | if mono { HwOut::MONO } else { 0 }],
                None => Vec::new(),
            };
            session.change_metronome(|m| m.outputs = outputs);
        }
        TransportCommand::CountIn { playback, record, measures } => {
            if let Some(n) = measures
                && !(n > 0.0 && n <= metronome::MAX_COUNT_IN)
            {
                return Err(ApiError::invalid("measures runs from just above 0 to 16"));
            }
            session.change_metronome(|m| {
                if let Some(on) = playback {
                    m.set(metronome::COUNT_IN_PLAY, on);
                }
                if let Some(on) = record {
                    m.set(metronome::COUNT_IN_RECORD, on);
                }
                if let Some(n) = measures {
                    m.count_in_measures = n;
                }
            });
        }
        TransportCommand::Health => return Ok(json(health(session))),
        TransportCommand::State => {}
    }
    Ok(json(snapshot::transport(session)))
}

/// The audio device and how it is doing: the `transport.health` answer and the `health` event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthInfo {
    pub device: String,
    pub sample_rate: u32,
    /// Frames per audio callback.
    pub block_size: Option<u32>,
    /// `block_size` in milliseconds.
    pub buffer_ms: Option<f64>,
    /// From rendering a block to hearing it, as the device reports it.
    pub output_latency_ms: f64,
    /// DSP time over buffer time in the last callback, in percent; at 100 the audio drops out.
    pub cpu: f64,
    /// The highest `cpu` of the last few seconds.
    pub cpu_peak: f64,
    /// Buffer underruns and overruns the device reported since the start.
    pub xruns: u64,
    /// Callbacks that took longer to render than the audio they made, since the start.
    pub overloads: u64,
    /// `running`, or `silent` when no device plays (lost, missing, or none asked for).
    pub status: String,
    /// Why it is silent.
    pub reason: Option<String>,
    /// Times the device came back after going away.
    pub reconnects: u64,
}

pub fn health(session: &Session) -> HealthInfo {
    let h = session.health();
    let percent = |load: f32| (load as f64 * 1000.0).round() / 10.0;
    HealthInfo {
        device: h.device,
        sample_rate: h.sample_rate,
        block_size: h.block_size,
        buffer_ms: h.block_size.map(|b| (b as f64 * 1000.0 / h.sample_rate as f64 * 100.0).round() / 100.0),
        output_latency_ms: (h.output_latency * 100_000.0).round() / 100.0,
        cpu: percent(h.load),
        cpu_peak: percent(h.load_peak),
        xruns: h.xruns,
        overloads: h.overloads,
        status: if h.silent { "silent" } else { "running" }.into(),
        reason: h.reason,
        reconnects: h.reconnects,
    }
}
