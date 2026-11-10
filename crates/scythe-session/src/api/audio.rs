//! The audio device, as REAPER's Preferences > Audio > Device page sets it: which audio
//! system and devices play, at what rate and block size, how many channels, and the names
//! the channels go by.

use schemars::JsonSchema;
use scythe_engine::DeviceRequest;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ApiError, json};
use crate::Session;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum AudioCommand {
    /// The audio systems (hosts) and the devices each offers, to choose from in
    /// `audio.set_device`.
    #[serde(rename = "audio.devices")]
    Devices,
    /// The device playing now: system, devices, rate, block size, latency and the names of its
    /// input and output channels; also what the preferences ask for.
    #[serde(rename = "audio.device")]
    Device,
    /// Opens other audio devices without closing the project (the transport stops). Fields
    /// left out keep the current choice. When the devices cannot be opened the previous ones
    /// play on and the reason comes back as an error.
    #[serde(rename = "audio.set_device")]
    SetDevice {
        /// The audio system: `ALSA`, `JACK` or `Dummy` (no device, a silent clock), as
        /// `audio.devices` lists them.
        #[serde(default)]
        system: Option<String>,
        /// The output device: its id from `audio.devices` (`hw:CARD=USB,DEV=0`) or name; empty
        /// for the system default.
        #[serde(default)]
        output: Option<String>,
        /// The input device, like `output`.
        #[serde(default)]
        input: Option<String>,
        /// Sample rate in Hz to request; 0 for the device's own.
        #[serde(default)]
        sample_rate: Option<u32>,
        /// Frames per audio block to request; 0 for the device's own.
        #[serde(default)]
        block_size: Option<u32>,
        /// Output channels to open; 0 opens every channel of a hardware device (a pair on a
        /// sound server).
        #[serde(default)]
        output_channels: Option<u16>,
        /// Input channels to open, like `output_channels`.
        #[serde(default)]
        input_channels: Option<u16>,
        /// ALSA and PulseAudio: bits per sample (16, 24 or 32 bit integer); 0 for the device's own.
        #[serde(default)]
        bits: Option<u16>,
        /// Keep the choice for the next start (true when left out).
        #[serde(default)]
        save: Option<bool>,
    },
    /// Sets the channel map of the inputs or outputs (REAPER's channel alias/mapping window):
    /// one entry per channel tracks see, each playing a hardware channel or none, with an
    /// optional name. Turns aliasing/remapping on for that side; an empty list resets the map.
    #[serde(rename = "audio.channel_map")]
    ChannelMap {
        /// `input` or `output`.
        kind: ChannelKind,
        channels: Vec<MapSlot>,
    },
    /// Names a hardware input or output channel (REAPER's channel name aliasing), or clears
    /// its name with an empty `name`. Track inputs, hardware outputs and monitor mixes show it.
    #[serde(rename = "audio.channel_name")]
    ChannelName {
        /// `input` or `output`.
        kind: ChannelKind,
        /// The channel, from 1.
        channel: usize,
        name: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    Input,
    Output,
}

/// One channel of a channel map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MapSlot {
    /// The hardware channel it plays, from 1; `null` for none ("not connected").
    pub hardware: Option<usize>,
    /// Its name; left out or empty for the hardware channel's.
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DeviceEntry {
    pub id: String,
    pub name: String,
    pub input: bool,
    pub output: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HostEntry {
    pub system: String,
    pub devices: Vec<DeviceEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Requested {
    pub system: Option<String>,
    pub output: Option<String>,
    pub input: Option<String>,
    pub sample_rate: Option<u32>,
    pub block_size: Option<u32>,
    pub output_channels: Option<u16>,
    pub input_channels: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DeviceState {
    /// The audio system playing, empty while silent for want of a device.
    pub system: String,
    /// The output device's name and id.
    pub output: String,
    pub output_id: String,
    /// The input device's name, when there is one.
    pub input: Option<String>,
    pub sample_rate: u32,
    /// Frames per block, when known.
    pub block_size: Option<u32>,
    /// Seconds from rendering a block until it is heard.
    pub output_latency: f64,
    /// No device is playing (silent clock), and why.
    pub silent: bool,
    pub reason: Option<String>,
    /// The input channels by the names tracks show (aliases included).
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    /// What the preferences (and the command line) ask for.
    pub requested: Requested,
}

pub fn devices() -> Vec<HostEntry> {
    let all = scythe_engine::devices::list();
    scythe_engine::devices::hosts()
        .into_iter()
        .map(|system| HostEntry {
            devices: all
                .iter()
                .filter(|d| d.host == system)
                .map(|d| DeviceEntry { id: d.id.clone(), name: d.name.clone(), input: d.input, output: d.output })
                .collect(),
            system,
        })
        .collect()
}

pub fn state(session: &Session) -> DeviceState {
    let info = session.engine.info();
    let health = session.engine.health();
    let r = session.engine.devices();
    DeviceState {
        system: info.host.clone(),
        output: info.name.clone(),
        output_id: info.device_id.clone(),
        input: info.input.as_ref().map(|(name, _)| name.clone()),
        sample_rate: info.sample_rate,
        block_size: health.block_size,
        output_latency: health.output_latency,
        silent: health.silent,
        reason: health.reason,
        inputs: session.input_names(),
        outputs: session.output_names(),
        requested: Requested {
            system: r.host.clone(),
            output: r.output.clone(),
            input: r.input.clone(),
            sample_rate: r.sample_rate,
            block_size: r.buffer_size,
            output_channels: r.output_channels,
            input_channels: r.input_channels,
        },
    }
}

pub fn execute(session: &mut Session, command: AudioCommand) -> Result<Value, ApiError> {
    match command {
        AudioCommand::Devices => Ok(json(devices())),
        AudioCommand::Device => Ok(json(state(session))),
        AudioCommand::SetDevice {
            system,
            output,
            input,
            sample_rate,
            block_size,
            output_channels,
            input_channels,
            bits,
            save,
        } => {
            let mut r: DeviceRequest = session.engine.devices().clone();
            if let Some(system) = system {
                let known = scythe_engine::devices::hosts();
                let Some(host) = known.iter().find(|h| h.eq_ignore_ascii_case(&system)) else {
                    return Err(ApiError::invalid(format!("the audio systems are {}", known.join(", "))));
                };
                if r.host.as_deref() != Some(host) {
                    (r.output, r.input) = (None, None);
                }
                r.host = Some(host.clone());
            }
            let text = |v: String| Some(v).filter(|v| !v.is_empty());
            let number = |v: u32| Some(v).filter(|v| *v > 0);
            if let Some(v) = output {
                r.output = text(v);
            }
            if let Some(v) = input {
                r.input = text(v);
            }
            if let Some(v) = sample_rate {
                r.sample_rate = number(v);
            }
            if let Some(v) = block_size {
                r.buffer_size = number(v);
            }
            if let Some(v) = output_channels {
                r.output_channels = Some(v).filter(|v| *v > 0);
            }
            if let Some(v) = input_channels {
                r.input_channels = Some(v).filter(|v| *v > 0);
            }
            if let Some(v) = bits {
                r.bits = Some(v).filter(|v| *v > 0);
            }
            session.change_audio_device(r, save.unwrap_or(true)).map_err(ApiError::failed)?;
            Ok(json(state(session)))
        }
        AudioCommand::ChannelMap { kind, channels } => {
            let input = kind == ChannelKind::Input;
            let hardware = if input { session.engine.info().inputs.len() } else { session.engine.info().outputs.len() };
            if let Some(bad) = channels.iter().filter_map(|c| c.hardware).find(|h| *h == 0 || *h > hardware) {
                return Err(ApiError::invalid(format!(
                    "hardware channel {bad} is not one of the device's 1-{hardware}"
                )));
            }
            let map = if channels.is_empty() {
                scythe_core::prefs::ChannelAliases::identity(hardware)
            } else {
                scythe_core::prefs::ChannelAliases {
                    channels: channels
                        .iter()
                        .map(|c| (c.hardware.map_or(-1, |h| h as i32 - 1), c.name.clone().unwrap_or_default()))
                        .collect(),
                    hardware_channels: hardware,
                }
            };
            session.set_channel_map(input, map);
            Ok(json(state(session)))
        }
        AudioCommand::ChannelName { kind, channel, name } => {
            let input = kind == ChannelKind::Input;
            let count = if input { session.engine.info().inputs.len() } else { session.engine.info().outputs.len() };
            if channel == 0 || channel > count {
                return Err(ApiError::not_found(format!(
                    "the device has {count} {} channels, counted from 1",
                    if input { "input" } else { "output" }
                )));
            }
            session.set_channel_alias(input, channel - 1, &name);
            Ok(json(state(session)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Command, execute as run};

    fn session() -> Session {
        let pool = std::sync::Arc::new(scythe_engine::MediaPool::default());
        let mut s = Session::new(scythe_engine::Engine::start_offline(pool, 48_000));
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("scythe-audio-api-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        s.load_preferences(dir.join("preferences.ini"));
        s
    }

    fn cmd(s: &mut Session, value: serde_json::Value) -> Result<Value, ApiError> {
        let c: Command = serde_json::from_value(value).unwrap();
        run(s, c)
    }

    #[test]
    fn switching_to_the_dummy_system_keeps_the_project_and_saves_the_choice() {
        let mut s = session();
        s.edit("Add track", |p| {
            p.add_track("Kick");
        });
        let state = cmd(
            &mut s,
            serde_json::json!({"cmd": "audio.set_device", "system": "dummy", "sample_rate": 44100, "block_size": 256}),
        )
        .unwrap();
        assert_eq!(state["system"], "Dummy");
        assert_eq!(state["sample_rate"], 44100);
        assert_eq!(state["block_size"], 256);
        assert_eq!(state["outputs"].as_array().unwrap().len(), 2);
        assert_eq!(s.project.tracks.len(), 1);
        assert_eq!(s.engine.sample_rate(), 44100);
        assert_eq!(s.prefs.audio.system, Some(scythe_core::prefs::AudioSystem::Dummy));
        assert_eq!(s.prefs.audio.sample_rate, Some(44100));
        assert!(cmd(&mut s, serde_json::json!({"cmd": "audio.set_device", "system": "Beos"})).is_err());
    }

    #[test]
    fn output_channels_take_names_that_tracks_show() {
        let mut s = session();
        cmd(&mut s, serde_json::json!({"cmd": "audio.set_device", "system": "Dummy", "output_channels": 20})).unwrap();
        assert_eq!(s.output_names().len(), 20);
        assert_eq!(s.output_names()[8], "Output 9");
        cmd(
            &mut s,
            serde_json::json!({"cmd": "audio.channel_name", "kind": "output", "channel": 9, "name": "IEM Bass L"}),
        )
        .unwrap();
        assert_eq!(s.output_names()[8], "IEM Bass L");
        assert!(s.channel_aliasing(false));
        let text = std::fs::read_to_string(s.prefs_file.clone().unwrap()).unwrap();
        assert!(text.contains("[alias_out_Dummy_Audio]"), "{text}");
        assert!(text.contains("name8=IEM Bass L"), "{text}");
        assert!(text.contains("useinnc=2"), "{text}");
        assert!(
            cmd(&mut s, serde_json::json!({"cmd": "audio.channel_name", "kind": "output", "channel": 21, "name": "x"}))
                .is_err()
        );
    }

    #[test]
    fn a_channel_map_renames_and_reroutes_the_outputs() {
        let mut s = session();
        cmd(&mut s, serde_json::json!({"cmd": "audio.set_device", "system": "Dummy", "output_channels": 4})).unwrap();
        let map = serde_json::json!({"cmd": "audio.channel_map", "kind": "output", "channels": [
            {"hardware": 3}, {"hardware": 4, "name": "IEM"}, {"hardware": null}
        ]});
        cmd(&mut s, map).unwrap();
        assert_eq!(s.output_names(), ["Output 3", "IEM", "(not connected)"]);
        let text = std::fs::read_to_string(s.prefs_file.clone().unwrap()).unwrap();
        assert!(text.contains("ch0=2") && text.contains("ch2=-1") && text.contains("map_size=3"), "{text}");
        let bad = serde_json::json!({"cmd": "audio.channel_map", "kind": "output", "channels": [{"hardware": 9}]});
        assert!(cmd(&mut s, bad).is_err());
        cmd(&mut s, serde_json::json!({"cmd": "audio.channel_map", "kind": "output", "channels": []})).unwrap();
        assert_eq!(s.output_names().len(), 4);
    }

    #[test]
    fn the_device_closes_while_stopped_and_inactive_and_comes_back() {
        let mut s = session();
        cmd(&mut s, serde_json::json!({"cmd": "audio.set_device", "system": "Dummy"})).unwrap();
        s.change_preferences(|p| p.audio.close_inactive = Some(scythe_core::prefs::CLOSE_INACTIVE));
        s.idle_audio(true);
        assert_eq!(s.engine.info().fallback_reason, None, "open while active");
        s.idle_audio(false);
        assert!(s.engine.info().fallback_reason.is_some(), "closed while stopped and inactive");
        s.idle_audio(true);
        assert_eq!(s.engine.info().fallback_reason, None, "open again when active");
        s.change_preferences(|p| p.audio.close_stopped = Some(true));
        s.idle_audio(true);
        assert!(s.engine.info().fallback_reason.is_some(), "closed while stopped, active or not");
    }

    #[test]
    fn a_project_that_asks_for_its_rate_reopens_the_device_at_it() {
        let mut s = session();
        cmd(&mut s, serde_json::json!({"cmd": "audio.set_device", "system": "Dummy", "sample_rate": 48000})).unwrap();
        let plain = scythe_core::Project { sample_rate: Some(44_100), ..Default::default() };
        s.load(plain);
        assert_eq!(s.engine.sample_rate(), 48_000, "a project that does not ask leaves the device alone");
        let forced = scythe_core::Project { sample_rate: Some(44_100), force_sample_rate: true, ..Default::default() };
        s.load(forced);
        assert_eq!(s.engine.sample_rate(), 44_100);
        s.load(scythe_core::Project::default());
        assert_eq!(s.engine.sample_rate(), 44_100, "and the device stays there, as in REAPER");
        s.change_preferences(|p| p.audio.project_rate_override = Some(false));
        let forced = scythe_core::Project { sample_rate: Some(96_000), force_sample_rate: true, ..Default::default() };
        s.load(forced);
        assert_eq!(s.engine.sample_rate(), 44_100, "unless projects may not override it");
    }
}
