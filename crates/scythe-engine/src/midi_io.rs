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

/// Ports scythe opens itself show up in the system's lists too; using them would loop.
fn foreign(name: &str) -> bool {
    !name.contains("scythe")
}

pub fn input_names() -> Vec<String> {
    let Ok(input) = MidiInput::new("scythe") else { return Vec::new() };
    input.ports().iter().filter_map(|p| input.port_name(p).ok()).filter(|n| foreign(n)).collect()
}

pub fn output_names() -> Vec<String> {
    let Ok(output) = MidiOutput::new("scythe") else { return Vec::new() };
    output.ports().iter().filter_map(|p| output.port_name(p).ok()).filter(|n| foreign(n)).collect()
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
            if !foreign(&name) {
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

/// A message for a hardware output, due at an instant. Port `ALL_PORTS` with no bytes asks for
/// all notes off everywhere (the transport stopped).
#[derive(Debug, Clone, Copy)]
pub struct OutMessage {
    pub due: std::time::Instant,
    pub port: u8,
    pub midi: LiveMidi,
}

pub const ALL_PORTS: u8 = u8::MAX;

/// Sends queued messages to the hardware outputs when they fall due, on its own thread: the
/// audio callback only stamps and queues them.
pub struct MidiSender {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    pub names: Vec<String>,
}

impl MidiSender {
    /// Opens the outputs named in `names` (their order gives the port numbers) and returns the
    /// sender with the queue the audio callback writes to.
    pub fn open(names: &[String]) -> Result<(Self, rtrb::Producer<OutMessage>), String> {
        let mut ports = names.iter().map(|n| MidiOut::open(n)).collect::<Result<Vec<_>, _>>()?;
        let (tx, mut rx) = rtrb::RingBuffer::<OutMessage>::new(8192);
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_t = stop.clone();
        let thread = std::thread::Builder::new()
            .name("scythe-midi-out".into())
            .spawn(move || {
                let mut pending: Vec<OutMessage> = Vec::new();
                while !stop_t.load(std::sync::atomic::Ordering::Relaxed) {
                    while let Ok(m) = rx.pop() {
                        pending.push(m);
                    }
                    let now = std::time::Instant::now();
                    pending.sort_by_key(|m| m.due);
                    let due = pending.partition_point(|m| m.due <= now);
                    for m in pending.drain(..due) {
                        if m.port == ALL_PORTS {
                            for port in &mut ports {
                                for ch in 0..16u8 {
                                    let _ = port.send(&[0xb0 | ch, 0x7b, 0]);
                                }
                            }
                        } else if let Some(port) = ports.get_mut(m.port as usize) {
                            let _ = port.send(&m.midi.bytes[..m.midi.len as usize]);
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            })
            .map_err(|e| e.to_string())?;
        Ok((Self { stop, thread: Some(thread), names: names.to_vec() }, tx))
    }
}

impl Drop for MidiSender {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
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
