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
    /// only them. REAPER's metronome hardware output (`METRONOMEOUT`), saved with the project;
    /// with `default` the preference for projects that name none (`metronome_defout`).
    #[serde(rename = "transport.metronome_output")]
    MetronomeOutput {
        /// First device output, from 0; `null` plays it with the master (or, for the project,
        /// to the default output).
        output: Option<u32>,
        /// Play to that one output instead of the pair starting there.
        #[serde(default)]
        mono: bool,
        /// Set the default output, a preference, instead of the project's.
        #[serde(default)]
        default: bool,
    },
    /// Count-in: whether playback and/or recording start after `measures` measures of
    /// metronome at the tempo where they start. As in REAPER, there is no count-in while the
    /// metronome is off (a setlist's count-in clicks regardless).
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
        /// "Start count-in at start of measure": count in from the bar line before the start,
        /// a preference rather than a project setting.
        #[serde(default)]
        measure_start: Option<bool>,
    },
    /// Pre-roll (preferences, as in REAPER): playback and/or recording start `measures`
    /// measures before the position asked for; a recording keeps only what comes after it.
    #[serde(rename = "transport.preroll")]
    Preroll {
        /// Pre-roll before playback.
        #[serde(default)]
        playback: Option<bool>,
        /// Pre-roll before recording.
        #[serde(default)]
        record: Option<bool>,
        /// Length in measures, up to 16.
        #[serde(default)]
        measures: Option<f64>,
        /// "Start pre-roll at start of measure" (on by default).
        #[serde(default)]
        measure_start: Option<bool>,
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
        TransportCommand::MetronomeOutput { output, mono, default } => {
            let encoded = match output {
                Some(n) if n >= 512 => return Err(ApiError::invalid("output runs from 0 to 511")),
                Some(n) => Some(n as i64 | if mono { HwOut::MONO } else { 0 }),
                None => None,
            };
            if default {
                session.change_preferences(|p| p.metronome_default_output = encoded.unwrap_or(-1));
            } else {
                session.change_metronome(|m| m.outputs = encoded.into_iter().collect());
            }
        }
        TransportCommand::CountIn { playback, record, measures, measure_start } => {
            measures_in_range(measures)?;
            if let Some(on) = measure_start {
                session.change_preferences(|p| set_bit(&mut p.preroll, metronome::COUNT_IN_AT_MEASURE, on));
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
        TransportCommand::Preroll { playback, record, measures, measure_start } => {
            measures_in_range(measures)?;
            session.change_preferences(|p| {
                if let Some(on) = playback {
                    set_bit(&mut p.preroll, metronome::PREROLL_PLAY, on);
                }
                if let Some(on) = record {
                    set_bit(&mut p.preroll, metronome::PREROLL_RECORD, on);
                }
                if let Some(on) = measure_start {
                    set_bit(&mut p.preroll, metronome::PREROLL_MID_MEASURE, !on);
                }
                if let Some(n) = measures {
                    p.preroll_measures = n;
                }
            });
        }
        TransportCommand::Health => return Ok(json(health(session))),
        TransportCommand::State => {}
    }
    Ok(json(snapshot::transport(session)))
}

fn measures_in_range(measures: Option<f64>) -> Result<(), ApiError> {
    match measures {
        Some(n) if !(n > 0.0 && n <= metronome::MAX_COUNT_IN) => {
            Err(ApiError::invalid("measures runs from just above 0 to 16"))
        }
        _ => Ok(()),
    }
}

fn set_bit(flags: &mut u32, bit: u32, on: bool) {
    if on {
        *flags |= bit;
    } else {
        *flags &= !bit;
    }
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
    /// From rendering a block to hearing it, as the device measures it.
    pub output_latency_ms: f64,
    /// REAPER's latency readout for the block size and periods, input/output (`~2.9/2.9ms`).
    #[serde(default)]
    pub latency: Option<String>,
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
    /// How the audio thread is scheduled, once it has run.
    #[serde(default)]
    pub realtime: Option<RealtimeInfo>,
}

/// The audio thread's scheduling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RealtimeInfo {
    /// `SCHED_FIFO`, `SCHED_RR` or `SCHED_OTHER`.
    pub policy: String,
    /// Real-time priority, 0 when it has none.
    pub priority: i32,
    /// `direct` (the user's rtprio limit), `rtkit` (RealtimeKit), `host` (the audio system's
    /// thread already was real-time) or `none`.
    pub via: String,
    /// Why it has no real-time priority.
    pub reason: Option<String>,
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
        latency: h.block_latency.map(scythe_engine::devices::latency_text),
        cpu: percent(h.load),
        cpu_peak: percent(h.load_peak),
        xruns: h.xruns,
        overloads: h.overloads,
        status: if h.silent { "silent" } else { "running" }.into(),
        reason: h.reason,
        reconnects: h.reconnects,
        realtime: h.realtime.map(|r| RealtimeInfo {
            policy: r.policy.into(),
            priority: r.priority,
            via: r.via.into(),
            reason: r.reason,
        }),
    }
}
