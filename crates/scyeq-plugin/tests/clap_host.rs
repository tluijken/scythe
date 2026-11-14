//! Loads the built ScyEQ-Pro CLAP library the way a host does: its parameters, its state and its sound.

use std::ffi::{CStr, CString, c_void};
use std::ptr;
use std::sync::{Mutex, MutexGuard, OnceLock};

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::{clap_event_header, clap_input_events, clap_output_events};
use clap_sys::ext::params::{
    CLAP_EXT_PARAMS, CLAP_PARAM_IS_AUTOMATABLE, CLAP_PARAM_IS_STEPPED, clap_param_info, clap_plugin_params,
};
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_plugin_state};
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::host::clap_host;
use clap_sys::plugin::clap_plugin;
use clap_sys::process::clap_process;
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::CLAP_VERSION;
use scyeq_dsp::params;
use scyeq_plugin::plugin_format::{Kind, Map};

mod built;
mod cases;

fn plugin() -> &'static built::Built {
    static PLUGIN: OnceLock<built::Built> = OnceLock::new();
    PLUGIN.get_or_init(|| built::plugin("scyeq_plugin", "clap"))
}

/// The entry's init and deinit apply to the whole library, so tests take turns.
static SERIAL: Mutex<()> = Mutex::new(());

fn turn() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn library() -> &'static libloading::Library {
    static LIBRARY: OnceLock<libloading::Library> = OnceLock::new();
    LIBRARY.get_or_init(|| unsafe { libloading::Library::new(&plugin().binary).unwrap() })
}

unsafe fn entry() -> &'static clap_plugin_entry {
    unsafe {
        let entry: libloading::Symbol<*const clap_plugin_entry> = library().get(b"clap_entry\0").unwrap();
        &**entry
    }
}

unsafe extern "C" fn nothing(_: *const clap_host) {}
unsafe extern "C" fn no_extension(_: *const clap_host, _: *const std::ffi::c_char) -> *const c_void {
    ptr::null()
}

static HOST: clap_host = clap_host {
    clap_version: CLAP_VERSION,
    host_data: ptr::null_mut(),
    name: c"scyeq-plugin test host".as_ptr(),
    vendor: c"".as_ptr(),
    url: c"".as_ptr(),
    version: c"0".as_ptr(),
    get_extension: Some(no_extension),
    request_restart: Some(nothing),
    request_process: Some(nothing),
    request_callback: Some(nothing),
};

/// An initialised instance; destroyed, with the entry deinitialised, when dropped.
struct Instance(*const clap_plugin);

impl Instance {
    fn new() -> Self {
        unsafe {
            let entry = entry();
            let path = CString::new(plugin().path.to_str().unwrap()).unwrap();
            assert!((entry.init.unwrap())(path.as_ptr()));
            let factory = (entry.get_factory.unwrap())(CLAP_PLUGIN_FACTORY_ID.as_ptr()) as *const clap_plugin_factory;
            assert_eq!(((*factory).get_plugin_count.unwrap())(factory), 1);
            let desc = ((*factory).get_plugin_descriptor.unwrap())(factory, 0);
            assert_eq!(CStr::from_ptr((*desc).id), c"scythe.eq");
            assert_eq!(CStr::from_ptr((*desc).name), c"ScyEQ-Pro");
            assert_eq!(CStr::from_ptr((*desc).vendor), c"Scythe");
            assert_eq!(CStr::from_ptr((*desc).version).to_str().unwrap(), env!("CARGO_PKG_VERSION"));
            let plugin = ((*factory).create_plugin.unwrap())(factory, &HOST, (*desc).id);
            assert!(!plugin.is_null());
            assert!(((*plugin).init.unwrap())(plugin));
            Self(plugin)
        }
    }

    fn extension<T>(&self, id: &CStr) -> &T {
        unsafe {
            let ext = ((*self.0).get_extension.unwrap())(self.0, id.as_ptr()) as *const T;
            assert!(!ext.is_null(), "{id:?} missing");
            &*ext
        }
    }

    fn params(&self) -> &clap_plugin_params {
        self.extension(CLAP_EXT_PARAMS)
    }

    fn value(&self, id: u32) -> f64 {
        let mut value = f64::NAN;
        assert!(unsafe { (self.params().get_value.unwrap())(self.0, id, &mut value) });
        value
    }

    fn save(&self) -> Vec<u8> {
        let state: &clap_plugin_state = self.extension(CLAP_EXT_STATE);
        let mut out = Vec::new();
        let stream = clap_ostream { ctx: &mut out as *mut Vec<u8> as *mut c_void, write: Some(write) };
        assert!(unsafe { (state.save.unwrap())(self.0, &stream) });
        out
    }

    fn load(&self, bytes: &[u8]) -> bool {
        let state: &clap_plugin_state = self.extension(CLAP_EXT_STATE);
        let mut source = (bytes, 0usize);
        let stream = clap_istream { ctx: &mut source as *mut (&[u8], usize) as *mut c_void, read: Some(read) };
        unsafe { (state.load.unwrap())(self.0, &stream) }
    }

    fn activate(&self, rate: f64, block: u32) {
        unsafe {
            assert!(((*self.0).activate.unwrap())(self.0, rate, 1, block));
            assert!(((*self.0).start_processing.unwrap())(self.0));
        }
    }

    fn deactivate(&self) {
        unsafe {
            ((*self.0).stop_processing.unwrap())(self.0);
            ((*self.0).deactivate.unwrap())(self.0);
        }
    }

    /// Runs `input` (both channels the same) through in blocks of `block` frames, without any
    /// parameter events.
    fn run(&self, input: &[f32], block: usize) -> Vec<f32> {
        let mut output = Vec::with_capacity(input.len());
        for chunk in input.chunks(block) {
            let mut ins = [chunk.to_vec(), chunk.to_vec()];
            let mut outs = [vec![0.0f32; chunk.len()], vec![0.0f32; chunk.len()]];
            let mut in_ptrs = [ins[0].as_mut_ptr(), ins[1].as_mut_ptr()];
            let mut out_ptrs = [outs[0].as_mut_ptr(), outs[1].as_mut_ptr()];
            let input = clap_audio_buffer {
                data32: in_ptrs.as_mut_ptr(),
                data64: ptr::null_mut(),
                channel_count: 2,
                latency: 0,
                constant_mask: 0,
            };
            let mut out = clap_audio_buffer {
                data32: out_ptrs.as_mut_ptr(),
                data64: ptr::null_mut(),
                channel_count: 2,
                latency: 0,
                constant_mask: 0,
            };
            let in_events = clap_input_events { ctx: ptr::null_mut(), size: Some(no_events), get: Some(no_event) };
            let out_events = clap_output_events { ctx: ptr::null_mut(), try_push: Some(drop_event) };
            let process = clap_process {
                steady_time: -1,
                frames_count: chunk.len() as u32,
                transport: ptr::null(),
                audio_inputs: &input,
                audio_outputs: &mut out,
                audio_inputs_count: 1,
                audio_outputs_count: 1,
                in_events: &in_events,
                out_events: &out_events,
            };
            unsafe { ((*self.0).process.unwrap())(self.0, &process) };
            assert_eq!(outs[0], outs[1]);
            output.extend_from_slice(&outs[0]);
        }
        output
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe {
            ((*self.0).destroy.unwrap())(self.0);
            (entry().deinit.unwrap())();
        }
    }
}

unsafe extern "C" fn write(stream: *const clap_ostream, buffer: *const c_void, size: u64) -> i64 {
    unsafe {
        let out = &mut *((*stream).ctx as *mut Vec<u8>);
        let n = size.min(5) as usize;
        out.extend_from_slice(std::slice::from_raw_parts(buffer as *const u8, n));
        n as i64
    }
}

unsafe extern "C" fn read(stream: *const clap_istream, buffer: *mut c_void, size: u64) -> i64 {
    unsafe {
        let (bytes, at) = &mut *((*stream).ctx as *mut (&[u8], usize));
        let n = (bytes.len() - *at).min(size as usize).min(7);
        ptr::copy_nonoverlapping(bytes[*at..].as_ptr(), buffer as *mut u8, n);
        *at += n;
        n as i64
    }
}

unsafe extern "C" fn no_events(_: *const clap_input_events) -> u32 {
    0
}
unsafe extern "C" fn no_event(_: *const clap_input_events, _: u32) -> *const clap_event_header {
    ptr::null()
}
unsafe extern "C" fn drop_event(_: *const clap_output_events, _: *const clap_event_header) -> bool {
    true
}

#[test]
fn reports_every_parameter() {
    let _turn = turn();
    let plugin = Instance::new();
    let p = plugin.params();
    let ids: Vec<u32> = params::ids().collect();
    assert_eq!(unsafe { (p.count.unwrap())(plugin.0) } as usize, ids.len());
    let automatable: Vec<u32> = params::automatable().collect();
    for (index, &id) in ids.iter().enumerate() {
        let mut info: clap_param_info = unsafe { std::mem::zeroed() };
        assert!(unsafe { (p.get_info.unwrap())(plugin.0, index as u32, &mut info) });
        let spec = params::spec_of(id).unwrap();
        assert_eq!(info.id, id);
        assert_eq!(unsafe { CStr::from_ptr(info.name.as_ptr()) }.to_str().unwrap(), params::name(id));
        assert_eq!(
            (info.min_value, info.max_value, info.default_value),
            (spec.min, spec.max, params::default(id)),
            "{id}"
        );
        assert_eq!(info.flags & CLAP_PARAM_IS_AUTOMATABLE != 0, automatable.contains(&id), "{id}");
        assert_eq!(plugin.value(id), params::default(id), "a new instance has the defaults");
        let listed = scyeq_plugin::params::infos()[index];
        assert_eq!(info.flags & CLAP_PARAM_IS_STEPPED != 0, !matches!(listed.kind, Kind::Float { .. }), "{id}");
    }
    for id in cases::frequencies() {
        let listed = scyeq_plugin::params::infos().iter().find(|p| p.id == id).unwrap();
        assert!(matches!(listed.kind, Kind::Float { map: Map::Log, .. }), "{id}");
        let mut info: clap_param_info = unsafe { std::mem::zeroed() };
        let index = ids.iter().position(|&i| i == id).unwrap();
        assert!(unsafe { (p.get_info.unwrap())(plugin.0, index as u32, &mut info) });
        assert_eq!(info.flags & CLAP_PARAM_IS_STEPPED, 0);
    }
    let mut text = [0 as std::ffi::c_char; 64];
    let freq = params::band_param(3, params::band::FREQ);
    assert!(unsafe { (p.value_to_text.unwrap())(plugin.0, freq, 2500.0, text.as_mut_ptr(), 64) });
    assert_eq!(unsafe { CStr::from_ptr(text.as_ptr()) }, c"2.50 kHz");
    let mut value = 0.0;
    assert!(unsafe { (p.text_to_value.unwrap())(plugin.0, freq, c"2.50 kHz".as_ptr(), &mut value) });
    assert_eq!(value, 2500.0);
}

#[test]
fn loads_scythe_state() {
    let _turn = turn();
    let plugin = Instance::new();
    assert!(plugin.load(cases::SONG2));
    assert_eq!(plugin.save(), cases::SONG2);
    let stored = params::decode(cases::SONG2).unwrap().values;
    for id in params::ids() {
        assert_eq!(plugin.value(id), stored[id as usize], "{}", params::name(id));
    }
    assert!(!plugin.load(b"not a state"));
    assert_eq!(plugin.save(), cases::SONG2, "a refused state changes nothing");
}

#[test]
fn loads_old_titled_slot() {
    let _turn = turn();
    let state = cases::old_titled_slot_state();
    let plugin = Instance::new();
    assert!(plugin.load(&state));
    assert_eq!(plugin.save(), state);
    let stored = params::decode(&state).unwrap().values;
    let used = params::band_param(0, params::band::USED);
    assert_eq!(plugin.value(used), stored[used as usize]);
    assert!(cases::frequencies().all(|id| plugin.value(id) == stored[id as usize]));
}

#[test]
fn sample_rate_change_rebuilds() {
    let _turn = turn();
    let plugin = Instance::new();
    assert!(plugin.load(&cases::peak_at_1k()));
    plugin.activate(44_100.0, 512);
    plugin.run(&cases::sine(44_100.0, 1000.0, 4096), 512);
    plugin.deactivate();
    plugin.activate(96_000.0, 512);
    let input = cases::sine(96_000.0, 1000.0, 96_000);
    let output = plugin.run(&input, 512);
    let tail = input.len() - 9600;
    let gain = cases::gain_db(&input[tail..], &output[tail..]);
    assert!((gain - 6.0).abs() < 0.01, "{gain} dB at 1 kHz");
    plugin.deactivate();
}

#[test]
fn process_before_flush() {
    let _turn = turn();
    let plugin = Instance::new();
    plugin.activate(48_000.0, 256);
    let input = cases::noise(4096, 3);
    assert!(plugin.run(&input, 256) == input);
    plugin.deactivate();
}
