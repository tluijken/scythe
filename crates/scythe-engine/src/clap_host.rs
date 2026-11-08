//! CLAP plugin hosting: scanning `.clap` bundles, instantiating plugins, saving and restoring
//! their state and processing stereo audio in place.

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::{
    CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_NOTE_OFF, CLAP_EVENT_NOTE_ON, CLAP_EVENT_PARAM_VALUE, clap_event_header,
    clap_event_note, clap_event_param_value, clap_input_events, clap_output_events,
};
use clap_sys::ext::gui::{
    CLAP_EXT_GUI, CLAP_WINDOW_API_X11, clap_host_gui, clap_plugin_gui, clap_window, clap_window_handle, clap_xwnd,
};
use clap_sys::ext::params::{CLAP_EXT_PARAMS, clap_param_info, clap_plugin_params};
use clap_sys::ext::posix_fd_support::{
    CLAP_EXT_POSIX_FD_SUPPORT, CLAP_POSIX_FD_ERROR, CLAP_POSIX_FD_READ, clap_host_posix_fd_support,
    clap_plugin_posix_fd_support, clap_posix_fd_flags,
};
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_plugin_state};
use clap_sys::ext::timer_support::{CLAP_EXT_TIMER_SUPPORT, clap_host_timer_support, clap_plugin_timer_support};
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::host::clap_host;
use clap_sys::id::clap_id;
use clap_sys::plugin::{clap_plugin, clap_plugin_descriptor};
use clap_sys::process::clap_process;
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::{CLAP_VERSION, clap_version_is_compatible};

use crate::editor::{Editor, Timers, ready_fds};

#[derive(Debug, thiserror::Error)]
pub enum ClapError {
    #[error("{0}: {1}")]
    Load(PathBuf, String),
    #[error("{0}: not a CLAP bundle (no clap_entry)")]
    NoEntry(PathBuf),
    #[error("{0}: incompatible or failed to initialise")]
    Init(PathBuf),
    #[error("plugin {0} not found in {1}")]
    NoPlugin(String, PathBuf),
    #[error("plugin {0} failed to {1}")]
    Plugin(String, &'static str),
}

/// A loaded `.clap` file. Kept alive by every plugin created from it.
pub struct ClapLibrary {
    entry: *const clap_plugin_entry,
    path: PathBuf,
    _lib: libloading::Library,
}

// SAFETY: the entry is a static table of thread-safe function pointers (CLAP requires
// `get_factory` and the factory to be callable from any thread).
unsafe impl Send for ClapLibrary {}
unsafe impl Sync for ClapLibrary {}

#[derive(Debug, Clone, PartialEq)]
pub struct ClapDescriptor {
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub features: Vec<String>,
    pub path: PathBuf,
}

impl ClapDescriptor {
    pub fn is_instrument(&self) -> bool {
        self.features.iter().any(|f| f == "instrument")
    }
}

fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: CLAP strings are NUL-terminated and live as long as the descriptor.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

impl ClapLibrary {
    pub fn load(path: &Path) -> Result<Arc<Self>, ClapError> {
        // SAFETY: loading a plugin runs its initialisers; inherent to hosting plugins.
        let lib = unsafe { libloading::Library::new(path) }.map_err(|e| ClapError::Load(path.into(), e.to_string()))?;
        // SAFETY: `clap_entry` is a `clap_plugin_entry` by the CLAP ABI.
        let entry: *const clap_plugin_entry = unsafe {
            *lib.get::<*const clap_plugin_entry>(b"clap_entry\0").map_err(|_| ClapError::NoEntry(path.into()))?
        };
        // SAFETY: checked for null before use.
        let e = unsafe { entry.as_ref() }.ok_or_else(|| ClapError::NoEntry(path.into()))?;
        let cpath = CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| ClapError::Init(path.into()))?;
        // SAFETY: `init` is called once per load with the bundle path.
        let ok =
            clap_version_is_compatible(e.clap_version) && e.init.is_some_and(|init| unsafe { init(cpath.as_ptr()) });
        if !ok {
            return Err(ClapError::Init(path.into()));
        }
        Ok(Arc::new(Self { entry, path: path.into(), _lib: lib }))
    }

    fn factory(&self) -> Option<&clap_plugin_factory> {
        // SAFETY: entry is valid while the library is loaded.
        let get = unsafe { (*self.entry).get_factory }?;
        // SAFETY: the plugin factory id is a valid C string; the result has that type.
        unsafe { (get(CLAP_PLUGIN_FACTORY_ID.as_ptr()) as *const clap_plugin_factory).as_ref() }
    }

    pub fn descriptors(&self) -> Vec<ClapDescriptor> {
        let Some(f) = self.factory() else { return Vec::new() };
        let (Some(count), Some(get)) = (f.get_plugin_count, f.get_plugin_descriptor) else { return Vec::new() };
        // SAFETY: factory calls with indices below the reported count.
        (0..unsafe { count(f) })
            .filter_map(|i| unsafe { get(f, i).as_ref() })
            .map(|d: &clap_plugin_descriptor| {
                let mut features = Vec::new();
                let mut p = d.features;
                // SAFETY: `features` is a NULL-terminated array of C strings.
                while !p.is_null() && unsafe { !(*p).is_null() } {
                    features.push(cstr(unsafe { *p }));
                    p = unsafe { p.add(1) };
                }
                ClapDescriptor {
                    id: cstr(d.id),
                    name: cstr(d.name),
                    vendor: cstr(d.vendor),
                    features,
                    path: self.path.clone(),
                }
            })
            .collect()
    }
}

impl Drop for ClapLibrary {
    fn drop(&mut self) {
        // SAFETY: matches the successful `init` in `load`.
        if let Some(deinit) = unsafe { (*self.entry).deinit } {
            unsafe { deinit() };
        }
    }
}

/// Every `.clap` in `CLAP_PATH` and the standard locations.
pub fn scan() -> Vec<ClapDescriptor> {
    let mut dirs: Vec<PathBuf> =
        std::env::var_os("CLAP_PATH").map(|v| std::env::split_paths(&v).collect()).unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.extend([home.join(".clap"), home.join(".nix-profile/lib/clap")]);
    }
    dirs.extend(["/usr/lib/clap", "/usr/local/lib/clap", "/run/current-system/sw/lib/clap"].map(PathBuf::from));
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "clap")
                && let Ok(lib) = ClapLibrary::load(&p)
            {
                out.extend(lib.descriptors());
            }
        }
    }
    out
}

static HOST_NAME: &CStr = c"Scythe";
static HOST_VENDOR: &CStr = c"Scythe";
static HOST_URL: &CStr = c"";
static HOST_VERSION: &CStr = c"0.1";

/// What a plugin asks of the host on the main thread: timers, file descriptors and editor
/// requests. Shared with its editor.
#[derive(Debug, Default)]
pub struct HostData {
    timers: Mutex<Timers>,
    fds: Mutex<Vec<i32>>,
    resize: Mutex<Option<(u32, u32)>>,
    closed: AtomicBool,
}

unsafe fn host_data<'a>(host: *const clap_host) -> Option<&'a HostData> {
    // SAFETY: host_data is the plugin's HostData, alive as long as the host struct.
    unsafe { ((*host).host_data as *const HostData).as_ref() }
}

unsafe extern "C" fn gui_hints(_host: *const clap_host) {}

unsafe extern "C" fn gui_resize(host: *const clap_host, width: u32, height: u32) -> bool {
    let Some(d) = (unsafe { host_data(host) }) else { return false };
    *d.resize.lock().unwrap() = Some((width, height));
    true
}

unsafe extern "C" fn gui_show(_host: *const clap_host) -> bool {
    true
}

unsafe extern "C" fn gui_closed(host: *const clap_host, _was_destroyed: bool) {
    if let Some(d) = unsafe { host_data(host) } {
        d.closed.store(true, Ordering::Relaxed);
    }
}

unsafe extern "C" fn timer_register(host: *const clap_host, period_ms: u32, id: *mut clap_id) -> bool {
    let Some(d) = (unsafe { host_data(host) }) else { return false };
    let new = d.timers.lock().unwrap().add(std::time::Duration::from_millis(period_ms as u64));
    // SAFETY: the plugin passes a writable id.
    unsafe { *id = new };
    true
}

unsafe extern "C" fn timer_unregister(host: *const clap_host, id: clap_id) -> bool {
    unsafe { host_data(host) }.is_some_and(|d| d.timers.lock().unwrap().remove(id))
}

unsafe extern "C" fn fd_register(host: *const clap_host, fd: i32, _flags: clap_posix_fd_flags) -> bool {
    let Some(d) = (unsafe { host_data(host) }) else { return false };
    let mut fds = d.fds.lock().unwrap();
    if !fds.contains(&fd) {
        fds.push(fd);
    }
    true
}

unsafe extern "C" fn fd_modify(_host: *const clap_host, _fd: i32, _flags: clap_posix_fd_flags) -> bool {
    true
}

unsafe extern "C" fn fd_unregister(host: *const clap_host, fd: i32) -> bool {
    let Some(d) = (unsafe { host_data(host) }) else { return false };
    d.fds.lock().unwrap().retain(|&f| f != fd);
    true
}

static HOST_GUI: clap_host_gui = clap_host_gui {
    resize_hints_changed: Some(gui_hints),
    request_resize: Some(gui_resize),
    request_show: Some(gui_show),
    request_hide: Some(gui_show),
    closed: Some(gui_closed),
};
static HOST_TIMERS: clap_host_timer_support =
    clap_host_timer_support { register_timer: Some(timer_register), unregister_timer: Some(timer_unregister) };
static HOST_FDS: clap_host_posix_fd_support = clap_host_posix_fd_support {
    register_fd: Some(fd_register),
    modify_fd: Some(fd_modify),
    unregister_fd: Some(fd_unregister),
};

unsafe extern "C" fn host_get_extension(_host: *const clap_host, id: *const c_char) -> *const c_void {
    // SAFETY: plugins pass a NUL-terminated extension id.
    let id = unsafe { CStr::from_ptr(id) };
    if id == CLAP_EXT_GUI {
        &HOST_GUI as *const _ as *const c_void
    } else if id == CLAP_EXT_TIMER_SUPPORT {
        &HOST_TIMERS as *const _ as *const c_void
    } else if id == CLAP_EXT_POSIX_FD_SUPPORT {
        &HOST_FDS as *const _ as *const c_void
    } else {
        std::ptr::null()
    }
}

/// A CLAP plugin's editor, embedded through the gui extension. It holds the plugin pointer
/// without the effect's lock; the owner keeps the plugin alive while the editor is open.
pub struct ClapEditor {
    plugin: *const clap_plugin,
    data: Arc<HostData>,
    created: bool,
}

impl ClapEditor {
    fn ext<T>(&self, id: &CStr) -> Option<&T> {
        // SAFETY: the plugin is alive; get_extension returns null or the requested interface.
        unsafe { ((*self.plugin).get_extension?(self.plugin, id.as_ptr()) as *const T).as_ref() }
    }
}

impl Editor for ClapEditor {
    fn open(&mut self, parent: u32) -> Result<(u32, u32), String> {
        let gui = self.ext::<clap_plugin_gui>(CLAP_EXT_GUI).copied().ok_or("the plugin has no editor")?;
        let x11 = CLAP_WINDOW_API_X11.as_ptr();
        // SAFETY: main-thread gui calls in the order the extension prescribes.
        unsafe {
            if !gui.is_api_supported.is_some_and(|f| f(self.plugin, x11, false)) {
                return Err("the editor does not support X11 windows".into());
            }
            if !gui.create.is_some_and(|f| f(self.plugin, x11, false)) {
                return Err("the editor could not be created".into());
            }
            self.created = true;
            let (mut w, mut h) = (640u32, 480u32);
            if let Some(get) = gui.get_size {
                get(self.plugin, &mut w, &mut h);
            }
            let window = clap_window { api: x11, specific: clap_window_handle { x11: parent as clap_xwnd } };
            if !gui.set_parent.is_some_and(|f| f(self.plugin, &window)) {
                return Err("the editor could not be embedded".into());
            }
            if let Some(show) = gui.show {
                show(self.plugin);
            }
            Ok((w, h))
        }
    }

    fn idle(&mut self) -> Option<(u32, u32)> {
        let due = self.data.timers.lock().unwrap().due();
        if !due.is_empty()
            && let Some(on_timer) =
                self.ext::<clap_plugin_timer_support>(CLAP_EXT_TIMER_SUPPORT).and_then(|t| t.on_timer)
        {
            for id in due {
                // SAFETY: a main-thread callback for a timer the plugin registered.
                unsafe { on_timer(self.plugin, id) };
            }
        }
        let fds = self.data.fds.lock().unwrap().clone();
        let ready = ready_fds(&fds);
        if !ready.is_empty()
            && let Some(on_fd) =
                self.ext::<clap_plugin_posix_fd_support>(CLAP_EXT_POSIX_FD_SUPPORT).and_then(|f| f.on_fd)
        {
            for (fd, events) in ready {
                let mut flags = 0;
                if events & (libc::POLLIN | libc::POLLPRI) != 0 {
                    flags |= CLAP_POSIX_FD_READ;
                }
                if events & (libc::POLLERR | libc::POLLHUP) != 0 {
                    flags |= CLAP_POSIX_FD_ERROR;
                }
                // SAFETY: a main-thread callback for a descriptor the plugin registered.
                unsafe { on_fd(self.plugin, fd, flags) };
            }
        }
        self.data.resize.lock().unwrap().take()
    }

    fn close(&mut self) {
        if !std::mem::take(&mut self.created) {
            return;
        }
        if let Some(gui) = self.ext::<clap_plugin_gui>(CLAP_EXT_GUI).copied() {
            // SAFETY: hide and destroy the editor created in `open`.
            unsafe {
                if let Some(hide) = gui.hide {
                    hide(self.plugin);
                }
                if let Some(destroy) = gui.destroy {
                    destroy(self.plugin);
                }
            }
        }
    }

    fn closed(&self) -> bool {
        self.data.closed.load(Ordering::Relaxed)
    }
}

impl Drop for ClapEditor {
    fn drop(&mut self) {
        self.close();
    }
}

unsafe extern "C" fn host_request(_host: *const clap_host) {}

unsafe extern "C" fn events_size(list: *const clap_input_events) -> u32 {
    // SAFETY: ctx is the ordered event list of the ClapPlugin running `process`.
    unsafe { (*((*list).ctx as *const Vec<*const clap_event_header>)).len() as u32 }
}
unsafe extern "C" fn events_get(list: *const clap_input_events, index: u32) -> *const clap_event_header {
    // SAFETY: as above; the index is below `events_size`.
    let events = unsafe { &*((*list).ctx as *const Vec<*const clap_event_header>) };
    events.get(index as usize).copied().unwrap_or(std::ptr::null())
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParamInfo {
    pub id: u32,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    /// Edit on a logarithmic scale (frequencies, times).
    pub log: bool,
}

/// A note event for `process`, `offset` frames into the block.
pub fn note_event(on: bool, offset: u32, channel: u8, key: u8, velocity: u8) -> clap_event_note {
    clap_event_note {
        header: clap_event_header {
            size: std::mem::size_of::<clap_event_note>() as u32,
            time: offset,
            space_id: CLAP_CORE_EVENT_SPACE_ID,
            type_: if on { CLAP_EVENT_NOTE_ON } else { CLAP_EVENT_NOTE_OFF },
            flags: 0,
        },
        note_id: -1,
        port_index: 0,
        channel: channel as i16,
        key: key as i16,
        velocity: velocity as f64 / 127.0,
    }
}
unsafe extern "C" fn drop_event(_list: *const clap_output_events, _event: *const clap_event_header) -> bool {
    true
}

unsafe extern "C" fn write_vec(stream: *const clap_ostream, buffer: *const c_void, size: u64) -> i64 {
    // SAFETY: ctx is the Vec passed by `save_state`; the plugin hands a valid buffer.
    let out = unsafe { &mut *((*stream).ctx as *mut Vec<u8>) };
    out.extend_from_slice(unsafe { std::slice::from_raw_parts(buffer as *const u8, size as usize) });
    size as i64
}

unsafe extern "C" fn read_slice(stream: *const clap_istream, buffer: *mut c_void, size: u64) -> i64 {
    // SAFETY: ctx is the (data, position) pair passed by `load_state`.
    let (data, pos) = unsafe { &mut *((*stream).ctx as *mut (&[u8], usize)) };
    let n = (data.len() - *pos).min(size as usize);
    unsafe { std::ptr::copy_nonoverlapping(data[*pos..].as_ptr(), buffer as *mut u8, n) };
    *pos += n;
    n as i64
}

/// An instantiated, activated CLAP plugin processing stereo in place.
pub struct ClapPlugin {
    plugin: *const clap_plugin,
    _host: Box<clap_host>,
    host_data: Arc<HostData>,
    _lib: Arc<ClapLibrary>,
    pub id: String,
    processing: bool,
    scratch: [Vec<f32>; 2],
    /// Note events for the next `process` call, sorted by time; capacity is reserved up front.
    pub events: Vec<clap_event_note>,
    /// Parameter changes to apply at the start of the next `process` call.
    pending_params: Vec<clap_event_param_value>,
    /// All events of a `process` call in time order, as handed to the plugin.
    order: Vec<*const clap_event_header>,
}

// SAFETY: the engine serialises access; audio-thread calls (`process`) never overlap
// main-thread calls on the same instance.
unsafe impl Send for ClapPlugin {}

impl ClapPlugin {
    pub fn create(lib: Arc<ClapLibrary>, id: &str, sample_rate: f64, max_frames: u32) -> Result<Self, ClapError> {
        let factory = lib.factory().ok_or_else(|| ClapError::NoPlugin(id.into(), lib.path.clone()))?;
        let host_data = Arc::new(HostData::default());
        let host = Box::new(clap_host {
            clap_version: CLAP_VERSION,
            host_data: Arc::as_ptr(&host_data) as *mut c_void,
            name: HOST_NAME.as_ptr(),
            vendor: HOST_VENDOR.as_ptr(),
            url: HOST_URL.as_ptr(),
            version: HOST_VERSION.as_ptr(),
            get_extension: Some(host_get_extension),
            request_restart: Some(host_request),
            request_process: Some(host_request),
            request_callback: Some(host_request),
        });
        let cid = CString::new(id).map_err(|_| ClapError::NoPlugin(id.into(), lib.path.clone()))?;
        let create = factory.create_plugin.ok_or_else(|| ClapError::NoPlugin(id.into(), lib.path.clone()))?;
        // SAFETY: factory call with a host that outlives the plugin.
        let plugin = unsafe { create(factory, host.as_ref(), cid.as_ptr()) };
        if plugin.is_null() {
            return Err(ClapError::NoPlugin(id.into(), lib.path.clone()));
        }
        let me = Self {
            plugin,
            _host: host,
            host_data,
            _lib: lib,
            id: id.into(),
            processing: false,
            scratch: [Vec::new(), Vec::new()],
            events: Vec::with_capacity(1024),
            pending_params: Vec::with_capacity(64),
            order: Vec::with_capacity(1100),
        };
        // SAFETY: plugin is valid; init and activate are main-thread calls made once.
        let p = unsafe { &*plugin };
        if !p.init.is_some_and(|f| unsafe { f(plugin) }) {
            return Err(ClapError::Plugin(id.into(), "initialise"));
        }
        if !p.activate.is_some_and(|f| unsafe { f(plugin, sample_rate, 1, max_frames) }) {
            return Err(ClapError::Plugin(id.into(), "activate"));
        }
        let mut me = me;
        me.scratch = [vec![0.0; max_frames as usize], vec![0.0; max_frames as usize]];
        Ok(me)
    }

    fn raw(&self) -> &clap_plugin {
        // SAFETY: valid for the lifetime of self.
        unsafe { &*self.plugin }
    }

    fn state_ext(&self) -> Option<&clap_plugin_state> {
        let get = self.raw().get_extension?;
        // SAFETY: the state extension id is a C string; the result has that type or is null.
        unsafe { (get(self.plugin, CLAP_EXT_STATE.as_ptr()) as *const clap_plugin_state).as_ref() }
    }

    /// The plugin's editor, when it has one. Its calls bypass any lock around `self`; keep
    /// the plugin alive while the editor is in use.
    pub fn editor(&self) -> Option<ClapEditor> {
        let get = self.raw().get_extension?;
        // SAFETY: the gui extension id is a C string; the result is null or the extension.
        let has_gui = !unsafe { get(self.plugin, CLAP_EXT_GUI.as_ptr()) }.is_null();
        has_gui.then(|| ClapEditor { plugin: self.plugin, data: self.host_data.clone(), created: false })
    }

    /// Frames the plugin delays its output by.
    pub fn latency(&self) -> u32 {
        use clap_sys::ext::latency::{CLAP_EXT_LATENCY, clap_plugin_latency};
        let Some(get) = self.raw().get_extension else { return 0 };
        // SAFETY: the latency extension id is a C string; the result has that type or is null.
        let ext = unsafe { (get(self.plugin, CLAP_EXT_LATENCY.as_ptr()) as *const clap_plugin_latency).as_ref() };
        // SAFETY: called on an activated plugin, as the extension requires.
        ext.and_then(|e| e.get).map_or(0, |f| unsafe { f(self.plugin) })
    }

    /// Clears voices and internal state (audio thread).
    pub fn reset(&mut self) {
        if let Some(f) = self.raw().reset {
            // SAFETY: reset is an audio-thread call on an active plugin.
            unsafe { f(self.plugin) };
        }
    }

    fn params_ext(&self) -> Option<&clap_plugin_params> {
        let get = self.raw().get_extension?;
        // SAFETY: the params extension id is a C string; the result has that type or is null.
        unsafe { (get(self.plugin, CLAP_EXT_PARAMS.as_ptr()) as *const clap_plugin_params).as_ref() }
    }

    pub fn params(&self) -> Vec<ParamInfo> {
        let Some(ext) = self.params_ext() else { return Vec::new() };
        let (Some(count), Some(get_info)) = (ext.count, ext.get_info) else { return Vec::new() };
        // SAFETY: main-thread queries with a zeroed out-struct the plugin fills in.
        (0..unsafe { count(self.plugin) })
            .filter_map(|i| {
                let mut info: clap_param_info = unsafe { std::mem::zeroed() };
                unsafe { get_info(self.plugin, i, &mut info) }.then(|| ParamInfo {
                    id: info.id,
                    name: cstr(info.name.as_ptr()),
                    min: info.min_value,
                    max: info.max_value,
                    default: info.default_value,
                    log: false,
                })
            })
            .collect()
    }

    pub fn param_value(&self, id: u32) -> Option<f64> {
        let get = self.params_ext()?.get_value?;
        let mut v = 0.0;
        // SAFETY: as above.
        unsafe { get(self.plugin, id, &mut v) }.then_some(v)
    }

    pub fn param_text(&self, id: u32, value: f64) -> Option<String> {
        let to_text = self.params_ext()?.value_to_text?;
        let mut buf = [0 as c_char; 128];
        // SAFETY: as above, with a buffer of the stated capacity.
        unsafe { to_text(self.plugin, id, value, buf.as_mut_ptr(), buf.len() as u32) }.then(|| cstr(buf.as_ptr()))
    }

    /// Queues a parameter change for the next processed block.
    pub fn set_param(&mut self, id: u32, value: f64) {
        self.pending_params.retain(|e| e.param_id != id);
        self.pending_params.push(clap_event_param_value {
            header: clap_event_header {
                size: std::mem::size_of::<clap_event_param_value>() as u32,
                time: 0,
                space_id: CLAP_CORE_EVENT_SPACE_ID,
                type_: CLAP_EVENT_PARAM_VALUE,
                flags: 0,
            },
            param_id: id,
            cookie: std::ptr::null_mut(),
            note_id: -1,
            port_index: -1,
            channel: -1,
            key: -1,
            value,
        });
    }

    pub fn save_state(&self) -> Option<Vec<u8>> {
        let save = self.state_ext()?.save?;
        let mut out = Vec::new();
        let stream = clap_ostream { ctx: &mut out as *mut Vec<u8> as *mut c_void, write: Some(write_vec) };
        // SAFETY: the stream and its buffer outlive the call.
        unsafe { save(self.plugin, &stream) }.then_some(out)
    }

    pub fn load_state(&mut self, data: &[u8]) -> bool {
        let Some(load) = self.state_ext().and_then(|s| s.load) else { return false };
        let mut ctx: (&[u8], usize) = (data, 0);
        let stream = clap_istream { ctx: &mut ctx as *mut (&[u8], usize) as *mut c_void, read: Some(read_slice) };
        // SAFETY: as in `save_state`.
        unsafe { load(self.plugin, &stream) }
    }

    /// Processes a stereo block in place.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let n = left.len().min(right.len()).min(self.scratch[0].len());
        if !self.processing {
            // SAFETY: start_processing is an audio-thread call before the first process.
            self.processing = self.raw().start_processing.is_none_or(|f| unsafe { f(self.plugin) });
        }
        self.scratch[0][..n].copy_from_slice(&left[..n]);
        self.scratch[1][..n].copy_from_slice(&right[..n]);
        let mut in_ptrs = [self.scratch[0].as_mut_ptr(), self.scratch[1].as_mut_ptr()];
        let mut out_ptrs = [left.as_mut_ptr(), right.as_mut_ptr()];
        let input = clap_audio_buffer {
            data32: in_ptrs.as_mut_ptr(),
            data64: std::ptr::null_mut(),
            channel_count: 2,
            latency: 0,
            constant_mask: 0,
        };
        let mut output = clap_audio_buffer {
            data32: out_ptrs.as_mut_ptr(),
            data64: std::ptr::null_mut(),
            channel_count: 2,
            latency: 0,
            constant_mask: 0,
        };
        self.order.clear();
        self.order.extend(self.pending_params.iter().map(|e| &e.header as *const clap_event_header));
        self.order.extend(self.events.iter().map(|e| &e.header as *const clap_event_header));
        let in_events = clap_input_events {
            ctx: &self.order as *const Vec<*const clap_event_header> as *mut c_void,
            size: Some(events_size),
            get: Some(events_get),
        };
        let out_events = clap_output_events { ctx: std::ptr::null_mut(), try_push: Some(drop_event) };
        let process = clap_process {
            steady_time: -1,
            frames_count: n as u32,
            transport: std::ptr::null(),
            audio_inputs: &input,
            audio_outputs: &mut output,
            audio_inputs_count: 1,
            audio_outputs_count: 1,
            in_events: &in_events,
            out_events: &out_events,
        };
        if let Some(f) = self.raw().process {
            // SAFETY: every pointer in `process` is valid for the duration of the call.
            unsafe { f(self.plugin, &process) };
        }
        self.events.clear();
        self.pending_params.clear();
        self.order.clear();
    }
}

impl Drop for ClapPlugin {
    fn drop(&mut self) {
        let p = *self.raw();
        // SAFETY: tear-down in the order CLAP requires.
        unsafe {
            if self.processing
                && let Some(f) = p.stop_processing
            {
                f(self.plugin);
            }
            if let Some(f) = p.deactivate {
                f(self.plugin);
            }
            if let Some(f) = p.destroy {
                f(self.plugin);
            }
        }
    }
}
