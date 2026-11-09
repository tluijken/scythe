//! MIDI devices and synchronization: which ports are enabled and what they are called, which
//! outputs get MIDI clock, the virtual keyboard, and chasing external timecode.

use schemars::JsonSchema;
use scythe_core::prefs::MidiPrefs;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ApiError, json};
use crate::Session;
use crate::midi::CLOCK_CHASE;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum MidiCommand {
    /// The MIDI inputs and outputs Scythe knows, by their stable device numbers (the numbers
    /// tracks' MIDI inputs and outputs use), with what is enabled and which outputs get clock.
    #[serde(rename = "midi.devices")]
    Devices,
    /// Looks for MIDI ports again; new ones get the lowest free device number.
    #[serde(rename = "midi.rescan")]
    Rescan,
    /// Changes one device; fields left out stay.
    #[serde(rename = "midi.device")]
    Device {
        /// `input` or `output`.
        kind: DeviceKind,
        /// The device number, as `midi.devices` lists it.
        number: u8,
        /// Open the device for Scythe (REAPER's `midiins`/`midiouts`).
        #[serde(default)]
        enabled: Option<bool>,
        /// A name to show instead of the port's; empty clears it.
        #[serde(default)]
        alias: Option<String>,
        /// Outputs: send MIDI clock, start, stop and continue (`midiouts_clock`).
        #[serde(default)]
        clock: Option<bool>,
        /// Outputs: send song position pointers with the clock (off is `midiouts_clock_nospp`).
        #[serde(default)]
        spp: Option<bool>,
        /// Outputs: send all-notes-off, pitch bend and sustain resets when playback starts (off
        /// is `midiouts_noreset`).
        #[serde(default)]
        reset: Option<bool>,
    },
    /// Plays the virtual MIDI keyboard (device 62): a note on with `velocity` above 0, a note
    /// off with 0. Tracks whose MIDI input is the keyboard hear it and record it.
    #[serde(rename = "midi.keyboard")]
    Keyboard {
        /// MIDI note number, 0 to 127 (60 is middle C).
        note: u8,
        /// 1 to 127 plays the note, 0 releases it.
        velocity: u8,
        /// MIDI channel, 1 to 16 (1 when left out).
        #[serde(default)]
        channel: Option<u8>,
    },
    /// External timecode synchronization: what the transport chases and how.
    #[serde(rename = "sync.get")]
    SyncGet,
    /// Changes the synchronization; fields left out stay.
    #[serde(rename = "sync.set")]
    SyncSet {
        /// Chase the incoming timecode: start, move and stop with it.
        #[serde(default)]
        enabled: Option<bool>,
        /// `mtc` (MIDI timecode), `ltc` (timecode on an audio input) or `clock` (MIDI clock and
        /// song position).
        #[serde(default)]
        source: Option<ChaseKind>,
        /// MTC or clock: the input device number, 63 for all inputs; unchanged when left out.
        #[serde(default)]
        device: Option<u8>,
        /// LTC: the audio input channel, from 0.
        #[serde(default)]
        channel: Option<usize>,
        /// Seconds subtracted from the incoming time (the project time where timecode 0 lands is
        /// minus this).
        #[serde(default)]
        offset: Option<f64>,
        /// Milliseconds of drift before the playhead jumps back in line.
        #[serde(default)]
        resync_ms: Option<i64>,
        /// Milliseconds playback carries on after the timecode stops.
        #[serde(default)]
        freewheel_ms: Option<i64>,
        /// LTC frame rate: 24, 25, 29.97 or 30.
        #[serde(default)]
        frame_rate: Option<f64>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Input,
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChaseKind {
    Mtc,
    Ltc,
    Clock,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DeviceInfo {
    pub number: u8,
    /// The port's own name.
    pub name: String,
    pub alias: Option<String>,
    /// Plugged in now.
    pub present: bool,
    pub enabled: bool,
    /// Outputs only: clock, song position and reset at play.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clock: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spp: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DevicesInfo {
    pub inputs: Vec<DeviceInfo>,
    pub outputs: Vec<DeviceInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SyncInfo {
    pub enabled: bool,
    pub source: ChaseKind,
    /// MTC or clock input device; null for all.
    pub device: Option<u8>,
    /// LTC audio input channel, from 0.
    pub channel: usize,
    pub offset: f64,
    pub resync_ms: i64,
    pub freewheel_ms: i64,
    pub frame_rate: f64,
    /// Output device numbers that get MIDI clock.
    pub clock_outputs: Vec<u8>,
}

pub fn devices(session: &Session) -> DevicesInfo {
    let m = &session.prefs.midi;
    let inputs = session
        .midi_devices
        .inputs
        .iter()
        .map(|p| DeviceInfo {
            number: p.number,
            name: p.name.clone(),
            alias: p.alias.clone(),
            present: p.present,
            enabled: m.input_enabled(p.number),
            clock: None,
            spp: None,
            reset: None,
        })
        .collect();
    let outputs = session
        .midi_devices
        .outputs
        .iter()
        .map(|p| DeviceInfo {
            number: p.number,
            name: p.name.clone(),
            alias: p.alias.clone(),
            present: p.present,
            enabled: m.output_enabled(p.number),
            clock: Some(MidiPrefs::bit(m.clock, p.number)),
            spp: Some(!MidiPrefs::bit(m.clock_no_spp, p.number)),
            reset: Some(!MidiPrefs::bit(m.no_reset, p.number)),
        })
        .collect();
    DevicesInfo { inputs, outputs }
}

pub fn sync_info(session: &Session) -> SyncInfo {
    let s = &session.project.timecode_sync;
    let clock = session.project.ext_state(CLOCK_CHASE.0, CLOCK_CHASE.1);
    let midi = s.input & scythe_core::midi_route::MIDI_INPUT_FLAG != 0;
    let device = match &clock {
        Some(v) => v.trim().parse().ok(),
        None => Some(((s.input >> 5) & 127) as u8).filter(|d| midi && *d != 63),
    };
    SyncInfo {
        enabled: s.enabled(),
        source: if clock.is_some() {
            ChaseKind::Clock
        } else if midi {
            ChaseKind::Mtc
        } else {
            ChaseKind::Ltc
        },
        device,
        channel: if midi { 0 } else { (s.input.max(0) & 1023) as usize },
        offset: s.offset,
        resync_ms: s.resync_ms,
        freewheel_ms: s.freewheel_ms,
        frame_rate: s.frame_rate,
        clock_outputs: (0..64).filter(|&n| MidiPrefs::bit(session.prefs.midi.clock, n)).collect(),
    }
}

pub fn execute(session: &mut Session, command: MidiCommand) -> Result<Value, ApiError> {
    match command {
        MidiCommand::Devices => Ok(json(devices(session))),
        MidiCommand::Rescan => {
            session.rescan_midi_devices();
            Ok(json(devices(session)))
        }
        MidiCommand::Device { kind, number, enabled, alias, clock, spp, reset } => {
            let output = kind == DeviceKind::Output;
            let known = if output { session.midi_devices.output(number) } else { session.midi_devices.input(number) };
            if known.is_none() {
                return Err(ApiError::not_found(format!("there is no MIDI {kind:?} number {number}").to_lowercase()));
            }
            if !output && (clock.is_some() || spp.is_some() || reset.is_some()) {
                return Err(ApiError::invalid("clock, spp and reset are for outputs"));
            }
            if number >= 64 && enabled == Some(false) {
                return Err(ApiError::invalid("devices from 64 up are always enabled, as in REAPER"));
            }
            if let Some(alias) = alias {
                session.set_midi_alias(output, number, Some(alias));
            }
            session.change_preferences(|p| {
                let m = &mut p.midi;
                if let Some(on) = enabled {
                    let mask = if output { &mut m.outputs } else { &mut m.inputs };
                    *mask = Some(MidiPrefs::with(mask.unwrap_or(u64::MAX), number, on));
                }
                if let Some(on) = clock {
                    m.clock = MidiPrefs::with(m.clock, number, on);
                }
                if let Some(on) = spp {
                    m.clock_no_spp = MidiPrefs::with(m.clock_no_spp, number, !on);
                }
                if let Some(on) = reset {
                    m.no_reset = MidiPrefs::with(m.no_reset, number, !on);
                }
            });
            session.sync();
            Ok(json(devices(session)))
        }
        MidiCommand::Keyboard { note, velocity, channel } => {
            if note > 127 || velocity > 127 {
                return Err(ApiError::invalid("note and velocity run from 0 to 127"));
            }
            let channel = channel.unwrap_or(1);
            if !(1..=16).contains(&channel) {
                return Err(ApiError::invalid("channels run from 1 to 16"));
            }
            let status = if velocity > 0 { 0x90 } else { 0x80 } | (channel - 1);
            session.virtual_keyboard([status, note, velocity]);
            Ok(Value::Null)
        }
        MidiCommand::SyncGet => Ok(json(sync_info(session))),
        MidiCommand::SyncSet { enabled, source, device, channel, offset, resync_ms, freewheel_ms, frame_rate } => {
            if let Some(rate) = frame_rate
                && ![24.0, 25.0, 29.97, 30.0].iter().any(|r| (r - rate).abs() < 1e-6)
            {
                return Err(ApiError::invalid("frame rates are 24, 25, 29.97 and 30"));
            }
            if resync_ms.is_some_and(|v| v < 1) || freewheel_ms.is_some_and(|v| v < 1) {
                return Err(ApiError::invalid("resync_ms and freewheel_ms are at least 1"));
            }
            let current = sync_info(session);
            let source = source.unwrap_or(current.source);
            let device = match device {
                Some(63) => None,
                Some(d) => Some(d),
                None if source == current.source => current.device,
                None => None,
            };
            session.edit("Timecode synchronization", |p| {
                let s = &mut p.timecode_sync;
                if let Some(on) = enabled {
                    s.set_enabled(on);
                }
                match source {
                    ChaseKind::Clock => {
                        let v = device.map_or("all".to_owned(), |d| d.to_string());
                        p.set_ext_state(CLOCK_CHASE.0, CLOCK_CHASE.1, Some(&v));
                    }
                    ChaseKind::Mtc => {
                        p.set_ext_state(CLOCK_CHASE.0, CLOCK_CHASE.1, None);
                        let d = device.unwrap_or(scythe_core::midi_route::ALL_DEVICES);
                        p.timecode_sync.input = scythe_core::midi_route::MIDI_INPUT_FLAG | (d as i64) << 5;
                    }
                    ChaseKind::Ltc => {
                        p.set_ext_state(CLOCK_CHASE.0, CLOCK_CHASE.1, None);
                        p.timecode_sync.input = channel.unwrap_or(current.channel).min(1023) as i64;
                    }
                }
                let s = &mut p.timecode_sync;
                s.offset = offset.unwrap_or(s.offset);
                s.resync_ms = resync_ms.unwrap_or(s.resync_ms);
                s.freewheel_ms = freewheel_ms.unwrap_or(s.freewheel_ms);
                s.frame_rate = frame_rate.unwrap_or(s.frame_rate);
            });
            session.sync();
            Ok(json(sync_info(session)))
        }
    }
}
