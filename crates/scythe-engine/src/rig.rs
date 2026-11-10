//! A guitar rig: one REAPER FX container named "Rig: <amp> + <cab>" holding a noise gate
//! (ReaGate), the Neural Amp Modeler slot, the cabinet's impulse response (ReaVerb, fully wet,
//! with an optional second one in parallel to blend with), an EQ with low and high cut
//! (ReaEQ) and the output level (a ReaEQ whose only band is off). Each stage is an ordinary
//! slot with REAPER's own bypass, so REAPER opens and plays the rig as the container it is;
//! Scythe shows it as one slot and edits it as one rig.

use std::path::Path;

use scythe_core::FxSlot;
use scythe_core::fx_chain::Parallel;
use scythe_core::rpp::{Node, join_tokens, tokenize};
use serde::{Deserialize, Serialize};

use crate::builtin::{Builtin, eq_chunk, reverb_state};
use crate::fx::{clear_nam_model, is_nam, nam_settings, set_nam_levels, set_nam_model, slot_id};
use crate::reaeq::{BandKind, ReaEqCompat};
use crate::reaverb::ReaVerbSettings;
use crate::stock::reagate::Gate;
use crate::vst2::ReaperVstState;

/// How a rig container's name starts.
pub const RIG_PREFIX: &str = "Rig";

/// Every setting of a rig, as presets and snapshots keep it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RigSettings {
    pub gate: GateSettings,
    pub amp: AmpSettings,
    pub cab: CabSettings,
    pub eq: EqSettings,
    /// The rig's output level in dB.
    pub output_db: f64,
}

impl Default for RigSettings {
    fn default() -> Self {
        Self {
            gate: GateSettings::default(),
            amp: AmpSettings::default(),
            cab: CabSettings::default(),
            eq: EqSettings::default(),
            output_db: 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GateSettings {
    pub enabled: bool,
    pub threshold_db: f64,
    pub attack_ms: f64,
    pub release_ms: f64,
    /// How far below the threshold the gate closes again, in dB (ReaGate's hysteresis).
    pub hysteresis_db: f64,
}

impl Default for GateSettings {
    fn default() -> Self {
        Self { enabled: false, threshold_db: -50.0, attack_ms: 3.0, release_ms: 100.0, hysteresis_db: 0.0 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AmpSettings {
    pub enabled: bool,
    /// The `.nam` file, as the slot names it.
    pub model: Option<String>,
    pub input_db: f64,
    pub output_db: f64,
}

impl Default for AmpSettings {
    fn default() -> Self {
        Self { enabled: true, model: None, input_db: 0.0, output_db: 0.0 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CabSettings {
    pub enabled: bool,
    pub ir: Option<String>,
    /// A second impulse response mixed in by `blend`.
    pub ir2: Option<String>,
    /// The second response's share, 0-1.
    pub blend: f64,
    pub level_db: f64,
    /// The cab follows the amp model: off for captures that include a cabinet, on for
    /// amp-only ones. Switching the cab by hand turns this off.
    pub auto: bool,
}

impl Default for CabSettings {
    fn default() -> Self {
        Self { enabled: true, ir: None, ir2: None, blend: 0.0, level_db: 0.0, auto: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqSettings {
    pub enabled: bool,
    /// Low cut (high-pass) frequency in Hz; `None` when off.
    pub low_cut: Option<f64>,
    /// High cut (low-pass) frequency in Hz; `None` when off.
    pub high_cut: Option<f64>,
    pub bass_db: f64,
    pub mid_db: f64,
    pub treble_db: f64,
    /// Where the bass shelf, the mid peak and the treble shelf sit, in Hz.
    pub bass_hz: f64,
    pub mid_hz: f64,
    pub treble_hz: f64,
}

impl Default for EqSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            low_cut: Some(80.0),
            high_cut: Some(8000.0),
            bass_db: 0.0,
            mid_db: 0.0,
            treble_db: 0.0,
            bass_hz: BASS_HZ,
            mid_hz: MID_HZ,
            treble_hz: TREBLE_HZ,
        }
    }
}

/// The stages of a rig, in signal order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Gate,
    Amp,
    Cab,
    Eq,
    Output,
}

impl Stage {
    pub const ALL: [Stage; 5] = [Self::Gate, Self::Amp, Self::Cab, Self::Eq, Self::Output];

    pub fn label(self) -> &'static str {
        match self {
            Self::Gate => "Gate",
            Self::Amp => "Amp",
            Self::Cab => "Cab",
            Self::Eq => "EQ",
            Self::Output => "Output",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.label().eq_ignore_ascii_case(name))
    }
}

/// Where each stage sits among a rig container's effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Layout {
    pub gate: Option<usize>,
    pub amp: usize,
    pub cab: Option<usize>,
    /// The second impulse response, parallel to the first.
    pub cab2: Option<usize>,
    pub eq: Option<usize>,
    pub output: Option<usize>,
}

impl Layout {
    /// The slots a stage switches together.
    pub fn slots(&self, stage: Stage) -> Vec<usize> {
        match stage {
            Stage::Gate => self.gate.into_iter().collect(),
            Stage::Amp => vec![self.amp],
            Stage::Cab => self.cab.into_iter().chain(self.cab2).collect(),
            Stage::Eq => self.eq.into_iter().collect(),
            Stage::Output => self.output.into_iter().collect(),
        }
    }
}

const EQ_BANDS: [BandKind; 5] =
    [BandKind::HighPass, BandKind::LowShelf, BandKind::Peak, BandKind::HighShelf, BandKind::LowPass];
const BASS_HZ: f64 = 120.0;
const MID_HZ: f64 = 800.0;
const TREBLE_HZ: f64 = 3200.0;
/// Bandwidth of the cuts in octaves: close to a Butterworth response.
const CUT_WIDTH: f64 = 1.9;

fn plugin_display(slot: &FxSlot) -> Option<String> {
    slot.plugin().filter(|c| c.name() == "VST").and_then(|c| c.args().into_iter().next())
}

fn is_stock(slot: &FxSlot, name: &str) -> bool {
    plugin_display(slot).and_then(|d| crate::stock::find(&d)).is_some_and(|s| s.name == name)
}

fn vst_lines_of(slot: &FxSlot) -> Vec<String> {
    slot.plugin()
        .map(|c| {
            c.children
                .iter()
                .filter_map(|n| match n {
                    Node::Line(l) => Some(l.trim().to_owned()),
                    Node::Chunk(_) => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A stock slot's own state chunk.
fn stock_chunk(slot: &FxSlot) -> Option<Vec<u8>> {
    ReaperVstState::decode(&vst_lines_of(slot)).map(|s| s.chunk)
}

/// Replaces a stock slot's state chunk, keeping its header and program.
fn set_stock_chunk(slot: &mut FxSlot, chunk: &[u8]) {
    let Some(lines) = ReaperVstState::reencode(&vst_lines_of(slot), chunk) else { return };
    let Some(plugin) = slot.raw.iter_mut().find_map(|n| match n {
        Node::Chunk(c) => Some(c),
        Node::Line(_) => None,
    }) else {
        return;
    };
    plugin.children.retain(|n| matches!(n, Node::Chunk(_)));
    let at = 0;
    for (k, line) in lines.into_iter().enumerate() {
        plugin.children.insert(at + k, Node::Line(line));
    }
}

/// The name a container shows (its second header token; the first is the type).
pub fn container_name(slot: &FxSlot) -> Option<String> {
    slot.plugin().filter(|c| c.name() == "CONTAINER").and_then(|c| c.args().get(1).cloned())
}

pub fn set_container_name(slot: &mut FxSlot, name: &str) {
    let Some(c) = slot.raw.iter_mut().find_map(|n| match n {
        Node::Chunk(c) if c.name() == "CONTAINER" => Some(c),
        _ => None,
    }) else {
        return;
    };
    let mut t = tokenize(&c.header);
    while t.len() < 3 {
        t.push(String::new());
    }
    t[2] = name.to_owned();
    c.header = join_tokens(t);
}

/// Where the stages sit among a container's effects: the first NAM slot is the amp, a
/// ReaGate before it the gate, the ReaVerbs after it the cab (a parallel second one blends),
/// a ReaEQ whose only band is off the output and another ReaEQ the EQ.
pub fn layout(children: &[FxSlot]) -> Option<Layout> {
    let amp = children.iter().position(is_nam)?;
    let mut l = Layout { amp, ..Layout::default() };
    l.gate = children[..amp].iter().rposition(|s| is_stock(s, "ReaGate"));
    for (i, s) in children.iter().enumerate().skip(amp + 1) {
        if is_stock(s, "ReaVerb") {
            match (l.cab, l.cab2) {
                (None, _) => l.cab = Some(i),
                (Some(_), None) if s.parallel() != Parallel::Serial => l.cab2 = Some(i),
                _ => {}
            }
        } else if is_stock(s, "ReaEQ") {
            let level_only = stock_chunk(s)
                .and_then(|c| ReaEqCompat::from_chunk(&c, 48000))
                .is_some_and(|eq| eq.bands.iter().all(|b| !b.enabled) && eq.bands.len() <= 1);
            if level_only && l.output.is_none() {
                l.output = Some(i);
            } else if l.eq.is_none() {
                l.eq = Some(i);
            }
        }
    }
    Some(l)
}

/// Whether a slot is a rig: a container named "Rig…" with a NAM slot in it.
pub fn is_rig(slot: &FxSlot) -> bool {
    slot.is_container()
        && container_name(slot).is_some_and(|n| n.starts_with(RIG_PREFIX))
        && layout(&slot.container_children()).is_some()
}

fn stem(path: &str) -> String {
    Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_owned())
}

/// "Rig: 5150 III + Mesa 4x12" from the amp's and the cab's names.
pub fn rig_name(amp: Option<&str>, cab: Option<&str>) -> String {
    match (amp.filter(|a| !a.is_empty()), cab.filter(|c| !c.is_empty())) {
        (Some(a), Some(c)) => format!("{RIG_PREFIX}: {a} + {c}"),
        (Some(a), None) => format!("{RIG_PREFIX}: {a}"),
        (None, Some(c)) => format!("{RIG_PREFIX}: {c}"),
        (None, None) => RIG_PREFIX.to_owned(),
    }
}

/// The rig's name from its settings, with the model's and IR's file names.
pub fn default_name(s: &RigSettings) -> String {
    let cab = s.cab.ir.as_deref().filter(|_| s.cab.enabled).map(stem);
    rig_name(s.amp.model.as_deref().map(stem).as_deref(), cab.as_deref())
}

/// Whether a capture of `gear_type` (the `.nam` metadata's or TONE3000's) wants the rig's
/// cabinet: off for captures that include one, on for amps alone, `None` when it does not say.
pub fn cab_wanted(gear_type: &str) -> Option<bool> {
    let g = gear_type.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    match g.as_str() {
        "amp_cab" | "amp_pedal_cab" | "full_rig" | "fullrig" | "pedal_amp_cab" | "ir" => Some(false),
        "amp" | "preamp" | "pedal_amp" | "amp_pedal" | "head" => Some(true),
        _ => None,
    }
}

fn db(gain: f64) -> f64 {
    if gain <= 1e-9 { -150.0 } else { 20.0 * gain.log10() }
}

fn gain(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// A rig's settings as its effects hold them.
pub fn read(slot: &FxSlot) -> Option<RigSettings> {
    let children = slot.container_children();
    let l = layout(&children)?;
    let mut s = RigSettings::default();
    if let Some(g) = l.gate.map(|i| &children[i]) {
        s.gate.enabled = g.enabled;
        if let Some(mut gate) = stock_chunk(g).and_then(|c| Gate::from_reagate(&c, 48000)) {
            use crate::fx::Effect as _;
            s.gate.threshold_db = gate.param(0).map_or(s.gate.threshold_db, |(v, _)| v);
            let mut get = |id: u32, default: f64| gate.param(id).map_or(default, |(v, _)| round(v));
            s.gate.attack_ms = get(1, s.gate.attack_ms);
            s.gate.release_ms = get(2, s.gate.release_ms);
            s.gate.hysteresis_db = get(12, s.gate.hysteresis_db);
        }
    } else {
        s.gate.enabled = false;
    }
    let amp = &children[l.amp];
    let nam = nam_settings(amp).unwrap_or_default();
    s.amp = AmpSettings { enabled: amp.enabled, model: nam.model, input_db: nam.input_db, output_db: nam.output_db };
    let verb = |i: Option<usize>| {
        i.and_then(|i| stock_chunk(&children[i])).and_then(|c| ReaVerbSettings::parse(&c)).map(|v| {
            let wet = if v.wet_muted { 0.0 } else { v.wet as f64 };
            (v.ir.map(|p| p.to_string_lossy().into_owned()), wet)
        })
    };
    let (a, b) = (verb(l.cab), verb(l.cab2));
    s.cab.enabled = l.cab.is_some_and(|i| children[i].enabled);
    s.cab.ir = a.as_ref().and_then(|(p, _)| p.clone());
    s.cab.ir2 = b.as_ref().and_then(|(p, _)| p.clone());
    let (wa, wb) = (a.map_or(0.0, |(_, w)| w), b.map_or(0.0, |(_, w)| w));
    let level = wa + wb;
    s.cab.level_db = if s.cab.ir.is_some() { db(level) } else { 0.0 };
    s.cab.blend = if level > 0.0 && s.cab.ir2.is_some() { wb / level } else { 0.0 };
    match l.eq.map(|i| &children[i]) {
        Some(e) => {
            s.eq.enabled = e.enabled;
            if let Some(eq) = stock_chunk(e).and_then(|c| ReaEqCompat::from_chunk(&c, 48000)) {
                let band = |k: BandKind| eq.bands.iter().find(|b| b.kind == k);
                s.eq.low_cut = band(BandKind::HighPass).filter(|b| b.enabled).map(|b| b.freq);
                s.eq.high_cut = band(BandKind::LowPass).filter(|b| b.enabled).map(|b| b.freq);
                let tone = |k: BandKind| band(k).filter(|b| b.enabled).map_or(0.0, |b| round(db(b.gain)));
                s.eq.bass_db = tone(BandKind::LowShelf);
                s.eq.mid_db = eq
                    .bands
                    .iter()
                    .find(|b| matches!(b.kind, BandKind::Peak | BandKind::PeakWide) && b.enabled)
                    .map_or(0.0, |b| round(db(b.gain)));
                s.eq.treble_db = tone(BandKind::HighShelf);
                let at = |k: BandKind, default: f64| band(k).map_or(default, |b| b.freq.round());
                s.eq.bass_hz = at(BandKind::LowShelf, BASS_HZ);
                s.eq.mid_hz = eq
                    .bands
                    .iter()
                    .find(|b| matches!(b.kind, BandKind::Peak | BandKind::PeakWide))
                    .map_or(MID_HZ, |b| b.freq.round());
                s.eq.treble_hz = at(BandKind::HighShelf, TREBLE_HZ);
            }
        }
        None => s.eq = EqSettings { enabled: false, low_cut: None, high_cut: None, ..EqSettings::default() },
    }
    s.output_db = l
        .output
        .map(|i| &children[i])
        .filter(|o| o.enabled)
        .and_then(stock_chunk)
        .and_then(|c| ReaEqCompat::from_chunk(&c, 48000))
        .map_or(0.0, |eq| round(db(eq.output)));
    Some(s)
}

fn round(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn gate_chunk(base: Option<Vec<u8>>, g: &GateSettings) -> Vec<u8> {
    use crate::fx::Effect as _;
    let base = base.unwrap_or_else(|| Builtin::Gate.default_state());
    match Gate::from_reagate(&base, 48000) {
        Some(mut gate) => {
            gate.set_param(0, g.threshold_db);
            gate.set_param(1, g.attack_ms);
            gate.set_param(2, g.release_ms);
            gate.set_param(12, g.hysteresis_db);
            gate.save_state().unwrap_or(base)
        }
        None => base,
    }
}

fn eq_bands(e: &EqSettings) -> Vec<(BandKind, bool, f64, f64, f64)> {
    vec![
        (BandKind::HighPass, e.low_cut.is_some(), e.low_cut.unwrap_or(80.0), 1.0, CUT_WIDTH),
        (BandKind::LowShelf, true, e.bass_hz, gain(e.bass_db), 1.0),
        (BandKind::Peak, true, e.mid_hz, gain(e.mid_db), 1.5),
        (BandKind::HighShelf, true, e.treble_hz, gain(e.treble_db), 1.0),
        (BandKind::LowPass, e.high_cut.is_some(), e.high_cut.unwrap_or(8000.0), 1.0, CUT_WIDTH),
    ]
}

/// The EQ stage's state: the rig's five bands, keeping the tone bands' widths when the stored
/// EQ has the rig's layout.
fn eq_stage_chunk(base: Option<Vec<u8>>, e: &EqSettings) -> Vec<u8> {
    let mut bands = eq_bands(e);
    if let Some(old) = base.as_deref().and_then(|c| ReaEqCompat::from_chunk(c, 48000))
        && old.bands.iter().map(|b| b.kind).eq(EQ_BANDS)
    {
        for (new, old) in bands.iter_mut().zip(&old.bands).skip(1).take(3) {
            new.4 = old.bandwidth;
        }
    }
    eq_chunk(&bands.iter().map(|&(k, on, f, g, w)| (k.code(), on, f, g, w)).collect::<Vec<_>>(), 1.0)
}

fn output_chunk(output_db: f64) -> Vec<u8> {
    eq_chunk(&[(BandKind::Peak.code(), false, 1000.0, 1.0, 1.0)], gain(output_db))
}

fn stock_slot(builtin: Builtin, chunk: &[u8]) -> FxSlot {
    builtin.slot_with(chunk)
}

/// A slot's chunk set to `chunk`, or a new slot of `builtin` when there is none.
fn put(children: &mut Vec<FxSlot>, at: Option<usize>, insert_at: usize, builtin: Builtin, chunk: &[u8]) -> usize {
    match at {
        Some(i) => {
            if stock_chunk(&children[i]).as_deref() != Some(chunk) {
                set_stock_chunk(&mut children[i], chunk);
            }
            i
        }
        None => {
            children.insert(insert_at.min(children.len()), stock_slot(builtin, chunk));
            insert_at.min(children.len() - 1)
        }
    }
}

/// Makes a rig container's effects hold `s`, adding the stages it lacks, and names it after
/// its amp and cab.
pub fn apply(slot: &mut FxSlot, s: &RigSettings) {
    let mut children = slot.container_children();
    let Some(mut l) = layout(&children) else { return };
    // The gate, before the amp.
    let gate = gate_chunk(l.gate.and_then(|i| stock_chunk(&children[i])), &s.gate);
    if l.gate.is_none() {
        children.insert(l.amp, stock_slot(Builtin::Gate, &gate));
        l = layout(&children).unwrap_or(l);
    } else {
        put(&mut children, l.gate, 0, Builtin::Gate, &gate);
    }
    if let Some(i) = l.gate {
        children[i].enabled = s.gate.enabled;
    }
    // The amp.
    {
        let amp = &mut children[l.amp];
        amp.enabled = s.amp.enabled;
        match &s.amp.model {
            Some(m) if nam_settings(amp).and_then(|n| n.model).as_deref() != Some(m.as_str()) => {
                set_nam_model(amp, Path::new(m))
            }
            Some(_) => {}
            None => clear_nam_model(amp),
        }
        let now = nam_settings(amp).unwrap_or_default();
        if (now.input_db - s.amp.input_db).abs() > 1e-9 || (now.output_db - s.amp.output_db).abs() > 1e-9 {
            set_nam_levels(amp, s.amp.input_db, s.amp.output_db);
        }
    }
    // The cab: one ReaVerb, and a second in parallel when there are two responses.
    let level = gain(s.cab.level_db) as f32;
    let blend = if s.cab.ir2.is_some() { s.cab.blend.clamp(0.0, 1.0) as f32 } else { 0.0 };
    // Without a response the cab passes the amp on rather than going silent.
    let first = match &s.cab.ir {
        Some(ir) => reverb_state(Some(Path::new(ir)), level * (1.0 - blend), 0.0),
        None => reverb_state(None, 0.0, 1.0),
    };
    let cab = put(&mut children, l.cab, l.amp + 1, Builtin::Reverb, &first);
    children[cab].enabled = s.cab.enabled;
    children[cab].set_parallel(Parallel::Serial);
    l = layout(&children).unwrap_or(l);
    match (&s.cab.ir2, l.cab2) {
        (Some(ir2), at) => {
            let second = reverb_state(Some(Path::new(ir2)), level * blend, 0.0);
            let i = put(&mut children, at, cab + 1, Builtin::Reverb, &second);
            children[i].set_parallel(Parallel::Audio);
            children[i].enabled = s.cab.enabled;
        }
        (None, Some(i)) => {
            children.remove(i);
        }
        (None, None) => {}
    }
    l = layout(&children).unwrap_or(l);
    // The EQ, after the cab.
    let after_cab = l.cab2.or(l.cab).unwrap_or(l.amp) + 1;
    let eq = eq_stage_chunk(l.eq.and_then(|i| stock_chunk(&children[i])), &s.eq);
    let e = put(&mut children, l.eq, after_cab, Builtin::Eq, &eq);
    children[e].enabled = s.eq.enabled;
    l = layout(&children).unwrap_or(l);
    // The output level, last.
    let out = output_chunk(s.output_db);
    let end = children.len();
    let o = put(&mut children, l.output, end, Builtin::Eq, &out);
    children[o].enabled = true;
    slot.set_container_children(&children);
    set_container_name(slot, &default_name(s));
}

/// A new rig holding `s`.
pub fn new_rig(s: &RigSettings) -> FxSlot {
    let mut rig = FxSlot::container();
    let nam = crate::fx::nam_slot(s.amp.model.as_deref().map(Path::new));
    rig.set_container_children(&[nam]);
    apply(&mut rig, s);
    rig
}

/// Wraps the NAM slot at `index` of `chain` in a rig together with the effects around it that
/// play its parts: a ReaGate right before it, a ReaVerb (and a parallel second one) right
/// after it and a ReaEQ after those. The stages it lacks are added switched off, except the
/// output level, so the chain sounds the same. Returns the rig's index.
pub fn convert(chain: &mut Vec<FxSlot>, index: usize) -> Option<usize> {
    if !chain.get(index).is_some_and(is_nam) {
        return None;
    }
    let mut start = index;
    if index > 0 && is_stock(&chain[index - 1], "ReaGate") && chain[index - 1].parallel() == Parallel::Serial {
        start -= 1;
    }
    let mut end = index + 1;
    if chain.get(end).is_some_and(|s| is_stock(s, "ReaVerb") && s.parallel() == Parallel::Serial) {
        end += 1;
        if chain.get(end).is_some_and(|s| is_stock(s, "ReaVerb") && s.parallel() != Parallel::Serial) {
            end += 1;
        }
    }
    if chain.get(end).is_some_and(|s| is_stock(s, "ReaEQ") && s.parallel() == Parallel::Serial) {
        end += 1;
    }
    let parts: Vec<FxSlot> = chain.drain(start..end).collect();
    let had_gate = parts.iter().any(|s| is_stock(s, "ReaGate"));
    let had_cab = parts.iter().any(|s| is_stock(s, "ReaVerb"));
    let had_eq = parts.iter().any(|s| is_stock(s, "ReaEQ"));
    let mut rig = FxSlot::container();
    rig.set_container_children(&parts);
    let mut s = read(&rig).unwrap_or_default();
    s.gate.enabled &= had_gate;
    if !had_cab {
        s.cab = CabSettings { enabled: false, ..CabSettings::default() };
    }
    if !had_eq {
        s.eq = EqSettings { enabled: false, ..EqSettings::default() };
    }
    if had_cab && let Some(i) = parts.iter().position(|p| is_stock(p, "ReaVerb")) {
        // A ReaVerb that mixes in its input is kept as it is, dry share and all.
        let dry = stock_chunk(&parts[i]).and_then(|c| ReaVerbSettings::parse(&c)).is_some_and(|v| v.dry != 0.0);
        if dry {
            let mut rig = FxSlot::container();
            rig.set_container_children(&parts);
            apply_keeping_cab(&mut rig, &s);
            chain.insert(start, rig);
            return Some(start);
        }
    }
    apply(&mut rig, &s);
    chain.insert(start, rig);
    Some(start)
}

/// As [`apply`], but leaves the cab's own ReaVerb state alone.
fn apply_keeping_cab(rig: &mut FxSlot, s: &RigSettings) {
    let before = rig.container_children();
    let cab = layout(&before).and_then(|l| l.cab).map(|i| before[i].clone());
    apply(rig, s);
    let mut children = rig.container_children();
    if let (Some(cab), Some(i)) = (cab, layout(&children).and_then(|l| l.cab)) {
        children[i] = cab;
        rig.set_container_children(&children);
    }
}

/// The path to the NAM slot with FXID `fx_id` in `chain`: its index, or a rig's index and its
/// index inside.
pub fn find_nam(chain: &[FxSlot], fx_id: &str) -> Option<Vec<usize>> {
    for (i, slot) in chain.iter().enumerate() {
        if is_nam(slot) && slot_id(slot).as_deref() == Some(fx_id) {
            return Some(vec![i]);
        }
        if is_rig(slot)
            && let Some(l) = layout(&slot.container_children())
            && slot_id(&slot.container_children()[l.amp]).as_deref() == Some(fx_id)
        {
            return Some(vec![i, l.amp]);
        }
    }
    None
}

/// The slot at `path` (as [`find_nam`] gives it).
pub fn slot_at(chain: &[FxSlot], path: &[usize]) -> Option<FxSlot> {
    match path {
        [i] => chain.get(*i).cloned(),
        [i, j] => chain.get(*i)?.container_children().get(*j).cloned(),
        _ => None,
    }
}

/// Changes the slot at `path` in place.
pub fn edit_at(chain: &mut [FxSlot], path: &[usize], f: impl FnOnce(&mut FxSlot)) {
    match path {
        [i] => {
            if let Some(s) = chain.get_mut(*i) {
                f(s);
            }
        }
        [i, j] => {
            let Some(rig) = chain.get_mut(*i) else { return };
            let mut children = rig.container_children();
            if let Some(s) = children.get_mut(*j) {
                f(s);
                rig.set_container_children(&children);
            }
        }
        _ => {}
    }
}

/// The ReaVerb chunk of a raw state, for tests and probes.
pub fn reverb_chunk(slot: &FxSlot) -> Option<Vec<u8>> {
    stock_chunk(slot).filter(|_| is_stock(slot, "ReaVerb"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::{ChainInput, Channels, MidiState};
    use crate::fx::{FxOwner, FxRegistry, nam_slot};
    use std::path::PathBuf;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("scythe-rig-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_ir(path: &Path, taps: &[(usize, f32)]) {
        let len = taps.iter().map(|t| t.0 + 1).max().unwrap_or(1);
        let mut ir = vec![0.0f32; len];
        for &(at, g) in taps {
            ir[at] = g;
        }
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for s in ir {
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();
    }

    fn settings(ir: &Path) -> RigSettings {
        RigSettings {
            gate: GateSettings { enabled: true, threshold_db: -42.0, ..GateSettings::default() },
            amp: AmpSettings {
                enabled: true,
                model: Some("/models/5150 III.nam".into()),
                input_db: 3.0,
                output_db: -2.5,
            },
            cab: CabSettings { ir: Some(ir.display().to_string()), level_db: -3.0, ..CabSettings::default() },
            eq: EqSettings { low_cut: Some(90.0), high_cut: None, bass_db: 2.0, mid_db: -1.5, ..EqSettings::default() },
            output_db: 1.5,
        }
    }

    #[test]
    fn a_rig_survives_a_project_round_trip_as_one_container() {
        let s = settings(Path::new("/irs/Mesa 4x12.wav"));
        let mut project = scythe_core::Project::default();
        let t = project.add_track("Guitar");
        project.track_mut(t).unwrap().fx.push(new_rig(&s));
        let text = scythe_core::rpp::project_to_rpp(&project);
        assert!(text.contains("<CONTAINER Container \"Rig: 5150 III + Mesa 4x12\""), "{text}");
        let back = scythe_core::rpp::project_from_rpp(&text).unwrap();
        let fx = &back.tracks.iter().find(|t| t.name == "Guitar").unwrap().fx;
        assert_eq!(fx.len(), 1);
        assert!(is_rig(&fx[0]));
        let children = fx[0].container_children();
        assert_eq!(children.len(), 5, "gate, amp, cab, EQ, output");
        let l = layout(&children).unwrap();
        assert_eq!((l.gate, l.amp, l.cab, l.cab2, l.eq, l.output), (Some(0), 1, Some(2), None, Some(3), Some(4)));
        let mut read_back = read(&fx[0]).unwrap();
        assert!((read_back.gate.threshold_db - -42.0).abs() < 0.01);
        read_back.gate.threshold_db = -42.0;
        assert!((read_back.cab.level_db - -3.0).abs() < 1e-5);
        read_back.cab.level_db = -3.0;
        assert_eq!(read_back, s);

        let mut two = s.clone();
        two.cab.ir2 = Some("/irs/Greenback.wav".into());
        two.cab.blend = 0.25;
        two.cab.enabled = false;
        two.eq.enabled = false;
        let mut slot = fx[0].clone();
        apply(&mut slot, &two);
        let children = slot.container_children();
        let l = layout(&children).unwrap();
        assert_eq!(l.cab2, Some(3));
        assert_eq!(children[3].parallel(), Parallel::Audio);
        assert!(!children[2].enabled && !children[3].enabled && !children[l.eq.unwrap()].enabled);
        let mut back = read(&slot).unwrap();
        assert!((back.cab.blend - 0.25).abs() < 1e-6 && (back.cab.level_db - -3.0).abs() < 1e-5);
        (back.cab.blend, back.cab.level_db, back.gate.threshold_db) = (0.25, -3.0, -42.0);
        assert_eq!(back, two);
        assert_eq!(container_name(&slot).as_deref(), Some("Rig: 5150 III"), "a bypassed cab leaves the name");
        let mut one = two.clone();
        one.cab.ir2 = None;
        apply(&mut slot, &one);
        assert_eq!(slot.container_children().len(), 5);
    }

    #[test]
    fn gate_timing_and_tone_frequencies_live_in_reagate_and_reaeq() {
        let mut s = RigSettings {
            gate: GateSettings {
                enabled: true,
                threshold_db: -40.0,
                attack_ms: 20.0,
                release_ms: 300.0,
                hysteresis_db: -6.0,
            },
            ..RigSettings::default()
        };
        s.eq.mid_hz = 1200.0;
        s.eq.bass_hz = 90.0;
        s.eq.treble_hz = 4000.0;
        let rig = new_rig(&s);
        let back = read(&rig).unwrap();
        assert_eq!(
            (back.gate.attack_ms, back.gate.release_ms, back.gate.hysteresis_db),
            (20.0, 300.0, -6.0),
            "ReaGate holds the timing"
        );
        assert_eq!((back.eq.bass_hz, back.eq.mid_hz, back.eq.treble_hz), (90.0, 1200.0, 4000.0));
    }

    #[test]
    fn nam_followed_by_reaverb_converts_to_a_rig_keeping_both_slots() {
        let nam = nam_slot(Some(Path::new("/m/amp.nam")));
        let nam_id = slot_id(&nam).unwrap();
        let verb = Builtin::Reverb.slot_with(&reverb_state(Some(Path::new("/irs/cab.wav")), 1.0, 0.0));
        let verb_id = slot_id(&verb).unwrap();
        let comp = Builtin::Compressor.slot();
        let mut chain = vec![comp.clone(), nam, verb];
        assert_eq!(convert(&mut chain, 1), Some(1));
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0], comp);
        assert!(is_rig(&chain[1]));
        let children = chain[1].container_children();
        let l = layout(&children).unwrap();
        assert_eq!(slot_id(&children[l.amp]), Some(nam_id.clone()));
        assert_eq!(slot_id(&children[l.cab.unwrap()]), Some(verb_id));
        let s = read(&chain[1]).unwrap();
        assert!(!s.gate.enabled && !s.eq.enabled && s.cab.enabled && s.amp.enabled);
        assert_eq!(s.cab.ir.as_deref(), Some("/irs/cab.wav"));
        assert_eq!((s.cab.level_db, s.output_db), (0.0, 0.0));
        assert_eq!(container_name(&chain[1]).as_deref(), Some("Rig: amp + cab"));
        assert_eq!(find_nam(&chain, &nam_id), Some(vec![1, l.amp]));

        let mut bare = vec![nam_slot(None)];
        assert_eq!(convert(&mut bare, 0), Some(0));
        let s = read(&bare[0]).unwrap();
        assert!(!s.cab.enabled && s.cab.ir.is_none());
        assert_eq!(convert(&mut vec![Builtin::Eq.slot()], 0), None);
    }

    #[test]
    fn captures_with_a_cabinet_switch_the_cab_off() {
        for g in ["amp_cab", "full-rig", "amp-cab", "Amp Pedal Cab", "ir"] {
            assert_eq!(cab_wanted(g), Some(false), "{g}");
        }
        for g in ["amp", "preamp", "pedal_amp"] {
            assert_eq!(cab_wanted(g), Some(true), "{g}");
        }
        for g in ["pedal", "outboard", ""] {
            assert_eq!(cab_wanted(g), None, "{g}");
        }
    }

    fn run(chain: &crate::chain::FxChain, l: &mut [f32], r: &mut [f32], at: i64) {
        let tempo = |_: i64| 120.0;
        let timeline = scythe_core::TempoMap::default().timeline();
        let input = ChainInput {
            sample_rate: 48_000,
            playing: true,
            notes: None,
            live: &[],
            live_channel: None,
            tempo: &tempo,
            timeline: &timeline,
        };
        let n = l.len();
        let mut ch = Channels { l, r, aux: &mut [], n };
        chain.process(&mut ch, &input, at, &mut MidiState { bufs: None, has: false }, None);
    }

    fn render(registry: &mut FxRegistry, slots: &[FxSlot], input: &[f32]) -> Vec<f32> {
        let chain = crate::chain::build(registry, &FxOwner::Master, slots, 2);
        let mut out = Vec::new();
        for (k, block) in input.chunks(512).enumerate() {
            let (mut l, mut r) = (block.to_vec(), block.to_vec());
            run(&chain, &mut l, &mut r, (k * 512) as i64);
            out.extend(l);
        }
        out
    }

    fn noise(n: usize) -> Vec<f32> {
        let mut x = 0x1234_5678u32;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x as f32 / u32::MAX as f32 - 0.5) * 0.5
            })
            .collect()
    }

    #[test]
    fn a_rig_plays_as_its_effects_in_series() {
        let dir = temp("series");
        let ir = dir.join("cab.wav");
        write_ir(&ir, &[(0, 0.8), (37, -0.3), (400, 0.1)]);
        let mut s = settings(&ir);
        s.amp.model = None;
        s.amp.input_db = 0.0;
        s.amp.output_db = 0.0;
        let rig = new_rig(&s);
        let input = noise(48_000);
        let a = render(&mut FxRegistry::new(48_000), std::slice::from_ref(&rig), &input);
        let b = render(&mut FxRegistry::new(48_000), &rig.container_children(), &input);
        assert_eq!(a.len(), b.len());
        let worst = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-6, "{worst}");
        assert!(a.iter().any(|v| v.abs() > 0.01));
    }

    #[test]
    fn switching_a_stage_crossfades_without_a_click() {
        let dir = temp("stage");
        let ir = dir.join("flip.wav");
        write_ir(&ir, &[(0, -1.0)]);
        let mut s = RigSettings {
            gate: GateSettings { enabled: false, ..GateSettings::default() },
            amp: AmpSettings { model: None, input_db: 0.0, output_db: 0.0, ..AmpSettings::default() },
            cab: CabSettings { ir: Some(ir.display().to_string()), level_db: 0.0, ..CabSettings::default() },
            eq: EqSettings { enabled: false, ..EqSettings::default() },
            output_db: 0.0,
        };
        let mut rig = new_rig(&s);
        let mut registry = FxRegistry::new(48_000);
        let sine: Vec<f32> =
            (0..48_000).map(|i| (i as f32 * 440.0 / 48_000.0 * std::f32::consts::TAU).sin() * 0.5).collect();
        let mut out = Vec::new();
        let play = |registry: &mut FxRegistry, rig: &FxSlot, blocks: std::ops::Range<usize>, out: &mut Vec<f32>| {
            let chain = crate::chain::build(registry, &FxOwner::Master, std::slice::from_ref(rig), 2);
            for k in blocks {
                let block = &sine[k * 512..(k + 1) * 512];
                let (mut l, mut r) = (block.to_vec(), block.to_vec());
                run(&chain, &mut l, &mut r, (k * 512) as i64);
                out.extend(l);
            }
        };
        play(&mut registry, &rig, 0..20, &mut out);
        s.cab.enabled = false;
        apply(&mut rig, &s);
        play(&mut registry, &rig, 20..40, &mut out);
        s.cab.enabled = true;
        apply(&mut rig, &s);
        play(&mut registry, &rig, 40..60, &mut out);
        let slope = 0.5 * 440.0 / 48_000.0 * std::f32::consts::TAU;
        let worst = out.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        assert!(worst < slope * 1.5, "largest step {worst} against the sine's {slope}");
        let close = |a: f32, b: f32| (a - b).abs() < 1e-5;
        assert!((1000..20 * 512).all(|i| close(out[i], -sine[i])), "the cab flips the sine");
        assert!((21 * 512..40 * 512).all(|i| close(out[i], sine[i])), "switched off it passes the sine");
        assert!((41 * 512..60 * 512).all(|i| close(out[i], -sine[i])), "and flips it again once back on");
    }

    #[test]
    fn changing_the_impulse_response_crossfades_without_a_jump() {
        let dir = temp("xfade");
        let (a, b) = (dir.join("a.wav"), dir.join("b.wav"));
        write_ir(&a, &[(0, 1.0)]);
        write_ir(&b, &[(0, -1.0)]);
        let slot = |ir: &Path| Builtin::Reverb.slot_with(&reverb_state(Some(ir), 1.0, 0.0));
        let mut first = slot(&a);
        let mut second = slot(&b);
        let id = scythe_core::rpp::Node::Line("FXID {RIG-XFADE}".into());
        for s in [&mut first, &mut second] {
            let at = s.raw.iter().position(|n| matches!(n, Node::Line(l) if l.starts_with("FXID"))).unwrap();
            s.raw[at] = id.clone();
        }
        let mut registry = FxRegistry::new(48_000);
        let sine: Vec<f32> =
            (0..48_000).map(|i| (i as f32 * 440.0 / 48_000.0 * std::f32::consts::TAU).sin() * 0.5).collect();
        let before = crate::chain::build(&mut registry, &FxOwner::Master, std::slice::from_ref(&first), 2);
        let mut out = Vec::new();
        let mut k = 0;
        for block in sine.chunks(512).take(20) {
            let (mut l, mut r) = (block.to_vec(), block.to_vec());
            run(&before, &mut l, &mut r, k);
            out.extend(l);
            k += 512;
        }
        let kept = registry.instance(None, &first, 0).unwrap();
        let after = crate::chain::build(&mut registry, &FxOwner::Master, std::slice::from_ref(&second), 2);
        assert!(std::sync::Arc::ptr_eq(&kept, &registry.instance(None, &second, 0).unwrap()), "same instance");
        for block in sine.chunks(512).skip(20) {
            let (mut l, mut r) = (block.to_vec(), block.to_vec());
            run(&after, &mut l, &mut r, k);
            out.extend(l);
            k += 512;
        }
        let slope = 0.5 * 440.0 / 48_000.0 * std::f32::consts::TAU;
        let worst = out.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        assert!(worst < slope * 1.5, "largest step {worst} against the sine's {slope}");
        let switch = 20 * 512;
        assert!((out[switch - 1] - sine[switch - 1]).abs() < 1e-5);
        let settled = switch + 2_000;
        assert!(out[settled..].iter().zip(&sine[settled..]).all(|(o, s)| (o + s).abs() < 1e-5), "plays B after");

        let mut quieter = second.clone();
        let chunk = reverb_state(Some(&b), 0.5, 0.0);
        set_stock_chunk(&mut quieter, &chunk);
        crate::chain::build(&mut registry, &FxOwner::Master, std::slice::from_ref(&quieter), 2);
        assert!(std::sync::Arc::ptr_eq(&kept, &registry.instance(None, &quieter, 0).unwrap()));
        assert_eq!(kept.with(|e| e.param(0).map(|p| p.0)).flatten().map(|v| v.round()), Some(-6.0));
    }
}
