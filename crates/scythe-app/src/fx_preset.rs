//! REAPER's per-plugin preset files (`presets/vst-<plugin>.ini`, `js-…`, `vst3-…`, `clap-…`,
//! `lv2-…`). Each `[PresetN]` section holds the preset's `Name`, its length `Len` and its
//! bytes as hex `Data` lines of at most 16384 bytes (`Data`, `Data_1`, …), every line ending
//! in a checksum byte: the sum of that line's bytes. The bytes are the plugin's state the way
//! the project stores it: the decoded `<VST` block without its program line, the slider line
//! of a `<JS`, or the text of the lines inside a `<CLAP`/`<LV2`.

use base64::Engine as _;
use scythe_core::FxSlot;
use scythe_core::rpp::{Chunk, Node, join_tokens, line_key, quote_always, tokenize};

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

/// Length of REAPER's header in front of a VST plugin's state: id, magic, the input and
/// output pin maps, then the state's size and two more words.
fn vst_header_len(header: &[u8]) -> Option<(usize, usize)> {
    let word = |at: usize| header.get(at..at + 4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes);
    let n_in = word(8)? as usize;
    let after_in = 12 + n_in * 8;
    let n_out = word(after_in)? as usize;
    let size_at = after_in + 4 + n_out * 8;
    let size = word(size_at)? as usize;
    (header.len() >= size_at + 12).then_some((size_at + 12, size))
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
            let (header_len, size) = vst_header_len(&data)?;
            data.truncate(header_len);
            let body: String = lines.get(1..lines.len().saturating_sub(1))?.concat();
            let mut state = b64.decode(body).ok()?;
            state.truncate(size);
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

/// Sets the state of the plugin in `slot` from the bytes of preset `name`, keeping the slot's
/// own pin mapping, and records the preset's name where REAPER does. Returns false when the
/// data does not fit the plugin.
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
            let line = Node::Line(format!("{} {}", values.join(" "), quote_always(name)));
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
        let at = slot.raw.iter().position(|n| matches!(n, Node::Chunk(_))).map_or(0, |i| i + 1);
        slot.raw.retain(|n| !matches!(n, Node::Line(l) if line_key(l.trim()) == "PRESETNAME"));
        slot.raw.insert(at.min(slot.raw.len()), Node::Line(format!("PRESETNAME {}", quote_always(name))));
    }
    applied
}

/// The `<VST` block is REAPER's header, the plugin state and a program line holding the
/// preset's name between NULs and a closing word.
fn apply_vst(chunk: &mut Chunk, data: &[u8], name: &str) -> bool {
    let b64 = base64::engine::general_purpose::STANDARD;
    let Some((header_len, size)) = vst_header_len(data) else { return false };
    let Some(state) = data.get(header_len..).map(|s| &s[..size.min(s.len())]) else { return false };
    let old = lines(chunk);
    let old_program = old.last().filter(|_| old.len() >= 2).and_then(|l| b64.decode(l).ok());
    let closing = old_program
        .as_deref()
        .and_then(|p| p.len().checked_sub(4).map(|at| p[at..].to_vec()))
        .unwrap_or_else(|| vec![0x10, 0, 0, 0]);
    let mut program = vec![0];
    program.extend(name.as_bytes());
    program.push(0);
    program.extend(closing);
    let reencoded = (old.len() >= 2).then(|| scythe_engine::vst2::ReaperVstState::reencode(&old, state)).flatten();
    let mut new = reencoded.unwrap_or_else(|| {
        let mut header = data[..header_len].to_vec();
        header[header_len - 12..header_len - 8].copy_from_slice(&(state.len() as u32).to_le_bytes());
        let mut out = vec![b64.encode(&header)];
        out.extend(b64.encode(state).as_bytes().chunks(128).map(|c| String::from_utf8_lossy(c).into_owned()));
        out.push(String::new());
        out
    });
    if let Some(last) = new.last_mut() {
        *last = b64.encode(&program);
    }
    chunk.children = new.into_iter().map(Node::Line).collect();
    true
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
    if let Some(Node::Line(cfg)) =
        chunk.children.iter_mut().find(|n| matches!(n, Node::Line(l) if line_key(l.trim()) == "CFG"))
    {
        let mut t = tokenize(cfg);
        t.resize(t.len().max(5), String::new());
        *cfg = format!("{} {} {}", join_tokens(&t[..4]), quote_always(name), join_tokens(&t[5..]));
        *cfg = cfg.trim_end().to_owned();
    }
    true
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

    #[test]
    fn applies_a_vst_preset_keeping_pins_and_program() {
        let mut slot = slots().remove(0);
        let before = vst_state(&slot);
        let preset = &PresetFile::parse(REACOMP_INI).presets[0];
        assert!(apply(&mut slot, &preset.data, &preset.name));
        let after = vst_state(&slot);
        assert_eq!(after.chunk, &preset.data[76..]);
        assert_eq!(before.program, b"\0\0\x10\0\0\0");
        assert_eq!(after.program, [b"\0", preset.name.as_bytes(), b"\0\x10\0\0\0"].concat());
        assert!(slot.raw.contains(&Node::Line("PRESETNAME \"stock - Rock Snare - aim for 4dB GR\"".into())));
        assert_eq!((after.inputs, after.outputs), (before.inputs, before.outputs));
        let mut expected = capture(&slots()[0]).unwrap()[..76].to_vec();
        expected[64..68].copy_from_slice(&84u32.to_le_bytes());
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
        assert_eq!(lines(plugin_chunk(&lv2).unwrap()), ["CFG 0 0 0 \"low\"", "P gain 0.25"]);
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
