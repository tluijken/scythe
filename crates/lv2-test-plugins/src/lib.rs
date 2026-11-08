//! Minimal LV2 plugins used to test scythe's LV2 host. `urn:scythe:test-amp` scales stereo
//! audio by its `gain` control and keeps a text label in its state, reporting the label's
//! length on an output control. `urn:scythe:test-synth` writes the held MIDI key / 128 as a
//! constant level, so tests can read note timing exactly. The amp has an X11 UI that sets its
//! gain to 0.8 when it opens. The Turtle files live in the tests.

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

// ---- amp UI --------------------------------------------------------------------------------

#[repr(C)]
pub struct UiDescriptor {
    uri: *const c_char,
    instantiate: unsafe extern "C" fn(
        *const UiDescriptor,
        *const c_char,
        *const c_char,
        WriteFn,
        *mut c_void,
        *mut *mut c_void,
        *const *const Feature,
    ) -> *mut c_void,
    cleanup: unsafe extern "C" fn(*mut c_void),
    port_event: unsafe extern "C" fn(*mut c_void, u32, u32, u32, *const c_void),
    extension_data: unsafe extern "C" fn(*const c_char) -> *const c_void,
}

// SAFETY: the descriptor only holds pointers to static strings and functions.
unsafe impl Sync for UiDescriptor {}

type WriteFn = unsafe extern "C" fn(*mut c_void, u32, u32, u32, *const c_void);

#[repr(C)]
struct UiResize {
    handle: *mut c_void,
    ui_resize: unsafe extern "C" fn(*mut c_void, i32, i32) -> i32,
}

/// The UI: a coloured child window. On start it sets the gain to 0.8 through the host.
struct AmpUi {
    conn: x11rb::rust_connection::RustConnection,
    window: u32,
}

unsafe fn feature(features: *const *const Feature, uri: &[u8]) -> Option<*mut c_void> {
    let mut f = features;
    // SAFETY: the host passes a NUL-terminated array of valid features.
    unsafe {
        while !(*f).is_null() {
            if CStr::from_ptr((**f).uri).to_bytes() == uri {
                return Some((**f).data);
            }
            f = f.add(1);
        }
    }
    None
}

unsafe extern "C" fn ui_instantiate(
    _d: *const UiDescriptor,
    _plugin: *const c_char,
    _bundle: *const c_char,
    write: WriteFn,
    controller: *mut c_void,
    widget: *mut *mut c_void,
    features: *const *const Feature,
) -> *mut c_void {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{ConnectionExt as _, CreateWindowAux, WindowClass};
    // SAFETY: features come from the host.
    let Some(parent) = (unsafe { feature(features, b"http://lv2plug.in/ns/extensions/ui#parent") }) else {
        return std::ptr::null_mut();
    };
    let Ok((conn, _)) = x11rb::connect(None) else { return std::ptr::null_mut() };
    let Ok(window) = conn.generate_id() else { return std::ptr::null_mut() };
    let aux = CreateWindowAux::new().background_pixel(0xe76f51);
    let made = conn
        .create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            parent as usize as u32,
            0,
            0,
            320,
            200,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &aux,
        )
        .is_ok();
    if !made || conn.map_window(window).is_err() || conn.flush().is_err() {
        return std::ptr::null_mut();
    }
    // SAFETY: the host's resize feature, widget pointer and write function are valid here.
    unsafe {
        if let Some(r) = feature(features, b"http://lv2plug.in/ns/extensions/ui#resize") {
            let r = &*(r as *const UiResize);
            (r.ui_resize)(r.handle, 320, 200);
        }
        *widget = window as usize as *mut c_void;
        let gain = 0.8f32;
        write(controller, 4, 4, 0, &gain as *const f32 as *const c_void);
    }
    Box::into_raw(Box::new(AmpUi { conn, window })) as *mut c_void
}

unsafe extern "C" fn ui_cleanup(handle: *mut c_void) {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::ConnectionExt as _;
    // SAFETY: undoes the Box::into_raw in instantiate.
    let ui = unsafe { Box::from_raw(handle as *mut AmpUi) };
    let _ = ui.conn.destroy_window(ui.window);
    let _ = ui.conn.flush();
}

unsafe extern "C" fn ui_port_event(_h: *mut c_void, _port: u32, _size: u32, _format: u32, _buffer: *const c_void) {}

unsafe extern "C" fn ui_idle(_h: *mut c_void) -> i32 {
    0
}

#[repr(C)]
struct UiIdle {
    idle: unsafe extern "C" fn(*mut c_void) -> i32,
}

static UI_IDLE: UiIdle = UiIdle { idle: ui_idle };

unsafe extern "C" fn ui_extension(uri: *const c_char) -> *const c_void {
    // SAFETY: uri is a C string.
    if unsafe { CStr::from_ptr(uri) }.to_bytes() == b"http://lv2plug.in/ns/extensions/ui#idleInterface" {
        &UI_IDLE as *const _ as *const c_void
    } else {
        std::ptr::null()
    }
}

static AMP_UI: UiDescriptor = UiDescriptor {
    uri: c"urn:scythe:test-amp#ui".as_ptr(),
    instantiate: ui_instantiate,
    cleanup: ui_cleanup,
    port_event: ui_port_event,
    extension_data: ui_extension,
};

/// # Safety
/// Called by LV2 hosts with any index; returns null past the last UI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lv2ui_descriptor(index: u32) -> *const UiDescriptor {
    if index == 0 { &AMP_UI } else { std::ptr::null() }
}
