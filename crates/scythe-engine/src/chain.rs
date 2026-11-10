//! An FX chain as REAPER runs it: effects in series, groups of effects that run in parallel on
//! the same input (`PARALLEL`), containers with their own channels and pin maps, and each
//! slot's wet/dry mix and delta solo. Measured against REAPER 7.73 renders:
//!
//! - A slot mixes `wet * effect + (1 - wet) * input`, the input delayed by the effect's
//!   latency; with delta solo it plays `effect - input`, whatever the wet share.
//! - A parallel group sums its members' outputs, each scaled by its wet share; the input is
//!   not added back. Members are lined up to the slowest one. A bypassed member adds nothing;
//!   with none left the group passes its input on. A member in delta solo replaces the sum
//!   with its own difference. MIDI leaves the group as the first member's output, plus that of
//!   members set to merge it.
//! - A container gathers its input pins from the channels around it, runs its chain on its
//!   own channels (those past its input pins start silent) and writes its output pins back;
//!   channels no output pin feeds pass through.
//! - Wet/dry envelopes act per sample on slots marked sample-accurate. Otherwise the wet share
//!   ramps over each 512-frame block from the previous block's value to the envelope's value at
//!   the block's start (or at the first point inside the block).

use std::sync::{Arc, Mutex};

use scythe_core::fx_chain::Parallel;
use scythe_core::{Envelope, FxSlot};

use crate::fx::{
    FxInstance, FxOwner, FxRegistry, MidiBlock, ParamAutomation, ParamMapping, PinMap, ProcessContext, Target,
    parameter_envelopes, pin_map, slot_automation, slot_key,
};
use crate::midi_io::LiveMidi;
use crate::modulation::FxMods;
use crate::synth::NoteList;

const MAX_BLOCK: usize = crate::render::RenderScratch::MAX_BLOCK;
/// REAPER reads FX envelopes once per block of this many frames.
const SLICE: i64 = 512;

/// The channels a chain works on: 1 and 2, and the rest (each at least `n` frames long).
pub struct Channels<'a> {
    pub l: &'a mut [f32],
    pub r: &'a mut [f32],
    pub aux: &'a mut [Vec<f32>],
    pub n: usize,
}

impl Channels<'_> {
    pub fn count(&self) -> usize {
        2 + self.aux.len()
    }

    fn get(&self, c: usize) -> Option<&[f32]> {
        let n = self.n;
        match c {
            0 => Some(&self.l[..n]),
            1 => Some(&self.r[..n]),
            c => self.aux.get(c - 2).map(|b| &b[..n]),
        }
    }

    fn get_mut(&mut self, c: usize) -> Option<&mut [f32]> {
        let n = self.n;
        match c {
            0 => Some(&mut self.l[..n]),
            1 => Some(&mut self.r[..n]),
            c => self.aux.get_mut(c - 2).map(|b| &mut b[..n]),
        }
    }

    fn copy_to(&self, bufs: &mut [Vec<f32>]) {
        for (c, b) in bufs.iter_mut().enumerate() {
            match self.get(c) {
                Some(src) => b[..self.n].copy_from_slice(src),
                None => b[..self.n].fill(0.0),
            }
        }
    }

    fn copy_from(&mut self, bufs: &[Vec<f32>]) {
        let n = self.n;
        for (c, b) in bufs.iter().enumerate() {
            if let Some(dst) = self.get_mut(c) {
                dst.copy_from_slice(&b[..n]);
            }
        }
    }
}

/// A view of buffers (at least two) as chain channels.
fn view(bufs: &mut [Vec<f32>], n: usize) -> Channels<'_> {
    let (main, aux) = bufs.split_at_mut(2);
    let (l, r) = main.split_at_mut(1);
    Channels { l: &mut l[0][..n], r: &mut r[0][..n], aux, n }
}

fn buffers(channels: usize) -> Vec<Vec<f32>> {
    (0..channels.max(2)).map(|_| vec![0.0; MAX_BLOCK]).collect()
}

/// What the effects of a chain read besides audio.
#[derive(Clone, Copy)]
pub struct ChainInput<'a> {
    pub sample_rate: u32,
    pub playing: bool,
    /// The track's MIDI, for instruments; `None` while stopped.
    pub notes: Option<&'a NoteList>,
    pub live: &'a [LiveMidi],
    pub live_channel: Option<u8>,
    /// Project tempo at a project frame.
    pub tempo: &'a (dyn Fn(i64) -> f64 + Sync),
    pub timeline: &'a scythe_core::tempo::Timeline,
}

/// The MIDI passed down a chain: the current block (`bufs[0]`), room for the next effect's
/// output (`bufs[1]`), and whether any effect has passed MIDI on yet (until then effects read
/// the track's own).
pub struct MidiState<'a> {
    pub bufs: Option<&'a mut [MidiBlock; 3]>,
    pub has: bool,
}

/// A slot's wet/dry settings.
#[derive(Debug, Clone, Default)]
pub struct WetDry {
    pub wet: f64,
    pub delta: bool,
    pub wet_envelope: Option<Box<Envelope>>,
    pub delta_envelope: Option<Box<Envelope>>,
    pub sample_accurate: bool,
}

impl WetDry {
    pub fn of(slot: &FxSlot, ctx: &scythe_core::automation_item::AutomationContext) -> Self {
        let (wet, delta) = slot.wet();
        let mut w = Self { wet, delta, sample_accurate: slot.sample_accurate_wet(), ..Self::default() };
        for a in parameter_envelopes(slot, ctx) {
            match a.target {
                Target::Wet => w.wet_envelope = Some(Box::new(a.envelope)),
                Target::Delta => w.delta_envelope = Some(Box::new(a.envelope)),
                _ => {}
            }
        }
        w
    }

    /// Full wet and no delta solo: the effect's output as it is.
    pub fn plain(&self) -> bool {
        self.wet >= 1.0 && !self.delta && self.wet_envelope.is_none() && self.delta_envelope.is_none()
    }

    /// Wet share for `n` frames from project frame `start` into `out`, with delta solo as a
    /// negative share.
    fn fill(&self, ramp: &mut Ramp, start: i64, sr: f64, out: &mut [f32]) {
        for (i, o) in out.iter_mut().enumerate() {
            let frame = start + i as i64;
            let delta = match &self.delta_envelope {
                Some(env) => block_value(env, frame.div_euclid(SLICE), sr) >= 0.5,
                None => self.delta,
            };
            *o = if delta {
                -1.0
            } else {
                match &self.wet_envelope {
                    None => self.wet as f32,
                    Some(env) if self.sample_accurate => {
                        env.value_at(frame as f64 / sr).unwrap_or(1.0).clamp(0.0, 1.0) as f32
                    }
                    Some(env) => {
                        let block = frame.div_euclid(SLICE);
                        if ramp.block != Some(block) {
                            ramp.from = ramp.to;
                            ramp.to = block_value(env, block, sr).clamp(0.0, 1.0);
                            ramp.block = Some(block);
                        }
                        let k = (frame - block * SLICE) as f64 / SLICE as f64;
                        (ramp.from + (ramp.to - ramp.from) * k) as f32
                    }
                }
            };
        }
    }
}

/// An envelope's value for a 512-frame block: at its first point inside the block, else at
/// the block's start.
fn block_value(env: &Envelope, block: i64, sr: f64) -> f64 {
    let start = block * SLICE;
    let (t0, t1) = (start as f64 / sr, (start + SLICE) as f64 / sr);
    let at = env
        .points
        .iter()
        .find(|p| p.time >= t0 && p.time < t1)
        .map_or(t0, |p| (start as f64 + ((p.time - t0) * sr).floor()) / sr);
    env.value_at(at).unwrap_or(1.0)
}

#[derive(Debug, Default)]
struct Ramp {
    block: Option<i64>,
    from: f64,
    to: f64,
}

/// A fixed delay over a set of channels.
struct Delay {
    lines: Vec<Vec<f32>>,
    at: usize,
}

impl Delay {
    fn new(frames: usize, channels: usize) -> Self {
        Self { lines: (0..if frames > 0 { channels } else { 0 }).map(|_| vec![0.0; frames]).collect(), at: 0 }
    }

    fn run(&mut self, bufs: &mut [Vec<f32>], n: usize) {
        let Some(len) = self.lines.first().map(Vec::len).filter(|&l| l > 0) else { return };
        for (line, buf) in self.lines.iter_mut().zip(bufs.iter_mut()) {
            for (i, s) in buf[..n].iter_mut().enumerate() {
                let p = (self.at + i) % len;
                std::mem::swap(&mut line[p], s);
            }
        }
        self.at = (self.at + n) % len;
    }
}

/// Per-slot state for mixing wet and dry.
struct MixState {
    ramp: Ramp,
    wet: Vec<f32>,
    /// The input, delayed by the slot's latency; only kept for serial slots that mix it in.
    dry: Vec<Vec<f32>>,
    delay: Delay,
}

pub struct FxNode {
    pub fx: Arc<FxInstance>,
    /// Parameter envelopes, including those of container parameters mapped onto it.
    pub automation: Vec<ParamAutomation>,
    /// The pin map, for effects that do not simply process channels 1/2.
    pub pins: Option<PinMap>,
    /// Registry key, for finding the instance.
    pub key: String,
    /// Its parameter modulation and learned controls, when it has any.
    pub mods: Option<Arc<FxMods>>,
    /// Its position among `level`.
    pub own: usize,
    /// The running effects of the chain (or container) it sits in, which linked parameters
    /// follow.
    pub level: Arc<[Arc<FxInstance>]>,
}

pub struct Container {
    pub chain: FxChain,
    /// Pins: per input pin the outer channels it gathers, per output pin those it feeds.
    pub pins: PinMap,
    bufs: Mutex<Vec<Vec<f32>>>,
}

pub enum NodeKind {
    Fx(FxNode),
    Container(Box<Container>),
}

/// One active slot of a chain.
pub struct ChainNode {
    pub kind: NodeKind,
    pub wet: WetDry,
    pub parallel: Parallel,
    /// Frames it delays its output by.
    pub latency: i64,
    /// Whether it writes audio; a MIDI-only effect adds nothing to a parallel sum.
    pub audio: bool,
    mix: Mutex<MixState>,
    fade: Option<StageFade>,
}

/// A rig stage being switched on or off: rather than cutting in or out, it crossfades with
/// its input over [`StageFade::SECONDS`], so switching a stage on stage does not click. A
/// stage switched off stays in the chain, passing its input on, until the chain is rebuilt.
struct StageFade {
    fx: Arc<FxInstance>,
    on: bool,
    dry: Mutex<Vec<Vec<f32>>>,
}

impl StageFade {
    const SECONDS: f32 = 0.01;

    fn level(&self) -> f32 {
        f32::from_bits(self.fx.stage_level.load(std::sync::atomic::Ordering::Relaxed))
    }
}

impl ChainNode {
    fn new(kind: NodeKind, wet: WetDry, parallel: Parallel, channels: usize, serial: bool) -> Self {
        let latency = match &kind {
            NodeKind::Fx(f) => f.fx.with(|e| e.latency()).unwrap_or(0) as i64,
            NodeKind::Container(c) => c.chain.latency,
        };
        let audio = match &kind {
            NodeKind::Fx(f) => f.fx.audio,
            NodeKind::Container(_) => true,
        };
        let mixes = !wet.plain();
        let mix = MixState {
            ramp: Ramp { block: None, from: wet.wet, to: wet.wet },
            wet: if mixes { vec![0.0; MAX_BLOCK] } else { Vec::new() },
            dry: if mixes && serial { buffers(channels) } else { Vec::new() },
            delay: Delay::new(if mixes && serial { latency as usize } else { 0 }, channels.max(2)),
        };
        Self { kind, wet, parallel, latency, audio, mix: Mutex::new(mix), fade: None }
    }

    /// Runs the slot, crossfading with its input while its stage fades in or out.
    fn run_fading(
        &self,
        fade: &StageFade,
        ch: &mut Channels,
        pin_bufs: Option<&mut [Vec<f32>]>,
        input: &ChainInput,
        position: i64,
        midi: &mut MidiState,
    ) {
        let target = if fade.on { 1.0 } else { 0.0 };
        let mut level = fade.level();
        if level == target {
            if fade.on {
                self.run_mixed(ch, pin_bufs, input, position, midi);
            }
            return;
        }
        let Ok(mut dry) = fade.dry.try_lock() else {
            self.run_mixed(ch, pin_bufs, input, position, midi);
            return;
        };
        let n = ch.n;
        ch.copy_to(&mut dry);
        self.run_mixed(ch, pin_bufs, input, position, midi);
        let step = 1.0 / (StageFade::SECONDS * input.sample_rate as f32).max(1.0);
        let start = level;
        for (c, d) in dry.iter().enumerate() {
            let Some(out) = ch.get_mut(c) else { continue };
            level = start;
            for (o, &x) in out.iter_mut().zip(&d[..n]) {
                level = if target > level { (level + step).min(target) } else { (level - step).max(target) };
                *o = x + (*o - x) * level;
            }
        }
        fade.fx.stage_level.store(level.to_bits(), std::sync::atomic::Ordering::Relaxed);
    }

    /// Runs the slot's effect or container in place; returns whether it ran. With
    /// `mute_skipped`, blocks a bypass envelope skips come out silent instead of passing the
    /// input on, as a parallel member's do.
    fn run(
        &self,
        ch: &mut Channels,
        pin_bufs: Option<&mut [Vec<f32>]>,
        input: &ChainInput,
        position: i64,
        midi: &mut MidiState,
        mute_skipped: bool,
    ) -> bool {
        match &self.kind {
            NodeKind::Fx(f) => run_fx(f, ch, pin_bufs, input, position, midi, mute_skipped),
            NodeKind::Container(c) => c.run(ch, pin_bufs, input, position, midi),
        }
    }

    /// The slot on its own: its output mixed with its input as wet/dry says.
    fn run_serial(
        &self,
        ch: &mut Channels,
        pin_bufs: Option<&mut [Vec<f32>]>,
        input: &ChainInput,
        position: i64,
        midi: &mut MidiState,
    ) {
        match &self.fade {
            Some(fade) => self.run_fading(fade, ch, pin_bufs, input, position, midi),
            None => self.run_mixed(ch, pin_bufs, input, position, midi),
        }
    }

    fn run_mixed(
        &self,
        ch: &mut Channels,
        pin_bufs: Option<&mut [Vec<f32>]>,
        input: &ChainInput,
        position: i64,
        midi: &mut MidiState,
    ) {
        let mixing = if self.wet.plain() { None } else { self.mix.try_lock().ok() };
        let Some(mut mix) = mixing else {
            self.run(ch, pin_bufs, input, position, midi, false);
            return;
        };
        let n = ch.n;
        let MixState { ramp, wet, dry, delay } = &mut *mix;
        ch.copy_to(dry);
        self.run(ch, pin_bufs, input, position, midi, false);
        delay.run(dry, n);
        self.wet.fill(ramp, position, input.sample_rate as f64, &mut wet[..n]);
        for (c, d) in dry.iter().enumerate() {
            let Some(out) = ch.get_mut(c) else { continue };
            for ((o, &x), &w) in out.iter_mut().zip(&d[..n]).zip(&wet[..n]) {
                *o = if w < 0.0 { *o - x } else { w * *o + (1.0 - w) * x };
            }
        }
    }
}

impl Container {
    fn run(
        &self,
        ch: &mut Channels,
        pin_bufs: Option<&mut [Vec<f32>]>,
        input: &ChainInput,
        position: i64,
        midi: &mut MidiState,
    ) -> bool {
        let Ok(mut bufs) = self.bufs.try_lock() else { return false };
        let n = ch.n;
        gather_pins(&self.pins, &mut bufs, ch);
        self.chain.process(&mut view(&mut bufs, n), input, position, midi, pin_bufs);
        scatter_pins(&self.pins, &bufs, ch);
        true
    }
}

struct GroupScratch {
    input: Vec<Vec<f32>>,
    dry: Vec<Vec<f32>>,
    work: Vec<Vec<f32>>,
    sum: Vec<Vec<f32>>,
    dry_delay: Delay,
    /// Per member, the delay that lines it up with the slowest.
    align: Vec<Delay>,
    midi_in: MidiBlock,
    midi_out: MidiBlock,
}

/// Slots that run in parallel on the same input.
pub struct Group {
    pub members: Vec<ChainNode>,
    pub latency: i64,
    scratch: Mutex<GroupScratch>,
}

impl Group {
    fn new(members: Vec<ChainNode>, channels: usize) -> Self {
        let latency = members.iter().map(|m| m.latency).max().unwrap_or(0);
        let scratch = GroupScratch {
            input: buffers(channels),
            dry: buffers(channels),
            work: buffers(channels),
            sum: buffers(channels),
            dry_delay: Delay::new(latency as usize, channels.max(2)),
            align: members.iter().map(|m| Delay::new((latency - m.latency) as usize, channels.max(2))).collect(),
            midi_in: MidiBlock::default(),
            midi_out: MidiBlock::default(),
        };
        Self { members, latency, scratch: Mutex::new(scratch) }
    }

    fn run(
        &self,
        ch: &mut Channels,
        mut pin_bufs: Option<&mut [Vec<f32>]>,
        input: &ChainInput,
        position: i64,
        midi: &mut MidiState,
    ) {
        let Ok(mut scratch) = self.scratch.try_lock() else { return };
        let n = ch.n;
        let GroupScratch { input: group_in, dry, work, sum, dry_delay, align, midi_in, midi_out } = &mut *scratch;
        ch.copy_to(group_in);
        for (d, s) in dry.iter_mut().zip(group_in.iter()) {
            d[..n].copy_from_slice(&s[..n]);
        }
        dry_delay.run(dry, n);
        for b in sum.iter_mut() {
            b[..n].fill(0.0);
        }
        let has_in = midi.has;
        if let Some([held, ..]) = midi.bufs.as_deref() {
            midi_in.copy_from(held);
            midi_out.copy_from(held);
        }
        let mut has_out = has_in;
        let mut contributed = false;
        let mut delta = false;
        for (k, (member, align)) in self.members.iter().zip(align.iter_mut()).enumerate() {
            for (w, s) in work.iter_mut().zip(group_in.iter()) {
                w[..n].copy_from_slice(&s[..n]);
            }
            if k > 0
                && let Some([held, ..]) = midi.bufs.as_deref_mut()
            {
                held.copy_from(midi_in);
            }
            midi.has = has_in;
            let ran = member.run(&mut view(work, n), pin_bufs.as_deref_mut(), input, position, midi, true);
            align.run(work, n);
            if let Some([held, ..]) = midi.bufs.as_deref() {
                if k == 0 {
                    midi_out.copy_from(held);
                    has_out = midi.has;
                } else if member.parallel == Parallel::MergeMidi && midi.has {
                    if !has_out {
                        midi_out.clear();
                    }
                    midi_out.short.extend_from_slice(&held.short);
                    midi_out.long.extend(held.long.iter().cloned());
                    midi_out.bused.extend_from_slice(&held.bused);
                    midi_out.sort();
                    has_out = true;
                }
            }
            if !ran || !member.audio || delta {
                continue;
            }
            let Ok(mut mix) = member.mix.try_lock() else { continue };
            let MixState { ramp, wet, .. } = &mut *mix;
            let weights = if member.wet.plain() {
                None
            } else {
                member.wet.fill(ramp, position, input.sample_rate as f64, &mut wet[..n]);
                Some(&wet[..n])
            };
            if weights.is_some_and(|w| w.iter().any(|&w| w < 0.0)) {
                // Delta solo: this member's difference replaces the group's output.
                for ((s, w), d) in sum.iter_mut().zip(work.iter()).zip(dry.iter()) {
                    for i in 0..n {
                        s[i] = w[i] - d[i];
                    }
                }
                delta = true;
            } else {
                for (s, w) in sum.iter_mut().zip(work.iter()) {
                    match weights {
                        Some(g) => {
                            for i in 0..n {
                                s[i] += g[i] * w[i];
                            }
                        }
                        None => {
                            for i in 0..n {
                                s[i] += w[i];
                            }
                        }
                    }
                }
            }
            contributed = true;
        }
        if let Some([held, ..]) = midi.bufs.as_deref_mut() {
            held.copy_from(midi_out);
        }
        midi.has = has_out;
        ch.copy_from(if contributed { sum } else { dry });
    }
}

pub enum Step {
    Node(ChainNode),
    Group(Group),
}

/// A built chain: its steps in order, each a slot or a parallel group.
#[derive(Default)]
pub struct FxChain {
    pub steps: Vec<Step>,
    /// Frames the chain delays its output by.
    pub latency: i64,
}

impl std::fmt::Debug for FxChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FxChain").field("steps", &self.steps.len()).field("latency", &self.latency).finish()
    }
}

impl FxChain {
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Every active slot, containers' contents included, in order.
    pub fn nodes(&self) -> Vec<&ChainNode> {
        fn walk<'a>(chain: &'a FxChain, out: &mut Vec<&'a ChainNode>) {
            for step in &chain.steps {
                let members: &[ChainNode] = match step {
                    Step::Node(n) => std::slice::from_ref(n),
                    Step::Group(g) => &g.members,
                };
                for m in members {
                    out.push(m);
                    if let NodeKind::Container(c) = &m.kind {
                        walk(&c.chain, out);
                    }
                }
            }
        }
        let mut out = Vec::new();
        walk(self, &mut out);
        out
    }

    /// The effects, containers' contents included.
    pub fn effects(&self) -> impl Iterator<Item = &FxNode> {
        self.nodes().into_iter().filter_map(|n| match &n.kind {
            NodeKind::Fx(f) => Some(f),
            NodeKind::Container(_) => None,
        })
    }

    pub fn has_instrument(&self) -> bool {
        self.effects().any(|f| f.fx.instrument)
    }

    /// Whether a modulation reads the track's MIDI.
    pub fn reads_midi(&self) -> bool {
        self.effects().any(|f| f.mods.as_ref().is_some_and(|m| m.reads_midi()))
    }

    /// Whether an effect's parameter follows another effect's.
    pub fn links_effects(&self) -> bool {
        self.effects()
            .any(|f| f.mods.as_ref().is_some_and(|m| m.entries.iter().any(|e| e.source.is_some_and(|s| s.0 != f.own))))
    }

    /// Pin buffers the widest mapped effect needs (at least two audio pins and a sidechain
    /// pair), when any effect has a pin map.
    pub fn pin_buffers(&self) -> Option<usize> {
        self.effects()
            .filter_map(|f| {
                let m = f.pins.as_ref()?;
                Some(f.fx.pins.0.max(f.fx.pins.1).max(m.inputs.len()).max(m.outputs.len()).max(4))
            })
            .max()
    }

    /// Runs the chain in place on `ch`, its input at project frame `position`.
    pub fn process(
        &self,
        ch: &mut Channels,
        input: &ChainInput,
        position: i64,
        midi: &mut MidiState,
        mut pin_bufs: Option<&mut [Vec<f32>]>,
    ) {
        let mut position = position;
        for step in &self.steps {
            match step {
                Step::Node(node) => {
                    node.run_serial(ch, pin_bufs.as_deref_mut(), input, position, midi);
                    position -= node.latency;
                }
                Step::Group(g) => {
                    g.run(ch, pin_bufs.as_deref_mut(), input, position, midi);
                    position -= g.latency;
                }
            }
        }
    }
}

/// A chain with buffers of its own, for audio that joins a track from the side: a take's
/// effects, a track's input effects, the monitoring effects on the device output.
pub struct BufferedChain {
    pub chain: FxChain,
    pub channels: usize,
    scratch: Mutex<(Vec<Vec<f32>>, [MidiBlock; 3])>,
}

impl std::fmt::Debug for BufferedChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BufferedChain").field("chain", &self.chain).field("channels", &self.channels).finish()
    }
}

impl BufferedChain {
    pub fn new(chain: FxChain, channels: usize) -> Self {
        Self { chain, channels: channels.max(2), scratch: Mutex::new((buffers(channels), Default::default())) }
    }

    /// Runs the block of `n` frames at `position`. `io` first fills the chain's channels with
    /// the input (`ChainIo::Fill`, from the project frame the chain reads at, `chain.latency`
    /// ahead), then takes the result (`ChainIo::Drain`).
    pub fn run(&self, n: usize, position: i64, input: &ChainInput, mut io: impl FnMut(ChainIo, &mut [Vec<f32>])) {
        let Ok(mut scratch) = self.scratch.try_lock() else { return };
        let (bufs, midi) = &mut *scratch;
        for b in bufs.iter_mut() {
            b[..n].fill(0.0);
        }
        io(ChainIo::Fill(position + self.chain.latency), bufs);
        let mut state = MidiState { bufs: Some(midi), has: false };
        self.chain.process(&mut view(bufs, n), input, position + self.chain.latency, &mut state, None);
        io(ChainIo::Drain, bufs);
    }
}

/// What [`BufferedChain::run`] asks of its caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainIo {
    /// Write the input, read from this project frame on.
    Fill(i64),
    /// Take the output.
    Drain,
}

/// How long take effects keep running after their item ends, in seconds.
pub const TAKE_FX_TAIL: f64 = 2.0;

/// The project frames a take's effects run over, for an item at `start` of `length` frames.
/// REAPER runs them after the item's volume, pan and fades, in 512-frame blocks from the one
/// the item starts in until two seconds past its end, so effect tails ring on; what they
/// output before the item starts is dropped.
pub fn take_fx_span(start: i64, length: i64, sample_rate: u32) -> std::ops::Range<i64> {
    let tail = (TAKE_FX_TAIL * sample_rate as f64).round() as i64;
    let end = start + length + tail;
    start.div_euclid(SLICE) * SLICE..(end + SLICE - 1).div_euclid(SLICE) * SLICE
}

/// Builds `slots` of `owner` into a chain on `channels` channels, creating instances in the
/// registry. Offline slots are left unloaded; bypassed ones keep their instance but do not
/// run.
pub fn build(registry: &mut FxRegistry, owner: &FxOwner, slots: &[FxSlot], channels: usize) -> FxChain {
    build_at(registry, owner, slots, channels.max(2), "", Vec::new(), false)
}

/// With `stages` (a rig's children), slots switching on or off fade (see [`StageFade`]).
fn build_at(
    registry: &mut FxRegistry,
    owner: &FxOwner,
    slots: &[FxSlot],
    channels: usize,
    prefix: &str,
    mut mapped: Vec<Vec<ParamAutomation>>,
    stages: bool,
) -> FxChain {
    mapped.resize_with(slots.len(), Vec::new);
    // Instances first: a modulation linked to another effect finds it by its position among
    // the running effects of the chain.
    let instances: Vec<Option<Arc<FxInstance>>> = slots
        .iter()
        .enumerate()
        .map(|(i, slot)| {
            (!slot.offline() && !slot.is_container())
                .then(|| registry.slot_instance(owner, format!("{prefix}{}", slot_key(slot, i)), slot, channels))
                .flatten()
        })
        .collect();
    let mut running = 0;
    let positions: Vec<Option<usize>> = slots
        .iter()
        .zip(&instances)
        .map(|(slot, inst)| {
            (slot.enabled && inst.is_some()).then(|| {
                running += 1;
                running - 1
            })
        })
        .collect();
    let level: Arc<[Arc<FxInstance>]> =
        slots.iter().zip(&instances).filter(|(s, _)| s.enabled).filter_map(|(_, i)| i.clone()).collect();
    let mut entries: Vec<(Parallel, Option<ChainNode>)> = Vec::new();
    for (i, slot) in slots.iter().enumerate() {
        let parallel = if i == 0 { Parallel::Serial } else { slot.parallel() };
        let serial =
            slots.get(i + 1).is_none_or(|next| next.parallel() == Parallel::Serial) && parallel == Parallel::Serial;
        let key = format!("{prefix}{}", slot_key(slot, i));
        let extra = std::mem::take(&mut mapped[i]);
        let fade = instances[i].as_ref().filter(|_| stages && serial && parallel == Parallel::Serial).and_then(|fx| {
            use std::sync::atomic::Ordering::Relaxed;
            let target = if slot.enabled { 1.0f32 } else { 0.0 };
            let level = f32::from_bits(fx.stage_level.load(Relaxed));
            let latency = fx.with(|e| e.latency()).unwrap_or(0);
            if level.is_nan() || latency != 0 {
                fx.stage_level.store(target.to_bits(), Relaxed);
                return None;
            }
            (level != target || !slot.enabled).then(|| StageFade {
                fx: fx.clone(),
                on: slot.enabled,
                dry: Mutex::new(buffers(channels)),
            })
        });
        let fading_out = fade.as_ref().is_some_and(|f| !f.on && f.level() > 0.0);
        let kind = if slot.offline() {
            None
        } else if slot.is_container() {
            slot.enabled.then(|| NodeKind::Container(Box::new(container(registry, owner, slot, &key, extra))))
        } else {
            instances[i].clone().filter(|_| slot.enabled || fading_out).map(|fx| {
                let mut automation = slot_automation(slot, &fx, &registry.automation);
                if !extra.is_empty() {
                    let params = fx.with(|e| e.params()).unwrap_or_default();
                    automation.extend(extra.into_iter().map(|mut a| {
                        a.fallback =
                            params.get(a.index).map(|p| ParamMapping { id: p.id, from: a.range, to: (p.min, p.max) });
                        a
                    }));
                }
                let pins = effective_pins(slot, &fx);
                let mods = Some(Arc::new(FxMods::from_slot(slot, i, &positions))).filter(|m| !m.is_empty());
                let own = positions[i].unwrap_or(0);
                NodeKind::Fx(FxNode { fx, automation, pins, key, mods, own, level: level.clone() })
            })
        };
        entries.push((
            parallel,
            kind.map(|k| {
                let mut node = ChainNode::new(k, WetDry::of(slot, &registry.automation), parallel, channels, serial);
                node.fade = fade.filter(|f| f.on || fading_out);
                node
            }),
        ));
    }
    let mut steps = Vec::new();
    let mut entries = entries.into_iter().peekable();
    while let Some((_, first)) = entries.next() {
        let mut members: Vec<ChainNode> = first.into_iter().collect();
        let mut grouped = false;
        while let Some((_, next)) = entries.next_if(|(p, _)| *p != Parallel::Serial) {
            grouped = true;
            members.extend(next);
        }
        if grouped {
            if !members.is_empty() {
                steps.push(Step::Group(Group::new(members, channels)));
            }
        } else {
            steps.extend(members.into_iter().map(Step::Node));
        }
    }
    let latency = steps
        .iter()
        .map(|s| match s {
            Step::Node(n) => n.latency,
            Step::Group(g) => g.latency,
        })
        .sum();
    FxChain { steps, latency }
}

/// A container slot: its children built on its own channels, with the envelopes of its mapped
/// parameters (and those mapped from outside, `extra`, by its parameter positions) handed to
/// the children they stand for.
fn container(
    registry: &mut FxRegistry,
    owner: &FxOwner,
    slot: &FxSlot,
    key: &str,
    extra: Vec<ParamAutomation>,
) -> Container {
    let cfg = slot.container_config().unwrap_or_default();
    let children = slot.container_children();
    let params = slot.container_params();
    let mut mapped: Vec<Vec<ParamAutomation>> = vec![Vec::new(); children.len()];
    let own = parameter_envelopes(slot, &registry.automation).into_iter().filter_map(|mut a| {
        let Target::Named(ident) = &a.target else { return None };
        a.index = ident.parse::<u32>().ok().and_then(|id| params.iter().position(|p| p.id == id)).unwrap_or(a.index);
        Some(a)
    });
    for mut a in own.chain(extra) {
        let Some(p) = params.get(a.index) else { continue };
        let Some(list) = mapped.get_mut(p.child) else { continue };
        a.target = Target::Named(String::new());
        a.index = p.param;
        list.push(a);
    }
    let channels = cfg.channels as usize;
    let chain =
        build_at(registry, owner, &children, channels.max(2), &format!("{key}/"), mapped, crate::rig::is_rig(slot));
    let listed = |output: bool, count: u32| {
        let mut masks = slot.listed_pins(output).unwrap_or_default();
        let count = (count as usize).min(64);
        masks.truncate(count);
        let from = masks.len();
        masks.extend((from..count).map(|p| 1u64 << p));
        masks
    };
    let pins = PinMap { inputs: listed(false, cfg.inputs), outputs: listed(true, cfg.outputs) };
    Container { chain, pins, bufs: Mutex::new(buffers(channels)) }
}

/// The pin map an effect works through: the stored map, or REAPER's default of pin n on
/// channel n for effects with more than two pins on a side (fewer pins work on the first
/// pair as they are).
fn effective_pins(slot: &FxSlot, inst: &FxInstance) -> Option<PinMap> {
    // Pins a saved map leaves out keep their default channel (pin n on channel n).
    let widen = |masks: &mut Vec<u64>, n: usize| {
        let from = masks.len();
        masks.extend((from..n.min(64)).map(|p| 1u64 << p));
    };
    pin_map(slot)
        .map(|mut m| {
            widen(&mut m.inputs, inst.pins.0);
            widen(&mut m.outputs, inst.pins.1);
            m
        })
        .or_else(|| {
            (inst.pins.0 > 2 || inst.pins.1 > 2).then(|| PinMap {
                inputs: (0..inst.pins.0.min(64)).map(|p| 1u64 << p).collect(),
                outputs: (0..inst.pins.1.min(64)).map(|p| 1u64 << p).collect(),
            })
        })
}

/// Fills each pin buffer with the sum of the channels its input mask selects.
fn gather_pins(map: &PinMap, bufs: &mut [Vec<f32>], ch: &Channels) {
    let n = ch.n;
    for (pin, buf) in bufs.iter_mut().enumerate() {
        buf[..n].fill(0.0);
        let mask = map.input(pin);
        for c in (0..ch.count().min(64)).filter(|c| mask & (1 << c) != 0) {
            let Some(src) = ch.get(c) else { continue };
            for (d, s) in buf[..n].iter_mut().zip(src) {
                *d += s;
            }
        }
    }
}

/// Writes the output pins back: a channel any output pin feeds takes the sum of those pins;
/// the others pass through.
fn scatter_pins(map: &PinMap, bufs: &[Vec<f32>], ch: &mut Channels) {
    for c in 0..ch.count().min(64) {
        let bit = 1u64 << c;
        let feeding = || (0..bufs.len()).filter(move |&p| map.output(p) & bit != 0);
        if feeding().next().is_none() {
            continue;
        }
        let Some(dst) = ch.get_mut(c) else { continue };
        for (i, d) in dst.iter_mut().enumerate() {
            *d = feeding().map(|p| bufs[p][i]).sum();
        }
    }
}

/// Runs one effect, through its pin map when it has one, with its parameter envelopes set
/// once per 512-frame block as REAPER does, and passes its MIDI on. Returns whether it ran.
fn run_fx(
    node: &FxNode,
    ch: &mut Channels,
    pin_bufs: Option<&mut [Vec<f32>]>,
    input: &ChainInput,
    position: i64,
    midi: &mut MidiState,
    mute_skipped: bool,
) -> bool {
    let fx = &node.fx;
    let automation = &node.automation[..];
    let n = ch.n;
    let sr = input.sample_rate as f64;
    let mut ctx = ProcessContext {
        sample_rate: input.sample_rate,
        position,
        playing: input.playing,
        aux: None,
        notes: input.notes,
        live: input.live,
        live_channel: input.live_channel,
        bpm: (input.tempo)(position),
        midi: None,
        midi_from: 0,
    };
    let (chain_midi, next_midi) = match midi.bufs.as_deref_mut() {
        Some([held, next, _]) => (midi.has.then_some(&*held), Some(next)),
        None => (None, None),
    };
    ctx.midi = chain_midi;
    let t = position as f64 / sr;
    let slices = |n: usize| {
        let mut at = 0;
        std::iter::from_fn(move || {
            (at < n).then(|| {
                let frame = position + at as i64;
                let len = ((SLICE - frame.rem_euclid(SLICE)) as usize).min(n - at);
                at += len;
                (at - len, len, frame)
            })
        })
    };
    let mod_ctx = node.mods.as_deref().map(|m| crate::modulation::ModContext {
        mods: m,
        chain: &node.level,
        own: node.own,
        timeline: input.timeline,
        sample_rate: input.sample_rate,
    });
    let plain = automation.is_empty() && mod_ctx.is_none();
    let mut ran = false;
    let pins = node.pins.as_ref().filter(|_| pin_bufs.is_some());
    if let (Some(map), Some(bufs)) = (pins, pin_bufs) {
        gather_pins(map, bufs, ch);
        if fx.pins.0 > 2 || fx.pins.1 > 2 {
            // Several pins: the effect works on all the gathered buffers at once.
            if plain {
                ran = fx.process_pins(bufs, 0..n, &ctx);
            } else {
                for (at, len, frame) in slices(n) {
                    let slice_ctx = ProcessContext {
                        position: frame,
                        live: if at == 0 { ctx.live } else { &[] },
                        midi_from: at,
                        ..ctx
                    };
                    let done = fx.process_pins_automated(
                        bufs,
                        at..at + len,
                        &slice_ctx,
                        automation,
                        t + at as f64 / sr,
                        mod_ctx.as_ref(),
                    );
                    if !done && mute_skipped {
                        bufs.iter_mut().for_each(|b| b[at..at + len].fill(0.0));
                    }
                    ran |= done;
                }
            }
        } else {
            let (main, side) = bufs.split_at_mut(2);
            let (a, b) = main.split_at_mut(1);
            ctx.aux = (map.inputs.len() >= 4).then(|| (&side[0][..n], &side[1][..n]));
            let (pl, pr) = (&mut a[0][..n], &mut b[0][..n]);
            if plain {
                ran = fx.process(pl, pr, &ctx);
            } else {
                for (at, len, frame) in slices(n) {
                    let slice_ctx = ProcessContext {
                        position: frame,
                        live: if at == 0 { ctx.live } else { &[] },
                        aux: ctx.aux.map(|(a, b)| (&a[at..at + len], &b[at..at + len])),
                        midi_from: at,
                        ..ctx
                    };
                    let done = fx.process_automated(
                        &mut pl[at..at + len],
                        &mut pr[at..at + len],
                        &slice_ctx,
                        automation,
                        t + at as f64 / sr,
                        mod_ctx.as_ref(),
                    );
                    if !done && mute_skipped {
                        pl[at..at + len].fill(0.0);
                        pr[at..at + len].fill(0.0);
                    }
                    ran |= done;
                }
            }
        }
        scatter_pins(map, bufs, ch);
    } else {
        let Channels { l, r, aux, .. } = ch;
        let mut skipped = [(0, 0); MAX_BLOCK / SLICE as usize + 1];
        let mut skips = 0;
        ctx.aux = (aux.len() >= 2).then(|| (&aux[0][..n], &aux[1][..n]));
        if plain {
            ran = fx.process(l, r, &ctx);
        } else {
            for (at, len, frame) in slices(n) {
                let slice_ctx = ProcessContext {
                    position: frame,
                    live: if at == 0 { ctx.live } else { &[] },
                    aux: ctx.aux.map(|(a, b)| (&a[at..at + len], &b[at..at + len])),
                    midi_from: at,
                    ..ctx
                };
                let done = fx.process_automated(
                    &mut l[at..at + len],
                    &mut r[at..at + len],
                    &slice_ctx,
                    automation,
                    t + at as f64 / sr,
                    mod_ctx.as_ref(),
                );
                if !done && mute_skipped {
                    l[at..at + len].fill(0.0);
                    r[at..at + len].fill(0.0);
                    if let Some(s) = skipped.get_mut(skips) {
                        *s = (at, len);
                        skips += 1;
                    }
                }
                ran |= done;
            }
        }
        for &(at, len) in &skipped[..skips] {
            aux.iter_mut().for_each(|b| b[at..at + len].fill(0.0));
        }
    }
    if let Some(next) = next_midi {
        next.clear();
        if fx.take_midi(next) {
            if !fx.midi_buses
                && let Some(held) = chain_midi
            {
                next.bused.extend_from_slice(&held.bused);
            }
            midi.has = true;
            if let Some([held, next, _]) = midi.bufs.as_deref_mut() {
                std::mem::swap(held, next);
            }
        }
    }
    ran
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::Effect;

    struct Gain(f32);

    impl Effect for Gain {
        fn process(&mut self, left: &mut [f32], right: &mut [f32], _: &ProcessContext) {
            left.iter_mut().chain(right.iter_mut()).for_each(|s| *s *= self.0);
        }
    }

    struct Swap;

    impl Effect for Swap {
        fn process(&mut self, left: &mut [f32], right: &mut [f32], _: &ProcessContext) {
            left.swap_with_slice(right);
        }
    }

    fn fx(e: impl Effect + 'static, wet: f64, delta: bool, parallel: Parallel) -> ChainNode {
        let node = FxNode {
            fx: Arc::new(FxInstance::new(Box::new(e))),
            automation: Vec::new(),
            pins: None,
            key: String::new(),
            mods: None,
            own: 0,
            level: Arc::new([]),
        };
        let wet = WetDry { wet, delta, ..WetDry::default() };
        ChainNode::new(NodeKind::Fx(node), wet, parallel, 2, parallel == Parallel::Serial)
    }

    fn chain(steps: Vec<Step>) -> FxChain {
        FxChain { steps, latency: 0 }
    }

    fn group(members: Vec<ChainNode>) -> Step {
        Step::Group(Group::new(members, 2))
    }

    /// Runs `chain` on one frame of (1, 2).
    fn run(chain: &FxChain) -> (f32, f32) {
        let (mut l, mut r) = (vec![1.0f32], vec![2.0f32]);
        let tempo = |_: i64| 120.0;
        let input = ChainInput {
            sample_rate: 48_000,
            playing: true,
            notes: None,
            live: &[],
            live_channel: None,
            tempo: &tempo,
            timeline: &scythe_core::TempoMap::default().timeline(),
        };
        let mut ch = Channels { l: &mut l, r: &mut r, aux: &mut [], n: 1 };
        chain.process(&mut ch, &input, 0, &mut MidiState { bufs: None, has: false }, None);
        (l[0], r[0])
    }

    #[test]
    fn wet_mixes_the_input_back() {
        assert_eq!(run(&chain(vec![Step::Node(fx(Gain(0.5), 0.25, false, Parallel::Serial))])), (0.875, 1.75));
        assert_eq!(run(&chain(vec![Step::Node(fx(Gain(0.5), 0.25, true, Parallel::Serial))])), (-0.5, -1.0));
    }

    #[test]
    fn parallel_members_sum_without_the_input() {
        let g = group(vec![fx(Gain(0.5), 1.0, false, Parallel::Serial), fx(Swap, 0.5, false, Parallel::Audio)]);
        assert_eq!(run(&chain(vec![g])), (1.5, 1.5));
        let g = group(vec![fx(Gain(0.5), 1.0, false, Parallel::Serial), fx(Gain(0.25), 1.0, true, Parallel::Audio)]);
        assert_eq!(run(&chain(vec![g])), (-0.75, -1.5));
        assert_eq!(run(&chain(vec![group(Vec::new())])), (1.0, 2.0));
    }

    #[test]
    fn containers_route_through_their_pins() {
        let inner = chain(vec![Step::Node(fx(Gain(0.5), 1.0, false, Parallel::Serial))]);
        let c = Container {
            chain: inner,
            pins: PinMap { inputs: vec![2, 1], outputs: vec![0b11] },
            bufs: Mutex::new(buffers(4)),
        };
        let node = ChainNode::new(
            NodeKind::Container(Box::new(c)),
            WetDry { wet: 1.0, ..WetDry::default() },
            Parallel::Serial,
            2,
            true,
        );
        // Pin 1 takes the right channel; its output feeds both, pin 2 has none.
        assert_eq!(run(&chain(vec![Step::Node(node)])), (1.0, 1.0));
    }
}
