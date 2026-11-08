//! CLAP preset discovery: the providers of a library's preset-discovery factory declare
//! locations (inside the plugin, or preset files and folders), then list each location's presets
//! by name and load key. Presets load through the plugin's preset-load extension.

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};

use clap_sys::factory::preset_discovery::{
    CLAP_PRESET_DISCOVERY_FACTORY_ID, CLAP_PRESET_DISCOVERY_FACTORY_ID_COMPAT, CLAP_PRESET_DISCOVERY_LOCATION_FILE,
    clap_preset_discovery_factory, clap_preset_discovery_filetype, clap_preset_discovery_indexer,
    clap_preset_discovery_location, clap_preset_discovery_location_kind, clap_preset_discovery_metadata_receiver,
    clap_preset_discovery_soundpack,
};
use clap_sys::universal_plugin_id::clap_universal_plugin_id;
use clap_sys::version::CLAP_VERSION;

use crate::clap_host::ClapLibrary;

/// A preset a CLAP plugin's preset providers offer.
#[derive(Debug, Clone, PartialEq)]
pub struct ClapPreset {
    pub name: String,
    pub kind: clap_preset_discovery_location_kind,
    /// The preset file for file locations.
    pub location: Option<String>,
    pub load_key: Option<String>,
}

fn text(p: *const c_char) -> Option<String> {
    // SAFETY: CLAP strings are NUL-terminated and valid for the call.
    (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

/// What a provider declares while it initialises.
#[derive(Default)]
struct Declared {
    extensions: Vec<String>,
    locations: Vec<(clap_preset_discovery_location_kind, Option<String>)>,
}

unsafe fn declared<'a>(indexer: *const clap_preset_discovery_indexer) -> &'a mut Declared {
    // SAFETY: `indexer_data` is the `Declared` of the indexer built in `presets`.
    unsafe { &mut *((*indexer).indexer_data as *mut Declared) }
}

unsafe extern "C" fn declare_filetype(
    indexer: *const clap_preset_discovery_indexer,
    filetype: *const clap_preset_discovery_filetype,
) -> bool {
    // SAFETY: the provider passes a valid file type.
    if let Some(ext) = text(unsafe { (*filetype).file_extension }) {
        unsafe { declared(indexer) }.extensions.push(ext.trim_start_matches('.').to_ascii_lowercase());
    }
    true
}

unsafe extern "C" fn declare_location(
    indexer: *const clap_preset_discovery_indexer,
    location: *const clap_preset_discovery_location,
) -> bool {
    // SAFETY: the provider passes a valid location.
    let l = unsafe { &*location };
    unsafe { declared(indexer) }.locations.push((l.kind, text(l.location)));
    true
}

unsafe extern "C" fn declare_soundpack(
    _indexer: *const clap_preset_discovery_indexer,
    _soundpack: *const clap_preset_discovery_soundpack,
) -> bool {
    true
}

unsafe extern "C" fn indexer_extension(_i: *const clap_preset_discovery_indexer, _id: *const c_char) -> *const c_void {
    std::ptr::null()
}

/// Presets a location lists, each with the plugins it is for.
#[derive(Default)]
struct Listed {
    presets: Vec<(ClapPreset, Vec<String>)>,
    kind: clap_preset_discovery_location_kind,
    location: Option<String>,
}

unsafe fn listed<'a>(receiver: *const clap_preset_discovery_metadata_receiver) -> &'a mut Listed {
    // SAFETY: `receiver_data` is the `Listed` of the receiver built in `list`.
    unsafe { &mut *((*receiver).receiver_data as *mut Listed) }
}

unsafe extern "C" fn on_error(_r: *const clap_preset_discovery_metadata_receiver, _e: i32, _m: *const c_char) {}

unsafe extern "C" fn begin_preset(
    receiver: *const clap_preset_discovery_metadata_receiver,
    name: *const c_char,
    load_key: *const c_char,
) -> bool {
    let l = unsafe { listed(receiver) };
    // A preset file without a name is named after the file.
    let name = text(name).or_else(|| {
        let file = l.location.as_deref()?;
        Some(Path::new(file).file_stem()?.to_string_lossy().into_owned())
    });
    let preset = ClapPreset {
        name: name.unwrap_or_default(),
        kind: l.kind,
        location: l.location.clone(),
        load_key: text(load_key),
    };
    l.presets.push((preset, Vec::new()));
    true
}

unsafe extern "C" fn add_plugin_id(
    receiver: *const clap_preset_discovery_metadata_receiver,
    id: *const clap_universal_plugin_id,
) {
    // SAFETY: the provider passes a valid id.
    let id = unsafe { &*id };
    if text(id.abi).as_deref() == Some("clap")
        && let (Some(id), Some((_, ids))) = (text(id.id), unsafe { listed(receiver) }.presets.last_mut())
    {
        ids.push(id);
    }
}

unsafe extern "C" fn ignore_text(_r: *const clap_preset_discovery_metadata_receiver, _t: *const c_char) {}
unsafe extern "C" fn ignore_flags(_r: *const clap_preset_discovery_metadata_receiver, _f: u32) {}
unsafe extern "C" fn ignore_times(_r: *const clap_preset_discovery_metadata_receiver, _c: u64, _m: u64) {}
unsafe extern "C" fn ignore_pair(
    _r: *const clap_preset_discovery_metadata_receiver,
    _k: *const c_char,
    _v: *const c_char,
) {
}

/// Files under `path` (or `path` itself) with one of `extensions` (any when none are declared),
/// in name order.
fn preset_files(path: &Path, extensions: &[String]) -> Vec<PathBuf> {
    if path.is_file() {
        return vec![path.to_owned()];
    }
    let Ok(entries) = std::fs::read_dir(path) else { return Vec::new() };
    let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    entries
        .into_iter()
        .flat_map(|p| {
            if p.is_dir() {
                preset_files(&p, extensions)
            } else {
                let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase());
                let fits = extensions.is_empty() || ext.is_some_and(|e| extensions.contains(&e));
                if fits { vec![p] } else { Vec::new() }
            }
        })
        .collect()
}

impl ClapLibrary {
    fn preset_factory(&self) -> Option<&clap_preset_discovery_factory> {
        [CLAP_PRESET_DISCOVERY_FACTORY_ID, CLAP_PRESET_DISCOVERY_FACTORY_ID_COMPAT].into_iter().find_map(|id| {
            self.factory_ptr(id).and_then(|f| unsafe { (f as *const clap_preset_discovery_factory).as_ref() })
        })
    }

    /// The presets the library's preset providers list for plugin `plugin_id`, location by
    /// location in the order the providers declare them. Like REAPER, only a provider whose id
    /// is the plugin's id is asked.
    pub fn presets(&self, plugin_id: &str) -> Vec<ClapPreset> {
        let Some(factory) = self.preset_factory() else { return Vec::new() };
        let (Some(count), Some(describe), Some(create)) = (factory.count, factory.get_descriptor, factory.create)
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        // SAFETY: factory calls with indices below the reported count; the indexer and its data
        // outlive every provider, which is destroyed before they go.
        unsafe {
            for i in 0..count(factory) {
                let Some(desc) = describe(factory, i).as_ref() else { continue };
                if text(desc.id).as_deref() != Some(plugin_id) {
                    continue;
                }
                let mut declared = Declared::default();
                let indexer = clap_preset_discovery_indexer {
                    clap_version: CLAP_VERSION,
                    name: c"Scythe".as_ptr(),
                    vendor: c"Scythe".as_ptr(),
                    url: c"".as_ptr(),
                    version: c"1".as_ptr(),
                    indexer_data: &mut declared as *mut Declared as *mut c_void,
                    declare_filetype: Some(declare_filetype),
                    declare_location: Some(declare_location),
                    declare_soundpack: Some(declare_soundpack),
                    get_extension: Some(indexer_extension),
                };
                let Some(provider) = create(factory, &indexer, desc.id).as_ref() else { continue };
                if provider.init.is_some_and(|init| init(provider)) {
                    let locations = std::mem::take(&mut declared.locations);
                    for (kind, location) in locations {
                        let targets = match (kind, &location) {
                            (CLAP_PRESET_DISCOVERY_LOCATION_FILE, Some(path)) => {
                                preset_files(Path::new(path), &declared.extensions)
                                    .into_iter()
                                    .map(|p| Some(p.to_string_lossy().into_owned()))
                                    .collect()
                            }
                            (CLAP_PRESET_DISCOVERY_LOCATION_FILE, None) => Vec::new(),
                            _ => vec![None],
                        };
                        for target in targets {
                            out.extend(list(provider, kind, target, plugin_id));
                        }
                    }
                }
                if let Some(destroy) = provider.destroy {
                    destroy(provider);
                }
            }
        }
        out
    }
}

/// The presets one location lists for `plugin_id`; presets naming no plugin are for all.
unsafe fn list(
    provider: &clap_sys::factory::preset_discovery::clap_preset_discovery_provider,
    kind: clap_preset_discovery_location_kind,
    location: Option<String>,
    plugin_id: &str,
) -> Vec<ClapPreset> {
    let Some(get) = provider.get_metadata else { return Vec::new() };
    let c_location = location.as_deref().and_then(|l| CString::new(l).ok());
    let mut listed = Listed { presets: Vec::new(), kind, location };
    let receiver = clap_preset_discovery_metadata_receiver {
        receiver_data: &mut listed as *mut Listed as *mut c_void,
        on_error: Some(on_error),
        begin_preset: Some(begin_preset),
        add_plugin_id: Some(add_plugin_id),
        set_soundpack_id: Some(ignore_text),
        set_flags: Some(ignore_flags),
        add_creator: Some(ignore_text),
        set_description: Some(ignore_text),
        set_timestamps: Some(ignore_times),
        add_feature: Some(ignore_text),
        add_extra_info: Some(ignore_pair),
    };
    let ptr = c_location.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    // SAFETY: the receiver and location outlive the call.
    if !unsafe { get(provider, kind, ptr, &receiver) } {
        return Vec::new();
    }
    listed
        .presets
        .into_iter()
        .filter(|(_, ids)| ids.is_empty() || ids.iter().any(|i| i == plugin_id))
        .map(|(p, _)| p)
        .collect()
}
