//! ScyEQ-Pro in Scythe: the DSP of `scyeq_dsp` as one of the engine's effects.
//!
//! It is stored as a CLAP slot (`<CLAP "CLAP: ScyEQ-Pro (Scythe)" scythe.eq "">` with its
//! state in `<STATE`), a block REAPER keeps byte for byte when it does not know the plugin.
//! A slot of the VST3 plugin, which REAPER writes when that is the one inserted, runs as the
//! built-in EQ too and keeps its `<VST` block.

pub use scyeq_dsp::*;

use scythe_core::rpp::{Chunk, Node};

use crate::fx::{Effect, ProcessContext};

pub const NAME: &str = "ScyEQ-Pro";
pub const CLAP_ID: &str = "scythe.eq";
pub const DISPLAY: &str = "CLAP: ScyEQ-Pro (Scythe)";
/// The title projects saved before the EQ had its name carry; the slot is found by `CLAP_ID`.
pub const OLD_DISPLAY: &str = "CLAP: Scythe EQ (Scythe)";
pub const VST3_CLASS_ID: [u8; 16] = *b"ScytheScyEQPro01";
/// The title REAPER gives a slot of the VST3 plugin; the slot is found by `VST3_CLASS_ID`.
pub const VST3_DISPLAY: &str = "VST3: ScyEQ-Pro (Scythe)";
pub const STATUS: &str = "parametric EQ";

/// Whether `slot` holds a ScyEQ-Pro.
pub fn is_slot(slot: &scythe_core::FxSlot) -> bool {
    slot.raw.iter().any(|n| matches!(n, Node::Chunk(c) if is_chunk(c)))
}

/// Whether a slot's plugin block holds a ScyEQ-Pro, as the CLAP or the VST3 plugin.
pub(crate) fn is_chunk(c: &Chunk) -> bool {
    match c.name() {
        "CLAP" => c.args().get(1).is_some_and(|id| id == CLAP_ID),
        "VST" => is_vst3_chunk(c),
        _ => false,
    }
}

/// Whether a `<VST` block holds the VST3 plugin: its class id follows the numeric id, `123{…}`.
pub(crate) fn is_vst3_chunk(c: &Chunk) -> bool {
    let args = c.args();
    c.name() == "VST"
        && args.first().is_some_and(|d| d.starts_with("VST3"))
        && args.get(4).and_then(|a| a.split_once('{')).is_some_and(|(_, cid)| {
            cid.trim_end_matches('}').eq_ignore_ascii_case(&crate::vst3_host::cid_hex(&VST3_CLASS_ID))
        })
}

/// The EQ state a ScyEQ-Pro block holds: the CLAP `<STATE`, or the component state inside the
/// VST3 plugin's REAPER chunk. `None` when the block holds none.
pub(crate) fn chunk_state(c: &Chunk) -> Option<Vec<u8>> {
    if c.name() == "CLAP" {
        return crate::fx::clap_state(c);
    }
    let lines: Vec<String> = c
        .children
        .iter()
        .filter_map(|n| match n {
            Node::Line(l) => Some(l.trim().to_owned()),
            Node::Chunk(_) => None,
        })
        .collect();
    let reaper = crate::vst2::ReaperVstState::decode(&lines)?;
    let (component, _) = crate::vst3_host::split_reaper_state(&reaper.chunk)?;
    (!component.is_empty()).then(|| component.to_vec())
}

/// The values stored in a ScyEQ-Pro slot.
pub fn slot_values(slot: &scythe_core::FxSlot) -> Option<Vec<f64>> {
    let chunk = slot.raw.iter().find_map(|n| match n {
        Node::Chunk(c) if is_chunk(c) => Some(c),
        _ => None,
    })?;
    params::decode(&chunk_state(chunk)?).map(|d| d.values)
}

/// A new slot holding a ScyEQ-Pro with `state` (the default when `None`).
pub fn slot(state: Option<&[u8]>) -> scythe_core::FxSlot {
    use base64::Engine as _;
    let state = state.map_or_else(|| params::encode(&params::defaults(), &[], params::VERSION), <[u8]>::to_vec);
    let mut chunk = Chunk::new("CLAP", &[DISPLAY, CLAP_ID, ""]);
    chunk.line(["CFG", "4", "0", "0", ""]);
    let mut st = Chunk::new("STATE", &[]);
    let text = base64::engine::general_purpose::STANDARD.encode(&state);
    for line in text.as_bytes().chunks(128) {
        st.line([String::from_utf8_lossy(line)]);
    }
    chunk.push_chunk(st);
    scythe_core::FxSlot {
        name: DISPLAY.into(),
        enabled: true,
        raw: vec![
            Node::Line("BYPASS 0 0 0".into()),
            Node::Chunk(chunk),
            Node::Line(format!("FXID {}", scythe_core::model::new_guid())),
            Node::Line("WAK 0 0".into()),
        ],
    }
}

impl Effect for ScytheEq {
    fn process(&mut self, left: &mut [f32], right: &mut [f32], ctx: &ProcessContext) {
        // Offline renders run with `playing` set too; stopped, the position stands still.
        self.process_block(left, right, ctx.aux, ctx.playing.then_some(ctx.position));
        if let Some(sc) = ctx.aux {
            let sidechain = &self.meters().sidechain;
            if sidechain.active() {
                sidechain.tap(probe::Side::Pre, sc.0, sc.1, ctx.sample_rate);
            }
        }
    }

    fn reset(&mut self) {
        ScytheEq::reset(self);
    }

    fn latency(&mut self) -> usize {
        ScytheEq::latency(self)
    }

    fn params(&mut self) -> Vec<ParamInfo> {
        params::automatable().filter_map(params::info).collect()
    }

    fn param(&mut self, id: u32) -> Option<(f64, String)> {
        params::spec_of(id)?;
        let v = self.value(id);
        Some((v, params::format(id, v)))
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.set(id, value);
    }

    fn set_normalized(&mut self, index: usize, value: f64) -> bool {
        let Some(id) = params::automatable().nth(index) else { return false };
        self.set(id, params::from_normalized(id, value));
        true
    }

    fn normalized(&mut self, index: usize) -> Option<f64> {
        let id = params::automatable().nth(index)?;
        Some(params::to_normalized(id, self.value(id)))
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        Some(self.state())
    }

    fn update_state(&mut self, chunk: &[u8]) -> bool {
        self.load_state(chunk)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::params::{BAND_STRIDE, GLOBAL, band, global};
    use super::*;

    fn ctx(sr: u32) -> ProcessContext<'static> {
        ProcessContext { sample_rate: sr, ..crate::stock::testing::context() }
    }

    fn dynamic_band(eq: &mut ScytheEq, b: usize, mode: f64, range: f64, threshold: f64) {
        let id = |o: u32| b as u32 * BAND_STRIDE + o;
        eq.set(id(band::USED), 1.0);
        eq.set(id(band::SHAPE), filters::Shape::Bell.index() as f64);
        eq.set(id(band::FREQ), 1000.0);
        eq.set(id(band::GAIN), 0.0);
        eq.set(id(band::Q), 2.0);
        eq.set(id(band::DYNAMIC), mode);
        eq.set(id(band::RANGE), range);
        eq.set(id(band::THRESHOLD), threshold);
        eq.set(id(band::ATTACK), 2.0);
        eq.set(id(band::RELEASE), 50.0);
    }

    /// The 32-frame grid follows the sample position: a dynamic and a spectral band give the same
    /// output whatever block sizes the host uses, and so does a stopped transport counting frames.
    #[test]
    fn dynamic_bands_ignore_block_size() {
        let sr = 48000u32;
        let mut seed = 11u64;
        let mut next = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            seed >> 33
        };
        let noise: Vec<f32> = (0..2 * sr as usize).map(|_| (next() as f32 / (1u64 << 31) as f32 - 0.5) * 0.8).collect();
        let random: Vec<usize> = std::iter::repeat_with(|| 1 + next() as usize % 700).take(noise.len()).collect();
        for mode in [0.0, 2.0] {
            let render = |sizes: &[usize], playing: bool, from: i64| {
                let mut eq = ScytheEq::new(sr);
                eq.set(GLOBAL + global::PHASE, mode);
                dynamic_band(&mut eq, 0, 1.0, -9.0, -30.0);
                dynamic_band(&mut eq, 1, 2.0, -12.0, -30.0);
                eq.set(BAND_STRIDE + band::FREQ, 4000.0);
                let (mut l, mut r) = (noise.clone(), noise.iter().map(|x| -0.5 * x).collect::<Vec<_>>());
                let mut at = 0;
                for &n in sizes.iter().cycle() {
                    if at == l.len() {
                        break;
                    }
                    let n = n.min(l.len() - at);
                    let c = ProcessContext { position: from + at as i64, playing, ..ctx(sr) };
                    eq.process(&mut l[at..at + n], &mut r[at..at + n], &c);
                    at += n;
                }
                (l, r)
            };
            let reference = render(&[512], true, 0);
            assert!(reference.0.iter().zip(&noise).any(|(a, b)| a != b));
            for (name, sizes, playing) in
                [("37", &[37][..], true), ("random", &random, true), ("random, stopped", &random, false)]
            {
                let out = render(sizes, playing, 0);
                assert!(out == reference, "mode {mode}, blocks of {name}");
            }
            // Playback starting part way into a grid cell.
            assert!(render(&random, true, 1013) == render(&[512], true, 1013), "mode {mode}, from 1013");
        }
    }
}
