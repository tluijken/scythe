//! Control surfaces as REAPER lists them in reaper.ini: `csurf_cnt=N`, then one
//! `csurf_<i>=<TYPE> <config>` line each. OSC devices are
//! `OSC "<name>" <flags> <local port> "<device ip>" <device port> <max packet> <wait ms> "<pattern file>"`;
//! the MIDI surfaces (MCU, MCU extender, HUI, FaderPort) are
//! `<TYPE> <offset> <size tweak> <input device> <output device> <flags>`. A type Scythe does
//! not drive keeps its line as it was.

use crate::rpp::tokenize;

/// `flags` bit of an OSC surface: listen on the local port.
pub const OSC_RECEIVE: i64 = 1;
/// `flags` bit of an OSC surface: send feedback to the device.
pub const OSC_SEND: i64 = 2;
/// `flags` bit of an OSC surface: messages may run actions (`/action`) and FX learn.
pub const OSC_ALLOW_ACTIONS: i64 = 4;

#[derive(Debug, Clone, PartialEq)]
pub enum Surface {
    Osc(OscSurface),
    Midi(MidiSurface),
    /// A surface type Scythe does not drive, kept as written.
    Other {
        kind: String,
        config: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct OscSurface {
    pub name: String,
    /// `OSC_*` bits.
    pub flags: i64,
    pub local_port: u16,
    pub device_ip: String,
    pub device_port: u16,
    /// Feedback is bundled into packets of at most this many bytes.
    pub max_packet: u32,
    /// Milliseconds to wait between feedback packets.
    pub wait_ms: u32,
    /// The `.ReaperOSC` pattern file (a name in the OSC folder or a path); empty for the default.
    pub pattern: String,
}

impl Default for OscSurface {
    fn default() -> Self {
        Self {
            name: String::new(),
            flags: OSC_RECEIVE | OSC_SEND | OSC_ALLOW_ACTIONS,
            local_port: 8000,
            device_ip: "127.0.0.1".into(),
            device_port: 9000,
            max_packet: 1024,
            wait_ms: 10,
            pattern: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MidiKind {
    /// Mackie Control Universal (`MCU`).
    Mcu,
    /// Mackie Control extender (`MCUEX`).
    McuExtender,
    /// HUI (`HUI`, "partial" in REAPER).
    Hui,
    /// PreSonus FaderPort (`FADERPORT`).
    FaderPort,
    /// PreSonus FaderPort v2, 2018 (`FADERPORT2`).
    FaderPort2,
}

impl MidiKind {
    pub const ALL: [MidiKind; 5] = [Self::Mcu, Self::McuExtender, Self::Hui, Self::FaderPort, Self::FaderPort2];

    /// The type name in reaper.ini.
    pub fn id(self) -> &'static str {
        match self {
            Self::Mcu => "MCU",
            Self::McuExtender => "MCUEX",
            Self::Hui => "HUI",
            Self::FaderPort => "FADERPORT",
            Self::FaderPort2 => "FADERPORT2",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.id() == id)
    }

    /// The name REAPER's control surface list shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::Mcu => "Mackie Control Universal",
            Self::McuExtender => "Mackie Control Extender",
            Self::Hui => "HUI (partial)",
            Self::FaderPort => "PreSonus FaderPort",
            Self::FaderPort2 => "PreSonus FaderPort v2 (2018)",
        }
    }

    /// Channel strips on the device.
    pub fn strips(self) -> usize {
        match self {
            Self::Mcu | Self::McuExtender | Self::Hui => 8,
            Self::FaderPort | Self::FaderPort2 => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MidiSurface {
    pub kind: MidiKind,
    /// Tracks the first strip is moved on by (an extender to the right of an MCU uses 8).
    pub offset: i64,
    /// REAPER's "size tweak"; kept, not used.
    pub size: i64,
    /// MIDI input and output device, counted from 0; -1 for none.
    pub input: i64,
    pub output: i64,
    /// Option bits; kept as written.
    pub flags: i64,
}

impl MidiSurface {
    pub fn new(kind: MidiKind, input: i64, output: i64) -> Self {
        Self { kind, offset: 0, size: 0, input, output, flags: 0 }
    }
}

impl Surface {
    /// The type name in reaper.ini.
    pub fn kind(&self) -> &str {
        match self {
            Self::Osc(_) => "OSC",
            Self::Midi(m) => m.kind.id(),
            Self::Other { kind, .. } => kind,
        }
    }

    /// The part of the line after the type.
    pub fn config(&self) -> String {
        match self {
            Self::Osc(o) => format!(
                "{} {} {} {} {} {} {} {}",
                quote(&o.name),
                o.flags,
                o.local_port,
                quote(&o.device_ip),
                o.device_port,
                o.max_packet,
                o.wait_ms,
                quote(&o.pattern)
            ),
            Self::Midi(m) => format!("{} {} {} {} {}", m.offset, m.size, m.input, m.output, m.flags),
            Self::Other { config, .. } => config.clone(),
        }
    }

    /// The value of a `csurf_<i>=` line.
    pub fn line(&self) -> String {
        let config = self.config();
        if config.is_empty() { self.kind().to_owned() } else { format!("{} {config}", self.kind()) }
    }

    /// Reads the value of a `csurf_<i>=` line.
    pub fn parse(line: &str) -> Self {
        let line = line.trim();
        let (kind, config) = line.split_once(' ').unwrap_or((line, ""));
        let config = config.trim();
        let other = || Self::Other { kind: kind.to_owned(), config: config.to_owned() };
        let tokens = tokenize(config);
        let int = |i: usize| tokens.get(i).and_then(|t| t.parse::<i64>().ok());
        match kind {
            "OSC" => {
                let defaults = OscSurface::default();
                let port = |i: usize, d: u16| int(i).and_then(|v| u16::try_from(v).ok()).unwrap_or(d);
                let count = |i: usize, d: u32| int(i).and_then(|v| u32::try_from(v).ok()).unwrap_or(d);
                Self::Osc(OscSurface {
                    name: tokens.first().cloned().unwrap_or_default(),
                    flags: int(1).unwrap_or(defaults.flags),
                    local_port: port(2, defaults.local_port),
                    device_ip: tokens.get(3).cloned().unwrap_or(defaults.device_ip),
                    device_port: port(4, defaults.device_port),
                    max_packet: count(5, defaults.max_packet),
                    wait_ms: count(6, defaults.wait_ms),
                    pattern: tokens.get(7).cloned().unwrap_or_default(),
                })
            }
            _ => match MidiKind::from_id(kind) {
                Some(kind) if tokens.len() >= 4 && (0..tokens.len().min(5)).all(|i| int(i).is_some()) => {
                    Self::Midi(MidiSurface {
                        kind,
                        offset: int(0).unwrap_or(0),
                        size: int(1).unwrap_or(0),
                        input: int(2).unwrap_or(-1),
                        output: int(3).unwrap_or(-1),
                        flags: int(4).unwrap_or(0),
                    })
                }
                _ => other(),
            },
        }
    }

    /// A short description for lists: the device name or ports.
    pub fn describe(&self) -> String {
        match self {
            Self::Osc(o) => {
                let name = if o.name.is_empty() { "OSC".to_owned() } else { format!("OSC \"{}\"", o.name) };
                format!("{name}: port {} → {}:{}", o.local_port, o.device_ip, o.device_port)
            }
            Self::Midi(m) => format!("{} (devices {}, {})", m.kind.label(), m.input, m.output),
            Self::Other { kind, config } => format!("{kind} {config}"),
        }
    }
}

/// REAPER quotes every string field, even empty ones; a value holding a double quote is
/// wrapped in single quotes or backticks instead, as RPP files do.
fn quote(s: &str) -> String {
    if !s.contains('"') {
        format!("\"{s}\"")
    } else if !s.contains('\'') {
        format!("'{s}'")
    } else {
        format!("`{}`", s.replace('`', "'"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_reaper_lines() {
        // Lines as REAPER 7.73 reads them (behaviour checked against a running REAPER: the
        // ports it binds and sends to, the strip offset and devices an MCU uses).
        let osc = r#"OSC "TouchOSC" 3 9123 "127.0.0.1" 9124 1024 10 """#;
        let s = Surface::parse(osc);
        assert_eq!(
            s,
            Surface::Osc(OscSurface {
                name: "TouchOSC".into(),
                flags: 3,
                local_port: 9123,
                device_ip: "127.0.0.1".into(),
                device_port: 9124,
                max_packet: 1024,
                wait_ms: 10,
                pattern: String::new(),
            })
        );
        assert_eq!(s.line(), osc);
        let mcu = "MCU 8 0 2 3 0";
        let s = Surface::parse(mcu);
        assert_eq!(
            s,
            Surface::Midi(MidiSurface { kind: MidiKind::Mcu, offset: 8, size: 0, input: 2, output: 3, flags: 0 })
        );
        assert_eq!(s.line(), mcu);
        for line in ["MCUEX 8 0 1 1 0", "HUI 0 0 0 0 0", "FADERPORT 0 0 4 5 0", "FADERPORT2 0 0 4 5 0"] {
            assert!(matches!(Surface::parse(line), Surface::Midi(_)), "{line}");
            assert_eq!(Surface::parse(line).line(), line);
        }
        let other = "BCF2000 0 1 2";
        assert_eq!(Surface::parse(other), Surface::Other { kind: "BCF2000".into(), config: "0 1 2".into() });
        assert_eq!(Surface::parse(other).line(), other);
        assert_eq!(Surface::parse("HUI 0 x").line(), "HUI 0 x");
        let named = Surface::Osc(OscSurface { name: "say \"hi\"".into(), ..Default::default() });
        assert_eq!(Surface::parse(&named.line()), named);
    }
}
