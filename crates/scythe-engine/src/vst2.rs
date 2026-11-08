//! Minimal VST 2.4 host for Linux `.so` plugins, written against the publicly known binary
//! interface (no SDK headers). It covers what playback needs: loading, state chunks,
//! parameters, MIDI events and `processReplacing`.

use std::ffi::{CStr, c_char, c_void};
use std::path::{Path, PathBuf};

const MAGIC: i32 = i32::from_be_bytes(*b"VstP");

const EFF_OPEN: i32 = 0;
const EFF_CLOSE: i32 = 1;
const EFF_GET_PARAM_NAME: i32 = 8;
const EFF_GET_PARAM_DISPLAY: i32 = 7;
const EFF_SET_SAMPLE_RATE: i32 = 10;
const EFF_SET_BLOCK_SIZE: i32 = 11;
const EFF_EDIT_GET_RECT: i32 = 13;
const EFF_EDIT_OPEN: i32 = 14;
const EFF_EDIT_CLOSE: i32 = 15;
const EFF_EDIT_IDLE: i32 = 19;
const EFF_MAINS_CHANGED: i32 = 12;
const EFF_GET_CHUNK: i32 = 23;
const EFF_SET_CHUNK: i32 = 24;
const EFF_PROCESS_EVENTS: i32 = 25;
const EFF_GET_EFFECT_NAME: i32 = 45;
const EFF_GET_VENDOR_STRING: i32 = 47;
const EFF_START_PROCESS: i32 = 71;
const EFF_STOP_PROCESS: i32 = 72;

const AM_VERSION: i32 = 1;
const AM_CURRENT_ID: i32 = 2;
const AM_GET_TIME: i32 = 7;
const AM_SIZE_WINDOW: i32 = 15;
const AM_GET_SAMPLE_RATE: i32 = 16;
const AM_GET_BLOCK_SIZE: i32 = 17;
const AM_GET_CURRENT_PROCESS_LEVEL: i32 = 23;
const AM_GET_VENDOR_STRING: i32 = 32;
const AM_GET_PRODUCT_STRING: i32 = 33;
const AM_GET_VENDOR_VERSION: i32 = 34;
const AM_CAN_DO: i32 = 37;

pub const FLAG_HAS_EDITOR: i32 = 1;
pub const FLAG_CAN_REPLACING: i32 = 1 << 4;
pub const FLAG_PROGRAM_CHUNKS: i32 = 1 << 5;
pub const FLAG_IS_SYNTH: i32 = 1 << 8;

type HostCallback = extern "C" fn(*mut AEffect, i32, i32, isize, *mut c_void, f32) -> isize;
type Dispatcher = extern "C" fn(*mut AEffect, i32, i32, isize, *mut c_void, f32) -> isize;
type ProcessReplacing = extern "C" fn(*mut AEffect, *const *const f32, *mut *mut f32, i32);
type SetParameter = extern "C" fn(*mut AEffect, i32, f32);
type GetParameter = extern "C" fn(*mut AEffect, i32) -> f32;
type PluginMain = extern "C" fn(HostCallback) -> *mut AEffect;

#[repr(C)]
pub struct AEffect {
    magic: i32,
    dispatcher: Dispatcher,
    process: *const c_void,
    set_parameter: SetParameter,
    get_parameter: GetParameter,
    num_programs: i32,
    num_params: i32,
    num_inputs: i32,
    num_outputs: i32,
    flags: i32,
    reserved1: isize,
    reserved2: isize,
    initial_delay: i32,
    real_qualities: i32,
    off_qualities: i32,
    io_ratio: f32,
    object: *mut c_void,
    user: *mut c_void,
    unique_id: i32,
    version: i32,
    process_replacing: ProcessReplacing,
    process_double_replacing: *const c_void,
    future: [u8; 56],
}

/// Host-side transport information handed to plugins on request.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct TimeInfo {
    pub sample_pos: f64,
    pub sample_rate: f64,
    pub nano_seconds: f64,
    pub ppq_pos: f64,
    pub tempo: f64,
    pub bar_start_pos: f64,
    pub cycle_start_pos: f64,
    pub cycle_end_pos: f64,
    pub time_sig_numerator: i32,
    pub time_sig_denominator: i32,
    pub smpte_offset: i32,
    pub smpte_frame_rate: i32,
    pub samples_to_next_clock: i32,
    pub flags: i32,
}

pub const TIME_PLAYING: i32 = 1 << 1;
pub const TIME_PPQ_VALID: i32 = 1 << 9;
pub const TIME_TEMPO_VALID: i32 = 1 << 10;
pub const TIME_BARS_VALID: i32 = 1 << 11;
pub const TIME_SIG_VALID: i32 = 1 << 13;

#[repr(C)]
struct MidiEvent {
    kind: i32,
    byte_size: i32,
    delta_frames: i32,
    flags: i32,
    note_length: i32,
    note_offset: i32,
    midi_data: [u8; 4],
    detune: i8,
    note_off_velocity: u8,
    reserved1: u8,
    reserved2: u8,
}

const MAX_EVENTS: usize = 512;

#[repr(C)]
struct Events {
    num_events: i32,
    reserved: isize,
    events: [*mut MidiEvent; MAX_EVENTS],
}

/// Per-instance data the host callback reaches through `AEffect::user`.
struct HostData {
    sample_rate: f32,
    block_size: i32,
    time: TimeInfo,
    /// Editor size the plugin asked for, as width << 32 | height (0 when none).
    resize: std::sync::atomic::AtomicU64,
}

extern "C" fn host_callback(
    effect: *mut AEffect,
    opcode: i32,
    _index: i32,
    _value: isize,
    ptr: *mut c_void,
    _opt: f32,
) -> isize {
    if std::env::var_os("SCYTHE_VST_TRACE").is_some() {
        eprintln!("vst host callback: opcode {opcode} (0x{opcode:x}) index {_index} value {_value}");
    }
    let host = if effect.is_null() {
        None
    } else {
        // SAFETY: `user` is either null or the HostData box owned by the Vst2Plugin of `effect`.
        unsafe { ((*effect).user as *mut HostData).as_mut() }
    };
    match opcode {
        AM_VERSION => 2400,
        AM_CURRENT_ID => 0,
        AM_GET_SAMPLE_RATE => host.map_or(48000, |h| h.sample_rate as isize),
        AM_GET_BLOCK_SIZE => host.map_or(512, |h| h.block_size as isize),
        AM_GET_TIME => host.map_or(0, |h| &mut h.time as *mut TimeInfo as isize),
        AM_SIZE_WINDOW => {
            if let Some(h) = host {
                let packed = ((_index.max(1) as u64) << 32) | (_value.max(1) as u64 & 0xffff_ffff);
                h.resize.store(packed, std::sync::atomic::Ordering::Relaxed);
            }
            1
        }
        AM_GET_CURRENT_PROCESS_LEVEL => 0,
        AM_GET_VENDOR_STRING | AM_GET_PRODUCT_STRING if !ptr.is_null() => {
            let s = b"Scythe\0";
            // SAFETY: the VST convention gives a buffer of at least 64 bytes for these strings.
            unsafe { std::ptr::copy_nonoverlapping(s.as_ptr(), ptr as *mut u8, s.len()) };
            1
        }
        AM_GET_VENDOR_VERSION => 1,
        AM_CAN_DO if !ptr.is_null() => {
            // SAFETY: canDo strings are NUL-terminated C strings.
            let what = unsafe { CStr::from_ptr(ptr as *const c_char) }.to_string_lossy();
            matches!(what.as_ref(), "sendVstTimeInfo" | "sendVstMidiEvent" | "sendVstEvents" | "receiveVstEvents")
                as isize
        }
        _ => 0,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Vst2Error {
    #[error("{0}: {1}")]
    Load(PathBuf, String),
    #[error("{0}: no VSTPluginMain entry point")]
    NoEntry(PathBuf),
    #[error("{0}: plugin did not return a valid instance")]
    BadInstance(PathBuf),
}

/// A loaded plugin instance. Not thread safe by itself: the engine serialises access.
pub struct Vst2Plugin {
    effect: *mut AEffect,
    host: Box<HostData>,
    events: Box<Events>,
    event_storage: Box<[MidiEvent; MAX_EVENTS]>,
    pub path: PathBuf,
    // Dropped last, after the effect is closed.
    _lib: libloading::Library,
}

// SAFETY: the plugin is only used from one thread at a time (the engine wraps it in a mutex);
// VST 2 plugins may be called from any thread as long as calls do not overlap.
unsafe impl Send for Vst2Plugin {}

impl Vst2Plugin {
    pub fn load(path: &Path, sample_rate: f32, block_size: i32) -> Result<Self, Vst2Error> {
        // SAFETY: loading a plugin runs its initialisers; that is inherent to hosting plugins.
        let lib = unsafe { libloading::Library::new(path) }.map_err(|e| Vst2Error::Load(path.into(), e.to_string()))?;
        // SAFETY: the symbol has the VST entry point signature by convention.
        let main: libloading::Symbol<PluginMain> =
            unsafe { lib.get(b"VSTPluginMain\0").or_else(|_| lib.get(b"main\0")) }
                .map_err(|_| Vst2Error::NoEntry(path.into()))?;
        let effect = main(host_callback);
        // SAFETY: checked for null before reading the magic.
        if effect.is_null() || unsafe { (*effect).magic } != MAGIC {
            return Err(Vst2Error::BadInstance(path.into()));
        }
        let mut host = Box::new(HostData {
            sample_rate,
            block_size,
            time: TimeInfo { sample_rate: sample_rate as f64, ..Default::default() },
            resize: Default::default(),
        });
        // SAFETY: `user` is reserved for the host; the box outlives the effect.
        unsafe { (*effect).user = host.as_mut() as *mut HostData as *mut c_void };
        let event_storage = Box::new(std::array::from_fn(|_| MidiEvent {
            kind: 1,
            byte_size: std::mem::size_of::<MidiEvent>() as i32,
            delta_frames: 0,
            flags: 0,
            note_length: 0,
            note_offset: 0,
            midi_data: [0; 4],
            detune: 0,
            note_off_velocity: 0,
            reserved1: 0,
            reserved2: 0,
        }));
        let events = Box::new(Events { num_events: 0, reserved: 0, events: [std::ptr::null_mut(); MAX_EVENTS] });
        let mut plugin = Self { effect, host, events, event_storage, path: path.into(), _lib: lib };
        plugin.dispatch(EFF_OPEN, 0, 0, std::ptr::null_mut(), 0.0);
        plugin.dispatch(EFF_SET_SAMPLE_RATE, 0, 0, std::ptr::null_mut(), sample_rate);
        plugin.dispatch(EFF_SET_BLOCK_SIZE, 0, block_size as isize, std::ptr::null_mut(), 0.0);
        Ok(plugin)
    }

    fn dispatch(&mut self, opcode: i32, index: i32, value: isize, ptr: *mut c_void, opt: f32) -> isize {
        // SAFETY: `effect` is a live instance for the lifetime of self.
        unsafe { ((*self.effect).dispatcher)(self.effect, opcode, index, value, ptr, opt) }
    }

    fn info(&self) -> &AEffect {
        // SAFETY: as above.
        unsafe { &*self.effect }
    }

    pub fn unique_id(&self) -> i32 {
        self.info().unique_id
    }

    /// The plugin's editor, when it has one. Its calls bypass any lock around `self`; keep
    /// the plugin alive while the editor is in use.
    pub fn editor(&self) -> Option<Vst2Editor> {
        (self.flags() & FLAG_HAS_EDITOR != 0).then(|| Vst2Editor {
            effect: self.effect,
            host: &*self.host as *const HostData,
            open: false,
        })
    }

    pub fn flags(&self) -> i32 {
        self.info().flags
    }

    pub fn is_synth(&self) -> bool {
        self.flags() & FLAG_IS_SYNTH != 0
    }

    pub fn num_inputs(&self) -> usize {
        self.info().num_inputs.max(0) as usize
    }

    pub fn num_outputs(&self) -> usize {
        self.info().num_outputs.max(0) as usize
    }

    pub fn num_params(&self) -> usize {
        self.info().num_params.max(0) as usize
    }

    pub fn latency(&self) -> usize {
        self.info().initial_delay.max(0) as usize
    }

    fn string(&mut self, opcode: i32, index: i32) -> String {
        let mut buf = [0u8; 256];
        self.dispatch(opcode, index, 0, buf.as_mut_ptr() as *mut c_void, 0.0);
        CStr::from_bytes_until_nul(&buf).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    }

    pub fn name(&mut self) -> String {
        self.string(EFF_GET_EFFECT_NAME, 0)
    }

    pub fn vendor(&mut self) -> String {
        self.string(EFF_GET_VENDOR_STRING, 0)
    }

    pub fn param_name(&mut self, index: usize) -> String {
        self.string(EFF_GET_PARAM_NAME, index as i32)
    }

    pub fn param_display(&mut self, index: usize) -> String {
        self.string(EFF_GET_PARAM_DISPLAY, index as i32)
    }

    pub fn param(&self, index: usize) -> f32 {
        // SAFETY: plugin call on a live instance.
        unsafe { ((*self.effect).get_parameter)(self.effect, index as i32) }
    }

    pub fn set_param(&mut self, index: usize, value: f32) {
        // SAFETY: plugin call on a live instance.
        unsafe { ((*self.effect).set_parameter)(self.effect, index as i32, value) }
    }

    /// Restores the plugin's own state blob (`effSetChunk`, bank chunk).
    pub fn set_chunk(&mut self, data: &[u8]) {
        let mut copy = data.to_vec();
        self.dispatch(EFF_SET_CHUNK, 0, copy.len() as isize, copy.as_mut_ptr() as *mut c_void, 0.0);
    }

    pub fn chunk(&mut self) -> Vec<u8> {
        let mut ptr: *mut c_void = std::ptr::null_mut();
        let len = self.dispatch(EFF_GET_CHUNK, 0, 0, &mut ptr as *mut *mut c_void as *mut c_void, 0.0);
        if ptr.is_null() || len <= 0 {
            return Vec::new();
        }
        // SAFETY: the plugin returned a buffer of `len` bytes that stays valid until the next call.
        unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) }.to_vec()
    }

    pub fn resume(&mut self) {
        self.dispatch(EFF_MAINS_CHANGED, 0, 1, std::ptr::null_mut(), 0.0);
        self.dispatch(EFF_START_PROCESS, 0, 0, std::ptr::null_mut(), 0.0);
    }

    pub fn suspend(&mut self) {
        self.dispatch(EFF_STOP_PROCESS, 0, 0, std::ptr::null_mut(), 0.0);
        self.dispatch(EFF_MAINS_CHANGED, 0, 0, std::ptr::null_mut(), 0.0);
    }

    pub fn set_time(&mut self, time: TimeInfo) {
        self.host.time = TimeInfo { sample_rate: self.host.sample_rate as f64, ..time };
    }

    /// Queues MIDI for the next `process` call: `(frame offset, bytes)`. Allocation free.
    pub fn send_midi(&mut self, events: impl Iterator<Item = (usize, [u8; 3])>) {
        let mut n = 0;
        for (offset, bytes) in events.take(MAX_EVENTS) {
            let e = &mut self.event_storage[n];
            e.delta_frames = offset as i32;
            e.midi_data = [bytes[0], bytes[1], bytes[2], 0];
            self.events.events[n] = e as *mut MidiEvent;
            n += 1;
        }
        if n == 0 {
            return;
        }
        self.events.num_events = n as i32;
        let ptr = self.events.as_mut() as *mut Events as *mut c_void;
        self.dispatch(EFF_PROCESS_EVENTS, 0, 0, ptr, 0.0);
    }

    /// Runs `processReplacing` on channel buffers. Callers pass as many inputs and outputs as
    /// the plugin declares.
    pub fn process(&mut self, inputs: &[*const f32], outputs: &mut [*mut f32], frames: usize) {
        // SAFETY: the pointer arrays and buffers are valid for `frames` samples per channel.
        unsafe { ((*self.effect).process_replacing)(self.effect, inputs.as_ptr(), outputs.as_mut_ptr(), frames as i32) }
    }
}

impl Drop for Vst2Plugin {
    fn drop(&mut self) {
        self.suspend();
        self.dispatch(EFF_CLOSE, 0, 0, std::ptr::null_mut(), 0.0);
    }
}

/// REAPER's per-plugin state block in an RPP `<VST` chunk: a header with the plugin id and pin
/// mappings, the plugin's own chunk, and the program name.
#[derive(Debug, Clone, PartialEq)]
pub struct ReaperVstState {
    pub unique_id: u32,
    pub chunk: Vec<u8>,
    pub program: Vec<u8>,
    /// Track channels each input and output pin is connected to, as bitmasks (channel 1 is
    /// bit 0).
    pub inputs: Vec<u64>,
    pub outputs: Vec<u64>,
}

impl ReaperVstState {
    /// The lines of `original` with the plugin chunk replaced by `chunk`: the header keeps
    /// everything but the size, the program line stays as it was.
    pub fn reencode(original: &[String], chunk: &[u8]) -> Option<Vec<String>> {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        let mut header = b64.decode(original.first()?.trim()).ok()?;
        let word = |h: &[u8], at: usize| h.get(at..at + 4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes);
        let n_in = word(&header, 8)? as usize;
        let after_in = 12 + n_in * 8;
        let n_out = word(&header, after_in)? as usize;
        let size_at = after_in + 4 + n_out * 8;
        header.get_mut(size_at..size_at + 4)?.copy_from_slice(&(chunk.len() as u32).to_le_bytes());
        let mut lines = vec![b64.encode(&header)];
        lines.extend(b64.encode(chunk).as_bytes().chunks(128).map(|c| String::from_utf8_lossy(c).into_owned()));
        lines.push(original.last()?.trim().to_owned());
        Some(lines)
    }

    /// Decodes the base64 lines of a `<VST` chunk. The header block is the first line; the
    /// plugin data runs until a line that ends a base64 group; the last line is the program.
    pub fn decode(lines: &[String]) -> Option<Self> {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        let header = b64.decode(lines.first()?.trim()).ok()?;
        // 0xfeed5eef marks a plugin with extra (sidechain) pins; the layout is the same.
        if header.len() < 8 || !matches!(u32::from_le_bytes(header[4..8].try_into().ok()?), 0xfeed_5eee | 0xfeed_5eef) {
            return None;
        }
        let unique_id = u32::from_le_bytes(header[0..4].try_into().ok()?);
        let word = |at: usize| header.get(at..at + 4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes);
        let mask = |at: usize| header.get(at..at + 8).and_then(|b| b.try_into().ok()).map(u64::from_le_bytes);
        let n_in = word(8)? as usize;
        let after_in = 12 + n_in * 8;
        let n_out = word(after_in)? as usize;
        let size_at = after_in + 4 + n_out * 8;
        let size = word(size_at)? as usize;
        let inputs = (0..n_in).filter_map(|i| mask(12 + i * 8)).collect();
        let outputs = (0..n_out).filter_map(|i| mask(after_in + 4 + i * 8)).collect();

        let rest = &lines[1..];
        let (data_lines, program_line) = match rest.split_last() {
            Some((last, body)) => (body, Some(last)),
            None => (rest, None),
        };
        let joined: String = data_lines.iter().map(|l| l.trim()).collect();
        let mut chunk = b64.decode(joined).ok()?;
        chunk.truncate(size);
        let program = program_line.and_then(|l| b64.decode(l.trim()).ok()).unwrap_or_default();
        Some(Self { unique_id, chunk, program, inputs, outputs })
    }
}

#[repr(C)]
struct ERect {
    top: i16,
    left: i16,
    bottom: i16,
    right: i16,
}

/// A VST 2 plugin's editor in an X11 window, idled from the UI thread.
pub struct Vst2Editor {
    effect: *mut AEffect,
    host: *const HostData,
    open: bool,
}

impl Vst2Editor {
    fn dispatch(&self, opcode: i32, value: isize, ptr: *mut c_void) -> isize {
        // SAFETY: the owner keeps the effect alive while its editor exists.
        unsafe { ((*self.effect).dispatcher)(self.effect, opcode, 0, value, ptr, 0.0) }
    }

    fn rect(&self) -> Option<(u32, u32)> {
        let mut rect: *mut ERect = std::ptr::null_mut();
        self.dispatch(EFF_EDIT_GET_RECT, 0, &mut rect as *mut *mut ERect as *mut c_void);
        // SAFETY: the plugin returns null or a rectangle it owns.
        let r = unsafe { rect.as_ref() }?;
        let (w, h) = ((r.right - r.left) as i32, (r.bottom - r.top) as i32);
        (w > 0 && h > 0).then_some((w as u32, h as u32))
    }
}

impl crate::editor::Editor for Vst2Editor {
    fn open(&mut self, parent: u32) -> Result<(u32, u32), String> {
        let before = self.rect();
        if self.dispatch(EFF_EDIT_OPEN, 0, parent as usize as *mut c_void) == 0 && before.is_none() {
            return Err("the editor could not be opened".into());
        }
        self.open = true;
        Ok(self.rect().or(before).unwrap_or((640, 480)))
    }

    fn idle(&mut self) -> Option<(u32, u32)> {
        if self.open {
            self.dispatch(EFF_EDIT_IDLE, 0, std::ptr::null_mut());
        }
        // SAFETY: the host data lives as long as the effect.
        let packed = unsafe { (*self.host).resize.swap(0, std::sync::atomic::Ordering::Relaxed) };
        (packed != 0).then_some(((packed >> 32) as u32, packed as u32))
    }

    fn close(&mut self) {
        if std::mem::take(&mut self.open) {
            self.dispatch(EFF_EDIT_CLOSE, 0, std::ptr::null_mut());
        }
    }
}

impl Drop for Vst2Editor {
    fn drop(&mut self) {
        crate::editor::Editor::close(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_layout_matches_the_abi() {
        assert_eq!(std::mem::size_of::<MidiEvent>(), 32);
        assert_eq!(std::mem::offset_of!(AEffect, num_programs), 40);
        assert_eq!(std::mem::offset_of!(AEffect, user), 104);
        assert_eq!(std::mem::offset_of!(AEffect, unique_id), 112);
        assert_eq!(std::mem::offset_of!(AEffect, process_replacing), 120);
        assert_eq!(std::mem::offset_of!(TimeInfo, time_sig_numerator), 64);
    }

    #[test]
    fn decodes_reaper_state() {
        let lines: Vec<String> = [
            "cWVlcu5e7f4CAAAAAQAAAAAAAAACAAAAAAAAAAIAAAABAAAAAAAAAAIAAAAAAAAAzQAAAAEAAAAAABAA",
            "IQAAAAUAAAAEAAAAAQAAAAAAAAAAAFlAAAAAAAAA8D+amZmZmZnpPwEIAAAAAQAAAAAAAAAAwHJAAAAAAAAA8D8AAAAAAAAAQAEIAAAAAQAAAAAAAAAAQI9AAAAAAAAA",
            "8D8AAAAAAAAAQAEDAAAAAQAAAAAAAAAAiLNAAAAAAAAA8D+amZmZmZnpPwEEAAAAAAAAAAAAAAAAAFlAAAAAAAAA8D8AAAAAAAAAQAEBAAAAAQAAAAAAAAAAAPA/AAAA",
            "AIQCAACQBQAAAgAAAA==",
            "AFByb2dyYW0gMQAQAAAA",
        ]
        .map(String::from)
        .to_vec();
        let s = ReaperVstState::decode(&lines).unwrap();
        assert_eq!(s.unique_id, 1919247729);
        assert_eq!(s.chunk.len(), 205);
        assert_eq!(&s.program[1..10], b"Program 1");
    }
}
