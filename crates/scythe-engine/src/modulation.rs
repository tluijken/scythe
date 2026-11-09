//! Parameter modulation and MIDI learn as REAPER runs them, once per 512-frame block.
//!
//! Measured against REAPER 7.73 renders: an effect's modulated values for a block are worked
//! out right after it processed the block before (the LFO for the next block's position, the
//! audio follower from the input it just saw, links from the values their sources hold at that
//! moment), so a link to an earlier effect follows it in the same block and one to a later
//! effect, or to another parameter of the same effect, one block behind. The first values come
//! in order of the `<PROGRAMENV` chunks just before the effect first processes, and the update
//! after that first block keeps the links as they are. Values add up in the parameter's 0..1
//! range: the baseline (or the parameter's envelope, which takes its place), the LFO, the
//! audio follower and the link.

use std::collections::HashMap;
use std::sync::Arc;

use scythe_core::modulation::{
    LEARN_LFO_RETRIGGER, LEARN_SOFT_TAKEOVER, LearnMode, LinkSource, ParamLearn, ParamModulation,
};
use scythe_core::tempo::Timeline;

use crate::fx::{Effect, FxInstance, Target};
use crate::midi_io::LiveMidi;

/// One modulated parameter of an effect, as the render graph plays it.
#[derive(Debug, Clone)]
pub struct ModEntry {
    pub def: ParamModulation,
    pub index: usize,
    pub target: Target,
    /// Chain position (among the running effects) and parameter index of a linked source.
    pub source: Option<(usize, usize)>,
}

/// The modulation and learned controls of one effect in a chain.
#[derive(Debug, Clone, Default)]
pub struct FxMods {
    pub entries: Vec<ModEntry>,
    pub learns: Vec<ParamLearn>,
}

impl FxMods {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.learns.is_empty()
    }

    /// Whether a parameter follows MIDI in the chain.
    pub fn reads_midi(&self) -> bool {
        self.entries.iter().any(|e| {
            e.def.link_active && e.def.link.as_ref().is_some_and(|l| matches!(l.source, LinkSource::Midi { .. }))
        })
    }

    /// The modulations of `slot` that move something, and its learned MIDI bindings. `chain`
    /// maps each slot index to its position among the running effects.
    pub fn from_slot(slot: &scythe_core::FxSlot, own_slot: usize, chain: &[Option<usize>]) -> Self {
        let entries = scythe_core::modulation::modulations(slot)
            .into_iter()
            .filter(|m| m.is_active())
            .filter_map(|def| {
                let index = def.index()?;
                let name = def.param.split_once(':').map(|(_, n)| n.to_owned()).unwrap_or_default();
                let target = if name == "bypass" { Target::Bypass } else { Target::Named(name) };
                let source = def.link.as_ref().filter(|_| def.link_active).and_then(|l| match &l.source {
                    LinkSource::Fx { param, .. } => {
                        let fx = l.source_fx(own_slot)?;
                        Some((chain.get(fx).copied().flatten()?, usize::try_from(*param).ok()?))
                    }
                    LinkSource::Midi { .. } => None,
                });
                Some(ModEntry { def, index, target, source })
            })
            .collect();
        let learns = scythe_core::modulation::learns(slot).into_iter().filter(|l| l.status() >= 0x80).collect();
        Self { entries, learns }
    }
}

/// What a modulated effect sees of the rest of its chain and the transport for one block.
pub struct ModContext<'a> {
    pub mods: &'a FxMods,
    pub chain: &'a [Arc<FxInstance>],
    pub own: usize,
    pub timeline: &'a Timeline,
    /// Messages from the MIDI inputs for this block, for learned controls.
    pub control: &'a [LiveMidi],
    pub sample_rate: u32,
}

/// Per-parameter state that outlives graph rebuilds (kept in the effect's instance).
#[derive(Debug, Clone, Default)]
struct ParamState {
    /// The value it was last given (or will get next block, after the update), 0..1.
    value: Option<f64>,
    follower: f64,
    /// The follower's peak over the last block.
    level: f64,
    link: Option<f64>,
    midi: Option<f64>,
    midi_next: Option<f64>,
    /// A baseline set by a learned control, in place of the stored one.
    base: Option<f64>,
    /// Where a retriggered or free-running LFO starts counting its phase.
    origin: Option<(f64, f64)>,
    /// The last value a learned absolute control sent, for soft takeover.
    control: Option<f64>,
}

#[derive(Debug, Default)]
pub struct ModState {
    started: bool,
    /// Blocks processed since the first.
    blocks: u64,
    params: HashMap<usize, ParamState>,
    /// The values learned controls set on unmodulated parameters.
    learned: HashMap<usize, f64>,
}

impl ModState {
    /// The value modulation gives parameter `index`, if it is modulated.
    pub fn value(&self, index: usize) -> Option<f64> {
        self.params.get(&index).and_then(|p| p.value)
    }

    /// Baselines learned controls moved, to store back as `PARAMBASE`.
    pub fn learned_bases(&self) -> Vec<(usize, f64)> {
        self.params.iter().filter_map(|(i, p)| Some((*i, p.base?))).collect()
    }

    /// Sets a modulated parameter's baseline (0..1), as moving its control does.
    pub fn set_base(&mut self, index: usize, value: f64) {
        self.params.entry(index).or_default().base = Some(value.clamp(0.0, 1.0));
    }

    /// Starts over, as after loading: the next block sets the first values again.
    pub fn restart(&mut self) {
        self.started = false;
        for p in self.params.values_mut() {
            p.follower = 0.0;
            p.level = 0.0;
            p.link = None;
            p.midi = None;
            p.midi_next = None;
        }
    }
}

/// The parameter's range in its own units: what `PARAMBASE` counts in.
pub fn native_range(fx: &mut dyn Effect, index: usize) -> (f64, f64) {
    fx.param_range(index).filter(|(lo, hi)| hi > lo).unwrap_or((0.0, 1.0))
}

/// A parameter's current value, 0..1.
pub fn normalized(fx: &mut dyn Effect, index: usize) -> Option<f64> {
    if let Some(v) = fx.normalized(index) {
        return Some(v);
    }
    let p = fx.params().into_iter().nth(index)?;
    let (v, _) = fx.param(p.id)?;
    (p.max > p.min).then(|| ((v - p.min) / (p.max - p.min)).clamp(0.0, 1.0))
}

pub fn apply(fx: &mut dyn Effect, target: &Target, index: usize, value: f64) -> bool {
    match target {
        Target::Bypass => value >= 0.5,
        Target::Named(name) => {
            if !fx.set_normalized(index, value) && !fx.set_named_param(name, value) {
                let params = fx.params();
                if let Some(p) = params.get(index) {
                    fx.set_param(p.id, p.min + value * (p.max - p.min));
                }
            }
            false
        }
    }
}

fn direction(d: i32, x: f64) -> f64 {
    match d {
        1 => x,
        -1 => -x,
        _ => x - 0.5,
    }
}

/// Deterministic levels for the random LFO shape, one per cycle.
fn random_level(seed: usize, cycle: i64) -> f64 {
    let mut x = (cycle as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ (seed as u64).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 31;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^= x >> 29;
    (x >> 11) as f64 / (1u64 << 53) as f64
}

struct Reading<'a> {
    ctx: &'a ModContext<'a>,
    frame: i64,
}

impl Reading<'_> {
    fn seconds(&self) -> f64 {
        self.frame as f64 / self.ctx.sample_rate as f64
    }

    fn lfo(&self, e: &ModEntry, p: &ParamState) -> f64 {
        let l = &e.def.lfo;
        if !l.active {
            return 0.0;
        }
        let t = self.seconds();
        let tl = self.ctx.timeline;
        let phase = match (l.tempo_sync, p.origin) {
            (true, Some((_, qn0))) => (tl.qn_at(t) - qn0) / l.period_qn(),
            (true, None) => tl.qn_at(t) / l.period_qn(),
            (false, Some((t0, _))) => (t - t0) * l.hz(),
            (false, None) => t * l.hz(),
        } + l.phase;
        l.strength * direction(l.direction, l.wave(phase, |c| random_level(e.index, c)))
    }

    /// The value for the block at `frame`, or `None` while a MIDI link has heard nothing.
    fn value(&self, e: &ModEntry, p: &ParamState, base: f64) -> Option<f64> {
        let mut v = p.base.unwrap_or(base) + self.lfo(e, p);
        if e.def.acs.active {
            v += e.def.acs.strength * direction(e.def.acs.direction, e.def.acs.shape(p.level));
        }
        if let (Some(link), true) = (&e.def.link, e.def.link_active) {
            let source = match link.source {
                LinkSource::Midi { .. } => p.midi?,
                LinkSource::Fx { .. } => p.link?,
            };
            v += link.scale * (source + link.offset);
        }
        Some(v.clamp(0.0, 1.0))
    }

    /// What a link's source holds now: the parameter's own value (as the effect rounds it),
    /// else the value modulation last gave it.
    fn source(&self, fx: &mut dyn Effect, state: &ModState, (pos, param): (usize, usize)) -> Option<f64> {
        if pos == self.ctx.own {
            return normalized(fx, param).or_else(|| state.value(param));
        }
        let inst = self.ctx.chain.get(pos)?;
        inst.try_with(|e| normalized(e, param))
            .flatten()
            .or_else(|| inst.mods.try_lock().ok().and_then(|s| s.value(param)))
    }
}

fn midi_link_value(link: &LinkSource, msg: [u8; 3]) -> Option<f64> {
    let LinkSource::Midi { channel, status, data1, .. } = *link else { return None };
    let kind = msg[0] & 0xf0;
    if kind as i32 != status & 0xf0 || channel > 0 && (msg[0] & 0x0f) as i32 != channel - 1 {
        return None;
    }
    match kind {
        0xe0 => Some((msg[1] as u32 | (msg[2] as u32) << 7) as f64 / 16383.0),
        0xc0 | 0xd0 => Some(msg[1] as f64 / 127.0),
        _ if msg[1] as i32 == data1 => Some(msg[2] as f64 / 127.0),
        _ => None,
    }
}

/// The base value of each entry: its envelope's (0..1) when it has one, else `PARAMBASE`.
pub type Bases<'a> = &'a dyn Fn(usize, f64) -> f64;

/// Before an effect processes the block at `frame`: applies learned controls, sets the
/// modulated values and runs the audio followers over the block's input (`inputs`, by input
/// channel). `midi` is the MIDI reaching the effect in this block. Returns whether a
/// modulated bypass switches the effect off.
#[allow(clippy::too_many_arguments)]
pub fn before_block(
    fx: &mut dyn Effect,
    state: &mut ModState,
    ctx: &ModContext,
    frame: i64,
    envelope: Bases,
    inputs: &[&[f32]],
    midi: &[(usize, [u8; 3])],
) -> bool {
    let read = Reading { ctx, frame };
    learn(fx, state, ctx, frame);
    let mut bypassed = false;
    // MIDI links take their first message at once, later ones from the next block on.
    for e in &ctx.mods.entries {
        let Some(link) = e.def.link.as_ref().filter(|_| e.def.link_active) else { continue };
        let last = midi.iter().rev().find_map(|(_, m)| midi_link_value(&link.source, *m));
        if let Some(v) = last {
            let p = state.params.entry(e.index).or_default();
            if p.midi.is_none() {
                p.midi = Some(v);
            }
            p.midi_next = Some(v);
        }
    }
    let first = !state.started;
    if first {
        state.started = true;
        state.blocks = 0;
    }
    for e in &ctx.mods.entries {
        let (lo, hi) = native_range(fx, e.index);
        let stored = ((e.def.base - lo) / (hi - lo)).clamp(0.0, 1.0);
        let base = envelope(e.index, stored);
        if first {
            let p = state.params.entry(e.index).or_default();
            if e.def.lfo.free && p.origin.is_none() {
                p.origin = Some((read.seconds(), ctx.timeline.qn_at(read.seconds())));
            }
            p.follower = 0.0;
            p.level = 0.0;
            let link = e.source.and_then(|s| read.source(fx, state, s));
            state.params.entry(e.index).or_default().link = link;
        }
        let p = state.params.entry(e.index).or_default();
        let Some(v) = read.value(e, p, base) else { continue };
        p.value = Some(v);
        bypassed |= apply(fx, &e.target, e.index, v);
    }
    let sr = ctx.sample_rate as f64;
    for e in ctx.mods.entries.iter().filter(|e| e.def.acs.active) {
        let a = &e.def.acs;
        let coef = |ms: f64| if ms > 0.0 { (-2000.0 / (ms * sr)).exp() } else { 0.0 };
        let (ca, cr) = (coef(a.attack_ms), coef(a.release_ms));
        let ch = usize::try_from(a.channel).ok();
        let first = ch.and_then(|c| inputs.get(c));
        let second = ch.filter(|_| a.stereo).and_then(|c| inputs.get(c + 1));
        let p = state.params.entry(e.index).or_default();
        let mut s = p.follower;
        let mut peak = 0.0f64;
        let n = inputs.first().map_or(0, |b| b.len());
        for i in 0..n {
            let x = match (first, second) {
                (Some(l), Some(r)) => (l[i] as f64 + r[i] as f64) / 2.0,
                (Some(l), None) => l[i] as f64,
                _ => 0.0,
            };
            let v = x.abs();
            s = v + (s - v) * if v > s { ca } else { cr };
            peak = peak.max(s);
        }
        p.follower = s;
        p.level = peak;
    }
    bypassed
}

/// After an effect processed the block that ends at `next`: works out its values for the next
/// block and publishes them for links read later in this block.
pub fn after_block(fx: &mut dyn Effect, state: &mut ModState, ctx: &ModContext, next: i64, envelope: Bases) {
    let read = Reading { ctx, frame: next };
    let links: Vec<(usize, Option<f64>)> = if state.blocks == 0 {
        Vec::new()
    } else {
        ctx.mods.entries.iter().filter_map(|e| Some((e.index, read.source(fx, state, e.source?)))).collect()
    };
    state.blocks += 1;
    for (index, v) in links {
        state.params.entry(index).or_default().link = v;
    }
    for e in &ctx.mods.entries {
        let (lo, hi) = native_range(fx, e.index);
        let stored = ((e.def.base - lo) / (hi - lo)).clamp(0.0, 1.0);
        let base = envelope(e.index, stored);
        let p = state.params.entry(e.index).or_default();
        if p.midi_next.is_some() {
            p.midi = p.midi_next;
        }
        if let Some(v) = read.value(e, p, base) {
            p.value = Some(v);
            if !matches!(e.target, Target::Bypass) {
                apply(fx, &e.target, e.index, v);
            }
        }
    }
}

/// Applies the learned MIDI controls among `ctx.control` to their parameters.
fn learn(fx: &mut dyn Effect, state: &mut ModState, ctx: &ModContext, frame: i64) {
    for l in &ctx.mods.learns {
        let Some(index) = l.index() else { continue };
        for m in ctx.control.iter().filter(|m| l.matches(m.bytes)) {
            if l.flags & LEARN_LFO_RETRIGGER != 0 {
                let t = frame as f64 / ctx.sample_rate as f64;
                state.params.entry(index).or_default().origin = Some((t, ctx.timeline.qn_at(t)));
                continue;
            }
            let modulated = ctx.mods.entries.iter().find(|e| e.index == index);
            let current = match modulated {
                Some(e) => {
                    let (lo, hi) = native_range(fx, index);
                    let stored = ((e.def.base - lo) / (hi - lo)).clamp(0.0, 1.0);
                    state.params.get(&index).and_then(|p| p.base).unwrap_or(stored)
                }
                None => state.learned.get(&index).copied().or_else(|| normalized(fx, index)).unwrap_or(0.0),
            };
            let Some(value) = learned_value(l, m.bytes, current, state.params.get(&index).and_then(|p| p.control))
            else {
                continue;
            };
            let p = state.params.entry(index).or_default();
            if matches!(l.mode(), LearnMode::Absolute | LearnMode::Other(_)) {
                p.control = Some(control_value(m.bytes));
            }
            if modulated.is_some() {
                p.base = Some(value);
            } else {
                state.learned.insert(index, value);
                let name = l.param.split_once(':').map(|(_, n)| n.to_owned()).unwrap_or_default();
                let target = if name == "bypass" { Target::Bypass } else { Target::Named(name) };
                apply(fx, &target, index, value);
            }
        }
    }
}

/// A controller message's value, 0..1.
fn control_value(msg: [u8; 3]) -> f64 {
    match msg[0] & 0xf0 {
        0xe0 => (msg[1] as u32 | (msg[2] as u32) << 7) as f64 / 16383.0,
        0xc0 | 0xd0 => msg[1] as f64 / 127.0,
        0x80 => 0.0,
        _ => msg[2] as f64 / 127.0,
    }
}

/// The value a learned message gives a parameter now at `current`; `None` when it leaves it
/// (soft takeover waiting for the control to reach the value).
pub fn learned_value(l: &ParamLearn, msg: [u8; 3], current: f64, last_control: Option<f64>) -> Option<f64> {
    let mode = l.mode();
    if let Some(step) = mode.relative_step(msg[2]) {
        return Some((current + step as f64 / 127.0).clamp(0.0, 1.0));
    }
    if mode == LearnMode::Toggle {
        let on = control_value(msg) > 0.0;
        return on.then_some(if current >= 0.5 { 0.0 } else { 1.0 });
    }
    let v = control_value(msg);
    if l.flags & LEARN_SOFT_TAKEOVER != 0 {
        let step = 1.5 / 127.0;
        let crossed = last_control.is_some_and(|last| (last - current) * (v - current) <= 0.0);
        if !crossed && (v - current).abs() > step {
            return None;
        }
    }
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_and_toggle_modes() {
        let mut l = ParamLearn::midi("0", 0xb0, 1, LearnMode::Relative1);
        assert!((learned_value(&l, [0xb0, 1, 1], 0.5, None).unwrap() - (0.5 + 1.0 / 127.0)).abs() < 1e-12);
        assert!((learned_value(&l, [0xb0, 1, 127], 0.5, None).unwrap() - (0.5 - 1.0 / 127.0)).abs() < 1e-12);
        l.set_mode(LearnMode::Relative2);
        assert!((learned_value(&l, [0xb0, 1, 63], 0.5, None).unwrap() - (0.5 - 1.0 / 127.0)).abs() < 1e-12);
        l.set_mode(LearnMode::Relative3);
        assert!((learned_value(&l, [0xb0, 1, 66], 0.5, None).unwrap() - (0.5 - 2.0 / 127.0)).abs() < 1e-12);
        l.set_mode(LearnMode::Toggle);
        assert_eq!(learned_value(&l, [0xb0, 1, 127], 0.2, None), Some(1.0));
        assert_eq!(learned_value(&l, [0xb0, 1, 0], 0.2, None), None);
    }

    #[test]
    fn soft_takeover_waits_for_the_control() {
        let mut l = ParamLearn::midi("0", 0xb0, 1, LearnMode::Absolute);
        l.flags = LEARN_SOFT_TAKEOVER;
        assert_eq!(learned_value(&l, [0xb0, 1, 0], 0.5, None), None);
        assert_eq!(learned_value(&l, [0xb0, 1, 100], 0.5, Some(0.0)), Some(100.0 / 127.0));
        assert_eq!(learned_value(&l, [0xb0, 1, 64], 0.5, None), Some(64.0 / 127.0));
    }

    #[test]
    fn midi_link_reads_controllers_and_pitch() {
        let cc = LinkSource::Midi { bus: 0, channel: 0, status: 176, data1: 11 };
        assert_eq!(midi_link_value(&cc, [0xb3, 11, 127]), Some(1.0));
        assert_eq!(midi_link_value(&cc, [0xb3, 7, 127]), None);
        let ch2 = LinkSource::Midi { bus: 0, channel: 2, status: 176, data1: 11 };
        assert_eq!(midi_link_value(&ch2, [0xb0, 11, 127]), None);
        let pb = LinkSource::Midi { bus: 0, channel: 0, status: 224, data1: 0 };
        assert!((midi_link_value(&pb, [0xe0, 0, 0x60]).unwrap() - 0.75005).abs() < 1e-5);
    }
}
