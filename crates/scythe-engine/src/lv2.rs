//! LV2 plugin hosting: bundle discovery from the Turtle descriptions, instantiation with the
//! common host features (URID map/unmap, options, bounded block length, worker), audio and
//! control ports, MIDI through atom sequences, and state in REAPER's chunk layout.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::turtle::{self, Graph, Term};

const LV2: &str = "http://lv2plug.in/ns/lv2core#";
const ATOM: &str = "http://lv2plug.in/ns/ext/atom#";
const UI: &str = "http://lv2plug.in/ns/extensions/ui#";
const MIDI_EVENT: &str = "http://lv2plug.in/ns/ext/midi#MidiEvent";
const RDFS_SEE_ALSO: &str = "http://www.w3.org/2000/01/rdf-schema#seeAlso";
const DOAP_NAME: &str = "http://usefulinc.com/ns/doap#name";
const URID_MAP: &str = "http://lv2plug.in/ns/ext/urid#map";
const URID_UNMAP: &str = "http://lv2plug.in/ns/ext/urid#unmap";
const OPTIONS: &str = "http://lv2plug.in/ns/ext/options#options";
const BOUNDED_BLOCK: &str = "http://lv2plug.in/ns/ext/buf-size#boundedBlockLength";
const MAX_BLOCK: &str = "http://lv2plug.in/ns/ext/buf-size#maxBlockLength";
const NOMINAL_BLOCK: &str = "http://lv2plug.in/ns/ext/buf-size#nominalBlockLength";
const SAMPLE_RATE: &str = "http://lv2plug.in/ns/ext/parameters#sampleRate";
const WORKER_SCHEDULE: &str = "http://lv2plug.in/ns/ext/worker#schedule";
const WORKER_INTERFACE: &str = "http://lv2plug.in/ns/ext/worker#interface";
const STATE_INTERFACE: &str = "http://lv2plug.in/ns/ext/state#interface";
const ATOM_STRING: &str = "http://lv2plug.in/ns/ext/atom#String";
const ATOM_PATH: &str = "http://lv2plug.in/ns/ext/atom#Path";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortKind {
    Audio,
    Control,
    Atom,
    Cv,
}

#[derive(Debug, Clone)]
pub struct PortInfo {
    pub index: u32,
    pub symbol: String,
    pub name: String,
    pub kind: PortKind,
    pub input: bool,
    pub default: f32,
    pub min: f32,
    pub max: f32,
    pub toggled: bool,
    pub integer: bool,
    pub logarithmic: bool,
    /// An atom port that carries MIDI.
    pub midi: bool,
    /// Bytes the plugin asks for in an atom buffer.
    pub min_size: usize,
}

#[derive(Debug, Clone)]
pub struct Lv2Info {
    pub uri: String,
    pub name: String,
    pub bundle: PathBuf,
    pub binary: PathBuf,
    pub ports: Vec<PortInfo>,
    pub instrument: bool,
    /// An X11 editor (`ui:X11UI`): its URI and binary.
    pub ui: Option<(String, PathBuf)>,
}

/// Directories LV2 bundles are looked up in: `$LV2_PATH`, then the usual user and system
/// locations.
pub fn search_paths() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> =
        std::env::var_os("LV2_PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.extend([home.join(".lv2"), home.join(".nix-profile/lib/lv2")]);
    }
    if let Some(user) = std::env::var_os("USER") {
        dirs.push(Path::new("/etc/profiles/per-user").join(user).join("lib/lv2"));
    }
    dirs.extend(["/usr/lib/lv2", "/usr/local/lib/lv2", "/run/current-system/sw/lib/lv2"].map(PathBuf::from));
    dirs
}

/// Every plugin in the bundles under `dirs`.
pub fn scan(dirs: &[PathBuf]) -> Vec<Lv2Info> {
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        let mut bundles: Vec<PathBuf> =
            entries.flatten().map(|e| e.path()).filter(|p| p.join("manifest.ttl").is_file()).collect();
        bundles.sort();
        for bundle in bundles {
            match load_bundle(&bundle) {
                Ok(mut plugins) => out.append(&mut plugins),
                Err(e) => eprintln!("lv2: {}: {e}", bundle.display()),
            }
        }
    }
    out
}

fn file_iri(path: &Path) -> String {
    format!("file://{}", path.display().to_string().replace(' ', "%20"))
}

fn iri_path(iri: &str) -> Option<PathBuf> {
    Some(PathBuf::from(iri.strip_prefix("file://")?.replace("%20", " ")))
}

/// Reads a bundle's manifest and the files it points to.
pub fn load_bundle(bundle: &Path) -> Result<Vec<Lv2Info>, String> {
    let manifest = bundle.join("manifest.ttl");
    let text = std::fs::read_to_string(&manifest).map_err(|e| e.to_string())?;
    let mut graph = turtle::parse(&text, &file_iri(&manifest))?;
    let extra: Vec<String> = graph
        .triples
        .iter()
        .filter(|(_, p, _)| p == RDFS_SEE_ALSO)
        .filter_map(|(_, _, o)| o.iri().map(str::to_owned))
        .collect();
    let mut seen = Vec::new();
    for iri in extra {
        if seen.contains(&iri) {
            continue;
        }
        let Some(path) = iri_path(&iri) else { continue };
        if let Ok(text) = std::fs::read_to_string(&path) {
            let mut more = turtle::parse(&text, &iri)?;
            // Blank nodes are numbered per file; shift them so files do not collide.
            let offset = graph.triples.len() * 4 + 1_000_000;
            let shift = |t: &mut Term| {
                if let Term::Blank(b) = t {
                    *b += offset;
                }
            };
            for (s, _, o) in &mut more.triples {
                shift(s);
                shift(o);
            }
            graph.triples.append(&mut more.triples);
        }
        seen.push(iri);
    }
    let plugin_class = format!("{LV2}Plugin");
    let mut plugins: Vec<Term> = Vec::new();
    for p in graph.subjects(turtle::RDF_TYPE, &Term::Iri(plugin_class)) {
        if !plugins.contains(p) {
            plugins.push(p.clone());
        }
    }
    Ok(plugins.iter().filter_map(|p| plugin_info(&graph, p, bundle)).collect())
}

fn plugin_info(g: &Graph, plugin: &Term, bundle: &Path) -> Option<Lv2Info> {
    let uri = plugin.iri()?.to_owned();
    let binary = iri_path(g.object(plugin, &format!("{LV2}binary"))?.iri()?)?;
    let name = g.object(plugin, DOAP_NAME).and_then(Term::literal).unwrap_or(&uri).to_owned();
    let lv2 = |s: &str| format!("{LV2}{s}");
    let mut ports: Vec<PortInfo> = g
        .objects(plugin, &lv2("port"))
        .filter_map(|port| {
            let num = |p: &str| g.object(port, p).and_then(Term::number);
            let kind = if g.has_type(port, &lv2("AudioPort")) {
                PortKind::Audio
            } else if g.has_type(port, &lv2("ControlPort")) {
                PortKind::Control
            } else if g.has_type(port, &format!("{ATOM}AtomPort")) {
                PortKind::Atom
            } else if g.has_type(port, &lv2("CVPort")) {
                PortKind::Cv
            } else {
                return None;
            };
            let prop_key = lv2("portProperty");
            let props: Vec<&str> = g.objects(port, &prop_key).filter_map(Term::iri).collect();
            let symbol = g.object(port, &lv2("symbol")).and_then(Term::literal).unwrap_or_default().to_owned();
            let default = num(&lv2("default")).unwrap_or(0.0) as f32;
            Some(PortInfo {
                index: num(&lv2("index"))? as u32,
                name: g.object(port, &lv2("name")).and_then(Term::literal).unwrap_or(&symbol).to_owned(),
                symbol,
                kind,
                input: g.has_type(port, &lv2("InputPort")),
                default,
                min: num(&lv2("minimum")).unwrap_or(0.0) as f32,
                max: num(&lv2("maximum")).unwrap_or(1.0) as f32,
                toggled: props.contains(&lv2("toggled").as_str()),
                integer: props.contains(&lv2("integer").as_str()),
                logarithmic: props.contains(&"http://lv2plug.in/ns/ext/port-props#logarithmic"),
                midi: g.objects(port, &format!("{ATOM}supports")).any(|o| o.iri() == Some(MIDI_EVENT)),
                min_size: num("http://lv2plug.in/ns/ext/resize-port#minimumSize").unwrap_or(0.0) as usize,
            })
        })
        .collect();
    ports.sort_by_key(|p| p.index);
    let midi_in = ports.iter().any(|p| p.kind == PortKind::Atom && p.input && p.midi);
    let audio_in = ports.iter().any(|p| p.kind == PortKind::Audio && p.input);
    let instrument = g.has_type(plugin, &lv2("InstrumentPlugin")) || (midi_in && !audio_in);
    let ui = g
        .objects(plugin, &format!("{UI}ui"))
        .filter(|u| g.has_type(u, &format!("{UI}X11UI")))
        .find_map(|u| Some((u.iri()?.to_owned(), iri_path(g.object(u, &format!("{UI}binary"))?.iri()?)?)));
    Some(Lv2Info { uri, name, bundle: bundle.to_owned(), binary, ports, instrument, ui })
}

// ---- C ABI ---------------------------------------------------------------------------------

#[repr(C)]
struct Feature {
    uri: *const c_char,
    data: *mut c_void,
}

#[repr(C)]
struct Descriptor {
    uri: *const c_char,
    instantiate:
        Option<unsafe extern "C" fn(*const Descriptor, f64, *const c_char, *const *const Feature) -> *mut c_void>,
    connect_port: Option<unsafe extern "C" fn(*mut c_void, u32, *mut c_void)>,
    activate: Option<unsafe extern "C" fn(*mut c_void)>,
    run: Option<unsafe extern "C" fn(*mut c_void, u32)>,
    deactivate: Option<unsafe extern "C" fn(*mut c_void)>,
    cleanup: Option<unsafe extern "C" fn(*mut c_void)>,
    extension_data: Option<unsafe extern "C" fn(*const c_char) -> *const c_void>,
}

#[repr(C)]
struct UridMap {
    handle: *mut c_void,
    map: unsafe extern "C" fn(*mut c_void, *const c_char) -> u32,
}

#[repr(C)]
struct UridUnmap {
    handle: *mut c_void,
    unmap: unsafe extern "C" fn(*mut c_void, u32) -> *const c_char,
}

#[repr(C)]
struct OptionsOption {
    context: u32,
    subject: u32,
    key: u32,
    size: u32,
    kind: u32,
    value: *const c_void,
}

type Respond = unsafe extern "C" fn(*mut c_void, u32, *const c_void) -> u32;

#[repr(C)]
struct WorkerSchedule {
    handle: *mut c_void,
    schedule_work: unsafe extern "C" fn(*mut c_void, u32, *const c_void) -> u32,
}

#[repr(C)]
struct WorkerInterface {
    work: Option<unsafe extern "C" fn(*mut c_void, Respond, *mut c_void, u32, *const c_void) -> u32>,
    work_response: Option<unsafe extern "C" fn(*mut c_void, u32, *const c_void) -> u32>,
    end_run: Option<unsafe extern "C" fn(*mut c_void) -> u32>,
}

type StoreFn = unsafe extern "C" fn(*mut c_void, u32, *const c_void, usize, u32, u32) -> u32;
type RetrieveFn = unsafe extern "C" fn(*mut c_void, u32, *mut usize, *mut u32, *mut u32) -> *const c_void;

#[repr(C)]
struct StateInterface {
    save: Option<unsafe extern "C" fn(*mut c_void, StoreFn, *mut c_void, u32, *const *const Feature) -> u32>,
    restore: Option<unsafe extern "C" fn(*mut c_void, RetrieveFn, *mut c_void, u32, *const *const Feature) -> u32>,
}

/// URIs mapped to numbers, shared by every plugin so they agree on the ids.
#[derive(Default)]
struct Urids {
    ids: HashMap<String, u32>,
    uris: Vec<CString>,
}

fn urids() -> &'static Mutex<Urids> {
    static URIDS: std::sync::OnceLock<Mutex<Urids>> = std::sync::OnceLock::new();
    URIDS.get_or_init(Default::default)
}

pub fn map(uri: &str) -> u32 {
    let mut u = urids().lock().unwrap();
    if let Some(&id) = u.ids.get(uri) {
        return id;
    }
    u.uris.push(CString::new(uri).unwrap_or_default());
    let id = u.uris.len() as u32;
    u.ids.insert(uri.to_owned(), id);
    id
}

pub fn unmap(id: u32) -> Option<String> {
    let u = urids().lock().unwrap();
    u.uris.get(id.checked_sub(1)? as usize).map(|c| c.to_string_lossy().into_owned())
}

unsafe extern "C" fn map_cb(_handle: *mut c_void, uri: *const c_char) -> u32 {
    // SAFETY: plugins pass a NUL-terminated URI.
    map(&unsafe { CStr::from_ptr(uri) }.to_string_lossy())
}

unsafe extern "C" fn unmap_cb(_handle: *mut c_void, id: u32) -> *const c_char {
    let u = urids().lock().unwrap();
    // The CString's heap buffer stays put for the life of the process.
    id.checked_sub(1).and_then(|i| u.uris.get(i as usize)).map_or(std::ptr::null(), |c| c.as_ptr())
}

#[derive(Default)]
struct Worker {
    requests: Vec<Vec<u8>>,
    responses: Vec<Vec<u8>>,
}

unsafe extern "C" fn schedule_cb(handle: *mut c_void, size: u32, data: *const c_void) -> u32 {
    // SAFETY: handle is the plugin's boxed Worker; data holds `size` bytes.
    unsafe {
        let worker = &mut *(handle as *mut Worker);
        worker.requests.push(std::slice::from_raw_parts(data as *const u8, size as usize).to_vec());
    }
    0
}

unsafe extern "C" fn respond_cb(handle: *mut c_void, size: u32, data: *const c_void) -> u32 {
    // SAFETY: as for `schedule_cb`.
    unsafe {
        let worker = &mut *(handle as *mut Worker);
        worker.responses.push(std::slice::from_raw_parts(data as *const u8, size as usize).to_vec());
    }
    0
}

/// One saved state property: key URI, type URI, flags and the value bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct StateEntry {
    pub key: String,
    pub kind: String,
    pub flags: u32,
    pub value: Vec<u8>,
}

unsafe extern "C" fn store_cb(
    handle: *mut c_void,
    key: u32,
    value: *const c_void,
    size: usize,
    kind: u32,
    flags: u32,
) -> u32 {
    // SAFETY: handle is the Vec being filled; value holds `size` bytes.
    unsafe {
        let out = &mut *(handle as *mut Vec<StateEntry>);
        out.push(StateEntry {
            key: unmap(key).unwrap_or_default(),
            kind: unmap(kind).unwrap_or_default(),
            flags,
            value: std::slice::from_raw_parts(value as *const u8, size).to_vec(),
        });
    }
    0
}

unsafe extern "C" fn retrieve_cb(
    handle: *mut c_void,
    key: u32,
    size: *mut usize,
    kind: *mut u32,
    flags: *mut u32,
) -> *const c_void {
    // SAFETY: handle is the entries slice; the out pointers are valid or null.
    unsafe {
        let entries = &*(handle as *const Vec<StateEntry>);
        let Some(key) = unmap(key) else { return std::ptr::null() };
        let Some(e) = entries.iter().find(|e| e.key == key) else { return std::ptr::null() };
        if !size.is_null() {
            *size = e.value.len();
        }
        if !kind.is_null() {
            *kind = map(&e.kind);
        }
        if !flags.is_null() {
            *flags = e.flags;
        }
        e.value.as_ptr() as *const c_void
    }
}

// ---- instance ------------------------------------------------------------------------------

/// The host side of an instance's features; boxed so the pointers handed out stay valid.
struct Features {
    _uris: Vec<CString>,
    _map: Box<UridMap>,
    _unmap: Box<UridUnmap>,
    _options: Vec<OptionsOption>,
    _option_values: Box<(i32, f32)>,
    _schedule: Box<WorkerSchedule>,
    _list: Vec<Feature>,
    ptrs: Vec<*const Feature>,
}

pub struct Lv2Plugin {
    _lib: Arc<libloading::Library>,
    desc: *const Descriptor,
    handle: *mut c_void,
    pub info: Lv2Info,
    controls: Vec<f32>,
    audio: Vec<Vec<f32>>,
    /// Atom buffers as 8-byte words, per atom port index.
    atoms: Vec<(u32, bool, Vec<u64>)>,
    worker: Box<Worker>,
    features: Features,
    block: usize,
    /// Control changes the editor made, applied before the next block.
    ui_writes: Arc<Mutex<Vec<(u32, f32)>>>,
    /// Control values after the last block, for the editor (f32 bits per port index).
    mirror: Arc<Vec<std::sync::atomic::AtomicU32>>,
    seq_type: u32,
    chunk_type: u32,
    midi_type: u32,
}

// SAFETY: the instance is only ever used by one thread at a time (behind the FX mutex).
unsafe impl Send for Lv2Plugin {}

impl Lv2Plugin {
    pub fn new(info: &Lv2Info, sample_rate: f64, block: usize) -> Result<Self, String> {
        // SAFETY: loading a plugin runs its initialisers; that is inherent to hosting plugins.
        let lib = Arc::new(unsafe { libloading::Library::new(&info.binary) }.map_err(|e| e.to_string())?);
        // SAFETY: `lv2_descriptor` has this signature in every LV2 binary.
        let entry: libloading::Symbol<unsafe extern "C" fn(u32) -> *const Descriptor> =
            unsafe { lib.get(b"lv2_descriptor") }.map_err(|e| e.to_string())?;
        let mut desc = std::ptr::null();
        for i in 0.. {
            // SAFETY: returns null past the last plugin.
            let d = unsafe { entry(i) };
            if d.is_null() {
                break;
            }
            // SAFETY: d is a valid descriptor with a URI.
            if unsafe { CStr::from_ptr((*d).uri) }.to_bytes() == info.uri.as_bytes() {
                desc = d;
                break;
            }
        }
        if desc.is_null() {
            return Err(format!("{} does not provide {}", info.binary.display(), info.uri));
        }

        let mut worker = Box::<Worker>::default();
        let uris: Vec<CString> = [URID_MAP, URID_UNMAP, OPTIONS, BOUNDED_BLOCK, WORKER_SCHEDULE]
            .iter()
            .map(|u| CString::new(*u).unwrap())
            .collect();
        let map_feature = Box::new(UridMap { handle: std::ptr::null_mut(), map: map_cb });
        let unmap_feature = Box::new(UridUnmap { handle: std::ptr::null_mut(), unmap: unmap_cb });
        let values = Box::new((block as i32, sample_rate as f32));
        let int = map(&format!("{ATOM}Int"));
        let float = map(&format!("{ATOM}Float"));
        let option = |key: &str, kind: u32, value: *const c_void, size: u32| OptionsOption {
            context: 0,
            subject: 0,
            key: map(key),
            size,
            kind,
            value,
        };
        let options = vec![
            option(MAX_BLOCK, int, &values.0 as *const i32 as *const c_void, 4),
            option(NOMINAL_BLOCK, int, &values.0 as *const i32 as *const c_void, 4),
            option(SAMPLE_RATE, float, &values.1 as *const f32 as *const c_void, 4),
            OptionsOption { context: 0, subject: 0, key: 0, size: 0, kind: 0, value: std::ptr::null() },
        ];
        let schedule =
            Box::new(WorkerSchedule { handle: &mut *worker as *mut Worker as *mut c_void, schedule_work: schedule_cb });
        let list = vec![
            Feature { uri: uris[0].as_ptr(), data: &*map_feature as *const UridMap as *mut c_void },
            Feature { uri: uris[1].as_ptr(), data: &*unmap_feature as *const UridUnmap as *mut c_void },
            Feature { uri: uris[2].as_ptr(), data: options.as_ptr() as *mut c_void },
            Feature { uri: uris[3].as_ptr(), data: std::ptr::null_mut() },
            Feature { uri: uris[4].as_ptr(), data: &*schedule as *const WorkerSchedule as *mut c_void },
        ];
        let mut ptrs: Vec<*const Feature> = list.iter().map(|f| f as *const Feature).collect();
        ptrs.push(std::ptr::null());
        let features = Features {
            _uris: uris,
            _map: map_feature,
            _unmap: unmap_feature,
            _options: options,
            _option_values: values,
            _schedule: schedule,
            _list: list,
            ptrs,
        };

        let bundle = CString::new(format!("{}/", info.bundle.display())).map_err(|e| e.to_string())?;
        // SAFETY: desc is valid; the feature array is NUL-terminated and outlives the instance.
        let handle = unsafe {
            let instantiate = (*desc).instantiate.ok_or("plugin cannot be instantiated")?;
            instantiate(desc, sample_rate, bundle.as_ptr(), features.ptrs.as_ptr())
        };
        if handle.is_null() {
            return Err(format!("{} refused to start", info.name));
        }

        let size = info.ports.iter().map(|p| p.index as usize + 1).max().unwrap_or(0);
        let mut controls = vec![0.0f32; size];
        for p in info.ports.iter().filter(|p| p.kind == PortKind::Control) {
            controls[p.index as usize] = p.default;
        }
        let audio = info
            .ports
            .iter()
            .filter(|p| matches!(p.kind, PortKind::Audio | PortKind::Cv))
            .map(|_| vec![0.0f32; block])
            .collect();
        let atoms = info
            .ports
            .iter()
            .filter(|p| p.kind == PortKind::Atom)
            .map(|p| (p.index, p.input, vec![0u64; p.min_size.max(8192).div_ceil(8)]))
            .collect();
        let mut plugin = Self {
            _lib: lib,
            desc,
            handle,
            info: info.clone(),
            controls,
            audio,
            atoms,
            worker,
            features,
            block,
            ui_writes: Default::default(),
            mirror: Arc::new((0..size).map(|_| std::sync::atomic::AtomicU32::new(0)).collect()),
            seq_type: map(&format!("{ATOM}Sequence")),
            chunk_type: map(&format!("{ATOM}Chunk")),
            midi_type: map(MIDI_EVENT),
        };
        plugin.connect();
        // SAFETY: activate is called once on a fresh instance with its ports connected.
        unsafe {
            if let Some(activate) = (*desc).activate {
                activate(handle);
            }
        }
        Ok(plugin)
    }

    fn connect(&mut self) {
        // SAFETY: every buffer lives as long as the instance and is not reallocated.
        unsafe {
            let Some(connect) = (*self.desc).connect_port else { return };
            let mut audio = self.audio.iter_mut();
            for p in &self.info.ports {
                let ptr: *mut c_void = match p.kind {
                    PortKind::Control => &mut self.controls[p.index as usize] as *mut f32 as *mut c_void,
                    PortKind::Audio | PortKind::Cv => {
                        audio.next().map_or(std::ptr::null_mut(), |b| b.as_mut_ptr() as *mut c_void)
                    }
                    PortKind::Atom => self
                        .atoms
                        .iter_mut()
                        .find(|a| a.0 == p.index)
                        .map_or(std::ptr::null_mut(), |a| a.2.as_mut_ptr() as *mut c_void),
                };
                connect(self.handle, p.index, ptr);
            }
        }
    }

    fn extension<T>(&self, uri: &str) -> Option<&T> {
        let uri = CString::new(uri).ok()?;
        // SAFETY: extension_data returns null or a static struct of the requested interface.
        unsafe { ((*self.desc).extension_data?(uri.as_ptr()) as *const T).as_ref() }
    }

    pub fn control(&self, index: u32) -> Option<f32> {
        self.controls.get(index as usize).copied()
    }

    pub fn set_control(&mut self, index: u32, value: f32) {
        if let Some(c) = self.controls.get_mut(index as usize) {
            *c = value;
        }
    }

    /// Sets an input control by its port symbol (REAPER stores controls that way).
    pub fn set_control_by_symbol(&mut self, symbol: &str, value: f32) -> bool {
        let Some(index) =
            self.info.ports.iter().find(|p| p.symbol == symbol && p.kind == PortKind::Control).map(|p| p.index)
        else {
            return false;
        };
        self.set_control(index, value);
        true
    }

    /// Runs one block in place on a stereo pair, with MIDI events `(frame, bytes)` for an atom
    /// input that takes MIDI.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32], midi: &[(u32, [u8; 3])]) {
        let n = left.len().min(right.len()).min(self.block);
        let writes = self.ui_writes.clone();
        if let Ok(mut writes) = writes.try_lock() {
            for (port, value) in writes.drain(..) {
                self.set_control(port, value);
            }
        }
        let ports: Vec<(PortKind, bool)> = self.info.ports.iter().map(|p| (p.kind, p.input)).collect();
        let (mut ins, mut outs) = (0, 0);
        for ((kind, input), buf) in
            ports.iter().filter(|(k, _)| matches!(k, PortKind::Audio | PortKind::Cv)).zip(&mut self.audio)
        {
            match (kind, input) {
                (PortKind::Audio, true) => {
                    let src: &[f32] = if ins == 0 { left } else { right };
                    buf[..n].copy_from_slice(&src[..n]);
                    if ins > 1 {
                        buf[..n].fill(0.0);
                    }
                    ins += 1;
                }
                (PortKind::Audio, false) => outs += 1,
                _ => buf[..n].fill(0.0),
            }
        }
        for (index, input, words) in &mut self.atoms {
            let bytes = words.len() * 8;
            let midi_port = self.info.ports.iter().any(|p| p.index == *index && p.midi);
            let buf = words.as_mut_ptr() as *mut u8;
            // SAFETY: the buffer is at least 8 KiB of 8-byte aligned memory.
            unsafe {
                if *input {
                    // An LV2_Atom_Sequence: header, body (unit, pad), then events.
                    let mut at = 16usize;
                    if midi_port {
                        for &(frame, data) in midi {
                            if at + 32 > bytes {
                                break;
                            }
                            (buf.add(at) as *mut i64).write(frame.min(n as u32 - 1) as i64);
                            (buf.add(at + 8) as *mut u32).write(3);
                            (buf.add(at + 12) as *mut u32).write(self.midi_type);
                            std::ptr::copy_nonoverlapping(data.as_ptr(), buf.add(at + 16), 3);
                            at += 16 + 8;
                        }
                    }
                    (buf as *mut u32).write((at - 8) as u32);
                    (buf.add(4) as *mut u32).write(self.seq_type);
                    (buf.add(8) as *mut u64).write(0);
                } else {
                    // Output atom ports announce their capacity before each run.
                    (buf as *mut u32).write((bytes - 8) as u32);
                    (buf.add(4) as *mut u32).write(self.chunk_type);
                }
            }
        }
        // SAFETY: ports are connected and every buffer holds at least `n` frames.
        unsafe {
            if let Some(run) = (*self.desc).run {
                run(self.handle, n as u32);
            }
        }
        self.run_worker();
        for (m, v) in self.mirror.iter().zip(&self.controls) {
            m.store(v.to_bits(), std::sync::atomic::Ordering::Relaxed);
        }
        let mut out_bufs = ports
            .iter()
            .filter(|(k, _)| matches!(k, PortKind::Audio | PortKind::Cv))
            .zip(&self.audio)
            .filter(|((k, input), _)| *k == PortKind::Audio && !input)
            .map(|(_, b)| b);
        match (out_bufs.next(), out_bufs.next()) {
            (Some(l), Some(r)) => {
                left[..n].copy_from_slice(&l[..n]);
                right[..n].copy_from_slice(&r[..n]);
            }
            (Some(mono), None) => {
                left[..n].copy_from_slice(&mono[..n]);
                right[..n].copy_from_slice(&mono[..n]);
            }
            _ if outs == 0 => {}
            _ => {}
        }
    }

    /// Requests a plugin scheduled during `run` are handled right away, on this thread.
    fn run_worker(&mut self) {
        if self.worker.requests.is_empty() {
            return;
        }
        let Some(iface) =
            self.extension::<WorkerInterface>(WORKER_INTERFACE).map(|i| (i.work, i.work_response, i.end_run))
        else {
            self.worker.requests.clear();
            return;
        };
        let requests = std::mem::take(&mut self.worker.requests);
        let handle = &mut *self.worker as *mut Worker as *mut c_void;
        // SAFETY: the worker interface functions take the instance and the data we pass.
        unsafe {
            if let Some(work) = iface.0 {
                for r in &requests {
                    work(self.handle, respond_cb, handle, r.len() as u32, r.as_ptr() as *const c_void);
                }
            }
            let responses = std::mem::take(&mut self.worker.responses);
            if let Some(respond) = iface.1 {
                for r in &responses {
                    respond(self.handle, r.len() as u32, r.as_ptr() as *const c_void);
                }
            }
            if let Some(end) = iface.2 {
                end(self.handle);
            }
        }
    }

    pub fn save_state(&self) -> Vec<StateEntry> {
        let mut out: Vec<StateEntry> = Vec::new();
        if let Some(save) = self.extension::<StateInterface>(STATE_INTERFACE).and_then(|s| s.save) {
            // SAFETY: the store callback appends to `out`, which outlives the call.
            unsafe {
                save(
                    self.handle,
                    store_cb,
                    &mut out as *mut Vec<StateEntry> as *mut c_void,
                    3,
                    self.features.ptrs.as_ptr(),
                )
            };
        }
        out
    }

    pub fn restore_state(&mut self, entries: &[StateEntry]) -> bool {
        let Some(restore) = self.extension::<StateInterface>(STATE_INTERFACE).and_then(|s| s.restore) else {
            return false;
        };
        let entries = entries.to_vec();
        // SAFETY: the retrieve callback reads from `entries`, which outlives the call.
        unsafe {
            restore(
                self.handle,
                retrieve_cb,
                &entries as *const Vec<StateEntry> as *mut c_void,
                3,
                self.features.ptrs.as_ptr(),
            ) == 0
        }
    }
}

impl Drop for Lv2Plugin {
    fn drop(&mut self) {
        // SAFETY: deactivate and cleanup end the instance's life; nothing uses it afterwards.
        unsafe {
            if let Some(deactivate) = (*self.desc).deactivate {
                deactivate(self.handle);
            }
            if let Some(cleanup) = (*self.desc).cleanup {
                cleanup(self.handle);
            }
        }
    }
}

// ---- REAPER's chunk layout -----------------------------------------------------------------

/// Reads the `<STATE` children of an `<LV2` slot: `P key "path" flags` for paths, and
/// `<S key flags` blocks whose lines are `n|text` (a new line) or ` |text` (continued) for
/// strings. Values gain the terminating NUL that LV2 strings carry.
pub fn parse_reaper_state(state: &scythe_core::rpp::Chunk) -> Vec<StateEntry> {
    use scythe_core::rpp::{Node, tokenize};
    let mut out = Vec::new();
    for node in &state.children {
        match node {
            Node::Line(l) => {
                let t = tokenize(l);
                if t.first().map(String::as_str) == Some("P") && t.len() >= 3 {
                    let mut value = t[2].clone().into_bytes();
                    value.push(0);
                    out.push(StateEntry {
                        key: t[1].clone(),
                        kind: ATOM_PATH.into(),
                        flags: t.get(3).and_then(|f| f.parse().ok()).unwrap_or(3),
                        value,
                    });
                }
            }
            Node::Chunk(c) if c.name() == "S" => {
                let args = c.args();
                let mut text = String::new();
                for (i, n) in c.children.iter().enumerate() {
                    let Node::Line(l) = n else { continue };
                    let l = l.trim_start();
                    let (marker, rest) = match l.split_once('|') {
                        Some((m, r)) => (m, r),
                        None => ("", l),
                    };
                    if marker == "n" && i > 0 {
                        text.push('\n');
                    }
                    text.push_str(rest);
                }
                let mut value = text.into_bytes();
                value.push(0);
                out.push(StateEntry {
                    key: args.first().cloned().unwrap_or_default(),
                    kind: ATOM_STRING.into(),
                    flags: args.get(1).and_then(|f| f.parse().ok()).unwrap_or(3),
                    value,
                });
            }
            _ => {}
        }
    }
    out
}

/// Writes state entries back as REAPER's `<STATE` chunk. Types other than strings and paths
/// are kept as base64 in a `<B key type flags` block.
pub fn reaper_state(entries: &[StateEntry]) -> scythe_core::rpp::Chunk {
    use base64::Engine as _;
    use scythe_core::rpp::Chunk;
    let mut state = Chunk::new("STATE", &[]);
    let text = |e: &StateEntry| String::from_utf8_lossy(e.value.strip_suffix(&[0]).unwrap_or(&e.value)).into_owned();
    for e in entries {
        if e.kind == ATOM_PATH {
            state.line(["P".to_owned(), e.key.clone(), text(e), e.flags.to_string()]);
        } else if e.kind == ATOM_STRING {
            let mut s = Chunk::new("S", &[e.key.as_str(), &e.flags.to_string()]);
            for (i, line) in text(e).split('\n').enumerate() {
                let mut chars: Vec<char> = line.chars().collect();
                let mut first = true;
                // Long lines continue on the next row so rows stay readable.
                loop {
                    let take = chars.len().min(1024);
                    let piece: String = chars.drain(..take).collect();
                    let marker = if first && i > 0 { "n" } else { " " };
                    s.children.push(scythe_core::rpp::Node::Line(format!("{marker}|{piece}")));
                    first = false;
                    if chars.is_empty() {
                        break;
                    }
                }
            }
            state.push_chunk(s);
        } else {
            let mut b = Chunk::new("B", &[e.key.as_str(), e.kind.as_str(), &e.flags.to_string()]);
            let encoded = base64::engine::general_purpose::STANDARD.encode(&e.value);
            for piece in encoded.as_bytes().chunks(128) {
                b.children.push(scythe_core::rpp::Node::Line(String::from_utf8_lossy(piece).into_owned()));
            }
            state.push_chunk(b);
        }
    }
    state
}

// ---- editors -----------------------------------------------------------------------------

const UI_PARENT: &str = "http://lv2plug.in/ns/extensions/ui#parent";
const UI_RESIZE: &str = "http://lv2plug.in/ns/extensions/ui#resize";
const UI_IDLE: &str = "http://lv2plug.in/ns/extensions/ui#idleInterface";
const INSTANCE_ACCESS: &str = "http://lv2plug.in/ns/ext/instance-access";

type WriteFn = unsafe extern "C" fn(*mut c_void, u32, u32, u32, *const c_void);

#[repr(C)]
struct UiDescriptor {
    uri: *const c_char,
    instantiate: Option<
        unsafe extern "C" fn(
            *const UiDescriptor,
            *const c_char,
            *const c_char,
            WriteFn,
            *mut c_void,
            *mut *mut c_void,
            *const *const Feature,
        ) -> *mut c_void,
    >,
    cleanup: Option<unsafe extern "C" fn(*mut c_void)>,
    port_event: Option<unsafe extern "C" fn(*mut c_void, u32, u32, u32, *const c_void)>,
    extension_data: Option<unsafe extern "C" fn(*const c_char) -> *const c_void>,
}

#[repr(C)]
struct UiResize {
    handle: *mut c_void,
    ui_resize: unsafe extern "C" fn(*mut c_void, i32, i32) -> i32,
}

#[repr(C)]
struct UiIdle {
    idle: Option<unsafe extern "C" fn(*mut c_void) -> i32>,
}

/// What the UI's callbacks reach: its control writes and its size requests.
#[derive(Default)]
struct UiShared {
    writes: Arc<Mutex<Vec<(u32, f32)>>>,
    resize: Mutex<Option<(u32, u32)>>,
}

unsafe extern "C" fn ui_write(controller: *mut c_void, port: u32, size: u32, protocol: u32, buffer: *const c_void) {
    // Protocol 0 is a float control value; atom transfers are not passed on.
    if protocol != 0 || size != 4 || buffer.is_null() {
        return;
    }
    // SAFETY: controller is the editor's UiShared; buffer holds one f32.
    unsafe {
        let shared = &*(controller as *const UiShared);
        shared.writes.lock().unwrap().push((port, *(buffer as *const f32)));
    }
}

unsafe extern "C" fn ui_resize(handle: *mut c_void, width: i32, height: i32) -> i32 {
    // SAFETY: handle is the editor's UiShared.
    let shared = unsafe { &*(handle as *const UiShared) };
    *shared.resize.lock().unwrap() = Some((width.max(1) as u32, height.max(1) as u32));
    0
}

/// An LV2 plugin's X11 UI, embedded through `ui:parent`.
pub struct Lv2Editor {
    ui: (String, PathBuf),
    plugin_uri: String,
    plugin: *mut c_void,
    shared: Box<UiShared>,
    mirror: Arc<Vec<std::sync::atomic::AtomicU32>>,
    controls: Vec<u32>,
    sent: Vec<u32>,
    lib: Option<libloading::Library>,
    desc: *const UiDescriptor,
    handle: *mut c_void,
    closed: bool,
    _features: Vec<CString>,
}

impl Lv2Plugin {
    /// The plugin's X11 UI, when it has one. The editor talks to the plugin through shared
    /// queues and only reads the instance pointer; keep the plugin alive while it is open.
    pub fn editor(&self) -> Option<Lv2Editor> {
        let ui = self.info.ui.clone()?;
        let controls: Vec<u32> =
            self.info.ports.iter().filter(|p| p.kind == PortKind::Control).map(|p| p.index).collect();
        Some(Lv2Editor {
            ui,
            plugin_uri: self.info.uri.clone(),
            plugin: self.handle,
            shared: Box::new(UiShared { writes: self.ui_writes.clone(), resize: Mutex::new(None) }),
            mirror: self.mirror.clone(),
            sent: vec![u32::MAX; controls.len()],
            controls,
            lib: None,
            desc: std::ptr::null(),
            handle: std::ptr::null_mut(),
            closed: false,
            _features: Vec::new(),
        })
    }
}

impl Lv2Editor {
    /// Tells the UI about control values that changed since it last heard.
    fn send_controls(&mut self) {
        // SAFETY: desc and handle are valid while the UI is open.
        let Some(port_event) = (unsafe { self.desc.as_ref() }).and_then(|d| d.port_event) else { return };
        for (i, &port) in self.controls.iter().enumerate() {
            let bits = self.mirror.get(port as usize).map_or(0, |m| m.load(std::sync::atomic::Ordering::Relaxed));
            if bits != self.sent[i] {
                self.sent[i] = bits;
                let value = f32::from_bits(bits);
                // SAFETY: format 0 is a float control value.
                unsafe { port_event(self.handle, port, 4, 0, &value as *const f32 as *const c_void) };
            }
        }
    }
}

impl crate::editor::Editor for Lv2Editor {
    fn open(&mut self, parent: u32) -> Result<(u32, u32), String> {
        let (uri, binary) = self.ui.clone();
        // SAFETY: loading a plugin UI runs its initialisers; that is inherent to hosting it.
        let lib = unsafe { libloading::Library::new(&binary) }.map_err(|e| e.to_string())?;
        // SAFETY: `lv2ui_descriptor` has this signature in every LV2 UI binary.
        let entry: libloading::Symbol<unsafe extern "C" fn(u32) -> *const UiDescriptor> =
            unsafe { lib.get(b"lv2ui_descriptor") }.map_err(|e| e.to_string())?;
        let mut desc = std::ptr::null();
        for i in 0.. {
            // SAFETY: returns null past the last UI.
            let d = unsafe { entry(i) };
            // SAFETY: d is null or a valid descriptor with a URI.
            if d.is_null() || unsafe { CStr::from_ptr((*d).uri) }.to_bytes() == uri.as_bytes() {
                desc = d;
                break;
            }
        }
        if desc.is_null() {
            return Err(format!("{} does not provide {uri}", binary.display()));
        }
        let names: Vec<CString> = [URID_MAP, URID_UNMAP, UI_PARENT, UI_RESIZE, UI_IDLE, INSTANCE_ACCESS]
            .iter()
            .map(|u| CString::new(*u).unwrap())
            .collect();
        let map = UridMap { handle: std::ptr::null_mut(), map: map_cb };
        let unmap = UridUnmap { handle: std::ptr::null_mut(), unmap: unmap_cb };
        let shared = &*self.shared as *const UiShared as *mut c_void;
        let resize = UiResize { handle: shared, ui_resize };
        let list = [
            Feature { uri: names[0].as_ptr(), data: &map as *const UridMap as *mut c_void },
            Feature { uri: names[1].as_ptr(), data: &unmap as *const UridUnmap as *mut c_void },
            Feature { uri: names[2].as_ptr(), data: parent as usize as *mut c_void },
            Feature { uri: names[3].as_ptr(), data: &resize as *const UiResize as *mut c_void },
            Feature { uri: names[4].as_ptr(), data: std::ptr::null_mut() },
            Feature { uri: names[5].as_ptr(), data: self.plugin },
        ];
        let mut ptrs: Vec<*const Feature> = list.iter().map(|f| f as *const Feature).collect();
        ptrs.push(std::ptr::null());
        let bundle = binary.parent().map(|p| format!("{}/", p.display())).unwrap_or_default();
        let (bundle, plugin_uri) =
            (CString::new(bundle).unwrap_or_default(), CString::new(self.plugin_uri.as_str()).unwrap_or_default());
        let mut widget = std::ptr::null_mut();
        // SAFETY: the features are only used during instantiate except `parent` (a plain id),
        // the URID callbacks (static) and the resize handle (boxed in self).
        let handle = unsafe {
            let instantiate = (*desc).instantiate.ok_or("the UI cannot be instantiated")?;
            instantiate(desc, plugin_uri.as_ptr(), bundle.as_ptr(), ui_write, shared, &mut widget, ptrs.as_ptr())
        };
        if handle.is_null() {
            return Err("the UI refused to start".into());
        }
        self.lib = Some(lib);
        self.desc = desc;
        self.handle = handle;
        self.send_controls();
        Ok(self.shared.resize.lock().unwrap().take().unwrap_or((640, 480)))
    }

    fn idle(&mut self) -> Option<(u32, u32)> {
        if self.handle.is_null() {
            return None;
        }
        self.send_controls();
        let idle_uri = CString::new(UI_IDLE).unwrap();
        // SAFETY: extension_data returns null or the idle interface.
        let idle =
            unsafe { (*self.desc).extension_data.and_then(|f| (f(idle_uri.as_ptr()) as *const UiIdle).as_ref()) };
        if let Some(f) = idle.and_then(|i| i.idle)
            // SAFETY: an idle call on the UI's own thread.
            && unsafe { f(self.handle) } != 0
        {
            self.closed = true;
        }
        self.shared.resize.lock().unwrap().take()
    }

    fn close(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: ends the UI instantiated in `open`.
            unsafe {
                if let Some(cleanup) = (*self.desc).cleanup {
                    cleanup(self.handle);
                }
            }
            self.handle = std::ptr::null_mut();
        }
    }

    fn closed(&self) -> bool {
        self.closed
    }
}

impl Drop for Lv2Editor {
    fn drop(&mut self) {
        crate::editor::Editor::close(self);
    }
}
