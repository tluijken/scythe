//! Built-in effects offered in the plugin browser. They are stored exactly like REAPER stores
//! its own effects (ReaEQ, ReaGate, …), so projects open in both programs.

use base64::Engine as _;
use scythe_core::FxSlot;
use scythe_core::rpp::{Chunk, Node};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    Eq,
    Gate,
    Compressor,
    Delay,
    Reverb,
    Limiter,
    Multiband,
    Verbate,
    Synth,
    MidiControl,
}

/// Default states as REAPER writes them for a freshly added instance.
const GATE_DEFAULT: &str = "776t3g3wrd6c0QczppvEOwrXozwAAAAAAAAAAAAAgD8AAAAAAAAAAAAAAACc0QczAACAP5zRBzMAAIA/AAAAAAAAAAAAAAAALBYLPwAAAAAAAAAAAAAAAAAAAAA=";
const COMP_DEFAULT: &str = "776t3g3wrd4AAIA/ED74PKabxDsK16M8AAAAAAAAAAAAAIA/AAAAAAAAAAAAAAAAnNEHMwAAgD8AAAAAzcxMPQAAAAAAAAAAAAAAAAAAgD4AAAAAAAAAAAAAAAA=";
const DELAY_DEFAULT: &str =
    "AAAAAAAAAAABAAAALAAAAAIAAAAAAAAAQwIAPwAAgD8AAAAAAACAPwAAAAAAAAA8nNEHMwAAgD8AAAAAAACAPwAAgD8AAIA/AAAAPw==";
/// A fresh ReaControlMIDI's state, without the bank file path REAPER fills in.
const CONTROL_MIDI_DEFAULT: &str = "/////wAAAAAAAAAAAAAAAAkAAAAMAAAAAQAAAP8/AAAAIAAAACAAAAAAAAABAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAEAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABgAAAE1ham9yAA0AAAAxMDIwMzQwNTA2MDcAAQAAAAAAAAAAAAAAAAoAAAANAAAAAQAAAAAAAAAAAAAAAAAAAA==";
const LIMIT_DEFAULT: &str = "AwAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAgAAAAACAAAAAAAAAAAAAD+fduMQw8Y/";
const XCOMP_DEFAULT: &str = "OAAAAAQAAAAAAAAAAABpQAAAAAAAAPA/AAAAAAAA8D8AAAAAAAAAQAAAAAAAAAAADwAAAJYAAAAFAAAAEQAAAAAAAAAAQI9AAAAAAAAA8D8AAAAAAADwPwAAAAAAAABAAAAAAAAAAAAPAAAAlgAAAAUAAAARAAAAAAAAAACIs0AAAAAAAADwPwAAAAAAAPA/AAAAAAAAAEAAAAAAAAAAAA8AAACWAAAABQAAABEAAAAAAAAAAHDXQAAAAAAAAPA/AAAAAAAA8D8AAAAAAAAAQAAAAAAAAAAADwAAAJYAAAAFAAAAEQAAAAEAAAABAAAAAAAAAAAA8D8AAAAAAAAAAAAAAAAAAAAAAgAAAA==";

impl Builtin {
    pub const ALL: [Builtin; 10] = [
        Self::Eq,
        Self::Gate,
        Self::Compressor,
        Self::Delay,
        Self::Reverb,
        Self::Limiter,
        Self::Multiband,
        Self::Verbate,
        Self::Synth,
        Self::MidiControl,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Eq => "Equalizer",
            Self::Gate => "Gate",
            Self::Compressor => "Compressor",
            Self::Delay => "Delay",
            Self::Reverb => "Convolution reverb",
            Self::Limiter => "Limiter",
            Self::Multiband => "Multiband compressor",
            Self::Verbate => "Algorithmic reverb",
            Self::Synth => "Synth",
            Self::MidiControl => "MIDI control",
        }
    }

    /// The stock plugin REAPER knows the equivalent effect as.
    pub fn stock(self) -> &'static crate::stock::StockPlugin {
        let name = match self {
            Self::Eq => "ReaEQ",
            Self::Gate => "ReaGate",
            Self::Compressor => "ReaComp",
            Self::Delay => "ReaDelay",
            Self::Reverb => "ReaVerb",
            Self::Limiter => "ReaLimit",
            Self::Multiband => "ReaXcomp",
            Self::Verbate => "ReaVerbate",
            Self::Synth => "ReaSynth",
            Self::MidiControl => "ReaControlMIDI",
        };
        crate::stock::by_name(name).expect("built-ins are stock plugins")
    }

    pub fn default_state(self) -> Vec<u8> {
        let b64 = base64::engine::general_purpose::STANDARD;
        match self {
            Self::Eq => {
                eq_state(&[(0, 100.0, 1.0, 0.8), (8, 300.0, 1.0, 2.0), (8, 1000.0, 1.0, 2.0), (1, 5000.0, 1.0, 0.8)])
            }
            Self::Gate => b64.decode(GATE_DEFAULT).unwrap(),
            Self::Compressor => b64.decode(COMP_DEFAULT).unwrap(),
            Self::Delay => b64.decode(DELAY_DEFAULT).unwrap(),
            Self::Reverb => reverb_state(None, 0.25, 1.0),
            Self::Limiter => b64.decode(LIMIT_DEFAULT).unwrap(),
            Self::Multiband => b64.decode(XCOMP_DEFAULT).unwrap(),
            Self::Verbate => crate::stock::marked_chunk(&crate::stock::reaverbate::DEFAULTS),
            Self::Synth => crate::stock::marked_chunk(&crate::stock::reasynth::DEFAULTS),
            Self::MidiControl => b64.decode(CONTROL_MIDI_DEFAULT).unwrap(),
        }
    }

    /// A ready-to-use FX slot with the default state.
    pub fn slot(self) -> FxSlot {
        self.slot_with(&self.default_state())
    }

    /// An FX slot holding `state` in REAPER's VST chunk layout.
    pub fn slot_with(self, state: &[u8]) -> FxSlot {
        let stock = self.stock();
        let display = stock.display();
        let id = format!("{}<{}>", stock.vst_id, stock.guid);
        let mut chunk = Chunk::new("VST", &[&display, stock.file, "0", "", &id, ""]);
        let lines = if matches!(self, Self::Gate | Self::Compressor) {
            sidechain_vst_lines(stock.vst_id, state)
        } else {
            vst_lines(stock.vst_id, state)
        };
        for line in lines {
            chunk.line([line]);
        }
        FxSlot {
            name: display,
            enabled: true,
            raw: vec![
                Node::Line("BYPASS 0 0 0".into()),
                Node::Chunk(chunk),
                Node::Line(format!("FXID {}", scythe_core::model::new_guid())),
                Node::Line("WAK 0 0".into()),
            ],
        }
    }
}

/// The state block REAPER writes for ReaGate and ReaComp: four input pins (the last two the
/// sidechain, magic 0xfeed5eef), and the first flag word 0, which marks the data as the plugin's
/// parameters. REAPER ignores these states when written like other plugins'.
pub fn sidechain_vst_lines(unique_id: u32, chunk: &[u8]) -> Vec<String> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut h = Vec::new();
    h.extend(unique_id.to_le_bytes());
    h.extend(0xfeed_5eef_u32.to_le_bytes());
    h.extend(4u32.to_le_bytes());
    for mask in [1u64, 2, 4, 8] {
        h.extend(mask.to_le_bytes());
    }
    h.extend(2u32.to_le_bytes());
    for mask in [1u64, 2] {
        h.extend(mask.to_le_bytes());
    }
    h.extend((chunk.len() as u32).to_le_bytes());
    h.extend(0u32.to_le_bytes());
    h.extend(0x0010_0000u32.to_le_bytes());
    let mut lines = vec![b64.encode(&h)];
    lines.extend(b64.encode(chunk).as_bytes().chunks(128).map(|c| String::from_utf8_lossy(c).into_owned()));
    lines.push(b64.encode(b"\0Program 1\0\x10\0\0\0"));
    lines
}

/// REAPER's VST state block: a header (id, magic, two stereo pin maps, chunk size), the plugin
/// chunk in base64 lines of 128 characters, and a program line.
pub fn vst_lines(unique_id: u32, chunk: &[u8]) -> Vec<String> {
    vst_lines_with(unique_id, chunk, 0x0010_0000)
}

/// Like `vst_lines`, with the header's last field given: REAPER writes 0x100000 for VST 2
/// and 0xffff for VST 3.
pub fn vst_lines_with(unique_id: u32, chunk: &[u8], trailer: u32) -> Vec<String> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut h = Vec::new();
    h.extend(unique_id.to_le_bytes());
    h.extend(0xfeed_5eee_u32.to_le_bytes());
    h.extend(2u32.to_le_bytes());
    h.extend(1u64.to_le_bytes());
    h.extend(2u64.to_le_bytes());
    h.extend(2u32.to_le_bytes());
    h.extend(1u64.to_le_bytes());
    h.extend(2u64.to_le_bytes());
    h.extend((chunk.len() as u32).to_le_bytes());
    h.extend(1u32.to_le_bytes());
    h.extend(trailer.to_le_bytes());
    let data = b64.encode(chunk);
    let mut lines = vec![b64.encode(&h)];
    lines.extend(data.as_bytes().chunks(128).map(|c| String::from_utf8_lossy(c).into_owned()));
    lines.push(b64.encode(b"\0Program 1\0\x10\0\0\0"));
    lines
}

/// ReaEQ state: version 33, the bands `(type, frequency, gain, bandwidth)`, then the trailer
/// with unity output gain.
pub fn eq_state(bands: &[(u32, f64, f64, f64)]) -> Vec<u8> {
    let mut d = Vec::new();
    d.extend(33u32.to_le_bytes());
    d.extend((bands.len() as u32).to_le_bytes());
    for &(kind, freq, gain, bw) in bands {
        d.extend(kind.to_le_bytes());
        d.extend(1u32.to_le_bytes());
        d.extend(freq.to_le_bytes());
        d.extend(gain.to_le_bytes());
        d.extend(bw.to_le_bytes());
        d.push(1);
    }
    d.extend(1u32.to_le_bytes());
    d.extend(1u32.to_le_bytes());
    d.extend(1.0f64.to_le_bytes());
    for v in [0u32, 644, 1424, 2] {
        d.extend(v.to_le_bytes());
    }
    d
}

/// ReaVerb state with a file loader for `ir` (none: dry only).
pub fn reverb_state(ir: Option<&std::path::Path>, wet: f32, dry: f32) -> Vec<u8> {
    let mut d: Vec<u8> =
        [-9.0f32, wet, dry, 0.0, 1.0, 1.0, 0.5, 1.0, 1.0, 2.0].iter().flat_map(|v| v.to_le_bytes()).collect();
    if let Some(ir) = ir {
        let path = format!("{}\0", ir.display());
        d.extend(b"FILELDR\0");
        d.extend(((4 + path.len()) as u32).to_le_bytes());
        d.extend(0x3cu32.to_le_bytes());
        d.extend(path.as_bytes());
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::FxRegistry;

    #[test]
    fn edits_are_saved_and_untouched_slots_stay_byte_identical() {
        let mut project = scythe_core::Project::default();
        let track = project.add_track("t");
        let t = project.track_mut(track).unwrap();
        t.fx.push(Builtin::Eq.slot());
        t.fx.push(Builtin::Delay.slot());
        let before = project.clone();
        let mut fx = FxRegistry::new(48000);
        let chain = fx.chain(Some(track), &project.tracks[0].fx);
        fx.store_states(&mut project);
        assert_eq!(project, before, "nothing edited, nothing rewritten");

        // Band 2 gain to +6 dB, delay time to 250 ms.
        chain[0].with(|e| e.set_param(4 + 2, 6.0));
        chain[1].with(|e| e.set_param(2, 250.0));
        fx.store_states(&mut project);
        let reloaded = scythe_core::rpp::project_from_rpp(&scythe_core::rpp::project_to_rpp(&project)).unwrap();
        let mut fresh = FxRegistry::new(48000);
        let chain = fresh.chain(Some(reloaded.tracks[0].id), &reloaded.tracks[0].fx);
        let gain = chain[0].with(|e| e.param(6)).flatten().unwrap().0;
        let time = chain[1].with(|e| e.param(2)).flatten().unwrap().0;
        assert!((gain - 6.0).abs() < 1e-9 && (time - 250.0).abs() < 1e-3, "{gain} {time}");
    }

    /// A gate saved with the plain header (as Scythe once wrote it) gets REAPER's sidechain
    /// header when an edit is stored.
    #[test]
    fn edited_gates_get_the_sidechain_header() {
        let mut project = scythe_core::Project::default();
        let track = project.add_track("t");
        let mut slot = Builtin::Gate.slot();
        if let Some(Node::Chunk(c)) = slot.raw.iter_mut().find(|n| matches!(n, Node::Chunk(_))) {
            c.children = vst_lines(1919248244, &Builtin::Gate.default_state()).into_iter().map(Node::Line).collect();
        }
        project.track_mut(track).unwrap().fx.push(slot);
        let mut fx = FxRegistry::new(48000);
        let chain = fx.chain(Some(track), &project.tracks[0].fx);
        chain[0].with(|e| e.set_param(0, -30.0));
        fx.store_states(&mut project);
        let Some(Node::Chunk(c)) = project.tracks[0].fx[0].raw.iter().find(|n| matches!(n, Node::Chunk(_))) else {
            panic!()
        };
        let lines: Vec<String> =
            c.children.iter().filter_map(|n| if let Node::Line(l) = n { Some(l.clone()) } else { None }).collect();
        let state = crate::vst2::ReaperVstState::decode(&lines).unwrap();
        assert_eq!((state.inputs.len(), state.words[0]), (4, 0));
    }

    #[test]
    fn every_builtin_loads_natively() {
        let mut fx = FxRegistry::new(48000);
        let slots: Vec<FxSlot> = Builtin::ALL.iter().map(|b| b.slot()).collect();
        let track = scythe_core::TrackId::new();
        let chain = fx.chain(Some(track), &slots);
        assert_eq!(chain.len(), Builtin::ALL.len());
        for (i, slot) in slots.iter().enumerate() {
            let status = fx.status(Some(track), slot, i).unwrap();
            assert!(matches!(status, crate::fx::FxStatus::Native(_)), "{}: {status:?}", slot.name);
        }
    }
}
