//! REAPER's per-plugin preset files (`presets/vst-<plugin>.ini`, `js-…`, `vst3-…`, `clap-…`,
//! `lv2-…`). Each `[PresetN]` section holds the preset's `Name`, its length `Len` and its
//! bytes as hex `Data` lines of at most 16384 bytes (`Data`, `Data_1`, …), every line ending
//! in a checksum byte: the sum of that line's bytes. The bytes are the plugin's state the way
//! the project stores it: the decoded `<VST` block without its program line, the slider line
//! of a `<JS`, or the text of the lines inside a `<CLAP`/`<LV2`.

use base64::Engine as _;
use scythe_core::FxSlot;
use scythe_core::rpp::{Chunk, Node, join_tokens, line_key, quote, quote_always, tokenize};
use scythe_engine::vst2::{ReaperVstState, header_layout};

const LINE_BYTES: usize = 16384;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PresetFile {
    /// `[General]` keys other than `NbPresets`, such as `LastDefImpTime`.
    general: Vec<(String, String)>,
    pub presets: Vec<Preset>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    pub name: String,
    pub data: Vec<u8>,
}

impl PresetFile {
    /// Reads a preset file. Presets with a bad checksum or missing data are left out.
    pub fn parse(text: &str) -> Self {
        let mut general = Vec::new();
        let mut sections: Vec<(usize, Vec<(String, String)>)> = Vec::new();
        let mut current: Option<usize> = None;
        let mut in_general = false;
        for line in text.lines() {
            let line = line.trim();
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                in_general = name.eq_ignore_ascii_case("General");
                current = name
                    .get(..6)
                    .filter(|p| p.eq_ignore_ascii_case("Preset"))
                    .and_then(|_| name[6..].parse::<usize>().ok());
                if let Some(index) = current {
                    sections.push((index, Vec::new()));
                }
                continue;
            }
            let Some((key, value)) = line.split_once('=') else { continue };
            if in_general {
                if !key.eq_ignore_ascii_case("NbPresets") {
                    general.push((key.to_owned(), value.to_owned()));
                }
            } else if current.is_some()
                && let Some((_, keys)) = sections.last_mut()
            {
                keys.push((key.to_owned(), value.to_owned()));
            }
        }
        sections.sort_by_key(|(index, _)| *index);
        let presets = sections.iter().filter_map(|(_, keys)| preset_from_keys(keys)).collect();
        Self { general, presets }
    }

    pub fn to_text(&self) -> String {
        let mut out = String::from("[General]\n");
        for (key, value) in &self.general {
            out.push_str(&format!("{key}={value}\n"));
        }
        out.push_str(&format!("NbPresets={}\n", self.presets.len()));
        for (i, preset) in self.presets.iter().enumerate() {
            out.push_str(&format!("\n[Preset{i}]\n"));
            for (n, part) in preset.data.chunks(LINE_BYTES).enumerate() {
                let key = if n == 0 { "Data".to_owned() } else { format!("Data_{n}") };
                out.push_str(&format!("{key}={}\n", hex_line(part)));
            }
            out.push_str(&format!("Len={}\nName={}\n", preset.data.len(), preset.name));
        }
        out
    }

    /// Adds `preset`, replacing one of the same name.
    pub fn put(&mut self, preset: Preset) {
        match self.presets.iter_mut().find(|p| p.name == preset.name) {
            Some(existing) => *existing = preset,
            None => self.presets.push(preset),
        }
    }

    /// Drops preset `name`; false when there is none.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.presets.len();
        self.presets.retain(|p| p.name != name);
        self.presets.len() != before
    }

    /// Renames preset `from` in place; false when it is missing or `to` is taken.
    pub fn rename(&mut self, from: &str, to: &str) -> bool {
        if from != to && self.presets.iter().any(|p| p.name == to) {
            return false;
        }
        match self.presets.iter_mut().find(|p| p.name == from) {
            Some(p) => {
                p.name = to.to_owned();
                true
            }
            None => false,
        }
    }

    /// Takes in the presets of the plugin's default library (the `.rpl` beside it) when the
    /// library changed since the last import, as REAPER does: `LastDefImpTime` holds the
    /// library's modification time (seconds) at that import. Returns whether it imported.
    pub fn import_default(&mut self, library: &Library, modified: u64) -> bool {
        let last = self.general.iter().find(|(k, _)| k == "LastDefImpTime").and_then(|(_, v)| v.parse().ok());
        if last.is_some_and(|t: u64| t >= modified) {
            return false;
        }
        for preset in &library.presets {
            self.put(preset.clone());
        }
        self.general.retain(|(k, _)| k != "LastDefImpTime");
        self.general.insert(0, ("LastDefImpTime".into(), modified.to_string()));
        true
    }
}

/// A REAPER preset library (`.RPL`): `<REAPER_PRESET_LIBRARY` naming the plugin, with a
/// `<PRESET` per preset whose lines are its bytes, 40 to a separately padded base64 line.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Library {
    pub plugin: String,
    pub presets: Vec<Preset>,
}

const LIBRARY_LINE_BYTES: usize = 40;

impl Library {
    pub fn parse(text: &str) -> Option<Self> {
        let root = scythe_core::rpp::parse(text).ok()?;
        if root.name() != "REAPER_PRESET_LIBRARY" {
            return None;
        }
        let b64 = base64::engine::general_purpose::STANDARD;
        let presets = root
            .chunks("PRESET")
            .filter_map(|c| {
                let mut data = Vec::new();
                for node in &c.children {
                    if let Node::Line(l) = node {
                        data.extend(b64.decode(l.trim()).ok()?);
                    }
                }
                Some(Preset { name: c.args().first()?.clone(), data })
            })
            .collect();
        Some(Self { plugin: root.args().first().cloned().unwrap_or_default(), presets })
    }

    pub fn to_text(&self) -> String {
        let b64 = base64::engine::general_purpose::STANDARD;
        let mut out = format!("<REAPER_PRESET_LIBRARY {}\n", library_quote(&self.plugin));
        for preset in &self.presets {
            out.push_str(&format!("  <PRESET {}\n", library_quote(&preset.name)));
            for part in preset.data.chunks(LIBRARY_LINE_BYTES) {
                out.push_str(&format!("    {}\n", b64.encode(part)));
            }
            out.push_str("  >\n");
        }
        out.push_str(">\n");
        out
    }
}

/// REAPER quotes names in a library with backticks.
fn library_quote(s: &str) -> String {
    if s.contains('`') { quote_always(s) } else { format!("`{s}`") }
}

/// Where REAPER looks for a plugin's default library: beside the VST 2 binary (`<file>.rpl`)
/// or the JSFX (`<effect>.rpl`).
pub fn default_libraries(slot: &FxSlot, plugin_file: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Some(chunk) = plugin_chunk(slot) else { return Vec::new() };
    let mut with_rpl = plugin_file.as_os_str().to_owned();
    with_rpl.push(".rpl");
    match chunk.name() {
        "VST" if !is_vst3(chunk) => vec![with_rpl.into()],
        "JS" => {
            let mut out = vec![std::path::PathBuf::from(with_rpl)];
            if plugin_file.extension().is_some() {
                out.push(plugin_file.with_extension("rpl"));
            }
            out
        }
        _ => Vec::new(),
    }
}

fn is_vst3(chunk: &Chunk) -> bool {
    chunk.args().first().is_some_and(|d| d.starts_with("VST3"))
}

/// A VST 3 preset file (`.vstpreset`): `VST3`, a version, the class id as 32 hex digits and
/// the offset of a chunk list (`List`, a count, then id, offset and size per chunk) naming the
/// component (`Comp`) and controller (`Cont`) states.
#[derive(Debug, Clone, PartialEq)]
pub struct VstPreset {
    pub class_id: String,
    pub component: Vec<u8>,
    pub controller: Vec<u8>,
}

impl VstPreset {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.get(..4)? != b"VST3" {
            return None;
        }
        let class_id = String::from_utf8(bytes.get(8..40)?.to_vec()).ok()?;
        let u32_at = |at: usize| bytes.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
        let u64_at = |at: usize| bytes.get(at..at + 8).map(|b| u64::from_le_bytes(b.try_into().unwrap()) as usize);
        let list = u64_at(40)?;
        if bytes.get(list..list + 4)? != b"List" {
            return None;
        }
        let mut preset = Self { class_id, component: Vec::new(), controller: Vec::new() };
        for i in 0..u32_at(list + 4)? {
            let at = list + 8 + i * 20;
            let (offset, size) = (u64_at(at + 4)?, u64_at(at + 12)?);
            let data = bytes.get(offset..offset.checked_add(size)?)?.to_vec();
            match bytes.get(at..at + 4)? {
                b"Comp" => preset.component = data,
                b"Cont" => preset.controller = data,
                _ => {}
            }
        }
        Some(preset)
    }

    /// The preset as REAPER's preset bytes for `slot`: the slot's header with this state.
    pub fn data_for(&self, slot: &FxSlot) -> Option<Vec<u8>> {
        let current = capture(slot)?;
        let (_, size_at) = header_layout(&current)?;
        let state = scythe_engine::vst3_host::reaper_state(&self.component, &self.controller);
        let mut data = current[..size_at + 12].to_vec();
        data[size_at..size_at + 4].copy_from_slice(&(state.len() as u32).to_le_bytes());
        data.extend(state);
        Some(data)
    }
}

/// The class id (32 hex digits), vendor and name of the VST 3 plugin in `slot`, from its
/// `VST3: Name (Vendor)` title and `id{CLASSID}` argument.
pub fn vst3_identity(slot: &FxSlot) -> Option<(String, String, String)> {
    let chunk = plugin_chunk(slot)?;
    if chunk.name() != "VST" || !is_vst3(chunk) {
        return None;
    }
    let args = chunk.args();
    let cid = args.get(4)?.split_once('{')?.1.trim_end_matches('}').to_ascii_uppercase();
    let title = args.first()?.split_once(": ")?.1;
    let (name, vendor) = title.strip_suffix(')').and_then(|t| t.rsplit_once(" ("))?;
    Some((cid, vendor.to_owned(), name.to_owned()))
}

/// Where VST 3 presets live: `~/.vst3/presets`, on Windows `Documents\VST3 Presets`.
pub fn vst3_presets_dir(home: &std::path::Path) -> std::path::PathBuf {
    if cfg!(windows) { home.join("Documents").join("VST3 Presets") } else { home.join(".vst3/presets") }
}

/// The `.vstpreset` files REAPER lists for a VST 3 plugin: those under
/// `<presets>/<vendor>/<plugin>/`, each folder's files before its subfolders.
pub fn vst3_preset_files(home: &std::path::Path, vendor: &str, name: &str) -> Vec<std::path::PathBuf> {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<std::path::PathBuf> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        let (dirs, files): (Vec<_>, Vec<_>) = entries.into_iter().partition(|p| p.is_dir());
        out.extend(files.into_iter().filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("vstpreset"))));
        for d in dirs {
            walk(&d, out);
        }
    }
    let mut out = Vec::new();
    walk(&vst3_presets_dir(home).join(vendor).join(name), &mut out);
    out
}

fn preset_from_keys(keys: &[(String, String)]) -> Option<Preset> {
    let get = |k: &str| keys.iter().find(|(key, _)| key.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str());
    let name = get("Name")?.to_owned();
    let len: usize = get("Len")?.trim().parse().ok()?;
    let mut data = Vec::with_capacity(len);
    let mut n = 0;
    while let Some(line) = get(&if n == 0 { "Data".to_owned() } else { format!("Data_{n}") }) {
        data.extend(decode_line(line.trim())?);
        n += 1;
    }
    (n > 0 && data.len() >= len).then(|| {
        data.truncate(len);
        Preset { name, data }
    })
}

fn decode_line(hex: &str) -> Option<Vec<u8>> {
    if !hex.is_ascii() || !hex.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Vec<u8> =
        (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok()).collect::<Option<_>>()?;
    let (sum, data) = bytes.split_last()?;
    (checksum(data) == *sum).then(|| data.to_vec())
}

fn hex_line(data: &[u8]) -> String {
    data.iter().chain(std::iter::once(&checksum(data))).map(|b| format!("{b:02X}")).collect()
}

fn checksum(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |sum, b| sum.wrapping_add(*b))
}

fn plugin_chunk(slot: &FxSlot) -> Option<&Chunk> {
    slot.raw.iter().find_map(|n| match n {
        Node::Chunk(c) => Some(c),
        Node::Line(_) => None,
    })
}

fn plugin_chunk_mut(slot: &mut FxSlot) -> Option<&mut Chunk> {
    slot.raw.iter_mut().find_map(|n| match n {
        Node::Chunk(c) => Some(c),
        Node::Line(_) => None,
    })
}

/// The name REAPER gives the preset file of the plugin in `slot`: VST 2 by the binary's name
/// up to its first dot, VST 3 by the bundle's name and REAPER's plugin id, JSFX by path and
/// CLAP and LV2 by plugin id, with separators and dots made underscores.
pub fn file_name(slot: &FxSlot) -> Option<String> {
    let chunk = plugin_chunk(slot)?;
    let args = chunk.args();
    let stem = |file: &str| {
        let base = file.rsplit(['/', '\\']).next().unwrap_or(file);
        base.split('.').next().unwrap_or(base).to_owned()
    };
    let flat = |id: &str| id.replace(['/', '\\', '.', ':'], "_");
    let name = match chunk.name() {
        "VST" => {
            let file = args.get(1)?;
            if file.to_ascii_lowercase().ends_with(".vst3") {
                let id = args.get(4)?.split(['{', '<']).next()?.to_owned();
                format!("vst3-{}-{id}", stem(file))
            } else {
                format!("vst-{}", stem(file))
            }
        }
        "JS" => format!("js-{}", flat(args.first()?)),
        "CLAP" => format!("clap-{}", flat(args.get(1)?)),
        "LV2" => format!("lv2-{}", flat(args.get(1)?)),
        _ => return None,
    };
    (!name.ends_with('-')).then(|| format!("{name}.ini"))
}

fn lines(chunk: &Chunk) -> Vec<String> {
    chunk
        .children
        .iter()
        .filter_map(|n| match n {
            Node::Line(l) => Some(l.trim().to_owned()),
            Node::Chunk(_) => None,
        })
        .collect()
}

/// A JSFX slider line holds 64 values; REAPER puts the active preset's name after them.
const JS_SLIDERS: usize = 64;

/// The preset bytes for the plugin's state as stored in `slot`.
pub fn capture(slot: &FxSlot) -> Option<Vec<u8>> {
    let chunk = plugin_chunk(slot)?;
    match chunk.name() {
        "VST" => {
            let lines = lines(chunk);
            let b64 = base64::engine::general_purpose::STANDARD;
            let mut data = b64.decode(lines.first()?).ok()?;
            let header = VstHeader::parse(&data)?;
            data.truncate(header.size_at + 12);
            let body: String = lines.get(1..lines.len().saturating_sub(1))?.concat();
            let mut state = b64.decode(body).ok()?;
            state.truncate(header.size);
            data.extend(state);
            Some(data)
        }
        "JS" => {
            let line = lines(chunk).into_iter().next()?;
            let values = line.split_whitespace().take(JS_SLIDERS).take_while(|t| !t.starts_with(['"', '\'', '`']));
            Some(values.collect::<Vec<_>>().join(" ").into_bytes())
        }
        "CLAP" | "LV2" => Some(scythe_core::rpp::write_body(&chunk.children).into_bytes()),
        _ => None,
    }
}

/// Sets the state of the plugin in `slot` from the bytes of preset `name` and records the
/// preset's name where REAPER does. Returns false when the data does not fit the plugin.
pub fn apply(slot: &mut FxSlot, data: &[u8], name: &str) -> bool {
    let Some(chunk) = plugin_chunk_mut(slot) else { return false };
    let applied = match chunk.name() {
        "VST" => apply_vst(chunk, data, name),
        "JS" => {
            let text = String::from_utf8_lossy(data);
            let values: Vec<&str> = text.split_whitespace().take(JS_SLIDERS).collect();
            if values.is_empty() {
                return false;
            }
            let line = Node::Line(format!("{} {}", values.join(" "), quote(name)));
            match chunk.children.iter_mut().find(|n| matches!(n, Node::Line(_))) {
                Some(existing) => *existing = line,
                None => chunk.children.insert(0, line),
            }
            true
        }
        "CLAP" | "LV2" => apply_text(chunk, data, name),
        _ => false,
    };
    if applied {
        set_preset_name(slot, name);
    }
    applied
}

/// Records the name of a plugin's own program or discovered preset as REAPER does after
/// selecting it: in a VST 2 plugin's program line or a CLAP plugin's `CFG` line, and as the
/// slot's preset.
pub fn name_program(slot: &mut FxSlot, name: &str) {
    let b64 = base64::engine::general_purpose::STANDARD;
    let Some(chunk) = plugin_chunk_mut(slot) else { return };
    if chunk.name() != "VST" {
        name_cfg(chunk, name);
        return set_preset_name(slot, name);
    }
    let old = lines(chunk);
    let Some(state) = (old.len() >= 2).then(|| ReaperVstState::decode(&old)).flatten() else { return };
    let mut new = old;
    if let Some(last) = new.last_mut() {
        *last = b64.encode(program_line(name, state.words[1] >> 16));
    }
    chunk.children = new.into_iter().map(Node::Line).collect();
    set_preset_name(slot, name);
}

/// The plugin a preset library names: the slot's title (`VST: ReaComp (Cockos)`), or `JS: `
/// and the effect's path.
pub fn library_name(slot: &FxSlot) -> Option<String> {
    let chunk = plugin_chunk(slot)?;
    let first = chunk.args().first()?.clone();
    match chunk.name() {
        "JS" => Some(format!("JS: {first}")),
        "VST" | "CLAP" | "LV2" => Some(first),
        _ => None,
    }
}

/// Records `name` as the slot's preset (`PRESETNAME` after the plugin's block).
fn set_preset_name(slot: &mut FxSlot, name: &str) {
    let at = slot.raw.iter().position(|n| matches!(n, Node::Chunk(_))).map_or(0, |i| i + 1);
    slot.raw.retain(|n| !matches!(n, Node::Line(l) if line_key(l.trim()) == "PRESETNAME"));
    slot.raw.insert(at.min(slot.raw.len()), Node::Line(format!("PRESETNAME {}", quote(name))));
}

/// A `<VST` header as preset bytes start with it.
struct VstHeader {
    inputs: Vec<u64>,
    outputs: Vec<u64>,
    /// Where the state size sits; the state follows the two flag words after it.
    size_at: usize,
    size: usize,
    words: [u32; 2],
}

impl VstHeader {
    fn parse(data: &[u8]) -> Option<Self> {
        let (after_in, size_at) = header_layout(data)?;
        let word = |at: usize| data.get(at..at + 4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes);
        let masks = |from: usize, to: usize| -> Vec<u64> {
            data[from..to].as_chunks::<8>().0.iter().map(|b| u64::from_le_bytes(*b)).collect()
        };
        Some(Self {
            inputs: masks(12, after_in),
            outputs: masks(after_in + 4, size_at),
            size_at,
            size: word(size_at)? as usize,
            words: [word(size_at + 4)?, word(size_at + 8)?],
        })
    }

    fn state<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        let state = &data[(self.size_at + 12).min(data.len())..];
        &state[..self.size.min(state.len())]
    }
}

/// The `<VST` block is REAPER's header, the plugin state and a program line holding the
/// preset's name between NULs and the high half of the header's second flag word. As REAPER
/// 7.73 does, the preset brings its pin maps and those high flag bits; its state is taken only
/// when its first flag word (1 for a plugin chunk) matches the slot's, and with it a VST 2
/// plugin's program.
fn apply_vst(chunk: &mut Chunk, data: &[u8], name: &str) -> bool {
    let b64 = base64::engine::general_purpose::STANDARD;
    let Some(preset) = VstHeader::parse(data) else { return false };
    let state = preset.state(data);
    let vst3 = chunk.args().first().is_some_and(|d| d.starts_with("VST3"));
    let old = lines(chunk);
    let current = (old.len() >= 2).then(|| ReaperVstState::decode(&old)).flatten();
    let new = match current {
        Some(current) => {
            let take = current.words[0] == preset.words[0];
            let program = if take && !vst3 { preset.words[1] } else { current.words[1] } & 0xffff;
            let words = [current.words[0], preset.words[1] & 0xffff_0000 | program];
            let base = if take { ReaperVstState::reencode(&old, state) } else { Some(old.clone()) };
            base.and_then(|l| ReaperVstState::with_pins(&l, &preset.inputs, &preset.outputs))
                .and_then(|l| ReaperVstState::with_words(&l, words))
        }
        None => {
            let mut header = data[..preset.size_at + 12].to_vec();
            header[preset.size_at..preset.size_at + 4].copy_from_slice(&(state.len() as u32).to_le_bytes());
            let mut out = vec![b64.encode(&header)];
            out.extend(b64.encode(state).as_bytes().chunks(128).map(|c| String::from_utf8_lossy(c).into_owned()));
            out.push(String::new());
            Some(out)
        }
    };
    let Some(mut new) = new else { return false };
    let flags = ReaperVstState::decode(&new).map_or(0, |s| s.words[1] >> 16);
    if let Some(last) = new.last_mut() {
        *last = b64.encode(program_line(name, flags));
    }
    chunk.children = new.into_iter().map(Node::Line).collect();
    true
}

/// REAPER's program line: the preset or program name between NULs, then a flags word.
fn program_line(name: &str, flags: u32) -> Vec<u8> {
    let mut program = vec![0];
    program.extend(name.as_bytes());
    program.push(0);
    program.extend(flags.to_le_bytes());
    program
}

/// CLAP and LV2 presets are the lines inside the plugin's block; each replaces the slot's line
/// or block of the same kind (LV2 parameters by name). The name goes into the `CFG` line.
fn apply_text(chunk: &mut Chunk, data: &[u8], name: &str) -> bool {
    let text = String::from_utf8_lossy(data);
    let Ok(parsed) = scythe_core::rpp::parse(&format!("<PRESET\n{text}\n>\n")) else { return false };
    if parsed.children.is_empty() {
        return false;
    }
    let key = |n: &Node| match n {
        Node::Line(l) => {
            let t = tokenize(l);
            match t.first().map(String::as_str) {
                Some("P") => t.iter().take(2).cloned().collect::<Vec<_>>().join(" "),
                _ => line_key(l.trim()).to_owned(),
            }
        }
        Node::Chunk(c) => format!("<{}", c.name()),
    };
    for node in parsed.children {
        let node = match node {
            Node::Line(l) => Node::Line(l.trim().to_owned()),
            block => block,
        };
        let k = key(&node);
        match chunk.children.iter_mut().find(|n| key(n) == k) {
            Some(existing) => *existing = node,
            None => chunk.children.push(node),
        }
    }
    name_cfg(chunk, name);
    true
}

/// Puts a CLAP or LV2 plugin's preset name in its `CFG` line, where REAPER keeps it.
fn name_cfg(chunk: &mut Chunk, name: &str) {
    if let Some(Node::Line(cfg)) =
        chunk.children.iter_mut().find(|n| matches!(n, Node::Line(l) if line_key(l.trim()) == "CFG"))
    {
        let mut t = tokenize(cfg);
        t.resize(t.len().max(5), String::new());
        *cfg = format!("{} {} {}", join_tokens(&t[..4]), quote(name), join_tokens(&t[5..]));
        *cfg = cfg.trim_end().to_owned();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The start of `presets/vst-reacomp.ini` as REAPER 7.73 wrote it when importing ReaComp's
    /// stock presets.
    const REACOMP_INI: &str = "[General]\nLastDefImpTime=1\nNbPresets=19\n\n[Preset0]\nData=6D636572EF5EEDFE0400000001000000000000000200000000000000040000000000000008000000000000000200000001000000000000000200000000000000540000000000000000000000EFBEADDE0DF0ADDE2FB2F13D65F1373C7B49A33DF98F543D00000000000000004D2BB73C5F96B83B0000000000000000BD3786350000803F000000005070813E2506813D00000000000000000000000000000000C9\nLen=160\nName=stock - Rock Snare - aim for 4dB GR\n\n";

    /// ReaComp, the VST 3 and CLAP test gains, JS volume and the LV2 test amp as REAPER 7.73
    /// stores them.
    const CHAIN: &str = r#"BYPASS 0 0 0
<VST "VST: ReaComp (Cockos)" reacomp.vst.so 0 "" 1919247213<5653547265636D726561636F6D700000> ""
bWNlcu9e7f4EAAAAAQAAAAAAAAACAAAAAAAAAAQAAAAAAAAACAAAAAAAAAACAAAAAQAAAAAAAAACAAAAAAAAAFwAAAAAAAAAAAAQAA==
776t3g3wrd4AAAA/ED74PKabxDsK16M8AAAAAAAAAAAAAIA/AAAAAAAAAAAAAAAAnNEHMwAAgD8AAAAAzcxMPQAAAAAAAAAAAAAAAAAAgD4AAAAAAAAAAAAAAAA=
AAAQAAAA
>
FXID {56EA5D55-1CD9-FFE2-B366-8E63F612F197}
BYPASS 0 0 0
<VST "VST3: Test Gain (Scythe)" "Scythe Test.vst3" 0 "" 1540441813{5C7E4E012A6B4D5A9F3B1C2D3E4F5061} ""
1UbRW+5e7f4EAAAAAQAAAAAAAAACAAAAAAAAAAQAAAAAAAAACAAAAAAAAAACAAAAAQAAAAAAAAACAAAAAAAAABgAAAABAAAA//8AAA==
CAAAAAEAAAAAAAAAAADgPwAAAAAAAAAA
AAAAAAAA
>
FXID {32954796-3B06-C778-4369-C1E11DF2CBBC}
BYPASS 0 0 0
<CLAP "CLAP: Test Gain (Scythe)" org.scythe.test-gain ""
CFG 4 0 0 ""
<IN_PINS
>
<STATE
AAAAPw==
>
>
FXID {69D83377-46C4-4F5C-A204-CB0D1E1D3083}
BYPASS 0 0 0
<JS utility/volume ""
-75 0 - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - -
>
FXID {6AED2547-8200-CC05-7FD8-2D4031CFC684}
BYPASS 0 0 0
<LV2 "LV2: Test Amp" urn:scythe:test-amp ""
CFG 0 0 0 ""
P gain 0.5
>
FXID {6F1D255F-BF34-9605-72AD-C41836EC6C7B}
"#;

    fn slots() -> Vec<FxSlot> {
        scythe_core::rpp::fx_chain_from_text(CHAIN).unwrap()
    }

    fn vst_state(slot: &FxSlot) -> scythe_engine::vst2::ReaperVstState {
        scythe_engine::vst2::ReaperVstState::decode(&lines(plugin_chunk(slot).unwrap())).unwrap()
    }

    #[test]
    fn reads_reaper_preset_file() {
        let file = PresetFile::parse(REACOMP_INI);
        assert_eq!(file.presets.len(), 1);
        let p = &file.presets[0];
        assert_eq!(p.name, "stock - Rock Snare - aim for 4dB GR");
        assert_eq!(p.data.len(), 160);
        assert_eq!(&p.data[..8], &[0x6D, 0x63, 0x65, 0x72, 0xEF, 0x5E, 0xED, 0xFE]);
        assert_eq!(file.general, vec![("LastDefImpTime".to_owned(), "1".to_owned())]);
    }

    #[test]
    fn bad_checksum_drops_the_preset() {
        let file = PresetFile::parse(&REACOMP_INI.replace("00C9\n", "00C8\n"));
        assert!(file.presets.is_empty());
    }

    #[test]
    fn file_names_follow_reaper() {
        let names: Vec<String> = slots().iter().filter_map(file_name).collect();
        assert_eq!(
            names,
            [
                "vst-reacomp.ini",
                "vst3-Scythe Test-1540441813.ini",
                "clap-org_scythe_test-gain.ini",
                "js-utility_volume.ini",
                "lv2-urn_scythe_test-amp.ini"
            ]
        );
        let mut slot = slots().remove(0);
        *plugin_chunk_mut(&mut slot).unwrap() =
            Chunk::new("VST", &["VSTi: Ab.c d.e", "Ab.c d.e.so", "0", "", "1399028052", ""]);
        assert_eq!(file_name(&slot).as_deref(), Some("vst-Ab.ini"));
        *plugin_chunk_mut(&mut slot).unwrap() = Chunk::new("JS", &["ptest/foo.jsfx", ""]);
        assert_eq!(file_name(&slot).as_deref(), Some("js-ptest_foo_jsfx.ini"));
    }

    /// A `<VST` header: id, magic, pin maps, state size and flag words.
    fn header(id: u32, inputs: &[u64], outputs: &[u64], size: u32, words: [u32; 2]) -> Vec<u8> {
        let mut h = [id, 0xfeed_5eee].iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<u8>>();
        for masks in [inputs, outputs] {
            h.extend((masks.len() as u32).to_le_bytes());
            h.extend(masks.iter().flat_map(|m| m.to_le_bytes()));
        }
        h.extend([size, words[0], words[1]].iter().flat_map(|w| w.to_le_bytes()));
        h
    }

    fn vst3_gain(gain: f64) -> Vec<u8> {
        [8u32.to_le_bytes(), 1u32.to_le_bytes()].concat().into_iter().chain(gain.to_le_bytes()).chain([0; 8]).collect()
    }

    fn with_lines(slot: &mut FxSlot, lines: &[&str]) {
        plugin_chunk_mut(slot).unwrap().children = lines.iter().map(|l| Node::Line((*l).into())).collect();
    }

    /// REAPER 7.73 loading presets into the VST 3 test gain whose first output pin had been
    /// moved to channel 2: the preset brings its pins and the high flag bits; a first flag
    /// word other than the slot's leaves the state as it was. Expected lines are REAPER's.
    #[test]
    fn vst_presets_bring_pins_and_flags_like_reaper() {
        const ID: u32 = 0x5bd1_46d5;
        let mut slot = slots().remove(1);
        with_lines(
            &mut slot,
            &[
                "1UbRW+5e7f4EAAAAAQAAAAAAAAACAAAAAAAAAAQAAAAAAAAACAAAAAAAAAACAAAAAgAAAAAAAAACAAAAAAAAABgAAAABAAAA//8AAA==",
                "CAAAAAEAAAAAAAAAAADgPwAAAAAAAAAA",
                "AAAAAAAA",
            ],
        );
        let preset =
            |ins: &[u64], outs: &[u64], words, gain| [header(ID, ins, outs, 24, words), vst3_gain(gain)].concat();
        assert!(apply(&mut slot, &preset(&[2, 1, 4, 8], &[2, 1], [1, 0xffff], 0.35), "pins"));
        assert_eq!(
            lines(plugin_chunk(&slot).unwrap()),
            [
                "1UbRW+5e7f4EAAAAAgAAAAAAAAABAAAAAAAAAAQAAAAAAAAACAAAAAAAAAACAAAAAgAAAAAAAAABAAAAAAAAABgAAAABAAAA//8AAA==",
                "CAAAAAEAAABmZmZmZmbWPwAAAAAAAAAA",
                "AHBpbnMAAAAAAA=="
            ]
        );
        assert!(apply(&mut slot, &preset(&[1, 2, 4, 8], &[1, 2], [0, 0xffff], 0.4), "w1_0"));
        assert_eq!(
            lines(plugin_chunk(&slot).unwrap()),
            [
                "1UbRW+5e7f4EAAAAAQAAAAAAAAACAAAAAAAAAAQAAAAAAAAACAAAAAAAAAACAAAAAQAAAAAAAAACAAAAAAAAABgAAAABAAAA//8AAA==",
                "CAAAAAEAAABmZmZmZmbWPwAAAAAAAAAA",
                "AHcxXzAAAAAAAA=="
            ]
        );
        assert!(apply(&mut slot, &preset(&[1, 2, 4, 8], &[1, 2], [1, 5], 0.45), "w2_5"));
        assert_eq!(vst_state(&slot).words, [1, 0xffff], "a VST 3 plugin has no program to take");
        assert!(apply(&mut slot, &preset(&[1, 2, 4, 8], &[1, 2], [1, 0x10_0000], 0.7), "w2_hi"));
        assert_eq!(
            lines(plugin_chunk(&slot).unwrap()),
            [
                "1UbRW+5e7f4EAAAAAQAAAAAAAAACAAAAAAAAAAQAAAAAAAAACAAAAAAAAAACAAAAAQAAAAAAAAACAAAAAAAAABgAAAABAAAA//8QAA==",
                "CAAAAAEAAABmZmZmZmbmPwAAAAAAAAAA",
                "AHcyX2hpABAAAAA="
            ]
        );
    }

    /// The VST 2 test synth in REAPER 7.73: a preset with a matching first flag word brings its
    /// chunk and program (REAPER then selects the program on the plugin); one without changes
    /// only the pins.
    #[test]
    fn vst2_presets_bring_their_program() {
        const ID: u32 = 0x5363_7954;
        let mut slot = slots().remove(0);
        *plugin_chunk_mut(&mut slot).unwrap() =
            Chunk::new("VST", &["VSTi: Test Synth Programs", "Test Synth Programs.so", "0", "", "1399027028", ""]);
        with_lines(
            &mut slot,
            &["VHljU+5e7f4AAAAAAgAAAAIAAAAAAAAAAgAAAAAAAAAEAAAAAQAAAAAAAAA=", "AACAPw==", "AAAAAAAA"],
        );
        let data = [header(ID, &[], &[1, 2], 4, [1, 2]), 0.6f32.to_le_bytes().to_vec()].concat();
        assert!(apply(&mut slot, &data, "prog2"));
        let state = vst_state(&slot);
        assert_eq!(
            lines(plugin_chunk(&slot).unwrap())[0],
            "VHljU+5e7f4AAAAAAgAAAAEAAAAAAAAAAgAAAAAAAAAEAAAAAQAAAAIAAAA="
        );
        assert_eq!((state.chunk, state.program), (0.6f32.to_le_bytes().to_vec(), b"\0prog2\0\0\0\0\0".to_vec()));
        let data = [header(ID, &[], &[2, 1], 4, [0, 1]), 0.7f32.to_le_bytes().to_vec()].concat();
        assert!(apply(&mut slot, &data, "prog1_w0"));
        let state = vst_state(&slot);
        assert_eq!((state.outputs, state.words, state.chunk), (vec![2, 1], [1, 2], 0.6f32.to_le_bytes().to_vec()));

        name_program(&mut slot, "Half");
        assert_eq!(vst_state(&slot).program, b"\0Half\0\0\0\0\0");
        assert!(slot.raw.contains(&Node::Line("PRESETNAME Half".into())));
    }

    #[test]
    fn presets_are_renamed_and_removed() {
        let mut file = PresetFile::parse(REACOMP_INI);
        file.put(Preset { name: "mine".into(), data: vec![1] });
        assert!(!file.rename("mine", "stock - Rock Snare - aim for 4dB GR"), "the name is taken");
        assert!(file.rename("mine", "ours"));
        assert_eq!(file.presets[1].name, "ours");
        assert!(file.remove("stock - Rock Snare - aim for 4dB GR"));
        assert!(!file.remove("stock - Rock Snare - aim for 4dB GR"));
        assert_eq!(file.presets.len(), 1);
        assert!(file.to_text().starts_with("[General]\nLastDefImpTime=1\nNbPresets=1\n"));
    }

    /// The start of REAPER 7.73's `Plugins/FX/reacomp.vst.so.rpl`, the library it imports
    /// ReaComp's stock presets from.
    const REACOMP_RPL: &str = "<REAPER_PRESET_LIBRARY `VST: ReaComp (Cockos)`\n  <PRESET `stock - Rock Snare - aim for 4dB GR`\n    bWNlcu9e7f4EAAAAAQAAAAAAAAACAAAAAAAAAAQAAAAAAAAACAAAAA==\n    AAAAAAIAAAABAAAAAAAAAAIAAAAAAAAAVAAAAAAAAAAAAAAA776t3g==\n    DfCt3i+y8T1l8Tc8e0mjPfmPVD0AAAAAAAAAAE0rtzxflrg7AAAAAA==\n    AAAAAL03hjUAAIA/AAAAAFBwgT4lBoE9AAAAAAAAAAAAAAAAAAAAAA==\n  >\n>\n";

    #[test]
    fn libraries_hold_the_same_bytes_as_preset_files() {
        let library = Library::parse(REACOMP_RPL).unwrap();
        assert_eq!(library.plugin, "VST: ReaComp (Cockos)");
        assert_eq!(library.presets, PresetFile::parse(REACOMP_INI).presets);
        assert_eq!(library.to_text(), REACOMP_RPL);

        let mut file = PresetFile::default();
        assert!(file.import_default(&library, 1794162119));
        assert!(!file.import_default(&library, 1794162119), "imported once per library change");
        assert!(file.to_text().starts_with("[General]\nLastDefImpTime=1794162119\nNbPresets=1\n"));
        file.presets[0].data.clear();
        assert!(file.import_default(&library, 1794162120));
        assert_eq!(file.presets, library.presets, "a newer library replaces presets of the same name");

        let plugin = std::path::Path::new("/opt/REAPER/Plugins/FX/reacomp.vst.so");
        assert_eq!(default_libraries(&slots()[0], plugin), [plugin.with_extension("so.rpl")]);
        assert!(default_libraries(&slots()[1], plugin).is_empty());
        let names: Vec<String> = slots().iter().filter_map(library_name).collect();
        assert_eq!(
            names[..4],
            ["VST: ReaComp (Cockos)", "VST3: Test Gain (Scythe)", "CLAP: Test Gain (Scythe)", "JS: utility/volume"]
        );
    }

    #[test]
    fn vstpresets_load_as_reaper_state() {
        let comp = 0.2f64.to_le_bytes();
        let mut bytes = b"VST3".to_vec();
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(b"5C7E4E012A6B4D5A9F3B1C2D3E4F5061");
        bytes.extend((48u64 + 10).to_le_bytes());
        bytes.extend(comp);
        bytes.extend(b"xy");
        bytes.extend(b"List");
        bytes.extend(2u32.to_le_bytes());
        for (id, at, len) in [(b"Comp", 48u64, 8u64), (b"Cont", 56, 2)] {
            bytes.extend(id);
            bytes.extend(at.to_le_bytes());
            bytes.extend(len.to_le_bytes());
        }
        let preset = VstPreset::parse(&bytes).unwrap();
        assert_eq!((preset.component.as_slice(), preset.controller.as_slice()), (comp.as_slice(), b"xy".as_slice()));
        let mut slot = slots().remove(1);
        let (cid, vendor, name) = vst3_identity(&slot).unwrap();
        assert_eq!((cid.as_str(), vendor.as_str(), name.as_str()), (preset.class_id.as_str(), "Scythe", "Test Gain"));
        let data = preset.data_for(&slot).unwrap();
        assert!(apply(&mut slot, &data, "/home/u/.vst3/presets/Scythe/Test Gain/User A.vstpreset"));
        let state = vst_state(&slot);
        assert_eq!(
            state.chunk,
            [&8u32.to_le_bytes()[..], &1u32.to_le_bytes(), &comp, &2u32.to_le_bytes(), &1u32.to_le_bytes(), b"xy"]
                .concat()
        );
        assert!(VstPreset::parse(b"VST2").is_none());
    }

    #[test]
    fn vstpreset_files_come_folder_by_folder() {
        let home = std::env::temp_dir().join(format!("scythe-vstpreset-{}", std::process::id()));
        let dir = vst3_presets_dir(&home).join("Scythe/Test Gain");
        std::fs::create_dir_all(dir.join("Sub")).unwrap();
        for f in ["User A.vstpreset", "Sub/User B.vstpreset", "notes.txt", "Z.vstpreset"] {
            std::fs::write(dir.join(f), b"").unwrap();
        }
        let found: Vec<_> = vst3_preset_files(&home, "Scythe", "Test Gain");
        let rel: Vec<_> = found.iter().map(|p| p.strip_prefix(&dir).unwrap().to_path_buf()).collect();
        assert_eq!(rel, ["User A.vstpreset", "Z.vstpreset", "Sub/User B.vstpreset"].map(std::path::PathBuf::from));
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn clap_program_names_go_in_the_cfg_line() {
        let mut clap = slots().remove(2);
        name_program(&mut clap, "Loud");
        assert_eq!(plugin_chunk(&clap).unwrap().find_line("CFG").unwrap(), ["4", "0", "0", "Loud"]);
        assert!(clap.raw.contains(&Node::Line("PRESETNAME Loud".into())));
    }

    /// REAPER 7.73 loading ReaComp's first stock preset: its header and program line take the
    /// preset's zero flags (REAPER's own ReaComp then stores its longer state).
    #[test]
    fn applies_a_vst_preset_with_its_pins_and_program() {
        let mut slot = slots().remove(0);
        let before = vst_state(&slot);
        let preset = &PresetFile::parse(REACOMP_INI).presets[0];
        assert!(apply(&mut slot, &preset.data, &preset.name));
        let after = vst_state(&slot);
        assert_eq!(after.chunk, &preset.data[76..]);
        assert_eq!(before.program, b"\0\0\x10\0\0\0");
        assert_eq!(
            lines(plugin_chunk(&slot).unwrap()).last().unwrap(),
            "AHN0b2NrIC0gUm9jayBTbmFyZSAtIGFpbSBmb3IgNGRCIEdSAAAAAAA="
        );
        assert!(slot.raw.contains(&Node::Line("PRESETNAME \"stock - Rock Snare - aim for 4dB GR\"".into())));
        assert_eq!((after.inputs, after.outputs, after.words), (vec![1, 2, 4, 8], vec![1, 2], [0, 0]));
        let mut expected = capture(&slots()[0]).unwrap()[..76].to_vec();
        expected[64..68].copy_from_slice(&84u32.to_le_bytes());
        expected[72..76].fill(0);
        expected.extend(&preset.data[76..]);
        assert_eq!(capture(&slot).unwrap(), expected);
    }

    #[test]
    fn vst3_capture_is_header_and_state() {
        let data = capture(&slots()[1]).unwrap();
        assert_eq!(data.len(), 76 + 24);
        assert_eq!(&data[76..92], &[8, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xE0, 0x3F]);
        let mut changed = data.clone();
        changed[84..92].copy_from_slice(&0.75f64.to_le_bytes());
        let mut slot = slots().remove(1);
        assert!(apply(&mut slot, &changed, "x"));
        assert_eq!(capture(&slot).unwrap(), changed);
    }

    #[test]
    fn clap_and_jsfx_presets_are_text() {
        let mut clap = slots().remove(2);
        assert_eq!(
            String::from_utf8(capture(&clap).unwrap()).unwrap(),
            "CFG 4 0 0 \"\"\n<IN_PINS\n>\n<STATE\n  AAAAPw==\n>\n"
        );
        assert!(apply(&mut clap, b"<STATE\nAABAPw==\n>\n", "bright"));
        let chunk = plugin_chunk(&clap).unwrap();
        assert_eq!(chunk.find_line("CFG").unwrap(), ["4", "0", "0", "bright"]);
        let state = chunk.chunks("STATE").next().unwrap();
        assert_eq!(state.children, vec![Node::Line("AABAPw==".into())]);
        assert_eq!(chunk.chunks("IN_PINS").count(), 1);

        let mut js = slots().remove(3);
        assert!(capture(&js).unwrap().starts_with(b"-75 0 - -"));
        assert!(apply(&mut js, b"-12 0 - -", "quiet one"));
        assert_eq!(lines(plugin_chunk(&js).unwrap())[0], "-12 0 - - \"quiet one\"");
        assert_eq!(capture(&js).unwrap(), b"-12 0 - -");

        let mut lv2 = slots().remove(4);
        assert!(apply(&mut lv2, b"CFG 0 0 0 \"\"\nP gain 0.25\n", "low"));
        assert_eq!(lines(plugin_chunk(&lv2).unwrap()), ["CFG 0 0 0 low", "P gain 0.25"]);
    }

    #[test]
    fn long_presets_split_into_data_lines() {
        let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        let mut file = PresetFile::default();
        file.put(Preset { name: "big".into(), data });
        file.put(Preset { name: "small".into(), data: b"3 4".to_vec() });
        let text = file.to_text();
        let lengths: Vec<usize> = text.lines().filter(|l| l.starts_with("Data")).map(str::len).collect();
        assert_eq!(lengths, [32775, 32777, 32777, 32777, 32777, 32777, 3401, 13]);
        assert!(text.contains("[Preset1]\nData=33203487\nLen=3\nName=small\n"));
        assert_eq!(PresetFile::parse(&text), file);
        file.put(Preset { name: "small".into(), data: b"5".to_vec() });
        assert_eq!(file.presets.len(), 2);
    }
}
