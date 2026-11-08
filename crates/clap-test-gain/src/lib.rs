//! Minimal CLAP plugins used to test scythe's CLAP host: an effect that scales stereo audio by
//! a gain that round-trips through the state extension, and an instrument that writes each
//! held key number as a constant level (key / 128) so tests can read note timing exactly.

use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicU32, Ordering};

use clap_sys::entry::clap_plugin_entry;
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_plugin_state};
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::host::clap_host;
use clap_sys::plugin::{clap_plugin, clap_plugin_descriptor};
use clap_sys::process::{CLAP_PROCESS_CONTINUE, clap_process, clap_process_status};
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::CLAP_VERSION;

const ID: &CStr = c"org.scythe.test-gain";
const SYNTH_ID: &CStr = c"org.scythe.test-synth";

struct SyncPtr<T>(T);
// SAFETY: the wrapped tables only hold pointers to static strings.
unsafe impl<T> Sync for SyncPtr<T> {}

static FEATURES: SyncPtr<[*const c_char; 2]> = SyncPtr([c"audio-effect".as_ptr(), std::ptr::null()]);
static SYNTH_FEATURES: SyncPtr<[*const c_char; 2]> = SyncPtr([c"instrument".as_ptr(), std::ptr::null()]);

static SYNTH_DESCRIPTOR: SyncPtr<clap_plugin_descriptor> = SyncPtr(clap_plugin_descriptor {
    clap_version: CLAP_VERSION,
    id: SYNTH_ID.as_ptr(),
    name: c"Test Synth".as_ptr(),
    vendor: c"Scythe".as_ptr(),
    url: c"".as_ptr(),
    manual_url: c"".as_ptr(),
    support_url: c"".as_ptr(),
    version: c"1.0".as_ptr(),
    description: c"Outputs the held key / 128".as_ptr(),
    features: SYNTH_FEATURES.0.as_ptr(),
});

struct Synth {
    plugin: clap_plugin,
    key: std::cell::Cell<Option<i16>>,
}

unsafe extern "C" fn synth_destroy(p: *const clap_plugin) {
    // SAFETY: undoes the Box::into_raw in `create_plugin`.
    drop(unsafe { Box::from_raw((*p).plugin_data as *mut Synth) });
}

unsafe extern "C" fn synth_reset(p: *const clap_plugin) {
    // SAFETY: plugin_data is the Synth.
    unsafe { (*((*p).plugin_data as *const Synth)).key.set(None) };
}

unsafe extern "C" fn synth_process(p: *const clap_plugin, process: *const clap_process) -> clap_process_status {
    use clap_sys::events::{CLAP_EVENT_NOTE_OFF, CLAP_EVENT_NOTE_ON, clap_event_note};
    // SAFETY: the host passes valid buffers and events for the block.
    unsafe {
        let synth = &*((*p).plugin_data as *const Synth);
        let pr = &*process;
        let events = &*pr.in_events;
        let count = (events.size.unwrap())(events);
        let mut next = 0;
        let output = &*pr.audio_outputs;
        for i in 0..pr.frames_count {
            while next < count {
                let e = &*(events.get.unwrap())(events, next);
                if e.time > i {
                    break;
                }
                let note = &*(e as *const _ as *const clap_event_note);
                match e.type_ {
                    CLAP_EVENT_NOTE_ON => synth.key.set(Some(note.key)),
                    CLAP_EVENT_NOTE_OFF if synth.key.get() == Some(note.key) => synth.key.set(None),
                    _ => {}
                }
                next += 1;
            }
            let v = synth.key.get().map_or(0.0, |k| k as f32 / 128.0);
            for c in 0..output.channel_count as usize {
                *(*output.data32.add(c)).add(i as usize) += v;
            }
        }
    }
    CLAP_PROCESS_CONTINUE
}

unsafe extern "C" fn no_extension(_p: *const clap_plugin, _id: *const c_char) -> *const c_void {
    std::ptr::null()
}

static DESCRIPTOR: SyncPtr<clap_plugin_descriptor> = SyncPtr(clap_plugin_descriptor {
    clap_version: CLAP_VERSION,
    id: ID.as_ptr(),
    name: c"Test Gain".as_ptr(),
    vendor: c"Scythe".as_ptr(),
    url: c"".as_ptr(),
    manual_url: c"".as_ptr(),
    support_url: c"".as_ptr(),
    version: c"1.0".as_ptr(),
    description: c"Halves the signal unless state says otherwise".as_ptr(),
    features: FEATURES.0.as_ptr(),
});

struct Gain {
    plugin: clap_plugin,
    gain: AtomicU32,
}

unsafe fn gain_of<'a>(plugin: *const clap_plugin) -> &'a Gain {
    // SAFETY: plugin_data points at the boxed Gain created in `create_plugin`.
    unsafe { &*((*plugin).plugin_data as *const Gain) }
}

unsafe extern "C" fn init(_p: *const clap_plugin) -> bool {
    true
}
unsafe extern "C" fn destroy(p: *const clap_plugin) {
    // SAFETY: undoes the Box::into_raw in `create_plugin`.
    drop(unsafe { Box::from_raw((*p).plugin_data as *mut Gain) });
}
unsafe extern "C" fn activate(_p: *const clap_plugin, _sr: f64, _min: u32, _max: u32) -> bool {
    true
}
unsafe extern "C" fn nop(_p: *const clap_plugin) {}
unsafe extern "C" fn start(_p: *const clap_plugin) -> bool {
    true
}

unsafe extern "C" fn process(p: *const clap_plugin, process: *const clap_process) -> clap_process_status {
    // SAFETY: the host passes valid buffers for `frames_count` frames.
    unsafe {
        let events = &*(*process).in_events;
        for i in 0..(events.size.unwrap())(events) {
            let e = &*(events.get.unwrap())(events, i);
            if e.type_ == clap_sys::events::CLAP_EVENT_PARAM_VALUE {
                let pv = &*(e as *const _ as *const clap_sys::events::clap_event_param_value);
                if pv.param_id == GAIN_PARAM {
                    gain_of(p).gain.store((pv.value as f32).to_bits(), Ordering::Relaxed);
                }
            }
        }
        let g = f32::from_bits(gain_of(p).gain.load(Ordering::Relaxed));
        let pr = &*process;
        let (input, output) = (&*pr.audio_inputs, &*pr.audio_outputs);
        for c in 0..output.channel_count.min(input.channel_count) as usize {
            let src = *input.data32.add(c);
            let dst = *output.data32.add(c);
            for i in 0..pr.frames_count as usize {
                *dst.add(i) = *src.add(i) * g;
            }
        }
    }
    CLAP_PROCESS_CONTINUE
}

unsafe extern "C" fn state_save(p: *const clap_plugin, stream: *const clap_ostream) -> bool {
    // SAFETY: valid plugin and stream for the call.
    unsafe {
        let bytes = gain_of(p).gain.load(Ordering::Relaxed).to_le_bytes();
        let write = (*stream).write.unwrap();
        write(stream, bytes.as_ptr() as *const c_void, 4) == 4
    }
}

unsafe extern "C" fn state_load(p: *const clap_plugin, stream: *const clap_istream) -> bool {
    // SAFETY: valid plugin and stream for the call.
    unsafe {
        let mut bytes = [0u8; 4];
        let read = (*stream).read.unwrap();
        if read(stream, bytes.as_mut_ptr() as *mut c_void, 4) != 4 {
            return false;
        }
        gain_of(p).gain.store(u32::from_le_bytes(bytes), Ordering::Relaxed);
        true
    }
}

static STATE: clap_plugin_state = clap_plugin_state { save: Some(state_save), load: Some(state_load) };

const GAIN_PARAM: u32 = 7;

unsafe extern "C" fn params_count(_p: *const clap_plugin) -> u32 {
    1
}

unsafe extern "C" fn params_info(
    _p: *const clap_plugin,
    index: u32,
    info: *mut clap_sys::ext::params::clap_param_info,
) -> bool {
    if index != 0 {
        return false;
    }
    // SAFETY: the host passes a writable info struct.
    unsafe {
        let info = &mut *info;
        info.id = GAIN_PARAM;
        info.flags = clap_sys::ext::params::CLAP_PARAM_IS_AUTOMATABLE;
        info.min_value = 0.0;
        info.max_value = 1.0;
        info.default_value = 0.5;
        for (d, s) in info.name.iter_mut().zip(c"Gain".to_bytes_with_nul()) {
            *d = *s as c_char;
        }
    }
    true
}

unsafe extern "C" fn params_value(p: *const clap_plugin, id: u32, out: *mut f64) -> bool {
    // SAFETY: valid plugin and out pointer.
    unsafe {
        if id != GAIN_PARAM {
            return false;
        }
        *out = f32::from_bits(gain_of(p).gain.load(Ordering::Relaxed)) as f64;
    }
    true
}

unsafe extern "C" fn params_text(_p: *const clap_plugin, _id: u32, value: f64, out: *mut c_char, cap: u32) -> bool {
    let text = format!("{value:.2}\0");
    // SAFETY: writes at most `cap` bytes into the host buffer.
    unsafe {
        for (i, b) in text.bytes().take(cap as usize).enumerate() {
            *out.add(i) = b as c_char;
        }
    }
    true
}

static PARAMS: clap_sys::ext::params::clap_plugin_params = clap_sys::ext::params::clap_plugin_params {
    count: Some(params_count),
    get_info: Some(params_info),
    get_value: Some(params_value),
    value_to_text: Some(params_text),
    text_to_value: None,
    flush: None,
};

unsafe extern "C" fn get_extension(_p: *const clap_plugin, id: *const c_char) -> *const c_void {
    // SAFETY: id is a C string.
    let id = unsafe { CStr::from_ptr(id) };
    if id == CLAP_EXT_STATE {
        &STATE as *const _ as *const c_void
    } else if id == clap_sys::ext::params::CLAP_EXT_PARAMS {
        &PARAMS as *const _ as *const c_void
    } else {
        std::ptr::null()
    }
}

unsafe extern "C" fn count(_f: *const clap_plugin_factory) -> u32 {
    2
}
unsafe extern "C" fn descriptor(_f: *const clap_plugin_factory, i: u32) -> *const clap_plugin_descriptor {
    match i {
        0 => &DESCRIPTOR.0,
        1 => &SYNTH_DESCRIPTOR.0,
        _ => std::ptr::null(),
    }
}
unsafe extern "C" fn create_plugin(
    _f: *const clap_plugin_factory,
    _host: *const clap_host,
    id: *const c_char,
) -> *const clap_plugin {
    // SAFETY: id is a C string.
    let id = unsafe { CStr::from_ptr(id) };
    if id == SYNTH_ID {
        let boxed = Box::into_raw(Box::new(Synth {
            plugin: clap_plugin {
                desc: &SYNTH_DESCRIPTOR.0,
                plugin_data: std::ptr::null_mut(),
                init: Some(init),
                destroy: Some(synth_destroy),
                activate: Some(activate),
                deactivate: Some(nop),
                start_processing: Some(start),
                stop_processing: Some(nop),
                reset: Some(synth_reset),
                process: Some(synth_process),
                get_extension: Some(no_extension),
                on_main_thread: Some(nop),
            },
            key: std::cell::Cell::new(None),
        }));
        // SAFETY: the box was just leaked and is valid.
        return unsafe {
            (*boxed).plugin.plugin_data = boxed as *mut c_void;
            &(*boxed).plugin
        };
    }
    if id != ID {
        return std::ptr::null();
    }
    let boxed = Box::into_raw(Box::new(Gain {
        plugin: clap_plugin {
            desc: &DESCRIPTOR.0,
            plugin_data: std::ptr::null_mut(),
            init: Some(init),
            destroy: Some(destroy),
            activate: Some(activate),
            deactivate: Some(nop),
            start_processing: Some(start),
            stop_processing: Some(nop),
            reset: Some(nop),
            process: Some(process),
            get_extension: Some(get_extension),
            on_main_thread: Some(nop),
        },
        gain: AtomicU32::new(0.5f32.to_bits()),
    }));
    // SAFETY: the box was just leaked and is valid.
    unsafe {
        (*boxed).plugin.plugin_data = boxed as *mut c_void;
        &(*boxed).plugin
    }
}

static FACTORY: clap_plugin_factory = clap_plugin_factory {
    get_plugin_count: Some(count),
    get_plugin_descriptor: Some(descriptor),
    create_plugin: Some(create_plugin),
};

unsafe extern "C" fn entry_init(_path: *const c_char) -> bool {
    true
}
unsafe extern "C" fn entry_deinit() {}
unsafe extern "C" fn get_factory(id: *const c_char) -> *const c_void {
    // SAFETY: id is a C string.
    if unsafe { CStr::from_ptr(id) } == CLAP_PLUGIN_FACTORY_ID {
        &FACTORY as *const _ as *const c_void
    } else {
        std::ptr::null()
    }
}

#[unsafe(no_mangle)]
#[used]
pub static clap_entry: clap_plugin_entry = clap_plugin_entry {
    clap_version: CLAP_VERSION,
    init: Some(entry_init),
    deinit: Some(entry_deinit),
    get_factory: Some(get_factory),
};
