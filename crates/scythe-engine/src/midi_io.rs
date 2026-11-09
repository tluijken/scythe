//! Hardware MIDI through `midir`: the device map REAPER keeps (stable device numbers, aliases,
//! which devices are enabled), input ports feeding lock-free rings the audio callback drains,
//! and outputs fed from a queue on their own thread.
//!
//! Other crates (control surfaces) open ports through [`MidiDevices`], [`open_input`] and
//! [`MidiOut`] so every part of Scythe agrees on device numbers and names.

use midir::{MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};
pub use scythe_core::midi_route::{ALL_DEVICES, MIDI_INPUT_FLAG, MidiDevice, MidiInput as RecInput, VIRTUAL_KEYBOARD};

/// A channel message from a MIDI input (or the virtual keyboard), with the device number it
/// came from. Sysex and system messages do not travel this way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveMidi {
    pub bytes: [u8; 3],
    pub len: u8,
    pub device: u8,
}

/// Bytes in a message with this status byte (sysex counts as its status alone).
pub fn message_len(status: u8) -> usize {
    match status {
        0xc0..=0xdf | 0xf1 | 0xf3 => 2,
        0x80..=0xbf | 0xe0..=0xef | 0xf2 => 3,
        _ => 1,
    }
}

impl LiveMidi {
    pub fn new(bytes: [u8; 3], len: u8) -> Self {
        Self { bytes, len, device: 0 }
    }

    /// A channel message, from device 0; `None` for anything else or a short message.
    pub fn parse(msg: &[u8]) -> Option<Self> {
        let status = *msg.first()?;
        if !(0x80..0xf0).contains(&status) {
            return None;
        }
        let len = message_len(status);
        if msg.len() < len {
            return None;
        }
        let mut bytes = [0u8; 3];
        bytes[..len].copy_from_slice(&msg[..len]);
        Some(Self::new(bytes, len as u8))
    }

    pub fn from_device(self, device: u8) -> Self {
        Self { device, ..self }
    }

    pub fn channel(&self) -> u8 {
        self.bytes[0] & 0x0f
    }
}

/// REAPER's `I_RECINPUT` for MIDI: 4096, plus the device number times 32 (63 for all inputs,
/// 62 for the virtual keyboard), plus the channel (0 for all, 1-16).
pub const ALL_MIDI_INPUTS: i64 = MIDI_INPUT_FLAG | (ALL_DEVICES as i64) << 5;

/// The channel filter of a MIDI record input: `Some(None)` for all channels, `Some(Some(c))`
/// for channel `c` (0-15), `None` when the input is not MIDI.
pub fn midi_input_channel(rec_input: i64) -> Option<Option<u8>> {
    RecInput::from_rec_input(rec_input).map(|i| i.channel)
}

/// Timing and transport messages an input can carry for synchronization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMsg {
    Clock,
    Start,
    Continue,
    Stop,
    /// Song position pointer, in sixteenth notes.
    SongPosition(u16),
    /// An MTC quarter frame: piece (0-7) in the high nibble, value in the low one.
    QuarterFrame(u8),
    /// An MTC full frame (locate): hours with the rate code in bits 5-6, minutes, seconds,
    /// frames.
    FullFrame([u8; 4]),
}

impl SyncMsg {
    pub fn parse(msg: &[u8]) -> Option<Self> {
        Some(match *msg {
            [0xf8, ..] => Self::Clock,
            [0xfa, ..] => Self::Start,
            [0xfb, ..] => Self::Continue,
            [0xfc, ..] => Self::Stop,
            [0xf2, lo, hi, ..] => Self::SongPosition((lo as u16 & 0x7f) | (hi as u16 & 0x7f) << 7),
            [0xf1, v, ..] => Self::QuarterFrame(v),
            [0xf0, 0x7f, _, 0x01, 0x01, h, m, s, f, 0xf7] => Self::FullFrame([h, m, s, f]),
            _ => return None,
        })
    }
}

/// A synchronization message, when it arrived and from which device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncInput {
    pub device: u8,
    pub at: std::time::Instant,
    pub msg: SyncMsg,
}

/// Ports scythe opens itself show up in the system's lists too; using them would loop.
fn foreign(name: &str) -> bool {
    !name.contains("scythe")
}

/// A port name without the ALSA client and port numbers, which change from one session to
/// the next ("Keystation:Keystation MIDI 1 24:0" becomes "Keystation:Keystation MIDI 1").
pub fn stable_name(name: &str) -> &str {
    let numeric = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    match name.rsplit_once(' ') {
        Some((head, ids)) if ids.split_once(':').is_some_and(|(c, p)| numeric(c) && numeric(p)) => head,
        _ => name,
    }
}

pub fn input_names() -> Vec<String> {
    let Ok(input) = MidiInput::new("scythe") else { return Vec::new() };
    input.ports().iter().filter_map(|p| input.port_name(p).ok()).filter(|n| foreign(n)).collect()
}

pub fn output_names() -> Vec<String> {
    let Ok(output) = MidiOutput::new("scythe") else { return Vec::new() };
    output.ports().iter().filter_map(|p| output.port_name(p).ok()).filter(|n| foreign(n)).collect()
}

/// One MIDI device in the map: its stable number, its port name, the alias the user gave it
/// and whether the system has it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiPort {
    pub number: u8,
    pub name: String,
    pub alias: Option<String>,
    pub present: bool,
}

impl MidiPort {
    /// The name to show: the alias when there is one.
    pub fn label(&self) -> &str {
        self.alias.as_deref().filter(|a| !a.is_empty()).unwrap_or(&self.name)
    }
}

/// The devices REAPER knows, as its `reaper-midihw-linux.ini` keeps them in `[mididevcache]`:
/// `in<n>`/`on<n>` name input and output `n`, `ia<n>`/`oa<n>` hold their aliases and
/// `it<n>`/`ot<n>` their type. A device keeps its number while it is unplugged, so projects
/// that name it by number find it again.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MidiDevices {
    pub inputs: Vec<MidiPort>,
    pub outputs: Vec<MidiPort>,
}

/// The device cache's file name beside the preferences.
pub const DEVICE_CACHE_FILE: &str = "reaper-midihw-linux.ini";
const CACHE_SECTION: &str = "[mididevcache]";

impl MidiDevices {
    /// Reads the device cache; the ports are marked absent until `merge_present`.
    pub fn parse_cache(text: &str) -> Self {
        let mut map = Self::default();
        let mut in_section = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_section = line.eq_ignore_ascii_case(CACHE_SECTION);
                continue;
            }
            let Some((key, value)) = line.split_once('=').filter(|_| in_section) else { continue };
            let (kind, number) = key.split_at(key.len().min(2));
            let Ok(number) = number.parse::<u8>() else { continue };
            let list = match kind.as_bytes().first() {
                Some(b'i') => &mut map.inputs,
                Some(b'o') => &mut map.outputs,
                _ => continue,
            };
            let at = match list.iter().position(|p| p.number == number) {
                Some(i) => i,
                None => {
                    list.push(MidiPort { number, name: String::new(), alias: None, present: false });
                    list.len() - 1
                }
            };
            match kind.as_bytes().get(1) {
                Some(b'n') => list[at].name = value.to_owned(),
                Some(b'a') => list[at].alias = Some(value.to_owned()),
                _ => {}
            }
        }
        map.inputs.retain(|p| !p.name.is_empty());
        map.outputs.retain(|p| !p.name.is_empty());
        map.inputs.sort_by_key(|p| p.number);
        map.outputs.sort_by_key(|p| p.number);
        map
    }

    /// `text` with this map as its `[mididevcache]` section; other sections are kept.
    pub fn write_cache(&self, text: &str) -> String {
        let mut out = Vec::new();
        let mut skipping = false;
        for line in text.lines() {
            let t = line.trim();
            if t.starts_with('[') {
                skipping = t.eq_ignore_ascii_case(CACHE_SECTION);
            }
            if !skipping {
                out.push(line.to_owned());
            }
        }
        while out.last().is_some_and(|l| l.trim().is_empty()) {
            out.pop();
        }
        if !out.is_empty() {
            out.push(String::new());
        }
        out.push(CACHE_SECTION.to_owned());
        for (prefix, list) in [('o', &self.outputs), ('i', &self.inputs)] {
            for p in list {
                out.push(format!("{prefix}t{}=1", p.number));
                out.push(format!("{prefix}n{}={}", p.number, p.name));
                if let Some(alias) = &p.alias {
                    out.push(format!("{prefix}a{}={alias}", p.number));
                }
            }
        }
        out.join("\n") + "\n"
    }

    /// Marks the ports the system has now as present, giving new ones the lowest free number
    /// (inputs skip 62 and 63, the virtual keyboard and "all inputs").
    pub fn merge_present(&mut self, inputs: &[String], outputs: &[String]) {
        fn merge(list: &mut Vec<MidiPort>, names: &[String], reserved: &[u8]) {
            for p in list.iter_mut() {
                p.present = false;
            }
            for name in names {
                let stable = stable_name(name);
                if let Some(p) = list.iter_mut().find(|p| p.name == stable) {
                    p.present = true;
                    continue;
                }
                let free = (0..=scythe_core::midi_route::MAX_DEVICE)
                    .find(|n| !reserved.contains(n) && list.iter().all(|p| p.number != *n));
                let Some(number) = free else { continue };
                list.push(MidiPort { number, name: stable.to_owned(), alias: None, present: true });
            }
            list.sort_by_key(|p| p.number);
        }
        merge(&mut self.inputs, inputs, &[VIRTUAL_KEYBOARD, ALL_DEVICES]);
        merge(&mut self.outputs, outputs, &[]);
    }

    /// The cache in `cache_text` merged with the ports the system has now.
    pub fn scan(cache_text: &str) -> Self {
        let mut map = Self::parse_cache(cache_text);
        map.merge_present(&input_names(), &output_names());
        map
    }

    pub fn input(&self, number: u8) -> Option<&MidiPort> {
        self.inputs.iter().find(|p| p.number == number)
    }

    pub fn output(&self, number: u8) -> Option<&MidiPort> {
        self.outputs.iter().find(|p| p.number == number)
    }

    /// The display name of a record input's device.
    pub fn input_label(&self, device: MidiDevice) -> String {
        match device {
            MidiDevice::All => "All MIDI inputs".into(),
            MidiDevice::VirtualKeyboard => "Virtual MIDI keyboard".into(),
            MidiDevice::Port(n) => {
                self.input(n).map_or_else(|| format!("MIDI input {}", n + 1), |p| p.label().to_owned())
            }
        }
    }
}

static SHARED: std::sync::Mutex<Option<MidiDevices>> = std::sync::Mutex::new(None);

/// Makes `map` the device map every part of Scythe numbers ports by (the session shares its
/// own after each scan).
pub fn share_devices(map: &MidiDevices) {
    if let Ok(mut shared) = SHARED.lock() {
        *shared = Some(map.clone());
    }
}

/// The shared device map, merged with the ports present now; a fresh scan when none was
/// shared.
pub fn current_devices() -> MidiDevices {
    let shared = SHARED.lock().ok().and_then(|s| s.clone());
    let mut map = shared.unwrap_or_default();
    map.merge_present(&input_names(), &output_names());
    map
}

/// Port names by device number (empty where the number is free or the device is absent).
pub fn names_by_number(ports: &[MidiPort]) -> Vec<String> {
    let len = ports.iter().filter(|p| p.present).map(|p| p.number as usize + 1).max().unwrap_or(0);
    let mut names = vec![String::new(); len];
    for p in ports.iter().filter(|p| p.present) {
        names[p.number as usize] = p.name.clone();
    }
    names
}

/// Which devices are enabled, as reaper.ini's 64-bit masks keep them (`midiins` and
/// `midiins_h` for inputs, `midiouts` and `midiouts_h` for outputs, and the same pairs for
/// clock output and its song position pointer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeviceMask(pub u128);

impl DeviceMask {
    pub const ALL: Self = Self(u128::MAX);

    pub fn has(self, number: u8) -> bool {
        number < 128 && self.0 >> number & 1 != 0
    }

    pub fn with(self, number: u8, on: bool) -> Self {
        if number >= 128 {
            return self;
        }
        Self(if on { self.0 | 1 << number } else { self.0 & !(1 << number) })
    }

    /// From the low and high 32-bit words REAPER stores (`<key>` and `<key>_h`); devices past
    /// 63 have no bits there and count as enabled.
    pub fn from_words(lo: u32, hi: u32) -> Self {
        Self(lo as u128 | (hi as u128) << 32 | !0u128 << 64)
    }

    pub fn words(self) -> (u32, u32) {
        (self.0 as u32, (self.0 >> 32) as u32)
    }
}

/// An open MIDI input: its device number and name. Dropping it closes the port.
pub struct OpenInput {
    _connection: MidiInputConnection<()>,
    pub number: u8,
    pub name: String,
}

/// The audio callback's ends of one input: channel messages and synchronization messages.
pub struct InputRings {
    pub midi: rtrb::Consumer<LiveMidi>,
    pub sync: rtrb::Consumer<SyncInput>,
}

/// Opens the input port named `name` (with or without its ALSA numbers) as device `number`.
pub fn open_input(number: u8, name: &str) -> Result<(OpenInput, InputRings), String> {
    let mut input = MidiInput::new("scythe").map_err(|e| e.to_string())?;
    input.ignore(midir::Ignore::ActiveSense);
    let port = input
        .ports()
        .into_iter()
        .find(|p| input.port_name(p).is_ok_and(|n| n == name || stable_name(&n) == name))
        .ok_or_else(|| format!("no MIDI input named {name}"))?;
    let (mut tx, midi) = rtrb::RingBuffer::<LiveMidi>::new(1024);
    let (mut sync_tx, sync) = rtrb::RingBuffer::<SyncInput>::new(1024);
    let connection = input
        .connect(
            &port,
            "scythe-in",
            move |_, msg, _| {
                if let Some(m) = LiveMidi::parse(msg) {
                    let _ = tx.push(m.from_device(number));
                } else if let Some(s) = SyncMsg::parse(msg) {
                    let _ = sync_tx.push(SyncInput { device: number, at: std::time::Instant::now(), msg: s });
                }
            },
            (),
        )
        .map_err(|e| e.to_string())?;
    Ok((OpenInput { _connection: connection, number, name: name.to_owned() }, InputRings { midi, sync }))
}

/// Open connections to MIDI inputs. Dropping it closes them.
pub struct MidiInputs {
    pub open: Vec<OpenInput>,
    pub names: Vec<String>,
}

impl MidiInputs {
    /// Opens the given devices (number and port name); ports that fail to open are left out.
    pub fn open(devices: &[(u8, String)]) -> (Self, Vec<InputRings>) {
        let mut open = Vec::new();
        let mut rings = Vec::new();
        for (number, name) in devices {
            if let Ok((input, ring)) = open_input(*number, name) {
                open.push(input);
                rings.push(ring);
            }
        }
        let names = open.iter().map(|i| i.name.clone()).collect();
        (Self { open, names }, rings)
    }

    /// Opens every input, numbered as a fresh device map numbers them.
    pub fn open_all() -> Result<(Self, Vec<InputRings>), String> {
        let map = MidiDevices::scan("");
        let devices: Vec<(u8, String)> = map.inputs.iter().map(|p| (p.number, p.name.clone())).collect();
        Ok(Self::open(&devices))
    }
}

/// A connection to one MIDI output port.
pub struct MidiOut {
    conn: MidiOutputConnection,
    pub name: String,
}

impl MidiOut {
    /// Opens the output named `name`, with or without its ALSA numbers.
    pub fn open(name: &str) -> Result<Self, String> {
        let output = MidiOutput::new("scythe").map_err(|e| e.to_string())?;
        let port = output
            .ports()
            .into_iter()
            .find(|p| output.port_name(p).is_ok_and(|n| n == name || stable_name(&n) == name))
            .ok_or_else(|| format!("no MIDI output named {name}"))?;
        let conn = output.connect(&port, "scythe-out").map_err(|e| e.to_string())?;
        Ok(Self { conn, name: name.to_owned() })
    }

    pub fn send(&mut self, msg: &[u8]) -> Result<(), String> {
        self.conn.send(msg).map_err(|e| e.to_string())
    }
}

/// A message for a hardware output, due at an instant: up to ten bytes, so an MTC full frame
/// fits. Port `ALL_PORTS` with no bytes asks for all notes off everywhere (the transport
/// stopped).
#[derive(Debug, Clone, Copy)]
pub struct OutMessage {
    pub due: std::time::Instant,
    pub port: u8,
    pub bytes: [u8; 10],
    pub len: u8,
}

impl OutMessage {
    pub fn new(due: std::time::Instant, port: u8, msg: &[u8]) -> Self {
        let mut bytes = [0u8; 10];
        let len = msg.len().min(10);
        bytes[..len].copy_from_slice(&msg[..len]);
        Self { due, port, bytes, len: len as u8 }
    }

    pub fn midi(due: std::time::Instant, port: u8, m: LiveMidi) -> Self {
        Self::new(due, port, &m.bytes[..m.len as usize])
    }

    pub fn all_notes_off(due: std::time::Instant) -> Self {
        Self::new(due, ALL_PORTS, &[])
    }

    pub fn message(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

pub const ALL_PORTS: u8 = u8::MAX;

/// Sends queued messages to the hardware outputs when they fall due, on its own thread: the
/// audio callback only stamps and queues them.
pub struct MidiSender {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// The open outputs: device number and name.
    pub ports: Vec<(u8, String)>,
}

impl MidiSender {
    /// Opens the outputs in `devices` (device number and port name; ports that fail to open
    /// are left out) and returns the sender with the queue the audio callback writes to.
    pub fn open(devices: &[(u8, String)]) -> Result<(Self, rtrb::Producer<OutMessage>), String> {
        let mut by_number: Vec<Option<MidiOut>> = (0..128).map(|_| None).collect();
        let mut ports = Vec::new();
        for (number, name) in devices {
            if let (Some(slot), Ok(out)) = (by_number.get_mut(*number as usize), MidiOut::open(name)) {
                *slot = Some(out);
                ports.push((*number, name.clone()));
            }
        }
        if ports.is_empty() && !devices.is_empty() {
            return Err(format!("cannot open MIDI output {}", devices[0].1));
        }
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
                            for port in by_number.iter_mut().flatten() {
                                for ch in 0..16u8 {
                                    let _ = port.send(&[0xb0 | ch, 0x7b, 0]);
                                }
                            }
                        } else if let Some(Some(port)) = by_number.get_mut(m.port as usize) {
                            let _ = port.send(m.message());
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            })
            .map_err(|e| e.to_string())?;
        Ok((Self { stop, thread: Some(thread), ports }, tx))
    }

    /// Names of the open outputs.
    pub fn names(&self) -> Vec<String> {
        self.ports.iter().map(|p| p.1.clone()).collect()
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

/// Virtual ports other programs (and tests) connect to, from `midir`'s ALSA backend.
#[cfg(unix)]
pub mod virtual_ports {
    use midir::os::unix::{VirtualInput, VirtualOutput};

    /// What arrived at a [`Recorder`], with when it arrived.
    pub type Received = std::sync::Arc<std::sync::Mutex<Vec<(std::time::Instant, Vec<u8>)>>>;

    /// A virtual input that records what arrives.
    pub struct Recorder {
        _conn: midir::MidiInputConnection<()>,
        pub received: Received,
    }

    impl Recorder {
        pub fn new(name: &str) -> Result<Self, String> {
            let mut input = midir::MidiInput::new(&format!("{name}-client")).map_err(|e| e.to_string())?;
            input.ignore(midir::Ignore::None);
            let received: Received = Default::default();
            let sink = received.clone();
            let conn = input
                .create_virtual(
                    name,
                    move |_, msg, _| {
                        if let Ok(mut r) = sink.lock() {
                            r.push((std::time::Instant::now(), msg.to_vec()));
                        }
                    },
                    (),
                )
                .map_err(|e| e.to_string())?;
            Ok(Self { _conn: conn, received })
        }

        pub fn take(&self) -> Vec<(std::time::Instant, Vec<u8>)> {
            self.received.lock().map(|mut r| std::mem::take(&mut *r)).unwrap_or_default()
        }
    }

    /// A virtual output a test plays into Scythe through.
    pub fn output(name: &str) -> Result<midir::MidiOutputConnection, String> {
        let out = midir::MidiOutput::new(&format!("{name}-client")).map_err(|e| e.to_string())?;
        out.create_virtual(name).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_channel_messages() {
        assert_eq!(LiveMidi::parse(&[0x91, 60, 100]), Some(LiveMidi::new([0x91, 60, 100], 3)));
        assert_eq!(LiveMidi::parse(&[0xc2, 5]), Some(LiveMidi::new([0xc2, 5, 0], 2)));
        assert_eq!(LiveMidi::parse(&[0xf8]), None);
        assert_eq!(LiveMidi::parse(&[0xf0, 1, 2, 0xf7]), None);
        assert_eq!(LiveMidi::parse(&[0x90, 60]), None);
    }

    #[test]
    fn parses_sync_messages() {
        assert_eq!(SyncMsg::parse(&[0xf8]), Some(SyncMsg::Clock));
        assert_eq!(SyncMsg::parse(&[0xf2, 0x0c, 0x01]), Some(SyncMsg::SongPosition(140)));
        assert_eq!(SyncMsg::parse(&[0xf1, 0x35]), Some(SyncMsg::QuarterFrame(0x35)));
        let full = [0xf0, 0x7f, 0x7f, 0x01, 0x01, 0x61, 2, 3, 4, 0xf7];
        assert_eq!(SyncMsg::parse(&full), Some(SyncMsg::FullFrame([0x61, 2, 3, 4])));
        assert_eq!(SyncMsg::parse(&[0x90, 1, 2]), None);
    }

    #[test]
    fn record_input_channels() {
        assert_eq!(midi_input_channel(ALL_MIDI_INPUTS), Some(None));
        assert_eq!(midi_input_channel(ALL_MIDI_INPUTS + 10), Some(Some(9)));
        assert_eq!(midi_input_channel(1024), None);
        assert_eq!(midi_input_channel(-1), None);
    }

    #[test]
    fn stable_names_drop_alsa_numbers() {
        assert_eq!(stable_name("Midi Through:Midi Through Port-0 14:0"), "Midi Through:Midi Through Port-0");
        assert_eq!(stable_name("MIDI Input 1"), "MIDI Input 1");
        assert_eq!(stable_name("x 1:"), "x 1:");
    }

    /// The layout REAPER 7.73 writes, including the alias lines it reads back (`ia`, `oa`).
    #[test]
    fn device_cache_round_trip() {
        let text = "[other]\nx=1\n\n[mididevcache]\not0=1\non0=MIDI Output 1\noa0=Synth\nit0=1\nin0=MIDI Input 1\nia0=Keys\nit1=1\nin1=MIDI Input 2\n";
        let mut map = MidiDevices::parse_cache(text);
        assert_eq!(map.inputs.len(), 2);
        assert_eq!(map.input(0).unwrap().label(), "Keys");
        assert_eq!(map.output(0).unwrap().label(), "Synth");
        assert_eq!(map.write_cache(text), text);
        map.merge_present(&["MIDI Input 2 20:0".into(), "Pads 24:0".into()], &[]);
        assert!(!map.input(0).unwrap().present && map.input(1).unwrap().present);
        assert_eq!(map.input(2).unwrap().name, "Pads", "a new device takes the lowest free number");
        let mut full = MidiDevices::default();
        let names: Vec<String> = (0..64).map(|i| format!("d{i}")).collect();
        full.merge_present(&names, &[]);
        assert!(full.input(62).is_none() && full.input(63).is_none() && full.input(65).is_some());
        assert_eq!(full.input_label(MidiDevice::VirtualKeyboard), "Virtual MIDI keyboard");
    }

    #[test]
    fn device_masks() {
        let m = DeviceMask::from_words(5, 1);
        assert!(m.has(0) && !m.has(1) && m.has(2) && m.has(32) && !m.has(33) && m.has(70));
        assert_eq!(m.with(1, true).words(), (7, 1));
        assert_eq!(m.with(32, false).words(), (5, 0));
    }

    #[test]
    fn out_messages_hold_a_full_frame() {
        let now = std::time::Instant::now();
        let full = [0xf0, 0x7f, 0x7f, 0x01, 0x01, 0x61, 2, 3, 4, 0xf7];
        assert_eq!(OutMessage::new(now, 0, &full).message(), &full);
        assert_eq!(message_len(0xf2), 3);
        assert_eq!(message_len(0xf8), 1);
    }
}
