//! A minimal VST 2.4 instrument used to test scythe's VST 2 host: it writes the held key / 128
//! times its level as a constant on both outputs, following note-on and note-off events. The
//! level is its one parameter and its chunk (a little-endian `f32`); its programs "Full",
//! "Half" and "Quiet" set it to 1, 0.5 and 0.25.

use std::ffi::{c_char, c_void};

type HostCallback = extern "C" fn(*mut AEffect, i32, i32, isize, *mut c_void, f32) -> isize;
type Dispatcher = extern "C" fn(*mut AEffect, i32, i32, isize, *mut c_void, f32) -> isize;
type ProcessReplacing = extern "C" fn(*mut AEffect, *const *const f32, *mut *mut f32, i32);
type SetParameter = extern "C" fn(*mut AEffect, i32, f32);
type GetParameter = extern "C" fn(*mut AEffect, i32) -> f32;

const EFF_SET_PROGRAM: i32 = 2;
const EFF_GET_PROGRAM: i32 = 3;
const EFF_SET_PROGRAM_NAME: i32 = 4;
const EFF_GET_PROGRAM_NAME: i32 = 5;
const EFF_GET_PARAM_DISPLAY: i32 = 7;
const EFF_GET_PARAM_NAME: i32 = 8;
const EFF_GET_CHUNK: i32 = 23;
const EFF_SET_CHUNK: i32 = 24;
const EFF_PROCESS_EVENTS: i32 = 25;
const EFF_GET_PROGRAM_NAME_INDEXED: i32 = 29;
const FLAG_CAN_REPLACING: i32 = 1 << 4;
const FLAG_PROGRAM_CHUNKS: i32 = 1 << 5;
const FLAG_IS_SYNTH: i32 = 1 << 8;

const PROGRAMS: [(&str, f32); 3] = [("Full", 1.0), ("Half", 0.5), ("Quiet", 0.25)];

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

#[repr(C)]
struct Events {
    num_events: i32,
    reserved: isize,
    events: [*mut MidiEvent; 2],
}

/// Plugin state behind `AEffect::object`: pending events of the block, the held key, the
/// level, the current program and the chunk last handed to the host.
struct Synth {
    pending: Vec<(i32, [u8; 3])>,
    key: Option<u8>,
    level: f32,
    program: usize,
    names: [String; 3],
    chunk: [u8; 4],
}

/// Copies `s` into a host string buffer, NUL-terminated within `max` bytes.
fn put_string(s: &str, ptr: *mut c_void, max: usize) {
    if ptr.is_null() {
        return;
    }
    let n = s.len().min(max - 1);
    // SAFETY: hosts pass buffers of at least `max` bytes for these strings.
    unsafe {
        std::ptr::copy_nonoverlapping(s.as_ptr(), ptr as *mut u8, n);
        *(ptr as *mut u8).add(n) = 0;
    }
}

fn synth(effect: *mut AEffect) -> &'static mut Synth {
    // SAFETY: `object` is the boxed Synth created in `VSTPluginMain`, alive until close.
    unsafe { &mut *((*effect).object as *mut Synth) }
}

extern "C" fn dispatcher(
    effect: *mut AEffect,
    opcode: i32,
    index: i32,
    value: isize,
    ptr: *mut c_void,
    _opt: f32,
) -> isize {
    let s = synth(effect);
    match opcode {
        EFF_SET_PROGRAM => {
            if let Some(&(_, level)) = PROGRAMS.get(value as usize) {
                s.program = value as usize;
                s.level = level;
            }
            return 0;
        }
        EFF_GET_PROGRAM => return s.program as isize,
        EFF_SET_PROGRAM_NAME if !ptr.is_null() => {
            // SAFETY: the host passes a NUL-terminated name.
            let name = unsafe { std::ffi::CStr::from_ptr(ptr as *const c_char) };
            s.names[s.program] = name.to_string_lossy().into_owned();
            return 0;
        }
        EFF_GET_PROGRAM_NAME => {
            put_string(&s.names[s.program], ptr, 24);
            return 1;
        }
        EFF_GET_PROGRAM_NAME_INDEXED => {
            let Some(name) = s.names.get(index as usize) else { return 0 };
            put_string(name, ptr, 24);
            return 1;
        }
        EFF_GET_PARAM_NAME => {
            put_string("Level", ptr, 8);
            return 0;
        }
        EFF_GET_PARAM_DISPLAY => {
            put_string(&format!("{:.2}", s.level), ptr, 8);
            return 0;
        }
        EFF_GET_CHUNK if !ptr.is_null() => {
            s.chunk = s.level.to_le_bytes();
            // SAFETY: the host passes a pointer to receive the chunk's address.
            unsafe { *(ptr as *mut *mut u8) = s.chunk.as_mut_ptr() };
            return 4;
        }
        EFF_SET_CHUNK if !ptr.is_null() && value >= 4 => {
            // SAFETY: the host passes `value` bytes.
            let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, 4) };
            s.level = f32::from_le_bytes(bytes.try_into().unwrap());
            return 1;
        }
        _ => {}
    }
    if opcode == EFF_PROCESS_EVENTS && !ptr.is_null() {
        // SAFETY: the host passes a VstEvents block with `num_events` valid pointers.
        unsafe {
            let events = &*(ptr as *const Events);
            let list = std::slice::from_raw_parts(events.events.as_ptr(), events.num_events as usize);
            let s = synth(effect);
            for &e in list {
                let d = (*e).midi_data;
                s.pending.push(((*e).delta_frames, [d[0], d[1], d[2]]));
            }
        }
    }
    0
}

extern "C" fn process_replacing(effect: *mut AEffect, _inputs: *const *const f32, outputs: *mut *mut f32, frames: i32) {
    let s = synth(effect);
    s.pending.sort_by_key(|e| e.0);
    let mut next = 0;
    // SAFETY: the host passes two output buffers of `frames` samples.
    let outs = unsafe { std::slice::from_raw_parts(outputs, 2) };
    for i in 0..frames {
        while let Some(&(at, msg)) = s.pending.get(next).filter(|e| e.0 <= i) {
            let _ = at;
            match msg[0] & 0xf0 {
                0x90 if msg[2] > 0 => s.key = Some(msg[1]),
                0x80 | 0x90 if s.key == Some(msg[1]) => s.key = None,
                _ => {}
            }
            next += 1;
        }
        let v = s.key.map_or(0.0, |k| k as f32 / 128.0) * s.level;
        // SAFETY: as above.
        unsafe {
            *outs[0].add(i as usize) = v;
            *outs[1].add(i as usize) = v;
        }
    }
    s.pending.clear();
}

extern "C" fn set_parameter(effect: *mut AEffect, index: i32, value: f32) {
    if index == 0 {
        synth(effect).level = value;
    }
}

extern "C" fn get_parameter(effect: *mut AEffect, index: i32) -> f32 {
    if index == 0 { synth(effect).level } else { 0.0 }
}

/// # Safety
/// Called by a VST 2 host with its callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn VSTPluginMain(_host: HostCallback) -> *mut AEffect {
    let object = Box::into_raw(Box::new(Synth {
        pending: Vec::with_capacity(512),
        key: None,
        level: 1.0,
        program: 0,
        names: PROGRAMS.map(|(n, _)| n.to_owned()),
        chunk: [0; 4],
    })) as *mut c_void;
    Box::into_raw(Box::new(AEffect {
        magic: i32::from_be_bytes(*b"VstP"),
        dispatcher,
        process: std::ptr::null(),
        set_parameter,
        get_parameter,
        num_programs: PROGRAMS.len() as i32,
        num_params: 1,
        num_inputs: 0,
        num_outputs: 2,
        flags: FLAG_CAN_REPLACING | FLAG_PROGRAM_CHUNKS | FLAG_IS_SYNTH,
        reserved1: 0,
        reserved2: 0,
        initial_delay: 0,
        real_qualities: 0,
        off_qualities: 0,
        io_ratio: 1.0,
        object,
        user: std::ptr::null_mut(),
        unique_id: i32::from_be_bytes(*b"ScyT"),
        version: 1,
        process_replacing,
        process_double_replacing: std::ptr::null(),
        future: [0; 56],
    }))
}
