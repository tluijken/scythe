use schemars::JsonSchema;
use scythe_core::surface::{MidiKind, MidiSurface, OSC_LEARN, OSC_RECEIVE, OSC_SEND, OscSurface, Surface};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ApiError, done, json};
use crate::{Session, actions};

/// Control surfaces (OSC devices, Mackie Control and extenders, HUI), kept in the preferences
/// as REAPER keeps them in reaper.ini (`csurf_<i>` lines). Changes apply at once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum SurfaceCommand {
    /// The surfaces with their settings and whether each is running.
    #[serde(rename = "surface.list")]
    List,
    /// Adds a surface at the end of the list.
    #[serde(rename = "surface.add")]
    Add { surface: SurfaceSpec },
    /// Replaces the settings of surface `index`.
    #[serde(rename = "surface.set")]
    Set {
        /// Position in `surface.list`, from 0.
        index: usize,
        surface: SurfaceSpec,
    },
    /// Removes surface `index`.
    #[serde(rename = "surface.remove")]
    Remove {
        /// Position in `surface.list`, from 0.
        index: usize,
    },
    /// The MIDI devices a surface can use, numbered as `input` and `output` take them.
    #[serde(rename = "surface.devices")]
    Devices,
    /// The actions surfaces, OSC `/action` messages and footswitches can run.
    #[serde(rename = "surface.action_list")]
    ActionList,
    /// Runs an action as a surface button would: a REAPER command id such as `40364`
    /// (metronome) or a named command such as `_SCYTHE_SETLIST_NEXT`.
    #[serde(rename = "surface.run_action")]
    RunAction {
        /// A command id or name from `surface.actions`.
        action: String,
    },
}

/// A surface's settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SurfaceSpec {
    /// An OSC device (a tablet app, a footswitch box) using a `.ReaperOSC` pattern file.
    Osc {
        /// A name to tell devices apart.
        #[serde(default)]
        name: String,
        /// The UDP port Scythe listens on.
        local_port: u16,
        /// Where feedback goes: the device's address.
        #[serde(default = "localhost")]
        device_ip: String,
        /// The device's UDP port.
        device_port: u16,
        /// Take messages from the device (default on).
        #[serde(default = "yes")]
        receive: bool,
        /// Send feedback to the device (default on).
        #[serde(default = "yes")]
        send: bool,
        /// Let the device's messages be bound to actions and FX parameters (learn).
        #[serde(default)]
        learn: bool,
        /// A pattern file name in Scythe's or REAPER's OSC folder, or a path; empty for the
        /// default patterns.
        #[serde(default)]
        pattern: String,
        /// Largest feedback packet in bytes (default 1024).
        #[serde(default)]
        max_packet: Option<u32>,
        /// Milliseconds between feedback packets (default 10).
        #[serde(default)]
        wait_ms: Option<u32>,
    },
    /// A MIDI surface.
    Midi {
        /// `mcu`, `mcu_extender`, `hui`, `faderport` or `faderport2`.
        kind: MidiKindName,
        /// MIDI input device number from `surface.devices`; -1 for none.
        input: i64,
        /// MIDI output device number from `surface.devices`; -1 for none.
        output: i64,
        /// Tracks the first strip is moved on by: 8 for an extender to the right of an MCU.
        #[serde(default)]
        offset: i64,
        /// REAPER's size tweak, kept as given.
        #[serde(default)]
        size: i64,
        /// REAPER's option bits, kept as given.
        #[serde(default)]
        flags: i64,
    },
    /// A `csurf_<i>=` line from REAPER's reaper.ini, as written there (`MCU 0 0 2 3 0`).
    Line { line: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MidiKindName {
    Mcu,
    McuExtender,
    Hui,
    Faderport,
    Faderport2,
}

impl From<MidiKindName> for MidiKind {
    fn from(k: MidiKindName) -> Self {
        match k {
            MidiKindName::Mcu => Self::Mcu,
            MidiKindName::McuExtender => Self::McuExtender,
            MidiKindName::Hui => Self::Hui,
            MidiKindName::Faderport => Self::FaderPort,
            MidiKindName::Faderport2 => Self::FaderPort2,
        }
    }
}

impl From<MidiKind> for MidiKindName {
    fn from(k: MidiKind) -> Self {
        match k {
            MidiKind::Mcu => Self::Mcu,
            MidiKind::McuExtender => Self::McuExtender,
            MidiKind::Hui => Self::Hui,
            MidiKind::FaderPort => Self::Faderport,
            MidiKind::FaderPort2 => Self::Faderport2,
        }
    }
}

fn localhost() -> String {
    "127.0.0.1".into()
}

fn yes() -> bool {
    true
}

impl SurfaceSpec {
    pub fn to_surface(&self) -> Result<Surface, ApiError> {
        Ok(match self {
            Self::Osc {
                name,
                local_port,
                device_ip,
                device_port,
                receive,
                send,
                learn,
                pattern,
                max_packet,
                wait_ms,
            } => {
                let defaults = OscSurface::default();
                let flags = (*receive as i64 * OSC_RECEIVE) | (*send as i64 * OSC_SEND) | (*learn as i64 * OSC_LEARN);
                Surface::Osc(OscSurface {
                    name: name.clone(),
                    flags,
                    local_port: *local_port,
                    device_ip: device_ip.clone(),
                    device_port: *device_port,
                    max_packet: max_packet.unwrap_or(defaults.max_packet).max(64),
                    wait_ms: wait_ms.unwrap_or(defaults.wait_ms),
                    pattern: pattern.clone(),
                })
            }
            Self::Midi { kind, input, output, offset, size, flags } => Surface::Midi(MidiSurface {
                kind: (*kind).into(),
                offset: *offset,
                size: *size,
                input: *input,
                output: *output,
                flags: *flags,
            }),
            Self::Line { line } => {
                if line.trim().is_empty() || line.contains('\n') {
                    return Err(ApiError::invalid("give one csurf line, such as \"MCU 0 0 2 3 0\""));
                }
                Surface::parse(line)
            }
        })
    }

    pub fn from_surface(s: &Surface) -> Self {
        match s {
            Surface::Osc(o) => Self::Osc {
                name: o.name.clone(),
                local_port: o.local_port,
                device_ip: o.device_ip.clone(),
                device_port: o.device_port,
                receive: o.flags & OSC_RECEIVE != 0,
                send: o.flags & OSC_SEND != 0,
                learn: o.flags & OSC_LEARN != 0,
                pattern: o.pattern.clone(),
                max_packet: Some(o.max_packet),
                wait_ms: Some(o.wait_ms),
            },
            Surface::Midi(m) => Self::Midi {
                kind: m.kind.into(),
                input: m.input,
                output: m.output,
                offset: m.offset,
                size: m.size,
                flags: m.flags,
            },
            Surface::Other { .. } => Self::Line { line: s.line() },
        }
    }
}

/// How a configured surface is doing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SurfaceStatus {
    /// The surface's `csurf_<i>` line.
    pub line: String,
    pub running: bool,
    /// Why it is not running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The UDP port an OSC surface listens on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SurfaceInfo {
    pub index: usize,
    /// As REAPER writes it in reaper.ini.
    pub line: String,
    pub description: String,
    pub surface: SurfaceSpec,
    pub running: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

pub fn list(session: &Session) -> Vec<SurfaceInfo> {
    session
        .prefs
        .surfaces
        .iter()
        .enumerate()
        .map(|(index, s)| {
            let line = s.line();
            let status = session.surface_status.iter().find(|st| st.line == line);
            SurfaceInfo {
                index,
                line,
                description: s.describe(),
                surface: SurfaceSpec::from_surface(s),
                running: status.is_some_and(|st| st.running),
                error: status.and_then(|st| st.error.clone()),
                port: status.and_then(|st| st.port),
            }
        })
        .collect()
}

fn check(index: usize, session: &Session) -> Result<(), ApiError> {
    if index >= session.prefs.surfaces.len() {
        return Err(ApiError::not_found(format!("no surface {index}")));
    }
    Ok(())
}

pub fn execute(session: &mut Session, command: SurfaceCommand) -> Result<Value, ApiError> {
    match command {
        SurfaceCommand::List => Ok(json(list(session))),
        SurfaceCommand::Add { surface } => {
            let s = surface.to_surface()?;
            session.change_preferences(|p| p.surfaces.push(s));
            Ok(json(list(session)))
        }
        SurfaceCommand::Set { index, surface } => {
            check(index, session)?;
            let s = surface.to_surface()?;
            session.change_preferences(|p| p.surfaces[index] = s);
            Ok(json(list(session)))
        }
        SurfaceCommand::Remove { index } => {
            check(index, session)?;
            session.change_preferences(|p| {
                p.surfaces.remove(index);
            });
            Ok(json(list(session)))
        }
        SurfaceCommand::Devices => Ok(serde_json::json!({
            "inputs": scythe_engine::midi_io::names_by_number(&scythe_engine::midi_io::current_devices().inputs),
            "outputs": scythe_engine::midi_io::names_by_number(&scythe_engine::midi_io::current_devices().outputs),
        })),
        SurfaceCommand::ActionList => {
            let commands =
                actions::COMMANDS.iter().map(|(id, name)| serde_json::json!({ "id": id.to_string(), "name": name }));
            let named = actions::NAMED.iter().map(|(id, name)| serde_json::json!({ "id": id, "name": name }));
            Ok(Value::Array(commands.chain(named).collect()))
        }
        SurfaceCommand::RunAction { action } => {
            actions::run(session, &action)?;
            Ok(done())
        }
    }
}
