//! Hardware MIDI through `midir`: every input port feeds a lock-free ring the audio callback
//! drains, and outputs take messages from the main thread.

use midir::{MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};

/// A channel message from a MIDI input (sysex and system messages are dropped).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveMidi {
    pub bytes: [u8; 3],
    pub len: u8,
}

impl LiveMidi {
    pub fn parse(msg: &[u8]) -> Option<Self> {
        let status = *msg.first()?;
        if !(0x80..0xf0).contains(&status) {
            return None;
        }
        let len = if matches!(status & 0xf0, 0xc0 | 0xd0) { 2 } else { 3 };
        if msg.len() < len {
            return None;
        }
        let mut bytes = [0u8; 3];
        bytes[..len].copy_from_slice(&msg[..len]);
        Some(Self { bytes, len: len as u8 })
    }

    pub fn channel(&self) -> u8 {
        self.bytes[0] & 0x0f
    }
}

/// REAPER's `I_RECINPUT` for MIDI: 4096, plus the device index times 32 (63 for all inputs),
/// plus the channel (0 for all, 1-16).
pub const MIDI_INPUT_FLAG: i64 = 4096;
pub const ALL_MIDI_INPUTS: i64 = MIDI_INPUT_FLAG | 63 << 5;

/// The channel filter of a MIDI record input: `Some(None)` for all channels, `Some(Some(c))`
/// for channel `c` (0-15), `None` when the input is not MIDI.
pub fn midi_input_channel(rec_input: i64) -> Option<Option<u8>> {
    if rec_input < 0 || rec_input & MIDI_INPUT_FLAG == 0 {
        return None;
    }
    let ch = (rec_input & 31) as u8;
    Some((ch > 0).then(|| ch - 1))
}

pub fn input_names() -> Vec<String> {
    let Ok(input) = MidiInput::new("scythe") else { return Vec::new() };
    input.ports().iter().filter_map(|p| input.port_name(p).ok()).collect()
}

pub fn output_names() -> Vec<String> {
    let Ok(output) = MidiOutput::new("scythe") else { return Vec::new() };
    output.ports().iter().filter_map(|p| output.port_name(p).ok()).collect()
}

/// Open connections to every MIDI input. Dropping it closes them.
pub struct MidiInputs {
    _connections: Vec<MidiInputConnection<()>>,
    pub names: Vec<String>,
}

impl MidiInputs {
    /// Connects to all input ports; returns the open connections and the consumer side of the
    /// ring each one writes to.
    pub fn open_all() -> Result<(Self, Vec<rtrb::Consumer<LiveMidi>>), String> {
        let probe = MidiInput::new("scythe").map_err(|e| e.to_string())?;
        let ports = probe.ports();
        let mut connections = Vec::new();
        let mut consumers = Vec::new();
        let mut names = Vec::new();
        for port in &ports {
            let name = probe.port_name(port).unwrap_or_else(|_| "MIDI input".into());
            // Our own output ports show up as inputs too; listening to them would loop.
            if name.contains("scythe") {
                continue;
            }
            let input = MidiInput::new("scythe").map_err(|e| e.to_string())?;
            let (mut tx, rx) = rtrb::RingBuffer::<LiveMidi>::new(1024);
            let conn = input.connect(
                port,
                "scythe-in",
                move |_, msg, _| {
                    if let Some(m) = LiveMidi::parse(msg) {
                        let _ = tx.push(m);
                    }
                },
                (),
            );
            if let Ok(conn) = conn {
                connections.push(conn);
                consumers.push(rx);
                names.push(name);
            }
        }
        Ok((Self { _connections: connections, names }, consumers))
    }
}

/// A connection to one MIDI output port.
pub struct MidiOut {
    conn: MidiOutputConnection,
    pub name: String,
}

impl MidiOut {
    pub fn open(name: &str) -> Result<Self, String> {
        let output = MidiOutput::new("scythe").map_err(|e| e.to_string())?;
        let port = output
            .ports()
            .into_iter()
            .find(|p| output.port_name(p).is_ok_and(|n| n == name))
            .ok_or_else(|| format!("no MIDI output named {name}"))?;
        let conn = output.connect(&port, "scythe-out").map_err(|e| e.to_string())?;
        Ok(Self { conn, name: name.to_owned() })
    }

    pub fn send(&mut self, msg: &[u8]) -> Result<(), String> {
        self.conn.send(msg).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_channel_messages() {
        assert_eq!(LiveMidi::parse(&[0x91, 60, 100]), Some(LiveMidi { bytes: [0x91, 60, 100], len: 3 }));
        assert_eq!(LiveMidi::parse(&[0xc2, 5]), Some(LiveMidi { bytes: [0xc2, 5, 0], len: 2 }));
        assert_eq!(LiveMidi::parse(&[0xf8]), None);
        assert_eq!(LiveMidi::parse(&[0xf0, 1, 2, 0xf7]), None);
        assert_eq!(LiveMidi::parse(&[0x90, 60]), None);
    }

    #[test]
    fn record_input_channels() {
        assert_eq!(midi_input_channel(ALL_MIDI_INPUTS), Some(None));
        assert_eq!(midi_input_channel(ALL_MIDI_INPUTS + 10), Some(Some(9)));
        assert_eq!(midi_input_channel(1024), None);
        assert_eq!(midi_input_channel(-1), None);
    }
}
