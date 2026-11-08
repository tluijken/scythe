//! Minimal LV2 plugins used to test scythe's LV2 host. `urn:scythe:test-amp` scales stereo
//! audio by its `gain` control and keeps a text label in its state, reporting the label's
//! length on an output control. `urn:scythe:test-synth` writes the held MIDI key / 128 as a
//! constant level, so tests can read note timing exactly. The Turtle files live in the tests.

use std::ffi::{CStr, c_char, c_void};

#[repr(C)]
struct Feature {
    uri: *const c_char,
    data: *mut c_void,
}

#[repr(C)]
struct UridMap {
    handle: *mut c_void,
    map: unsafe extern "C" fn(*mut c_void, *const c_char) -> u32,
}

#[repr(C)]
pub struct Descriptor {
    uri: *const c_char,
    instantiate: unsafe extern "C" fn(*const Descriptor, f64, *const c_char, *const *const Feature) -> *mut c_void,
    connect_port: unsafe extern "C" fn(*mut c_void, u32, *mut c_void),
    activate: Option<unsafe extern "C" fn(*mut c_void)>,
    run: unsafe extern "C" fn(*mut c_void, u32),
    deactivate: Option<unsafe extern "C" fn(*mut c_void)>,
    cleanup: unsafe extern "C" fn(*mut c_void),
    extension_data: unsafe extern "C" fn(*const c_char) -> *const c_void,
}

// SAFETY: the descriptors only hold pointers to static strings and functions.
unsafe impl Sync for Descriptor {}

type StoreFn = unsafe extern "C" fn(*mut c_void, u32, *const c_void, usize, u32, u32) -> u32;
type RetrieveFn = unsafe extern "C" fn(*mut c_void, u32, *mut usize, *mut u32, *mut u32) -> *const c_void;

#[repr(C)]
struct StateInterface {
    save: unsafe extern "C" fn(*mut c_void, StoreFn, *mut c_void, u32, *const *const Feature) -> u32,
    restore: unsafe extern "C" fn(*mut c_void, RetrieveFn, *mut c_void, u32, *const *const Feature) -> u32,
}

/// Looks up the URID map among the host features.
unsafe fn find_map(features: *const *const Feature) -> Option<&'static UridMap> {
    let mut f = features;
    // SAFETY: the host passes a NUL-terminated array of valid features.
    unsafe {
        while !(*f).is_null() {
            if CStr::from_ptr((**f).uri).to_bytes() == b"http://lv2plug.in/ns/ext/urid#map" {
                return ((**f).data as *const UridMap).as_ref();
            }
            f = f.add(1);
        }
    }
    None
}

unsafe fn map(m: &UridMap, uri: &CStr) -> u32 {
    // SAFETY: the host's map accepts any NUL-terminated URI.
    unsafe { (m.map)(m.handle, uri.as_ptr()) }
}

// ---- amp -----------------------------------------------------------------------------------

struct Amp {
    ports: [*mut f32; 6],
    label: Vec<u8>,
    label_key: u32,
    string_type: u32,
}

unsafe extern "C" fn amp_instantiate(
    _d: *const Descriptor,
    _rate: f64,
    _bundle: *const c_char,
    features: *const *const Feature,
) -> *mut c_void {
    // SAFETY: features come from the host.
    let Some(m) = (unsafe { find_map(features) }) else { return std::ptr::null_mut() };
    let amp = Amp {
        ports: [std::ptr::null_mut(); 6],
        label: Vec::new(),
        // SAFETY: valid map and URIs.
        label_key: unsafe { map(m, c"urn:scythe:test-amp#label") },
        string_type: unsafe { map(m, c"http://lv2plug.in/ns/ext/atom#String") },
    };
    Box::into_raw(Box::new(amp)) as *mut c_void
}

unsafe extern "C" fn amp_connect(h: *mut c_void, port: u32, data: *mut c_void) {
    // SAFETY: h is the Amp; ports 0..6 exist.
    unsafe {
        if let Some(p) = (*(h as *mut Amp)).ports.get_mut(port as usize) {
            *p = data as *mut f32;
        }
    }
}

unsafe extern "C" fn amp_run(h: *mut c_void, n: u32) {
    // SAFETY: every port is connected to a buffer of at least `n` frames.
    unsafe {
        let amp = &*(h as *const Amp);
        let gain = *amp.ports[4];
        for c in 0..2 {
            for i in 0..n as usize {
                *amp.ports[2 + c].add(i) = *amp.ports[c].add(i) * gain;
            }
        }
        *amp.ports[5] = amp.label.len() as f32;
    }
}

unsafe extern "C" fn amp_cleanup(h: *mut c_void) {
    // SAFETY: undoes the Box::into_raw in instantiate.
    drop(unsafe { Box::from_raw(h as *mut Amp) });
}

unsafe extern "C" fn amp_save(
    h: *mut c_void,
    store: StoreFn,
    handle: *mut c_void,
    _flags: u32,
    _f: *const *const Feature,
) -> u32 {
    // SAFETY: h is the Amp; the label is stored with its NUL as LV2 strings are.
    unsafe {
        let amp = &*(h as *const Amp);
        let mut value = amp.label.clone();
        value.push(0);
        store(handle, amp.label_key, value.as_ptr() as *const c_void, value.len(), amp.string_type, 3)
    }
}

unsafe extern "C" fn amp_restore(
    h: *mut c_void,
    retrieve: RetrieveFn,
    handle: *mut c_void,
    _flags: u32,
    _f: *const *const Feature,
) -> u32 {
    // SAFETY: h is the Amp; retrieve returns null or `size` bytes.
    unsafe {
        let amp = &mut *(h as *mut Amp);
        let (mut size, mut kind, mut flags) = (0usize, 0u32, 0u32);
        let value = retrieve(handle, amp.label_key, &mut size, &mut kind, &mut flags);
        if value.is_null() || kind != amp.string_type {
            return 1;
        }
        let bytes = std::slice::from_raw_parts(value as *const u8, size);
        amp.label = bytes.strip_suffix(&[0]).unwrap_or(bytes).to_vec();
    }
    0
}

static STATE: StateInterface = StateInterface { save: amp_save, restore: amp_restore };

unsafe extern "C" fn amp_extension(uri: *const c_char) -> *const c_void {
    // SAFETY: uri is a C string.
    if unsafe { CStr::from_ptr(uri) }.to_bytes() == b"http://lv2plug.in/ns/ext/state#interface" {
        &STATE as *const _ as *const c_void
    } else {
        std::ptr::null()
    }
}

static AMP: Descriptor = Descriptor {
    uri: c"urn:scythe:test-amp".as_ptr(),
    instantiate: amp_instantiate,
    connect_port: amp_connect,
    activate: None,
    run: amp_run,
    deactivate: None,
    cleanup: amp_cleanup,
    extension_data: amp_extension,
};

// ---- synth ---------------------------------------------------------------------------------

struct Synth {
    ports: [*mut c_void; 3],
    key: Option<u8>,
    midi_type: u32,
}

unsafe extern "C" fn synth_instantiate(
    _d: *const Descriptor,
    _rate: f64,
    _bundle: *const c_char,
    features: *const *const Feature,
) -> *mut c_void {
    // SAFETY: features come from the host.
    let Some(m) = (unsafe { find_map(features) }) else { return std::ptr::null_mut() };
    // SAFETY: valid map and URI.
    let midi_type = unsafe { map(m, c"http://lv2plug.in/ns/ext/midi#MidiEvent") };
    Box::into_raw(Box::new(Synth { ports: [std::ptr::null_mut(); 3], key: None, midi_type })) as *mut c_void
}

unsafe extern "C" fn synth_connect(h: *mut c_void, port: u32, data: *mut c_void) {
    // SAFETY: h is the Synth; ports 0..3 exist.
    unsafe {
        if let Some(p) = (*(h as *mut Synth)).ports.get_mut(port as usize) {
            *p = data;
        }
    }
}

unsafe extern "C" fn synth_run(h: *mut c_void, n: u32) {
    // SAFETY: port 0 is an atom sequence, ports 1 and 2 hold `n` frames.
    unsafe {
        let synth = &mut *(h as *mut Synth);
        let seq = synth.ports[0] as *const u8;
        let size = *(seq as *const u32) as usize;
        let (out_l, out_r) = (synth.ports[1] as *mut f32, synth.ports[2] as *mut f32);
        let mut at = 16usize;
        let mut events = Vec::new();
        while at + 16 <= size + 8 {
            let frame = *(seq.add(at) as *const i64) as usize;
            let len = *(seq.add(at + 8) as *const u32) as usize;
            let kind = *(seq.add(at + 12) as *const u32);
            if kind == synth.midi_type && len >= 3 {
                let d = std::slice::from_raw_parts(seq.add(at + 16), 3);
                events.push((frame, [d[0], d[1], d[2]]));
            }
            at += 16 + len.div_ceil(8) * 8;
        }
        let mut next = 0;
        for i in 0..n as usize {
            while next < events.len() && events[next].0 <= i {
                let [status, key, vel] = events[next].1;
                match status & 0xf0 {
                    0x90 if vel > 0 => synth.key = Some(key),
                    0x80 | 0x90 if synth.key == Some(key) => synth.key = None,
                    _ => {}
                }
                next += 1;
            }
            let v = synth.key.map_or(0.0, |k| k as f32 / 128.0);
            *out_l.add(i) = v;
            *out_r.add(i) = v;
        }
    }
}

unsafe extern "C" fn synth_cleanup(h: *mut c_void) {
    // SAFETY: undoes the Box::into_raw in instantiate.
    drop(unsafe { Box::from_raw(h as *mut Synth) });
}

unsafe extern "C" fn no_extension(_uri: *const c_char) -> *const c_void {
    std::ptr::null()
}

static SYNTH: Descriptor = Descriptor {
    uri: c"urn:scythe:test-synth".as_ptr(),
    instantiate: synth_instantiate,
    connect_port: synth_connect,
    activate: None,
    run: synth_run,
    deactivate: None,
    cleanup: synth_cleanup,
    extension_data: no_extension,
};

/// # Safety
/// Called by LV2 hosts with any index; returns null past the last plugin.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lv2_descriptor(index: u32) -> *const Descriptor {
    match index {
        0 => &AMP,
        1 => &SYNTH,
        _ => std::ptr::null(),
    }
}
