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
/// VST3 plugin's REAPER chunk. `None` when the block holds none (a new VST3 slot holds an empty
/// component); an error when it holds one that does not decode, which must not be replaced.
pub(crate) fn chunk_state(c: &Chunk) -> Result<Option<Vec<u8>>, &'static str> {
    const UNREADABLE: &str = "unreadable ScyEQ-Pro state";
    if c.name() == "CLAP" {
        if c.chunks("STATE").next().is_none() {
            return Ok(None);
        }
        return crate::fx::clap_state(c).map(Some).ok_or(UNREADABLE);
    }
    let lines: Vec<String> = c
        .children
        .iter()
        .filter_map(|n| match n {
            Node::Line(l) => Some(l.trim().to_owned()),
            Node::Chunk(_) => None,
        })
        .collect();
    if lines.is_empty() {
        return Ok(None);
    }
    let reaper = crate::vst2::ReaperVstState::decode(&lines).ok_or(UNREADABLE)?;
    let (component, _) = crate::vst3_host::split_reaper_state(&reaper.chunk).ok_or(UNREADABLE)?;
    Ok((!component.is_empty()).then(|| component.to_vec()))
}

/// The values stored in a ScyEQ-Pro slot.
pub fn slot_values(slot: &scythe_core::FxSlot) -> Option<Vec<f64>> {
    let chunk = slot.raw.iter().find_map(|n| match n {
        Node::Chunk(c) if is_chunk(c) => Some(c),
        _ => None,
    })?;
    params::decode(&chunk_state(chunk).ok()??).map(|d| d.values)
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

/// Whether `slot` holds the CLAP plugin's block, the form Scythe itself stores ScyEQ-Pro in.
pub(crate) fn is_clap_slot(slot: &scythe_core::FxSlot) -> bool {
    slot.raw.iter().any(|n| matches!(n, Node::Chunk(c) if c.name() == "CLAP" && is_chunk(c)))
}

/// The parameter at `index` in the host's list. The plugin lists every stored value but solo and
/// audition, so up to the analyzer settings the list is `params::automatable`.
fn param_at(index: usize) -> Option<u32> {
    params::automatable().nth(index)
}

/// How REAPER names parameter `index` of a ScyEQ-Pro slot in `PARMENV`: `<index>:<id>`.
pub fn envelope_spec(slot: &scythe_core::FxSlot, index: usize) -> Option<String> {
    is_slot(slot).then_some(())?;
    param_at(index).map(|id| format!("{index}:{id}"))
}

/// Rewrites the envelopes of a CLAP slot that Scythe stored before they followed REAPER: from
/// 0..1, mapped as `params::from_normalized` maps, to the parameter's own units and range.
/// Parameters whose range is 0..1 read the same either way. Returns whether anything changed.
/// Projects convert when they open (`upgrade::upgrade_project`, which also moves baselines and
/// pooled items); the engine plays a slot through this too, for projects rendered from a file.
pub fn upgrade_envelopes(slot: &mut scythe_core::FxSlot) -> bool {
    if !has_old_envelopes(slot) {
        return false;
    }
    let mut changed = false;
    for mut pe in slot.parameter_envelopes() {
        let Some((index, id)) = pe.param().and_then(|i| Some((i, param_at(i)?))) else { continue };
        let Some(spec) = params::spec_of(id) else { continue };
        if (pe.min, pe.max) != (0.0, 1.0) || (spec.min, spec.max) == (0.0, 1.0) {
            continue;
        }
        for p in &mut pe.envelope.points {
            p.value = params::from_normalized(id, p.value);
        }
        pe.default = (spec.min + spec.max) / 2.0;
        (pe.min, pe.max) = (spec.min, spec.max);
        pe.envelope.range = Some((spec.min, spec.max));
        pe.spec = format!("{index}:{id}");
        slot.set_parameter_envelope(&pe);
        changed = true;
    }
    changed
}

/// Whether [`upgrade_envelopes`] would change `slot`, read without copying it: the engine asks
/// on every chain build.
pub fn has_old_envelopes(slot: &scythe_core::FxSlot) -> bool {
    is_clap_slot(slot)
        && slot.raw.iter().any(|n| {
            let Node::Chunk(c) = n else { return false };
            if c.name() != "PARMENV" {
                return false;
            }
            let args = c.args();
            let num = |i: usize| args.get(i).and_then(|v| v.parse::<f64>().ok());
            let index = args.first().and_then(|s| s.split(':').next()?.parse::<usize>().ok());
            (num(1), num(2)) == (Some(0.0), Some(1.0))
                && index.and_then(param_at).and_then(params::spec_of).is_some_and(|s| (s.min, s.max) != (0.0, 1.0))
        })
}

/// A slot's parameter as REAPER sees the CLAP plugin's: in its own units, 0..1 being its range
/// end to end, also for frequencies and times.
fn linear(id: u32, normalized: f64) -> f64 {
    let spec = params::spec_of(id).map_or((0.0, 1.0), |s| (s.min, s.max));
    spec.0 + normalized.clamp(0.0, 1.0) * (spec.1 - spec.0)
}

fn linear_position(id: u32, v: f64) -> f64 {
    let spec = params::spec_of(id).map_or((0.0, 1.0), |s| (s.min, s.max));
    if spec.1 > spec.0 { ((v - spec.0) / (spec.1 - spec.0)).clamp(0.0, 1.0) } else { 0.0 }
}

/// The CLAP slot, which is also how Scythe stores ScyEQ-Pro: envelopes in the parameters' own
/// units, read at the end of each block, as REAPER automates the CLAP plugin.
impl Effect for ScytheEq {
    fn process(&mut self, left: &mut [f32], right: &mut [f32], ctx: &ProcessContext) {
        // As plugin-format runs the plugin, so built in and hosted it sounds the same to the bit on
        // every architecture. Only ScyEQ-Pro: Scythe's other effects are nulled against REAPER as
        // they run now, and the audio thread keeps the floating-point state it starts with.
        let flushed = crate::denormals::FlushDenormals::new();
        // Offline renders run with `playing` set too; stopped, the position stands still.
        self.process_block(left, right, ctx.aux, ctx.playing.then_some(ctx.position));
        drop(flushed);
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
        let Some(id) = param_at(index) else { return false };
        self.set(id, linear(id, value));
        true
    }

    fn normalized(&mut self, index: usize) -> Option<f64> {
        let id = param_at(index)?;
        Some(linear_position(id, self.value(id)))
    }

    fn param_range(&mut self, index: usize) -> Option<(f64, f64)> {
        params::spec_of(param_at(index)?).map(|s| (s.min, s.max))
    }

    fn automates_at_block_end(&self) -> bool {
        true
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

/// A slot of the VST3 plugin: envelopes hold VST3 normalised values, which the plugin maps as
/// `params::from_normalized` does (logarithmic for frequencies, Q and times).
pub struct Vst3Eq(pub ScytheEq);

impl Effect for Vst3Eq {
    fn process(&mut self, left: &mut [f32], right: &mut [f32], ctx: &ProcessContext) {
        self.0.process(left, right, ctx);
    }

    fn reset(&mut self) {
        Effect::reset(&mut self.0);
    }

    fn latency(&mut self) -> usize {
        Effect::latency(&mut self.0)
    }

    fn params(&mut self) -> Vec<ParamInfo> {
        self.0.params()
    }

    fn param(&mut self, id: u32) -> Option<(f64, String)> {
        self.0.param(id)
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.0.set(id, value);
    }

    fn set_normalized(&mut self, index: usize, value: f64) -> bool {
        let Some(id) = param_at(index) else { return false };
        self.0.set(id, params::from_normalized(id, value));
        true
    }

    fn normalized(&mut self, index: usize) -> Option<f64> {
        let id = param_at(index)?;
        Some(params::to_normalized(id, self.0.value(id)))
    }

    fn automates_at_block_end(&self) -> bool {
        true
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        self.0.save_state()
    }

    fn update_state(&mut self, chunk: &[u8]) -> bool {
        self.0.update_state(chunk)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(&mut self.0)
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

#[cfg(test)]
mod host_tests {
    use super::*;

    /// The VST3 slot maps a normalised value to the same value the plugin's VST3 wrapper does, for
    /// every parameter a host lists.
    #[test]
    fn vst3_mapping_matches_the_plugin() {
        let mut eq = Vst3Eq(ScytheEq::new(48_000));
        let infos = scyeq_plugin::params::infos();
        for (index, id) in params::automatable().enumerate() {
            let info = infos.iter().find(|p| p.id == id).unwrap();
            assert_eq!(infos.iter().position(|p| p.id == id), Some(index), "{}", params::name(id));
            for n in [0.0, 0.13, 0.5, 0.77, 1.0] {
                assert!(eq.set_normalized(index, n));
                assert_eq!(eq.0.value(id).to_bits(), info.plain(n).to_bits(), "{} at {n}", params::name(id));
            }
        }
    }

    /// The CLAP slot, Scythe's own form, takes values over the parameter's range end to end.
    #[test]
    fn clap_mapping_is_linear() {
        let mut eq = ScytheEq::new(48_000);
        let freq = params::band_param(0, params::band::FREQ);
        assert_eq!(eq.param_range(freq as usize), Some((10.0, 30000.0)));
        assert!(eq.set_normalized(freq as usize, 0.5));
        assert_eq!(eq.value(freq), 15005.0);
        assert_eq!(eq.normalized(freq as usize), Some(0.5));
        assert!(eq.automates_at_block_end());
        assert!(Vst3Eq(ScytheEq::new(48_000)).automates_at_block_end());
    }

    fn slot_with(envelopes: &str) -> scythe_core::FxSlot {
        let mut slot = slot(None);
        let text = format!("<X\n{envelopes}>\n");
        let parsed = scythe_core::rpp::parse(&text).unwrap();
        let wak = slot.raw.len() - 1;
        slot.raw.splice(wak..wak, parsed.children);
        slot
    }

    #[test]
    fn envelopes_are_named_as_reaper_names_them() {
        let slot = slot(None);
        assert_eq!(envelope_spec(&slot, 3).as_deref(), Some("3:3"));
        assert_eq!(envelope_spec(&slot, 23).as_deref(), Some("23:28"));
        let other = scythe_core::FxSlot { name: "JS".into(), enabled: true, raw: Vec::new() };
        assert_eq!(envelope_spec(&other, 3), None);
    }

    /// An envelope Scythe stored on 0..1 for a parameter with another range moves to the
    /// parameter's units; 0..1 parameters and envelopes already in units stay as they are.
    #[test]
    fn old_envelopes_move_to_the_parameters_units() {
        let mut slot = slot_with(concat!(
            "<PARMENV 2 0 1 0.25\n  ACT 1 -1\n  PT 0 0.5 0\n  PT 1 1 0\n>\n",
            "<PARMENV 0 0 1 0\n  ACT 1 -1\n  PT 0 1 1\n>\n",
            "<PARMENV 3:3 -30 30 0\n  ACT 1 -1\n  PT 0 6 0\n>\n",
        ));
        let before = slot.clone();
        assert!(has_old_envelopes(&slot));
        assert!(upgrade_envelopes(&mut slot));
        let freq = slot.parameter_envelope("2").unwrap();
        assert_eq!((freq.spec.as_str(), freq.min, freq.max), ("2:2", 10.0, 30000.0));
        assert_eq!(freq.default, 15005.0, "the middle of the range, as REAPER writes it");
        let values: Vec<f64> = freq.envelope.points.iter().map(|p| p.value).collect();
        assert_eq!(values, [params::from_normalized(2, 0.5), 30000.0]);
        assert_eq!(slot.parameter_envelope("0"), before.parameter_envelope("0"));
        assert_eq!(slot.parameter_envelope("3"), before.parameter_envelope("3"));
        let upgraded = slot.clone();
        assert!(!has_old_envelopes(&slot));
        assert!(!upgrade_envelopes(&mut slot), "a second load changes nothing");
        assert_eq!(slot, upgraded);
    }
}
