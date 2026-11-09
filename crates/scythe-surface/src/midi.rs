//! MIDI ports for hardware surfaces. Devices are numbered as in REAPER: by the stable device
//! numbers of the shared device map (`scythe_engine::midi_io`), leaving out Scythe's own ports.

use std::sync::mpsc::{Receiver, channel};

use midir::{MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};

use crate::Waker;

fn foreign(name: &str) -> bool {
    !name.contains("scythe")
}

/// The MIDI input ports, indexed by device number (empty where none is present).
pub fn input_names() -> Vec<String> {
    scythe_engine::midi_io::names_by_number(&scythe_engine::midi_io::current_devices().inputs)
}

/// The MIDI output ports, indexed by device number (empty where none is present).
pub fn output_names() -> Vec<String> {
    scythe_engine::midi_io::names_by_number(&scythe_engine::midi_io::current_devices().outputs)
}

/// Whether the system's port `full` is the device named `name` in the map.
fn is_port(full: &str, name: &str) -> bool {
    foreign(full) && (full == name || scythe_engine::midi_io::stable_name(full) == name)
}

/// An open input and output: messages from the device arrive on `rx`.
pub struct MidiPorts {
    _input: Option<MidiInputConnection<()>>,
    output: Option<MidiOutputConnection>,
    pub rx: Receiver<Vec<u8>>,
    pub names: (Option<String>, Option<String>),
}

impl MidiPorts {
    /// Opens input device `input` and output device `output` (negative for none).
    pub fn open(input: i64, output: i64, wake: Waker) -> Result<Self, String> {
        let (tx, rx) = channel();
        let mut names = (None, None);
        let input_conn = if input >= 0 {
            let midi = MidiInput::new("scythe-surface").map_err(|e| e.to_string())?;
            let name = input_names().get(input as usize).cloned().filter(|n| !n.is_empty());
            let name = name.ok_or_else(|| format!("no MIDI input device {input}"))?;
            let port = midi
                .ports()
                .into_iter()
                .find(|p| midi.port_name(p).is_ok_and(|n| is_port(&n, &name)))
                .ok_or_else(|| format!("no MIDI input device {input}"))?;
            names.0 = midi.port_name(&port).ok();
            let conn = midi
                .connect(
                    &port,
                    "scythe-surface-in",
                    move |_, bytes, _| {
                        if tx.send(bytes.to_vec()).is_ok() {
                            wake();
                        }
                    },
                    (),
                )
                .map_err(|e| e.to_string())?;
            Some(conn)
        } else {
            None
        };
        let output_conn = if output >= 0 {
            let midi = MidiOutput::new("scythe-surface").map_err(|e| e.to_string())?;
            let name = output_names().get(output as usize).cloned().filter(|n| !n.is_empty());
            let name = name.ok_or_else(|| format!("no MIDI output device {output}"))?;
            let port = midi
                .ports()
                .into_iter()
                .find(|p| midi.port_name(p).is_ok_and(|n| is_port(&n, &name)))
                .ok_or_else(|| format!("no MIDI output device {output}"))?;
            names.1 = midi.port_name(&port).ok();
            Some(midi.connect(&port, "scythe-surface-out").map_err(|e| e.to_string())?)
        } else {
            None
        };
        Ok(Self { _input: input_conn, output: output_conn, rx, names })
    }

    pub fn send(&mut self, bytes: &[u8]) {
        if let Some(out) = &mut self.output {
            let _ = out.send(bytes);
        }
    }
}

/// Splits a buffer from a port into MIDI messages (a port may deliver several at once, or
/// a SysEx whole).
pub fn split(bytes: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let status = bytes[at];
        let len = match status {
            0xf0 => bytes[at..].iter().position(|&b| b == 0xf7).map_or(bytes.len() - at, |p| p + 1),
            0x80..=0xbf | 0xe0..=0xef => 3,
            0xc0..=0xdf => 2,
            0xf1 | 0xf3 => 2,
            0xf2 => 3,
            _ => 1,
        };
        let end = (at + len).min(bytes.len());
        out.push(&bytes[at..end]);
        at = end;
    }
    out
}
