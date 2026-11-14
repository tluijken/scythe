//! VST 3 plugin hosting through the `vst3` bindings: bundle discovery, a host context that can
//! create messages for component/controller communication, stereo processing with note
//! events and parameter changes, and state in REAPER's chunk layout.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use vst3::Steinberg::Linux::{IEventHandlerTrait, IRunLoopTrait, ITimerHandlerTrait};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComRef, ComWrapper, Interface};

#[cfg(unix)]
use libloading::os::unix::Library as OsLibrary;
#[cfg(windows)]
use libloading::os::windows::Library as OsLibrary;

fn to_wide(s: &str, dst: &mut [TChar]) {
    let mut n = 0;
    for (d, c) in dst.iter_mut().zip(s.encode_utf16()) {
        *d = c as TChar;
        n += 1;
    }
    if let Some(end) = dst.get_mut(n.min(dst.len().saturating_sub(1))) {
        *end = 0;
    }
}

fn from_wide(src: &[TChar]) -> String {
    let len = src.iter().position(|&c| c == 0).unwrap_or(src.len());
    String::from_utf16_lossy(&src[..len])
}

fn from_c(src: &[c_char]) -> String {
    let bytes: Vec<u8> = src.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn tuid_bytes(t: &TUID) -> [u8; 16] {
    t.map(|c| c as u8)
}

fn guid_eq(t: &TUID, iid: &[u8; 16]) -> bool {
    tuid_bytes(t) == *iid
}

/// A class id as REAPER prints it in a slot: 32 upper-case hex digits.
pub fn cid_hex(cid: &[u8; 16]) -> String {
    cid.iter().map(|b| format!("{b:02X}")).collect()
}

// ---- host objects ------------------------------------------------------------------------

#[derive(Default)]
struct AttributeList {
    values: RefCell<HashMap<String, Attr>>,
}

#[derive(Clone)]
enum Attr {
    Int(i64),
    Float(f64),
    String(Vec<TChar>),
    Binary(Vec<u8>),
}

impl Class for AttributeList {
    type Interfaces = (IAttributeList,);
}

unsafe fn attr_id(id: IAttributeList_::AttrID) -> String {
    // SAFETY: attribute ids are NUL-terminated C strings.
    unsafe { CStr::from_ptr(id) }.to_string_lossy().into_owned()
}

impl IAttributeListTrait for AttributeList {
    unsafe fn setInt(&self, id: IAttributeList_::AttrID, value: int64) -> tresult {
        self.values.borrow_mut().insert(unsafe { attr_id(id) }, Attr::Int(value));
        kResultOk
    }
    unsafe fn getInt(&self, id: IAttributeList_::AttrID, value: *mut int64) -> tresult {
        match self.values.borrow().get(&unsafe { attr_id(id) }) {
            Some(Attr::Int(v)) => {
                unsafe { *value = *v };
                kResultOk
            }
            _ => kResultFalse,
        }
    }
    unsafe fn setFloat(&self, id: IAttributeList_::AttrID, value: f64) -> tresult {
        self.values.borrow_mut().insert(unsafe { attr_id(id) }, Attr::Float(value));
        kResultOk
    }
    unsafe fn getFloat(&self, id: IAttributeList_::AttrID, value: *mut f64) -> tresult {
        match self.values.borrow().get(&unsafe { attr_id(id) }) {
            Some(Attr::Float(v)) => {
                unsafe { *value = *v };
                kResultOk
            }
            _ => kResultFalse,
        }
    }
    unsafe fn setString(&self, id: IAttributeList_::AttrID, string: *const TChar) -> tresult {
        let mut s = Vec::new();
        // SAFETY: the string is NUL-terminated.
        unsafe {
            let mut p = string;
            while *p != 0 {
                s.push(*p);
                p = p.add(1);
            }
        }
        s.push(0);
        self.values.borrow_mut().insert(unsafe { attr_id(id) }, Attr::String(s));
        kResultOk
    }
    unsafe fn getString(&self, id: IAttributeList_::AttrID, string: *mut TChar, size: uint32) -> tresult {
        match self.values.borrow().get(&unsafe { attr_id(id) }) {
            Some(Attr::String(v)) => {
                let n = v.len().min(size as usize / std::mem::size_of::<TChar>());
                // SAFETY: the caller's buffer holds `size` bytes.
                unsafe { std::ptr::copy_nonoverlapping(v.as_ptr(), string, n) };
                kResultOk
            }
            _ => kResultFalse,
        }
    }
    unsafe fn setBinary(&self, id: IAttributeList_::AttrID, data: *const c_void, size: uint32) -> tresult {
        // SAFETY: data holds `size` bytes.
        let bytes = unsafe { std::slice::from_raw_parts(data as *const u8, size as usize) }.to_vec();
        self.values.borrow_mut().insert(unsafe { attr_id(id) }, Attr::Binary(bytes));
        kResultOk
    }
    unsafe fn getBinary(&self, id: IAttributeList_::AttrID, data: *mut *const c_void, size: *mut uint32) -> tresult {
        // The pointer stays valid while the message lives, as the API expects.
        match self.values.borrow().get(&unsafe { attr_id(id) }) {
            Some(Attr::Binary(v)) => {
                unsafe {
                    *data = v.as_ptr() as *const c_void;
                    *size = v.len() as uint32;
                }
                kResultOk
            }
            _ => kResultFalse,
        }
    }
}

struct Message {
    id: RefCell<CString>,
    attributes: ComWrapper<AttributeList>,
}

impl Class for Message {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for Message {
    unsafe fn getMessageID(&self) -> FIDString {
        self.id.borrow().as_ptr()
    }
    unsafe fn setMessageID(&self, id: FIDString) {
        // SAFETY: message ids are NUL-terminated C strings.
        *self.id.borrow_mut() = unsafe { CStr::from_ptr(id) }.to_owned();
    }
    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        self.attributes.as_com_ref::<IAttributeList>().map_or(std::ptr::null_mut(), |r| r.as_ptr())
    }
}

struct HostApp;

impl Class for HostApp {
    type Interfaces = (IHostApplication,);
}

impl IHostApplicationTrait for HostApp {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        // SAFETY: the plugin passes a String128 to fill.
        to_wide("Scythe", unsafe { &mut *name });
        kResultOk
    }

    unsafe fn createInstance(&self, cid: *mut TUID, _iid: *mut TUID, obj: *mut *mut c_void) -> tresult {
        // SAFETY: cid points at a class id; obj receives a new reference.
        unsafe {
            if guid_eq(&*cid, &IMessage::IID) {
                let msg = ComWrapper::new(Message {
                    id: RefCell::new(CString::default()),
                    attributes: ComWrapper::new(AttributeList::default()),
                });
                *obj = msg.to_com_ptr::<IMessage>().map_or(std::ptr::null_mut(), |p| p.into_raw() as *mut c_void);
                return kResultOk;
            }
            if guid_eq(&*cid, &IAttributeList::IID) {
                let list = ComWrapper::new(AttributeList::default());
                *obj =
                    list.to_com_ptr::<IAttributeList>().map_or(std::ptr::null_mut(), |p| p.into_raw() as *mut c_void);
                return kResultOk;
            }
            *obj = std::ptr::null_mut();
        }
        kResultFalse
    }
}

/// An in-memory IBStream for plugin state.
#[derive(Default)]
struct Stream {
    data: RefCell<Vec<u8>>,
    at: RefCell<usize>,
}

impl Class for Stream {
    type Interfaces = (IBStream,);
}

impl IBStreamTrait for Stream {
    unsafe fn read(&self, buffer: *mut c_void, n: int32, read: *mut int32) -> tresult {
        let data = self.data.borrow();
        let mut at = self.at.borrow_mut();
        let count = (n.max(0) as usize).min(data.len().saturating_sub(*at));
        // SAFETY: the buffer holds `n` bytes.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr().add(*at), buffer as *mut u8, count) };
        *at += count;
        if !read.is_null() {
            unsafe { *read = count as int32 };
        }
        kResultOk
    }
    unsafe fn write(&self, buffer: *mut c_void, n: int32, written: *mut int32) -> tresult {
        let mut data = self.data.borrow_mut();
        let mut at = self.at.borrow_mut();
        let n = n.max(0) as usize;
        if data.len() < *at + n {
            data.resize(*at + n, 0);
        }
        // SAFETY: the buffer holds `n` bytes.
        unsafe { std::ptr::copy_nonoverlapping(buffer as *const u8, data.as_mut_ptr().add(*at), n) };
        *at += n;
        if !written.is_null() {
            unsafe { *written = n as int32 };
        }
        kResultOk
    }
    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> tresult {
        let len = self.data.borrow().len() as i64;
        let mut at = self.at.borrow_mut();
        let base = match mode {
            m if m as i64 == IBStream_::IStreamSeekMode_::kIBSeekCur as i64 => *at as i64,
            m if m as i64 == IBStream_::IStreamSeekMode_::kIBSeekEnd as i64 => len,
            _ => 0,
        };
        *at = (base + pos).clamp(0, len) as usize;
        if !result.is_null() {
            unsafe { *result = *at as int64 };
        }
        kResultOk
    }
    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        if !pos.is_null() {
            unsafe { *pos = *self.at.borrow() as int64 };
        }
        kResultOk
    }
}

fn stream_with(bytes: &[u8]) -> ComWrapper<Stream> {
    ComWrapper::new(Stream { data: RefCell::new(bytes.to_vec()), at: RefCell::new(0) })
}

#[derive(Default)]
struct EventList {
    events: RefCell<Vec<Event>>,
}

impl Class for EventList {
    type Interfaces = (IEventList,);
}

impl IEventListTrait for EventList {
    unsafe fn getEventCount(&self) -> int32 {
        self.events.borrow().len() as int32
    }
    unsafe fn getEvent(&self, index: int32, e: *mut Event) -> tresult {
        match self.events.borrow().get(index as usize) {
            Some(ev) => {
                // SAFETY: the plugin passes an Event to fill.
                unsafe { *e = *ev };
                kResultOk
            }
            None => kResultFalse,
        }
    }
    unsafe fn addEvent(&self, e: *mut Event) -> tresult {
        // SAFETY: the plugin passes a valid event.
        self.events.borrow_mut().push(unsafe { *e });
        kResultOk
    }
}

struct ValueQueue {
    id: RefCell<ParamID>,
    points: RefCell<Vec<(int32, ParamValue)>>,
}

impl Class for ValueQueue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for ValueQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        *self.id.borrow()
    }
    unsafe fn getPointCount(&self) -> int32 {
        self.points.borrow().len() as int32
    }
    unsafe fn getPoint(&self, index: int32, offset: *mut int32, value: *mut ParamValue) -> tresult {
        match self.points.borrow().get(index as usize) {
            Some(&(o, v)) => {
                // SAFETY: the plugin passes valid out pointers.
                unsafe {
                    *offset = o;
                    *value = v;
                }
                kResultOk
            }
            None => kResultFalse,
        }
    }
    unsafe fn addPoint(&self, offset: int32, value: ParamValue, index: *mut int32) -> tresult {
        let mut points = self.points.borrow_mut();
        points.push((offset, value));
        if !index.is_null() {
            unsafe { *index = points.len() as int32 - 1 };
        }
        kResultOk
    }
}

/// Parameter changes for one block. Queues are kept and reused so processing never allocates
/// once every parameter has been touched.
#[derive(Default)]
struct ParamChanges {
    queues: RefCell<Vec<ComWrapper<ValueQueue>>>,
    used: RefCell<usize>,
}

impl Class for ParamChanges {
    type Interfaces = (IParameterChanges,);
}

impl ParamChanges {
    fn push(&self, id: ParamID, value: ParamValue) {
        self.push_at(id, 0, value);
    }

    /// Adds a change `offset` frames into the block; changes of one parameter come in order.
    fn push_at(&self, id: ParamID, offset: i32, value: ParamValue) {
        let mut queues = self.queues.borrow_mut();
        let mut used = self.used.borrow_mut();
        if let Some(q) = queues[..*used].iter().find(|q| *q.id.borrow() == id) {
            q.points.borrow_mut().push((offset, value));
            return;
        }
        if *used == queues.len() {
            queues.push(ComWrapper::new(ValueQueue { id: RefCell::new(id), points: RefCell::new(Vec::new()) }));
        }
        let q = &queues[*used];
        *q.id.borrow_mut() = id;
        q.points.borrow_mut().clear();
        q.points.borrow_mut().push((offset, value));
        *used += 1;
    }

    fn clear(&self) {
        *self.used.borrow_mut() = 0;
    }
}

impl IParameterChangesTrait for ParamChanges {
    unsafe fn getParameterCount(&self) -> int32 {
        *self.used.borrow() as int32
    }
    unsafe fn getParameterData(&self, index: int32) -> *mut IParamValueQueue {
        let queues = self.queues.borrow();
        if index < 0 || index as usize >= *self.used.borrow() {
            return std::ptr::null_mut();
        }
        queues[index as usize].as_com_ref::<IParamValueQueue>().map_or(std::ptr::null_mut(), |r| r.as_ptr())
    }
    unsafe fn addParameterData(&self, id: *const ParamID, index: *mut int32) -> *mut IParamValueQueue {
        // SAFETY: the plugin passes a valid id pointer.
        let id = unsafe { *id };
        let at = {
            let queues = self.queues.borrow();
            queues[..*self.used.borrow()].iter().position(|q| *q.id.borrow() == id)
        };
        let at = at.unwrap_or_else(|| {
            let mut queues = self.queues.borrow_mut();
            let mut used = self.used.borrow_mut();
            if *used == queues.len() {
                queues.push(ComWrapper::new(ValueQueue { id: RefCell::new(id), points: RefCell::new(Vec::new()) }));
            }
            *queues[*used].id.borrow_mut() = id;
            queues[*used].points.borrow_mut().clear();
            *used += 1;
            *used - 1
        });
        if !index.is_null() {
            unsafe { *index = at as int32 };
        }
        self.queues.borrow()[at].as_com_ref::<IParamValueQueue>().map_or(std::ptr::null_mut(), |r| r.as_ptr())
    }
}

/// Receives edits the controller makes (from its editor) so they reach the processor too.
#[derive(Default)]
struct ComponentHandler {
    edits: RefCell<Vec<(ParamID, ParamValue)>>,
}

impl Class for ComponentHandler {
    type Interfaces = (IComponentHandler,);
}

impl IComponentHandlerTrait for ComponentHandler {
    unsafe fn beginEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }
    unsafe fn performEdit(&self, id: ParamID, value: ParamValue) -> tresult {
        self.edits.borrow_mut().push((id, value));
        kResultOk
    }
    unsafe fn endEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }
    unsafe fn restartComponent(&self, _flags: int32) -> tresult {
        kResultOk
    }
}

// ---- modules and discovery ---------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Vst3Info {
    pub name: String,
    pub vendor: String,
    /// The shared library inside the bundle.
    pub path: PathBuf,
    /// The `.vst3` bundle (or file) name REAPER stores in the slot.
    pub file_name: String,
    pub cid: [u8; 16],
    pub instrument: bool,
}

pub struct Vst3Module {
    factory: Option<ComPtr<IPluginFactory>>,
    lib: Option<OsLibrary>,
    exit: Option<unsafe extern "system" fn() -> bool>,
    pub path: PathBuf,
}

// SAFETY: the factory is only used from one thread at a time.
unsafe impl Send for Vst3Module {}
unsafe impl Sync for Vst3Module {}

impl Vst3Module {
    pub fn load(path: &Path) -> Result<Arc<Self>, String> {
        // SAFETY: loading a plugin runs its initialisers; that is inherent to hosting plugins.
        let lib = unsafe { OsLibrary::new(path) }.map_err(|e| e.to_string())?;
        let raw = lib.into_raw();
        // SAFETY: `raw` came from `into_raw` just above.
        let lib = unsafe { OsLibrary::from_raw(raw) };
        // SAFETY: these entry points have these signatures in every VST 3 module.
        unsafe {
            #[cfg(all(unix, not(target_os = "macos")))]
            let (entered, exit) = (
                lib.get::<unsafe extern "system" fn(*mut c_void) -> bool>(b"ModuleEntry")
                    .map_or(true, |entry| entry(raw)),
                lib.get::<unsafe extern "system" fn() -> bool>(b"ModuleExit").ok().map(|s| *s),
            );
            #[cfg(target_os = "macos")]
            let (entered, exit) = (
                lib.get::<unsafe extern "system" fn(*mut c_void) -> bool>(b"bundleEntry")
                    .map_or(true, |entry| mac::with_bundle(path, |bundle| entry(bundle))),
                lib.get::<unsafe extern "system" fn() -> bool>(b"bundleExit").ok().map(|s| *s),
            );
            #[cfg(windows)]
            let (entered, exit) = (
                lib.get::<unsafe extern "system" fn() -> bool>(b"InitDll").map_or(true, |init| init()),
                lib.get::<unsafe extern "system" fn() -> bool>(b"ExitDll").ok().map(|s| *s),
            );
            if !entered {
                return Err("the module refused to initialise".into());
            }
            let get = lib
                .get::<unsafe extern "system" fn() -> *mut IPluginFactory>(b"GetPluginFactory")
                .map_err(|e| e.to_string())?;
            let factory = ComPtr::from_raw(get()).ok_or("no plugin factory")?;
            Ok(Arc::new(Self { factory: Some(factory), lib: Some(lib), exit, path: path.to_owned() }))
        }
    }

    fn factory(&self) -> &ComPtr<IPluginFactory> {
        self.factory.as_ref().expect("factory lives until drop")
    }

    /// The audio effect and instrument classes the module provides.
    pub fn classes(&self, file_name: &str) -> Vec<Vst3Info> {
        let factory = self.factory();
        let mut vendor = String::new();
        // SAFETY: plain queries on a valid factory with structs to fill.
        unsafe {
            let mut info: PFactoryInfo = std::mem::zeroed();
            if factory.getFactoryInfo(&mut info) == kResultOk {
                vendor = from_c(&info.vendor);
            }
            let factory2 = factory.cast::<IPluginFactory2>();
            (0..factory.countClasses())
                .filter_map(|i| {
                    let mut info: PClassInfo = std::mem::zeroed();
                    if factory.getClassInfo(i, &mut info) != kResultOk || from_c(&info.category) != "Audio Module Class"
                    {
                        return None;
                    }
                    let (mut class_vendor, mut instrument) = (vendor.clone(), false);
                    if let Some(f2) = &factory2 {
                        let mut info2: PClassInfo2 = std::mem::zeroed();
                        if f2.getClassInfo2(i, &mut info2) == kResultOk {
                            let v = from_c(&info2.vendor);
                            if !v.is_empty() {
                                class_vendor = v;
                            }
                            instrument = from_c(&info2.subCategories).split('|').any(|c| c == "Instrument");
                        }
                    }
                    Some(Vst3Info {
                        name: from_c(&info.name),
                        vendor: class_vendor,
                        path: self.path.clone(),
                        file_name: file_name.to_owned(),
                        cid: tuid_bytes(&info.cid),
                        instrument,
                    })
                })
                .collect()
        }
    }
}

impl Drop for Vst3Module {
    fn drop(&mut self) {
        self.factory = None;
        if let Some(exit) = self.exit {
            // SAFETY: called once, after the last object from the module was released.
            unsafe { exit() };
        }
        self.lib = None;
    }
}

/// `VST3_PATH`, then the standard locations unless `SCYTHE_SYSTEM_PLUGINS=0`.
pub fn search_paths() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> =
        std::env::var_os("VST3_PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    if !crate::plugin_scan::system_paths() {
        return dirs;
    }
    if let Some(home) = scythe_core::paths::home() {
        dirs.extend([home.join(".vst3"), home.join(".nix-profile/lib/vst3")]);
    }
    if let Some(user) = std::env::var_os("USER") {
        dirs.push(Path::new("/etc/profiles/per-user").join(user).join("lib/vst3"));
    }
    dirs.extend(["/usr/lib/vst3", "/usr/local/lib/vst3", "/run/current-system/sw/lib/vst3"].map(PathBuf::from));
    dirs.extend(scythe_core::paths::system_plugin_dirs("VST3"));
    dirs
}

/// The shared library of a `.vst3` entry: the bundle's binary for this system
/// (`Contents/x86_64-linux/<name>.so`, `Contents/x86_64-win/<name>.vst3`, `Contents/MacOS/<name>`),
/// or the file itself.
fn module_binary(entry: &Path) -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        scythe_core::paths::bundle_executable(entry)
    } else if entry.is_dir() {
        let stem = entry.file_stem()?.to_string_lossy().into_owned();
        let (system, ext) = if cfg!(windows) { ("win", "vst3") } else { ("linux", "so") };
        let so =
            entry.join("Contents").join(format!("{}-{system}", std::env::consts::ARCH)).join(format!("{stem}.{ext}"));
        so.is_file().then_some(so)
    } else {
        entry.is_file().then(|| entry.to_owned())
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;
    use std::path::Path;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFURLCreateFromFileSystemRepresentation(
            allocator: *const c_void,
            buffer: *const u8,
            length: isize,
            is_directory: u8,
        ) -> *const c_void;
        fn CFBundleCreate(allocator: *const c_void, url: *const c_void) -> *mut c_void;
        fn CFRelease(cf: *const c_void);
    }

    /// Calls `f` with a `CFBundleRef` for the bundle `binary` (`Foo.vst3/Contents/MacOS/Foo`)
    /// sits in, as `bundleEntry` takes; null when there is none. Modules retain it themselves.
    pub fn with_bundle(binary: &Path, f: impl FnOnce(*mut c_void) -> bool) -> bool {
        let Some(bundle) = binary.ancestors().nth(3) else { return f(std::ptr::null_mut()) };
        let bytes = bundle.as_os_str().as_encoded_bytes();
        // SAFETY: `bytes` is valid for its length; every object created is released below.
        unsafe {
            let url =
                CFURLCreateFromFileSystemRepresentation(std::ptr::null(), bytes.as_ptr(), bytes.len() as isize, 1);
            let cf_bundle = if url.is_null() { std::ptr::null_mut() } else { CFBundleCreate(std::ptr::null(), url) };
            let entered = f(cf_bundle);
            if !cf_bundle.is_null() {
                CFRelease(cf_bundle);
            }
            if !url.is_null() {
                CFRelease(url);
            }
            entered
        }
    }
}

/// A module to scan: the shared library and the `.vst3` name REAPER stores in slots.
#[derive(Debug, Clone, PartialEq)]
pub struct Vst3Bundle {
    pub binary: PathBuf,
    pub file_name: String,
}

/// The `.vst3` modules under `dirs`, looking into subfolders.
pub fn modules(dirs: &[PathBuf]) -> Vec<Vst3Bundle> {
    let mut found = Vec::new();
    let mut stack: Vec<PathBuf> = dirs.iter().rev().cloned().collect();
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            if path.extension().is_some_and(|e| e == "vst3") {
                let Some(binary) = module_binary(&path) else { continue };
                let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                found.push(Vst3Bundle { binary, file_name });
            } else if path.is_dir() {
                stack.push(path);
            }
        }
    }
    found
}

/// The classes of the module at `binary`, loading it in this process.
pub fn scan_module(binary: &Path, file_name: &str) -> Result<Vec<Vst3Info>, String> {
    Vst3Module::load(binary).map(|m| m.classes(file_name))
}

/// Every plugin under `dirs`, loading each module in this process.
pub fn scan(dirs: &[PathBuf]) -> Vec<Vst3Info> {
    let mut found: Vec<Vst3Info> = modules(dirs)
        .iter()
        .flat_map(|m| {
            scan_module(&m.binary, &m.file_name).unwrap_or_else(|e| {
                eprintln!("vst3: {}: {e}", m.binary.display());
                Vec::new()
            })
        })
        .collect();
    found.sort_by_key(|p| p.name.to_lowercase());
    found
}

/// A VST 3 state in REAPER's layout: the component state with its size and a 1, then the
/// controller state the same way (REAPER writes a zero size and flag when there is none).
pub fn reaper_state(component: &[u8], controller: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend((component.len() as u32).to_le_bytes());
    out.extend(1u32.to_le_bytes());
    out.extend(component);
    out.extend((controller.len() as u32).to_le_bytes());
    out.extend((!controller.is_empty() as u32).to_le_bytes());
    out.extend(controller);
    out
}

// ---- instances ---------------------------------------------------------------------------

pub struct ParamDesc {
    pub id: u32,
    pub name: String,
    pub default: f64,
    pub steps: i32,
}

pub struct Vst3Plugin {
    controller: Option<ComPtr<IEditController>>,
    processor: ComPtr<IAudioProcessor>,
    component: ComPtr<IComponent>,
    _host: ComWrapper<HostApp>,
    handler: ComWrapper<ComponentHandler>,
    events: ComWrapper<EventList>,
    /// System exclusive for the next `process` call, as frame offset and message.
    pub sysex: Vec<(usize, Vec<u8>)>,
    changes: ComWrapper<ParamChanges>,
    /// Where the plugin reports parameters it changed itself (meters, gain reduction): plugins
    /// built with DPF assert when a host passes none.
    out_changes: ComWrapper<ParamChanges>,
    /// One buffer per channel of every audio bus, inputs and outputs in bus order.
    inputs: Vec<Vec<f32>>,
    outputs: Vec<Vec<f32>>,
    /// Channels of each input and output bus.
    in_layout: Vec<usize>,
    out_layout: Vec<usize>,
    /// What `process` hands the plugin: channel pointers into `inputs`/`outputs`, grouped by
    /// bus. Built once; the buffers never move.
    in_ptrs: Vec<*mut f32>,
    out_ptrs: Vec<*mut f32>,
    in_buses: Vec<AudioBusBuffers>,
    out_buses: Vec<AudioBusBuffers>,
    /// Input and output channels in the plugin's own arrangements, before any were set.
    default_pins: (usize, usize),
    sample_rate: f64,
    block: usize,
    pub instrument: bool,
    /// Parameter each MIDI channel's controllers drive (`IMidiMapping`), indexed by
    /// `channel * MIDI_CONTROLLERS + controller`.
    midi_map: Vec<Option<ParamID>>,
    _module: Arc<Vst3Module>,
}

/// The speaker arrangements REAPER 7.73 offers a plugin, in the order it tries them: mono,
/// stereo, 3.0, 4.0 and first-order ambisonics, 5.0, 5.1, 7.0, 7.1, then the wider layouts up
/// to 16 channels.
const LAYOUTS: [SpeakerArrangement; 23] = [
    0x80000,
    0x3,
    0x103,
    0x33,
    0xf00000,
    0x37,
    0x3f,
    0x637,
    0x63f,
    0x2d037,
    0x7c000f00000,
    0x300063f,
    0x2d83f,
    0x2d63f,
    0x2f83f,
    0x1800_0000_0002_d637,
    0x2d6ff,
    0x1800_0000_0002_d63f,
    0x2fe3f,
    0x1800_0000_0302_d637,
    0x302d6ff,
    0x1800_0000_0302_d63f,
    0x3ffc000f00000,
];

/// The arrangements REAPER tries on a track of `channels` channels.
pub fn layouts_up_to(channels: usize) -> impl Iterator<Item = SpeakerArrangement> {
    LAYOUTS.into_iter().filter(move |a| a.count_ones() as usize <= channels)
}

fn channel_count(buses: &[SpeakerArrangement]) -> usize {
    buses.iter().map(|a| a.count_ones() as usize).sum()
}

/// Controllers 0-127, then channel aftertouch (128) and pitch bend (129), as VST 3 numbers them.
const MIDI_CONTROLLERS: usize = 130;

/// What a controller number and value a MIDI message sets, in VST 3 terms.
fn midi_controller(bytes: [u8; 3]) -> Option<(u8, usize, f64)> {
    let channel = bytes[0] & 0x0f;
    match bytes[0] & 0xf0 {
        0xb0 => Some((channel, bytes[1] as usize & 0x7f, bytes[2] as f64 / 127.0)),
        0xd0 => Some((channel, 128, bytes[1] as f64 / 127.0)),
        0xe0 => Some((channel, 129, ((bytes[2] as u32) << 7 | bytes[1] as u32) as f64 / 16383.0)),
        _ => None,
    }
}

// SAFETY: the instance is only ever used by one thread at a time (behind the FX mutex).
unsafe impl Send for Vst3Plugin {}

impl Vst3Plugin {
    /// An instance whose audio buses are set up for a track of `channels` channels, with `state`
    /// (in REAPER's layout) loaded before it is activated.
    pub fn new(
        module: Arc<Vst3Module>,
        info: &Vst3Info,
        sample_rate: f64,
        block: usize,
        channels: usize,
        state: Option<&[u8]>,
    ) -> Result<Self, String> {
        let host = ComWrapper::new(HostApp);
        let host_unknown = host.as_com_ref::<FUnknown>().map_or(std::ptr::null_mut(), |r| r.as_ptr());
        let cid: TUID = info.cid.map(|b| b as c_char);
        // SAFETY: standard VST 3 instantiation; every pointer handed out stays alive with `self`.
        unsafe {
            let mut obj = std::ptr::null_mut();
            let factory = module.factory();
            if factory.createInstance(cid.as_ptr(), IComponent::IID.as_ptr() as FIDString, &mut obj) != kResultOk {
                return Err(format!("{} could not be created", info.name));
            }
            let component = ComPtr::from_raw(obj as *mut IComponent).ok_or("no component")?;
            if component.initialize(host_unknown) != kResultOk {
                return Err(format!("{} failed to initialise", info.name));
            }
            let processor = component.cast::<IAudioProcessor>().ok_or("not an audio processor")?;
            let controller = match component.cast::<IEditController>() {
                Some(c) => Some(c),
                None => {
                    let mut ctrl_cid: TUID = [0; 16];
                    let mut obj = std::ptr::null_mut();
                    if component.getControllerClassId(&mut ctrl_cid) == kResultOk
                        && factory.createInstance(
                            ctrl_cid.as_ptr(),
                            IEditController::IID.as_ptr() as FIDString,
                            &mut obj,
                        ) == kResultOk
                    {
                        let c = ComPtr::from_raw(obj as *mut IEditController);
                        if let Some(c) = &c {
                            c.initialize(host_unknown);
                        }
                        c
                    } else {
                        None
                    }
                }
            };
            let handler = ComWrapper::new(ComponentHandler::default());
            if let Some(c) = &controller {
                if let Some(h) = handler.as_com_ref::<IComponentHandler>() {
                    c.setComponentHandler(h.as_ptr());
                }
                // Separate component and controller talk through connection points.
                if let (Some(a), Some(b)) = (component.cast::<IConnectionPoint>(), c.cast::<IConnectionPoint>())
                    && a.as_ptr() as *mut c_void != b.as_ptr() as *mut c_void
                {
                    a.connect(b.as_ptr());
                    b.connect(a.as_ptr());
                }
            }

            let audio = MediaTypes_::kAudio as MediaType;
            let event = MediaTypes_::kEvent as MediaType;
            let input = BusDirections_::kInput as BusDirection;
            let output = BusDirections_::kOutput as BusDirection;
            let arrangements = |dir: BusDirection| -> Vec<SpeakerArrangement> {
                (0..component.getBusCount(audio, dir))
                    .map(|i| {
                        let mut arr: SpeakerArrangement = SpeakerArr::kStereo;
                        processor.getBusArrangement(dir, i, &mut arr);
                        arr
                    })
                    .collect()
            };
            let (mut ins, mut outs) = (arrangements(input), arrangements(output));
            let default_pins = (channel_count(&ins), channel_count(&outs));
            let set = |ins: &mut [SpeakerArrangement], outs: &mut [SpeakerArrangement]| {
                processor.setBusArrangements(
                    ins.as_mut_ptr(),
                    ins.len() as int32,
                    outs.as_mut_ptr(),
                    outs.len() as int32,
                ) == kResultTrue
            };
            // As REAPER: every candidate up to the track's width on every bus at once; the last
            // one accepted stays. A plugin that takes none keeps its own arrangements.
            let accepted = layouts_up_to(channels)
                .filter(|&arr| set(&mut vec![arr; ins.len()], &mut vec![arr; outs.len()]))
                .last();
            match accepted {
                Some(arr) => {
                    set(&mut vec![arr; ins.len()], &mut vec![arr; outs.len()]);
                    (ins, outs) = (arrangements(input), arrangements(output));
                }
                None => {
                    set(&mut ins, &mut outs);
                }
            }
            for i in 0..ins.len() as int32 {
                component.activateBus(audio, input, i, 1);
            }
            for i in 0..outs.len() as int32 {
                component.activateBus(audio, output, i, 1);
            }
            let in_layout: Vec<usize> = ins.iter().map(|a| a.count_ones() as usize).collect();
            let out_layout: Vec<usize> = outs.iter().map(|a| a.count_ones() as usize).collect();
            if component.getBusCount(event, input) > 0 {
                component.activateBus(event, input, 0, 1);
            }
            let mut midi_map = vec![None; 16 * MIDI_CONTROLLERS];
            if let Some(mapping) = controller.as_ref().and_then(|c| c.cast::<IMidiMapping>()) {
                for (i, slot) in midi_map.iter_mut().enumerate() {
                    let mut id: ParamID = 0;
                    let (channel, ctrl) = (i / MIDI_CONTROLLERS, i % MIDI_CONTROLLERS);
                    if mapping.getMidiControllerAssignment(0, channel as int16, ctrl as CtrlNumber, &mut id)
                        == kResultOk
                    {
                        *slot = Some(id);
                    }
                }
            }
            let mut plugin = Self {
                midi_map,
                controller,
                processor,
                component,
                _host: host,
                handler,
                events: ComWrapper::new(EventList::default()),
                sysex: Vec::new(),
                changes: ComWrapper::new(ParamChanges::default()),
                out_changes: ComWrapper::new(ParamChanges::default()),
                inputs: vec![vec![0.0; block]; in_layout.iter().sum()],
                outputs: vec![vec![0.0; block]; out_layout.iter().sum()],
                in_layout,
                out_layout,
                in_ptrs: Vec::new(),
                out_ptrs: Vec::new(),
                in_buses: Vec::new(),
                out_buses: Vec::new(),
                default_pins,
                sample_rate,
                block,
                instrument: info.instrument,
                _module: module,
            };
            plugin.wire_buses();
            match state {
                Some(state) => {
                    plugin.load_state(state);
                }
                None => plugin.sync_controller(),
            }
            let mut setup = ProcessSetup {
                processMode: ProcessModes_::kRealtime as int32,
                symbolicSampleSize: SymbolicSampleSizes_::kSample32 as int32,
                maxSamplesPerBlock: block as int32,
                sampleRate: sample_rate,
            };
            plugin.processor.setupProcessing(&mut setup);
            plugin.component.setActive(1);
            plugin.processor.setProcessing(1);
            Ok(plugin)
        }
    }

    /// Points the bus descriptions at the channel buffers.
    fn wire_buses(&mut self) {
        self.in_ptrs = self.inputs.iter_mut().map(|b| b.as_mut_ptr()).collect();
        self.out_ptrs = self.outputs.iter_mut().map(|b| b.as_mut_ptr()).collect();
        let buses = |layout: &[usize], ptrs: &mut Vec<*mut f32>| -> Vec<AudioBusBuffers> {
            let mut at = 0;
            layout
                .iter()
                .map(|&n| {
                    let bus = AudioBusBuffers {
                        numChannels: n as int32,
                        silenceFlags: 0,
                        // SAFETY: `at` stays within `ptrs`, whose length is the layout's sum.
                        __field0: AudioBusBuffers__type0 { channelBuffers32: unsafe { ptrs.as_mut_ptr().add(at) } },
                    };
                    at += n;
                    bus
                })
                .collect()
        };
        self.in_buses = buses(&self.in_layout, &mut self.in_ptrs);
        self.out_buses = buses(&self.out_layout, &mut self.out_ptrs);
    }

    /// Audio channels over all input and output buses.
    pub fn pins(&self) -> (usize, usize) {
        (self.inputs.len(), self.outputs.len())
    }

    /// Audio channels over all buses in the plugin's own arrangements, as first created.
    pub fn default_pins(&self) -> (usize, usize) {
        self.default_pins
    }

    /// Hands the component's state to a separate controller, as hosts do after loading.
    fn sync_controller(&mut self) {
        let Some(controller) = &self.controller else { return };
        let state = self.component_state();
        let stream = stream_with(&state);
        // SAFETY: the stream outlives the call.
        unsafe {
            if let Some(s) = stream.as_com_ref::<IBStream>() {
                controller.setComponentState(s.as_ptr());
            }
        }
    }

    fn component_state(&self) -> Vec<u8> {
        let stream = ComWrapper::new(Stream::default());
        // SAFETY: the stream outlives the call.
        unsafe {
            if let Some(s) = stream.as_com_ref::<IBStream>() {
                self.component.getState(s.as_ptr());
            }
        }
        stream.data.borrow().clone()
    }

    fn controller_state(&self) -> Vec<u8> {
        let Some(controller) = &self.controller else { return Vec::new() };
        let stream = ComWrapper::new(Stream::default());
        // SAFETY: the stream outlives the call.
        unsafe {
            if let Some(s) = stream.as_com_ref::<IBStream>() {
                controller.getState(s.as_ptr());
            }
        }
        stream.data.borrow().clone()
    }

    /// State in REAPER's layout ([`reaper_state`]).
    pub fn save_state(&self) -> Vec<u8> {
        reaper_state(&self.component_state(), &self.controller_state())
    }

    pub fn load_state(&mut self, data: &[u8]) -> bool {
        let u32_at = |at: usize| data.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
        let Some(len) = u32_at(0) else { return false };
        let Some(component) = data.get(8..8 + len) else { return false };
        let controller = u32_at(8 + len).and_then(|n| data.get(16 + len..16 + len + n)).unwrap_or(&[]);
        let stream = stream_with(component);
        // SAFETY: the streams outlive the calls.
        unsafe {
            let Some(s) = stream.as_com_ref::<IBStream>() else { return false };
            if self.component.setState(s.as_ptr()) != kResultOk {
                return false;
            }
        }
        self.sync_controller();
        if let Some(c) = &self.controller
            && !controller.is_empty()
        {
            let stream = stream_with(controller);
            // SAFETY: as above.
            unsafe {
                if let Some(s) = stream.as_com_ref::<IBStream>() {
                    c.setState(s.as_ptr());
                }
            }
        }
        true
    }

    pub fn latency(&self) -> u32 {
        // SAFETY: a plain query on an active processor.
        unsafe { self.processor.getLatencySamples() }
    }

    pub fn params(&self) -> Vec<ParamDesc> {
        let Some(c) = &self.controller else { return Vec::new() };
        // SAFETY: plain queries with structs to fill.
        unsafe {
            (0..c.getParameterCount())
                .filter_map(|i| {
                    let mut info: ParameterInfo = std::mem::zeroed();
                    (c.getParameterInfo(i, &mut info) == kResultOk).then(|| ParamDesc {
                        id: info.id,
                        name: from_wide(&info.title),
                        default: info.defaultNormalizedValue,
                        steps: info.stepCount,
                    })
                })
                .collect()
        }
    }

    /// Normalised value and display text.
    pub fn param(&self, id: u32) -> Option<(f64, String)> {
        let c = self.controller.as_ref()?;
        // SAFETY: plain queries with a buffer to fill.
        unsafe {
            let v = c.getParamNormalized(id);
            let mut text: String128 = [0; 128];
            let shown = if c.getParamStringByValue(id, v, &mut text) == kResultOk {
                from_wide(&text)
            } else {
                format!("{v:.3}")
            };
            Some((v, shown))
        }
    }

    pub fn set_param(&mut self, id: u32, value: f64) {
        if let Some(c) = &self.controller {
            // SAFETY: a plain call on a valid controller.
            unsafe { c.setParamNormalized(id, value) };
        }
        self.changes.push(id, value);
    }

    /// Processes a stereo block in place on the main buses; other inputs get silence. `notes`
    /// are `(offset, on, channel, key, velocity)`; `controls` are other MIDI messages, which
    /// reach the parameters the plugin maps them to.
    pub fn process(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        notes: &[(i32, bool, u8, u8, u8)],
        controls: &[(i32, [u8; 3])],
        position: i64,
        bpm: f64,
    ) {
        let n = left.len().min(right.len()).min(self.block);
        for (c, buf) in self.inputs.iter_mut().enumerate() {
            match c {
                0 => buf[..n].copy_from_slice(&left[..n]),
                1 => buf[..n].copy_from_slice(&right[..n]),
                _ => buf[..n].fill(0.0),
            }
        }
        self.run(n, notes, controls, position, bpm);
        if let Some(l) = self.outputs.first() {
            left[..n].copy_from_slice(&l[..n]);
            right[..n].copy_from_slice(&self.outputs.get(1).unwrap_or(l)[..n]);
        }
    }

    /// Processes every channel of every bus: input channel k comes from `bufs[k]`, output
    /// channel k goes to it, over frames `range`.
    pub fn process_channels(
        &mut self,
        bufs: &mut [Vec<f32>],
        range: std::ops::Range<usize>,
        notes: &[(i32, bool, u8, u8, u8)],
        controls: &[(i32, [u8; 3])],
        position: i64,
        bpm: f64,
    ) {
        let n = range.len().min(self.block);
        for (c, buf) in self.inputs.iter_mut().enumerate() {
            match bufs.get(c) {
                Some(src) => buf[..n].copy_from_slice(&src[range.start..range.start + n]),
                None => buf[..n].fill(0.0),
            }
        }
        self.run(n, notes, controls, position, bpm);
        for (c, out) in self.outputs.iter().enumerate() {
            if let Some(dst) = bufs.get_mut(c) {
                dst[range.start..range.start + n].copy_from_slice(&out[..n]);
            }
        }
    }

    fn run(
        &mut self,
        n: usize,
        notes: &[(i32, bool, u8, u8, u8)],
        controls: &[(i32, [u8; 3])],
        position: i64,
        bpm: f64,
    ) {
        for (edit_id, value) in self.handler.edits.borrow_mut().drain(..) {
            self.changes.push(edit_id, value);
        }
        for &(offset, bytes) in controls {
            if let Some((channel, ctrl, value)) = midi_controller(bytes)
                && let Some(id) = self.midi_map[channel as usize * MIDI_CONTROLLERS + ctrl]
            {
                self.changes.push_at(id, offset, value);
            }
        }
        {
            let mut events = self.events.events.borrow_mut();
            events.clear();
            for &(offset, on, channel, key, velocity) in notes {
                // SAFETY: an all-zero Event is valid; the matching union member is filled in.
                let mut e: Event = unsafe { std::mem::zeroed() };
                e.sampleOffset = offset;
                if on {
                    e.r#type = Event_::EventTypes_::kNoteOnEvent as u16;
                    e.__field0.noteOn = NoteOnEvent {
                        channel: channel as int16,
                        pitch: key as int16,
                        tuning: 0.0,
                        velocity: velocity as f32 / 127.0,
                        length: 0,
                        noteId: -1,
                    };
                } else {
                    e.r#type = Event_::EventTypes_::kNoteOffEvent as u16;
                    e.__field0.noteOff = NoteOffEvent {
                        channel: channel as int16,
                        pitch: key as int16,
                        velocity: 0.0,
                        noteId: -1,
                        tuning: 0.0,
                    };
                }
                events.push(e);
            }
            for (offset, data) in &self.sysex {
                // SAFETY: as above; the bytes live in `self.sysex` until after `process`.
                let mut e: Event = unsafe { std::mem::zeroed() };
                e.sampleOffset = *offset as int32;
                e.r#type = Event_::EventTypes_::kDataEvent as u16;
                e.__field0.data = DataEvent {
                    size: data.len() as uint32,
                    r#type: DataEvent_::DataTypes_::kMidiSysEx as uint32,
                    bytes: data.as_ptr(),
                };
                events.push(e);
            }
            events.sort_by_key(|e| e.sampleOffset);
        }
        // SAFETY: an all-zero ProcessContext is valid; the used fields are set below.
        let mut context: ProcessContext = unsafe { std::mem::zeroed() };
        context.state =
            (ProcessContext_::StatesAndFlags_::kPlaying | ProcessContext_::StatesAndFlags_::kTempoValid) as uint32;
        context.sampleRate = self.sample_rate;
        context.projectTimeSamples = position;
        context.tempo = bpm;
        let mut data = ProcessData {
            processMode: ProcessModes_::kRealtime as int32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as int32,
            numSamples: n as int32,
            numInputs: self.in_buses.len() as int32,
            numOutputs: self.out_buses.len() as int32,
            inputs: if self.in_buses.is_empty() { std::ptr::null_mut() } else { self.in_buses.as_mut_ptr() },
            outputs: if self.out_buses.is_empty() { std::ptr::null_mut() } else { self.out_buses.as_mut_ptr() },
            inputParameterChanges: self
                .changes
                .as_com_ref::<IParameterChanges>()
                .map_or(std::ptr::null_mut(), |r| r.as_ptr()),
            outputParameterChanges: self
                .out_changes
                .as_com_ref::<IParameterChanges>()
                .map_or(std::ptr::null_mut(), |r| r.as_ptr()),
            inputEvents: self.events.as_com_ref::<IEventList>().map_or(std::ptr::null_mut(), |r| r.as_ptr()),
            outputEvents: std::ptr::null_mut(),
            processContext: &mut context,
        };
        // SAFETY: every buffer and object referenced by `data` lives through the call.
        unsafe { self.processor.process(&mut data) };
        self.changes.clear();
        self.out_changes.clear();
        self.sysex.clear();
    }

    pub fn reset(&mut self) {
        // SAFETY: toggling processing is how VST 3 hosts flush a plugin.
        unsafe {
            self.processor.setProcessing(0);
            self.component.setActive(0);
            self.component.setActive(1);
            self.processor.setProcessing(1);
        }
    }

    pub fn controller(&self) -> Option<ComRef<'_, IEditController>> {
        self.controller.as_ref().map(ComPtr::as_com_ref)
    }
}

impl Drop for Vst3Plugin {
    fn drop(&mut self) {
        // SAFETY: shutting down in the reverse order of setup.
        unsafe {
            self.processor.setProcessing(0);
            self.component.setActive(0);
            if let Some(c) = &self.controller {
                if let (Some(a), Some(b)) = (self.component.cast::<IConnectionPoint>(), c.cast::<IConnectionPoint>())
                    && a.as_ptr() as *mut c_void != b.as_ptr() as *mut c_void
                {
                    a.disconnect(b.as_ptr());
                    b.disconnect(a.as_ptr());
                }
                c.terminate();
            }
            self.component.terminate();
        }
    }
}

// ---- editors -----------------------------------------------------------------------------

/// The window side of a plugin view: size requests, plus the Linux run loop through which the
/// view registers its file descriptors and timers.
#[derive(Default)]
struct Frame {
    resize: RefCell<Option<(u32, u32)>>,
    handlers: RefCell<Vec<(ComPtr<Linux::IEventHandler>, i32)>>,
    timers: RefCell<Vec<(ComPtr<Linux::ITimerHandler>, u32)>>,
    schedule: RefCell<crate::editor::Timers>,
}

impl Class for Frame {
    type Interfaces = (IPlugFrame, Linux::IRunLoop);
}

impl IPlugFrameTrait for Frame {
    unsafe fn resizeView(&self, view: *mut IPlugView, size: *mut ViewRect) -> tresult {
        // SAFETY: the view passes itself and its new rectangle.
        unsafe {
            let r = &*size;
            *self.resize.borrow_mut() = Some(((r.right - r.left).max(1) as u32, (r.bottom - r.top).max(1) as u32));
            if let Some(view) = ComRef::from_raw(view) {
                view.onSize(size);
            }
        }
        kResultOk
    }
}

impl IRunLoopTrait for Frame {
    unsafe fn registerEventHandler(&self, handler: *mut Linux::IEventHandler, fd: Linux::FileDescriptor) -> tresult {
        // SAFETY: the handler is a live object; a ComRef converts to an owned reference.
        match unsafe { ComRef::from_raw(handler) } {
            Some(h) => {
                self.handlers.borrow_mut().push((h.to_com_ptr(), fd));
                kResultOk
            }
            None => kInvalidArgument,
        }
    }
    unsafe fn unregisterEventHandler(&self, handler: *mut Linux::IEventHandler) -> tresult {
        self.handlers.borrow_mut().retain(|(h, _)| h.as_ptr() != handler);
        kResultOk
    }
    unsafe fn registerTimer(&self, handler: *mut Linux::ITimerHandler, ms: Linux::TimerInterval) -> tresult {
        // SAFETY: as for event handlers.
        match unsafe { ComRef::from_raw(handler) } {
            Some(h) => {
                let id = self.schedule.borrow_mut().add(std::time::Duration::from_millis(ms));
                self.timers.borrow_mut().push((h.to_com_ptr(), id));
                kResultOk
            }
            None => kInvalidArgument,
        }
    }
    unsafe fn unregisterTimer(&self, handler: *mut Linux::ITimerHandler) -> tresult {
        let mut timers = self.timers.borrow_mut();
        for (_, id) in timers.iter().filter(|(h, _)| h.as_ptr() == handler) {
            self.schedule.borrow_mut().remove(*id);
        }
        timers.retain(|(h, _)| h.as_ptr() != handler);
        kResultOk
    }
}

/// A VST 3 plugin view embedded in an X11 window.
pub struct Vst3Editor {
    view: ComPtr<IPlugView>,
    frame: ComWrapper<Frame>,
    attached: bool,
    _module: Arc<Vst3Module>,
}

#[cfg(windows)]
const NATIVE_PLATFORM: FIDString = kPlatformTypeHWND;
#[cfg(target_os = "macos")]
const NATIVE_PLATFORM: FIDString = kPlatformTypeNSView;
#[cfg(not(any(windows, target_os = "macos")))]
const NATIVE_PLATFORM: FIDString = kPlatformTypeX11EmbedWindowID;

impl Vst3Plugin {
    /// The plugin's editor view, when it has one. It is used without any lock around `self`;
    /// keep the plugin alive while the editor is in use.
    pub fn editor(&self) -> Option<Vst3Editor> {
        let controller = self.controller.as_ref()?;
        // SAFETY: createView returns null or a new reference to a view.
        let view = unsafe { ComPtr::from_raw(controller.createView(ViewType::kEditor)) }?;
        // SAFETY: a plain query on a valid view.
        if unsafe { view.isPlatformTypeSupported(NATIVE_PLATFORM) } != kResultTrue {
            return None;
        }
        Some(Vst3Editor {
            view,
            frame: ComWrapper::new(Frame::default()),
            attached: false,
            _module: self._module.clone(),
        })
    }
}

impl crate::editor::Editor for Vst3Editor {
    fn open(&mut self, parent: usize) -> Result<(u32, u32), String> {
        // SAFETY: the frame outlives the view's use of it; `parent` is a native window.
        unsafe {
            if let Some(frame) = self.frame.as_com_ref::<IPlugFrame>() {
                self.view.setFrame(frame.as_ptr());
            }
            if self.view.attached(parent as *mut c_void, NATIVE_PLATFORM) != kResultOk {
                return Err("the editor could not be attached".into());
            }
            self.attached = true;
            let mut rect = ViewRect { left: 0, top: 0, right: 640, bottom: 480 };
            self.view.getSize(&mut rect);
            Ok(((rect.right - rect.left).max(1) as u32, (rect.bottom - rect.top).max(1) as u32))
        }
    }

    fn idle(&mut self) -> Option<(u32, u32)> {
        let fds: Vec<i32> = self.frame.handlers.borrow().iter().map(|(_, fd)| *fd).collect();
        for (fd, _) in crate::editor::ready_fds(&fds) {
            let handlers: Vec<ComPtr<Linux::IEventHandler>> =
                self.frame.handlers.borrow().iter().filter(|(_, f)| *f == fd).map(|(h, _)| h.clone()).collect();
            for h in handlers {
                // SAFETY: a main-thread callback for a descriptor the view registered.
                unsafe { h.onFDIsSet(fd) };
            }
        }
        let due = self.frame.schedule.borrow_mut().due();
        let timers: Vec<ComPtr<Linux::ITimerHandler>> =
            self.frame.timers.borrow().iter().filter(|(_, id)| due.contains(id)).map(|(h, _)| h.clone()).collect();
        for t in timers {
            // SAFETY: a main-thread callback for a timer the view registered.
            unsafe { t.onTimer() };
        }
        self.frame.resize.borrow_mut().take()
    }

    fn set_scale(&mut self, scale: f64) {
        if let Some(support) = self.view.cast::<IPlugViewContentScaleSupport>() {
            // SAFETY: a plain call on a valid view.
            unsafe { support.setContentScaleFactor(scale as f32) };
        }
    }

    fn close(&mut self) {
        if std::mem::take(&mut self.attached) {
            // SAFETY: detaching the view attached in `open`.
            unsafe {
                self.view.removed();
                self.view.setFrame(std::ptr::null_mut());
            }
        }
        self.frame.handlers.borrow_mut().clear();
        self.frame.timers.borrow_mut().clear();
    }
}

impl Drop for Vst3Editor {
    fn drop(&mut self) {
        crate::editor::Editor::close(self);
    }
}
