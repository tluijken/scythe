//! Track and master FX chains. Instances outlive render graph rebuilds: the registry keeps them
//! keyed by slot identity and recreates one only when its stored state changes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use scythe_core::rpp::{Chunk, Node, line_key, tokenize};
use scythe_core::{FxSlot, Project, TrackId};

use crate::clap_host::{ClapDescriptor, ClapLibrary, ClapPlugin, ParamInfo};
use crate::delay::{Limiter, ReaDelayCompat, ReaDelaySettings};
use crate::dynamics::{Compressor, Gate};
use crate::midi_io::LiveMidi;
use crate::nam::NamEffect;
use crate::reaeq::ReaEqCompat;
use crate::reaverb::{ReaVerbCompat, ReaVerbSettings};
use crate::sampler::{Rs5kCompat, Rs5kSettings};
use crate::synth::NoteList;
use crate::vst2::{ReaperVstState, Vst2Plugin};

#[derive(Clone, Copy)]
pub struct ProcessContext<'a> {
    pub sample_rate: u32,
    /// Project frame of the first sample in the block.
    pub position: i64,
    pub playing: bool,
    /// The track's MIDI notes, for instruments.
    pub notes: Option<&'a NoteList>,
    /// Live MIDI input, due at the start of the block; only the messages on `live_channel`
    /// (all when `None`) are meant for this track.
    pub live: &'a [LiveMidi],
    pub live_channel: Option<u8>,
    /// Track channels 3 and 4, for effects with an auxiliary (sidechain) input.
    pub aux: Option<(&'a [f32], &'a [f32])>,
    /// Project tempo at the block start, in quarter notes per minute.
    pub bpm: f64,
    /// MIDI an earlier effect in the chain passed on, as frame offsets from the start of the
    /// chain's block; when set it stands in for `notes` and `live`.
    pub midi: Option<&'a MidiBlock>,
    /// Where this call's frames start within the chain's block.
    pub midi_from: usize,
}

/// The MIDI an effect passes down the chain in a block, as frame offsets from its start:
/// channel messages, and system exclusive or meta messages kept whole.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MidiBlock {
    pub short: Vec<(usize, [u8; 3])>,
    pub long: Vec<(usize, Vec<u8>)>,
}

impl MidiBlock {
    pub fn clear(&mut self) {
        self.short.clear();
        self.long.clear();
    }
}

impl ProcessContext<'_> {
    /// Live MIDI messages for this track.
    pub fn live_midi(&self) -> impl Iterator<Item = &LiveMidi> {
        self.live.iter().filter(|m| self.live_channel.is_none_or(|c| m.channel() == c))
    }

    /// The chain's MIDI due in this call's `n` frames, as offsets from the call's first frame.
    pub fn chain_midi(&self, n: usize) -> Option<impl Iterator<Item = (usize, [u8; 3])> + '_> {
        let from = self.midi_from;
        self.midi.map(move |m| {
            m.short.iter().filter(move |e| (from..from + n).contains(&e.0)).map(move |&(o, b)| (o - from, b))
        })
    }

    /// System exclusive and meta messages due in this call's `n` frames: the chain's, or else
    /// the track's own.
    pub fn block_long(&self, n: usize, out: &mut Vec<(usize, Vec<u8>)>) {
        out.clear();
        let from = self.midi_from;
        match (self.midi, self.notes) {
            (Some(m), _) => out
                .extend(m.long.iter().filter(|e| (from..from + n).contains(&e.0)).map(|(o, d)| (o - from, d.clone()))),
            (None, Some(notes)) => {
                let start = self.position;
                out.extend(
                    notes.long_in(start..start + n as i64).iter().map(|(f, d)| ((f - start) as usize, d.clone())),
                );
            }
            (None, None) => {}
        }
    }

    /// Every MIDI message due in this call's `n` frames: the chain's, or else live input first
    /// and then the track's notes and controllers, note-offs before note-ons at the same frame.
    pub fn block_midi(&self, n: usize, out: &mut Vec<(usize, [u8; 3])>) {
        out.clear();
        if let Some(chain) = self.chain_midi(n) {
            out.extend(chain);
            return;
        }
        let live = self.live_midi().count();
        out.extend(self.live_midi().map(|m| (0, m.bytes)));
        if let Some(notes) = self.notes {
            let (start, end) = (self.position, self.position + n as i64);
            for note in notes.starting_in(start - notes.max_span()..end) {
                if (start..end).contains(&note.start) {
                    out.push(((note.start - start) as usize, [0x90 | note.channel, note.key, note.velocity]));
                }
                let off = note.end.min(note.cutoff);
                if (start..end).contains(&off) {
                    out.push(((off - start) as usize, [0x80 | note.channel, note.key, 0]));
                }
            }
            for c in notes.controls_in(start..end) {
                out.push(((c.frame - start) as usize, c.bytes));
            }
            out[live..].sort_by_key(|e| (e.0, e.1[0] & 0xf0 == 0x90));
        }
    }
}

/// An audio effect working in place on a stereo block.
pub trait Effect: Send {
    fn process(&mut self, left: &mut [f32], right: &mut [f32], ctx: &ProcessContext);

    /// Audio input and output pins, for effects with more than a stereo pair.
    fn pins(&self) -> (usize, usize) {
        (2, 2)
    }

    /// Pins in the plugin's own bus arrangements, for plugins a host sets up for the track's
    /// width (VST 3).
    fn default_pins(&self) -> Option<(usize, usize)> {
        None
    }

    /// The plugin's factory presets by name: a VST 2 plugin's programs, the presets a CLAP
    /// plugin's preset discovery lists.
    fn programs(&mut self) -> Vec<String> {
        Vec::new()
    }

    /// The current program, for plugins with programs.
    fn program(&mut self) -> Option<usize> {
        None
    }

    /// Selects one of [`Effect::programs`].
    fn set_program(&mut self, _index: usize) {}

    /// Whether parameter envelopes reach the effect with their value at the last frame of
    /// each block, as REAPER automates hosted plugins (its own effects take the first).
    fn automates_at_block_end(&self) -> bool {
        false
    }

    /// What a JSFX's `@serialize` writes, stored by REAPER as `<JS_SER` beside the effect.
    fn save_serialized(&mut self) -> Option<Vec<f64>> {
        None
    }

    /// Moves the MIDI the effect passed on since the last call into `out`, as offsets from the
    /// chain's block; false when the effect leaves the chain's MIDI as it is.
    fn take_midi(&mut self, _out: &mut MidiBlock) -> bool {
        false
    }

    /// Processes frames `range` of `bufs` in place: input pin `k` arrives in `bufs[k]`, output
    /// pin `k` leaves in it. Effects with more than two pins override this.
    fn process_pins(&mut self, bufs: &mut [Vec<f32>], range: std::ops::Range<usize>, ctx: &ProcessContext) {
        if let [l, r, ..] = bufs {
            self.process(&mut l[range.clone()], &mut r[range], ctx);
        }
    }
    /// Instruments turn the track's MIDI into audio; the built-in preview synth stays quiet
    /// on tracks that have one.
    fn is_instrument(&self) -> bool {
        false
    }
    /// Clears internal state such as filter memories and delay lines (on seek or stop).
    fn reset(&mut self) {}

    /// Frames the effect delays its output by; the graph compensates for it.
    fn latency(&mut self) -> usize {
        0
    }

    /// The plugin's own editor. Taken under the effect's lock and then used without it.
    fn editor(&mut self) -> Option<Box<dyn crate::editor::Editor>> {
        None
    }

    /// The view size a JSFX's `@gfx` asks for (0 where it does not care); `None` when the
    /// effect draws no view of its own.
    fn gfx_size(&self) -> Option<(u32, u32)> {
        None
    }

    /// Runs the effect's `@gfx` once on the UI thread; what it drew is drawn by rendering the
    /// output, which needs no lock on the effect.
    fn gfx(&mut self, _input: &crate::eel::gfx::Input) -> Option<crate::eel::gfx::Output> {
        None
    }

    /// Automatable parameters, for hosted plugins.
    fn params(&mut self) -> Vec<ParamInfo> {
        Vec::new()
    }

    /// Current value and its display text.
    fn param(&mut self, _id: u32) -> Option<(f64, String)> {
        None
    }

    fn set_param(&mut self, _id: u32, _value: f64) {}

    /// The plugin's own state, to store in the project.
    fn save_state(&mut self) -> Option<Vec<u8>> {
        None
    }

    /// State as the lines and chunks REAPER keeps inside the plugin chunk (LV2 controls and
    /// properties); each replaces the existing node with the same key.
    fn save_nodes(&mut self) -> Option<Vec<Node>> {
        None
    }

    /// Sets parameter `index` in REAPER's order from a normalised (0..=1) value, the way REAPER
    /// maps it for its own effects. Returns false when the effect does not take it that way.
    fn set_normalized(&mut self, _index: usize, _value: f64) -> bool {
        false
    }

    /// Sets a parameter by the name REAPER uses in `PARMENV` (`_Wet`, `_1__Enabled` …) for
    /// envelope playback. Returns false for names the effect does not know.
    fn set_named_param(&mut self, _name: &str, _value: f64) -> bool {
        false
    }
}

/// What a `PARMENV` drives: the generic bypass, or a parameter by name.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Bypass,
    Named(String),
}

/// Maps an envelope value onto a parameter: linearly from the envelope's range to the
/// parameter's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamMapping {
    pub id: u32,
    pub from: (f64, f64),
    pub to: (f64, f64),
}

impl ParamMapping {
    pub fn map(&self, v: f64) -> f64 {
        let span = self.from.1 - self.from.0;
        let k = if span.abs() > 1e-12 { ((v - self.from.0) / span).clamp(0.0, 1.0) } else { 0.0 };
        self.to.0 + k * (self.to.1 - self.to.0)
    }
}

#[derive(Debug, Clone)]
pub struct ParamAutomation {
    pub target: Target,
    pub envelope: scythe_core::Envelope,
    /// The parameter at the envelope's index, for effects that do not know its name.
    pub fallback: Option<ParamMapping>,
    /// Position in the effect's parameter list (`PARMENV <index>:<name>`).
    pub index: usize,
    /// The envelope's value range (`PARMENV … <min> <max>`).
    pub range: (f64, f64),
}

/// `<PARMENV <index>:<name> <min> <max> <default> "label"` chunks of a slot, with their points.
fn parameter_envelopes(slot: &FxSlot) -> Vec<ParamAutomation> {
    slot.raw
        .iter()
        .filter_map(|n| match n {
            Node::Chunk(c) if c.name() == "PARMENV" => Some(c),
            _ => None,
        })
        .filter_map(|c| {
            let args = c.args();
            let spec = args.first()?.clone();
            let (index, name) = match spec.split_once(':') {
                Some((i, n)) => (i.parse().unwrap_or(usize::MAX), n.to_owned()),
                None => (spec.parse().unwrap_or(usize::MAX), String::new()),
            };
            let bound = |i: usize, d: f64| args.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(d);
            let range = (bound(1, 0.0), bound(2, 1.0));
            let target = if name == "bypass" { Target::Bypass } else { Target::Named(name) };
            let mut envelope = scythe_core::Envelope::new(scythe_core::EnvelopeKind::Volume);
            for node in &c.children {
                let Node::Line(l) = node else { continue };
                let t = tokenize(l);
                let num = |i: usize| t.get(i).and_then(|v| v.parse::<f64>().ok());
                match t[0].as_str() {
                    "ACT" => envelope.active = num(1).is_none_or(|v| v != 0.0),
                    "PT" => envelope.points.push(scythe_core::EnvPoint {
                        time: num(1)?,
                        value: num(2)?,
                        shape: scythe_core::PointShape::from_rpp(num(3).unwrap_or(0.0) as i64, num(7).unwrap_or(0.0)),
                        raw: Vec::new(),
                    }),
                    _ => {}
                }
            }
            envelope.sort();
            (envelope.active && !envelope.points.is_empty()).then_some(ParamAutomation {
                target,
                envelope,
                fallback: None,
                index,
                range,
            })
        })
        .collect()
}

/// How a slot is being played, for the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum FxStatus {
    /// A built-in reimplementation reads the slot's stored settings.
    Native(&'static str),
    /// A plugin binary is hosted.
    Hosted(PathBuf),
    /// Passed through; the reason says why.
    Unavailable(String),
}

/// Sets the effect's enveloped parameters for time `t`; returns whether a bypass envelope
/// switches it off.
/// When in a block of `n` frames from `t` the effect's envelopes are read.
fn automation_time(fx: &dyn Effect, t: f64, n: usize, sample_rate: u32) -> f64 {
    if fx.automates_at_block_end() { t + n.saturating_sub(1) as f64 / sample_rate as f64 } else { t }
}

fn apply_automation(fx: &mut dyn Effect, automation: &[ParamAutomation], t: f64) -> bool {
    let mut bypassed = false;
    for a in automation {
        let Some(v) = a.envelope.value_at(t) else { continue };
        match &a.target {
            Target::Bypass => bypassed = v >= 0.5,
            Target::Named(name) => {
                let span = a.range.1 - a.range.0;
                let normalized = if span.abs() > 1e-12 { ((v - a.range.0) / span).clamp(0.0, 1.0) } else { 0.0 };
                if !fx.set_named_param(name, v)
                    && !fx.set_normalized(a.index, normalized)
                    && let Some(m) = &a.fallback
                {
                    fx.set_param(m.id, m.map(v));
                }
            }
        }
    }
    bypassed
}

pub struct FxInstance {
    effect: Mutex<Box<dyn Effect>>,
    pub instrument: bool,
    /// Audio input and output pins.
    pub pins: (usize, usize),
}

impl FxInstance {
    pub fn new(fx: Box<dyn Effect>) -> Self {
        Self { instrument: fx.is_instrument(), pins: fx.pins(), effect: Mutex::new(fx) }
    }

    /// Runs the effect with its parameter envelopes applied at time `t` (seconds). A bypass
    /// envelope above 0.5 skips the effect.
    pub fn process_automated(
        &self,
        left: &mut [f32],
        right: &mut [f32],
        ctx: &ProcessContext,
        automation: &[ParamAutomation],
        t: f64,
    ) {
        let Ok(mut fx) = self.effect.try_lock() else { return };
        let t = automation_time(fx.as_ref(), t, left.len(), ctx.sample_rate);
        if !apply_automation(fx.as_mut(), automation, t) {
            fx.process(left, right, ctx);
        }
    }

    /// Like `process_automated`, over the effect's pins.
    pub fn process_pins_automated(
        &self,
        bufs: &mut [Vec<f32>],
        range: std::ops::Range<usize>,
        ctx: &ProcessContext,
        automation: &[ParamAutomation],
        t: f64,
    ) {
        let Ok(mut fx) = self.effect.try_lock() else { return };
        let t = automation_time(fx.as_ref(), t, range.len(), ctx.sample_rate);
        if !apply_automation(fx.as_mut(), automation, t) {
            fx.process_pins(bufs, range, ctx);
        }
    }

    pub fn process_pins(&self, bufs: &mut [Vec<f32>], range: std::ops::Range<usize>, ctx: &ProcessContext) {
        if let Ok(mut fx) = self.effect.try_lock() {
            fx.process_pins(bufs, range, ctx);
        }
    }

    pub fn take_midi(&self, out: &mut MidiBlock) -> bool {
        self.effect.try_lock().is_ok_and(|mut fx| fx.take_midi(out))
    }

    /// Runs `f` on the effect from the UI thread, waiting for the audio thread to finish its
    /// block if needed.
    pub fn with<R>(&self, f: impl FnOnce(&mut dyn Effect) -> R) -> Option<R> {
        self.effect.lock().ok().map(|mut e| f(e.as_mut()))
    }

    /// Runs the effect unless another thread holds it (state being swapped), in which case
    /// the block passes through untouched.
    pub fn process(&self, left: &mut [f32], right: &mut [f32], ctx: &ProcessContext) {
        if let Ok(mut fx) = self.effect.try_lock() {
            fx.process(left, right, ctx);
        }
    }
}

impl std::fmt::Debug for FxInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FxInstance")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Owner {
    Track(TrackId),
    Master,
}

struct Entry {
    state: Vec<Node>,
    instance: Option<Arc<FxInstance>>,
    status: FxStatus,
    /// Channels of the track the instance was set up for.
    channels: usize,
}

/// Owns every effect instance across graph rebuilds.
#[derive(Default)]
pub struct FxRegistry {
    entries: HashMap<(Owner, String), Entry>,
    sample_rate: u32,
    search_paths: Vec<PathBuf>,
    jsfx_paths: std::sync::OnceLock<Vec<PathBuf>>,
    /// Relative file references in plugin state (impulse responses) resolve against this.
    pub project_dir: Option<PathBuf>,
    clap_cache: std::sync::OnceLock<Vec<ClapDescriptor>>,
    lv2_cache: std::sync::OnceLock<Vec<crate::lv2::Lv2Info>>,
    vst3_cache: std::sync::OnceLock<Vec<crate::vst3_host::Vst3Info>>,
    /// Loaded VST 3 modules, so each library is entered once.
    vst3_modules: Mutex<HashMap<PathBuf, Arc<crate::vst3_host::Vst3Module>>>,
}

impl FxRegistry {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            entries: HashMap::new(),
            sample_rate,
            search_paths: vst_search_paths(),
            jsfx_paths: Default::default(),
            project_dir: None,
            clap_cache: Default::default(),
            lv2_cache: Default::default(),
            vst3_cache: Default::default(),
            vst3_modules: Default::default(),
        }
    }

    /// The effect chain for a track (or the master when `track` is `None`), creating or
    /// replacing instances whose slot changed. Bypassed and unsupported slots are skipped.
    pub fn chain(&mut self, track: Option<TrackId>, slots: &[FxSlot]) -> Vec<Arc<FxInstance>> {
        self.chain_with_automation(track, slots, 2).0
    }

    /// Like `chain`, on a track of `channels` channels, with each effect's parameter envelopes.
    /// VST 3 instances are set up anew when the track's width changes, as REAPER does.
    pub fn chain_with_automation(
        &mut self,
        track: Option<TrackId>,
        slots: &[FxSlot],
        channels: usize,
    ) -> (Vec<Arc<FxInstance>>, Vec<Vec<ParamAutomation>>) {
        let owner = track.map_or(Owner::Master, Owner::Track);
        let mut out = Vec::new();
        let mut automation = Vec::new();
        for (i, slot) in slots.iter().enumerate() {
            let key = (owner.clone(), slot_key(slot, i));
            let state = plugin_state(slot);
            let stale =
                self.entries.get(&key).is_none_or(|e| e.state != state || (e.channels != channels && is_vst3(slot)));
            if stale {
                let (instance, status) = match self.create(slot, channels) {
                    Ok((fx, status)) => (Some(Arc::new(FxInstance::new(fx))), status),
                    Err(reason) => (None, FxStatus::Unavailable(reason)),
                };
                self.entries.insert(key.clone(), Entry { state, instance, status, channels });
            }
            if slot.enabled
                && let Some(inst) = &self.entries[&key].instance
            {
                out.push(inst.clone());
                let mut envs = parameter_envelopes(slot);
                if envs.iter().any(|a| matches!(a.target, Target::Named(_))) {
                    let params = inst.with(|e| e.params()).unwrap_or_default();
                    for a in &mut envs {
                        a.fallback =
                            params.get(a.index).map(|p| ParamMapping { id: p.id, from: a.range, to: (p.min, p.max) });
                    }
                }
                automation.push(envs);
            }
        }
        (out, automation)
    }

    pub fn instance(&self, track: Option<TrackId>, slot: &FxSlot, index: usize) -> Option<Arc<FxInstance>> {
        let owner = track.map_or(Owner::Master, Owner::Track);
        self.entries.get(&(owner, slot_key(slot, index))).and_then(|e| e.instance.clone())
    }

    /// Writes the current state of hosted CLAP plugins into their slots (as a `<STATE` chunk), so
    /// a save keeps parameter changes. The registry keeps the instances: their stored state now
    /// matches what they hold.
    pub fn store_states(&mut self, project: &mut Project) {
        use base64::Engine as _;
        let owners: Vec<(Option<TrackId>, usize)> = std::iter::once((None, usize::MAX))
            .chain(project.tracks.iter().enumerate().map(|(i, t)| (Some(t.id), i)))
            .collect();
        for (owner, ti) in owners {
            let channels = if ti == usize::MAX { project.master.channels } else { project.tracks[ti].channels };
            let channels = (channels as usize).max(2);
            let slots = if ti == usize::MAX { &mut project.master.fx } else { &mut project.tracks[ti].fx };
            for (i, slot) in slots.iter_mut().enumerate() {
                let Some(inst) = self.instance(owner, slot, i) else { continue };
                if let Some(nodes) = inst.with(|e| e.save_nodes()).flatten() {
                    let Some(Node::Chunk(chunk)) = slot.raw.iter_mut().find(|n| matches!(n, Node::Chunk(_))) else {
                        continue;
                    };
                    let key = |n: &Node| match n {
                        Node::Line(l) => line_key(l).to_owned(),
                        Node::Chunk(c) => format!("<{}", c.name()),
                    };
                    // A JSFX keeps its slider values as the first, unnamed line.
                    let js = chunk.name() == "JS";
                    for node in nodes.into_iter().filter(|_| !js) {
                        let k = key(&node);
                        match chunk.children.iter_mut().find(|n| key(n) == k) {
                            Some(existing) => *existing = node,
                            None => chunk.children.push(node),
                        }
                    }
                    if js
                        && let Some(Node::Line(values)) =
                            inst.with(|e| e.save_nodes()).flatten().and_then(|n| n.into_iter().next())
                    {
                        match chunk.children.iter_mut().find(|n| matches!(n, Node::Line(_))) {
                            Some(existing) => *existing = Node::Line(values),
                            None => chunk.children.insert(0, Node::Line(values)),
                        }
                    }
                    if js && let Some(state) = inst.with(|e| e.save_serialized()).flatten() {
                        set_js_serialized(slot, &state);
                    }
                    let key = (owner.map_or(Owner::Master, Owner::Track), slot_key(slot, i));
                    if let Some(entry) = self.entries.get_mut(&key) {
                        entry.state = plugin_state(slot);
                    }
                    continue;
                }
                let Some(state) = inst.with(|e| e.save_state()).flatten() else { continue };
                let Some(Node::Chunk(chunk)) =
                    slot.raw.iter_mut().find(|n| matches!(n, Node::Chunk(c) if matches!(c.name(), "CLAP" | "VST")))
                else {
                    continue;
                };
                if chunk.name() == "VST" {
                    // REAPER's layout: header, plugin chunk and program, as base64 lines.
                    let id = chunk.args().get(4).and_then(|a| a.split(['<', '{']).next()?.parse::<u32>().ok());
                    let Some(id) = id else { continue };
                    if plugin_state(slot).is_empty() {
                        continue;
                    }
                    let Some(Node::Chunk(chunk)) =
                        slot.raw.iter_mut().find(|n| matches!(n, Node::Chunk(c) if c.name() == "VST"))
                    else {
                        continue;
                    };
                    let lines: Vec<String> = chunk
                        .children
                        .iter()
                        .filter_map(|n| match n {
                            Node::Line(l) => Some(l.trim().to_owned()),
                            Node::Chunk(_) => None,
                        })
                        .collect();
                    let mut new_lines = match ReaperVstState::decode(&lines) {
                        Some(old) if old.chunk == state => lines.clone(),
                        _ => ReaperVstState::reencode(&lines, &state)
                            .unwrap_or_else(|| crate::builtin::vst_lines(id, &state)),
                    };
                    // REAPER maps the input pins of the plugin's own arrangement and an output
                    // pin per track channel, growing the map with the track but never shrinking it.
                    if let Some((inputs, _)) = inst.with(|e| e.default_pins()).flatten()
                        && let Some(old) = ReaperVstState::decode(&new_lines)
                        && (old.inputs.len() < inputs || old.outputs.len() < channels)
                    {
                        let grow = |mut masks: Vec<u64>, n: usize| {
                            let from = masks.len();
                            masks.extend((from..n).map(|p| 1u64 << p.min(63)));
                            masks
                        };
                        let (ins, outs) = (grow(old.inputs, inputs), grow(old.outputs, channels));
                        if let Some(grown) = ReaperVstState::with_pins(&new_lines, &ins, &outs) {
                            new_lines = grown;
                        }
                    }
                    if let Some(program) = inst.with(|e| e.program()).flatten()
                        && let Some(old) = ReaperVstState::decode(&new_lines)
                        && let Some(l) = ReaperVstState::with_words(
                            &new_lines,
                            [old.words[0], old.words[1] & 0xffff_0000 | program as u32],
                        )
                    {
                        new_lines = l;
                    }
                    if new_lines == lines {
                        continue;
                    }
                    chunk.children = new_lines.into_iter().map(Node::Line).collect();
                } else {
                    chunk.children.retain(|n| !matches!(n, Node::Chunk(c) if c.name() == "STATE"));
                    let text = base64::engine::general_purpose::STANDARD.encode(&state);
                    let mut st = Chunk::new("STATE", &[]);
                    for line in text.as_bytes().chunks(128) {
                        st.line([String::from_utf8_lossy(line)]);
                    }
                    chunk.push_chunk(st);
                }
                let key = (owner.map_or(Owner::Master, Owner::Track), slot_key(slot, i));
                if let Some(entry) = self.entries.get_mut(&key) {
                    entry.state = plugin_state(slot);
                }
            }
        }
    }

    pub fn status(&self, track: Option<TrackId>, slot: &FxSlot, index: usize) -> Option<&FxStatus> {
        let owner = track.map_or(Owner::Master, Owner::Track);
        self.entries.get(&(owner, slot_key(slot, index))).map(|e| &e.status)
    }

    /// Drops instances of tracks that no longer exist.
    pub fn retain(&mut self, project: &Project) {
        self.entries.retain(|(owner, _), _| match owner {
            Owner::Track(id) => project.track(*id).is_some(),
            Owner::Master => true,
        });
    }

    /// Looks for JSFX in `paths` instead of the standard locations.
    pub fn set_jsfx_paths(&mut self, paths: Vec<PathBuf>) {
        self.jsfx_paths = std::sync::OnceLock::from(paths);
    }

    /// Replaces the CLAP plugin list instead of scanning the standard locations.
    pub fn set_clap_plugins(&mut self, plugins: Vec<ClapDescriptor>) {
        self.clap_cache = std::sync::OnceLock::from(plugins);
    }

    /// Replaces the LV2 plugin list instead of scanning the standard locations.
    pub fn set_lv2_plugins(&mut self, plugins: Vec<crate::lv2::Lv2Info>) {
        self.lv2_cache = std::sync::OnceLock::from(plugins);
    }

    /// Replaces the VST 3 plugin list instead of scanning the standard locations.
    pub fn set_vst3_plugins(&mut self, plugins: Vec<crate::vst3_host::Vst3Info>) {
        self.vst3_cache = std::sync::OnceLock::from(plugins);
    }

    /// Installed VST 3 plugins, scanned on first use.
    pub fn vst3_plugins(&self) -> &[crate::vst3_host::Vst3Info] {
        self.vst3_cache.get_or_init(|| {
            crate::vst3_host::scan_cached(&crate::vst3_host::search_paths(), crate::scan_cache::default_path("vst3"))
        })
    }

    fn vst3_module(&self, path: &Path) -> Result<Arc<crate::vst3_host::Vst3Module>, String> {
        let mut modules = self.vst3_modules.lock().unwrap();
        if let Some(m) = modules.get(path) {
            return Ok(m.clone());
        }
        let module = crate::vst3_host::Vst3Module::load(path)?;
        modules.insert(path.to_owned(), module.clone());
        Ok(module)
    }

    /// Installed LV2 plugins, scanned on first use.
    pub fn lv2_plugins(&self) -> &[crate::lv2::Lv2Info] {
        self.lv2_cache.get_or_init(|| crate::lv2::scan(&crate::lv2::search_paths()))
    }

    /// Installed CLAP plugins, scanned on first use.
    pub fn clap_plugins(&self) -> &[ClapDescriptor] {
        self.clap_cache.get_or_init(crate::clap_host::scan)
    }

    fn create(&self, slot: &FxSlot, channels: usize) -> Result<(Box<dyn Effect>, FxStatus), String> {
        let plugin = slot_plugin(slot).ok_or("no plugin data")?;
        let plugin_chunk = plugin;
        let args = plugin.args();
        let lines: Vec<String> = plugin
            .children
            .iter()
            .filter_map(|n| match n {
                Node::Line(l) => Some(l.trim().to_owned()),
                Node::Chunk(_) => None,
            })
            .collect();
        match plugin.name() {
            "VST" => {
                let display = args.first().cloned().unwrap_or_default();
                let state = ReaperVstState::decode(&lines);
                if display.starts_with("VST3") {
                    // The class id is the hex in braces after the numeric id: `123{0D6A…}`.
                    let cid = args.get(4).and_then(|a| a.split_once('{')).map(|(_, rest)| rest.trim_end_matches('}'));
                    let file = args.get(1).cloned().unwrap_or_default();
                    let info = self
                        .vst3_plugins()
                        .iter()
                        .find(|p| cid.is_some_and(|c| c.eq_ignore_ascii_case(&crate::vst3_host::cid_hex(&p.cid))))
                        .or_else(|| self.vst3_plugins().iter().find(|p| p.file_name == file))
                        .cloned()
                        .ok_or_else(|| format!("{file} is not installed as a Linux VST 3 plugin"))?;
                    let module = self.vst3_module(&info.path)?;
                    let mut plugin = crate::vst3_host::Vst3Plugin::new(
                        module,
                        &info,
                        self.sample_rate as f64,
                        crate::render::RenderScratch::MAX_BLOCK,
                        channels,
                    )?;
                    if let Some(state) = state {
                        plugin.load_state(&state.chunk);
                    }
                    return Ok((Box::new(Vst3Effect::new(plugin)), FxStatus::Hosted(info.path.clone())));
                }
                if display.contains("ReaEQ (Cockos)") {
                    let state = state.ok_or("unreadable ReaEQ state")?;
                    let eq =
                        ReaEqCompat::from_chunk(&state.chunk, self.sample_rate).ok_or("unreadable ReaEQ settings")?;
                    return Ok((Box::new(eq), FxStatus::Native("ReaEQ-compatible EQ")));
                }
                if display.contains("ReaSamplOmatic5000 (Cockos)") {
                    let state = state.ok_or("unreadable ReaSamplOmatic5000 state")?;
                    let settings = Rs5kSettings::parse(&state.chunk).ok_or("unreadable ReaSamplOmatic5000 settings")?;
                    let sampler = Rs5kCompat::new(settings, self.sample_rate, self.project_dir.as_deref())?;
                    return Ok((Box::new(sampler), FxStatus::Native("ReaSamplOmatic5000-compatible sampler")));
                }
                if display.contains("ReaDelay (Cockos)") {
                    let state = state.ok_or("unreadable ReaDelay state")?;
                    let settings = ReaDelaySettings::parse(&state.chunk).ok_or("unreadable ReaDelay settings")?;
                    let mut delay = ReaDelayCompat::new(settings, self.sample_rate);
                    delay.chunk = state.chunk.clone();
                    return Ok((Box::new(delay), FxStatus::Native("ReaDelay-compatible delay")));
                }
                if display.contains("ReaLimit (Cockos)") {
                    let state = state.ok_or("unreadable ReaLimit state")?;
                    let limiter =
                        Limiter::from_realimit(&state.chunk, self.sample_rate).ok_or("unreadable ReaLimit settings")?;
                    return Ok((Box::new(limiter), FxStatus::Native("ReaLimit-compatible limiter")));
                }
                if display.contains("ReaGate (Cockos)") {
                    let state = state.ok_or("unreadable ReaGate state")?;
                    let gate =
                        Gate::from_reagate(&state.chunk, self.sample_rate).ok_or("unreadable ReaGate settings")?;
                    return Ok((Box::new(gate), FxStatus::Native("ReaGate-compatible gate")));
                }
                if display.contains("ReaComp (Cockos)") {
                    let state = state.ok_or("unreadable ReaComp state")?;
                    let comp = Compressor::from_reacomp(&state.chunk, self.sample_rate)
                        .ok_or("unreadable ReaComp settings")?;
                    return Ok((Box::new(comp), FxStatus::Native("ReaComp-compatible compressor")));
                }
                if display.contains("ReaVerb (Cockos)") {
                    let state = state.ok_or("unreadable ReaVerb state")?;
                    let settings = ReaVerbSettings::parse(&state.chunk).ok_or("unreadable ReaVerb settings")?;
                    let mut verb = ReaVerbCompat::new(settings, self.sample_rate, self.project_dir.as_deref())?;
                    verb.chunk = state.chunk.clone();
                    return Ok((Box::new(verb), FxStatus::Native("ReaVerb-compatible convolution")));
                }
                let file = args.get(1).cloned().unwrap_or_default();
                let path = self.find_vst(&file).ok_or_else(|| format!("{file} not found in the VST paths"))?;
                if !file.ends_with(".so") {
                    return Err(format!("{file} is not a Linux VST 2 plugin"));
                }
                let mut plugin =
                    Vst2Plugin::load(&path, self.sample_rate as f32, crate::render::RenderScratch::MAX_BLOCK as i32)
                        .map_err(|e| e.to_string())?;
                if let Some(state) = state {
                    plugin.set_chunk(&state.chunk);
                    // REAPER selects the stored program after restoring the chunk.
                    let program = (state.words[1] & 0xffff) as usize;
                    if program < plugin.num_programs() {
                        plugin.set_program(program);
                    }
                }
                plugin.resume();
                Ok((Box::new(Vst2Effect::new(plugin)), FxStatus::Hosted(path)))
            }
            "JS" => {
                let name = args.first().cloned().unwrap_or_default();
                let roots = self.jsfx_paths.get_or_init(crate::jsfx::search_paths);
                let path = crate::jsfx::find(&name, roots).ok_or_else(|| format!("JSFX {name} not found"))?;
                let script = crate::jsfx::Script::load(&path, roots)?;
                let values = lines.first().map(|l| crate::jsfx::parse_values(l, &script.sliders)).unwrap_or_default();
                let mut fx = crate::jsfx::JsfxEffect::new(script, &values, self.sample_rate)?;
                if let Some(state) = js_serialized(slot) {
                    fx.restore_serialized(state);
                }
                Ok((Box::new(fx), FxStatus::Native("JSFX run from its source")))
            }
            "LV2" if args.first().is_some_and(|n| n.contains("Neural Amp Modeler")) => {
                let state = plugin_chunk.chunks("STATE").next().ok_or("NAM slot without state")?;
                let model = state
                    .children
                    .iter()
                    .find_map(|n| match n {
                        Node::Line(l) if line_key(l) == "P" => {
                            let t = tokenize(l);
                            t.get(1).filter(|k| k.ends_with("#model")).and(t.get(2).cloned())
                        }
                        _ => None,
                    })
                    .ok_or("NAM slot without a model")?;
                // `P input_level <dB> output_level <dB>` on the plugin chunk itself.
                let levels = plugin_chunk.find_line("P").unwrap_or_default();
                let level = |name: &str| {
                    levels
                        .iter()
                        .position(|t| t == name)
                        .and_then(|i| levels.get(i + 1))
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0.0)
                };
                let path = PathBuf::from(&model);
                let path = match (path.is_absolute(), &self.project_dir) {
                    (false, Some(dir)) => dir.join(path),
                    _ => path,
                };
                let nam = NamEffect::new(&path, level("input_level"), level("output_level"), self.sample_rate)
                    .map_err(|e| format!("NAM model: {e}"))?;
                Ok((Box::new(nam), FxStatus::Native("Neural Amp Modeler (built in)")))
            }
            "LV2" => {
                let uri = args.get(1).cloned().ok_or("LV2 slot without a plugin URI")?;
                let info = self
                    .lv2_plugins()
                    .iter()
                    .find(|p| p.uri == uri)
                    .cloned()
                    .ok_or_else(|| format!("LV2 plugin {uri} is not installed"))?;
                let mut plugin = crate::lv2::Lv2Plugin::new(
                    &info,
                    self.sample_rate as f64,
                    crate::render::RenderScratch::MAX_BLOCK,
                )?;
                // `P symbol value symbol value …` holds the input controls.
                if let Some(values) = plugin_chunk.find_line("P") {
                    for pair in values.chunks(2) {
                        if let [symbol, value] = pair
                            && let Ok(v) = value.parse::<f32>()
                        {
                            plugin.set_control_by_symbol(symbol, v);
                        }
                    }
                }
                if let Some(state) = plugin_chunk.chunks("STATE").next() {
                    plugin.restore_state(&crate::lv2::parse_reaper_state(state));
                }
                let binary = info.binary.clone();
                Ok((Box::new(Lv2Effect::new(plugin)), FxStatus::Hosted(binary)))
            }
            "CLAP" => {
                let id = args.get(1).cloned().ok_or("CLAP slot without a plugin id")?;
                let desc = self
                    .clap_plugins()
                    .iter()
                    .find(|d| d.id == id)
                    .cloned()
                    .ok_or_else(|| format!("CLAP plugin {id} is not installed"))?;
                let lib = ClapLibrary::load(&desc.path).map_err(|e| e.to_string())?;
                let mut plugin = ClapPlugin::create(
                    lib,
                    &id,
                    self.sample_rate as f64,
                    crate::render::RenderScratch::MAX_BLOCK as u32,
                )
                .map_err(|e| e.to_string())?;
                if let Some(state) = clap_state(plugin_chunk) {
                    plugin.load_state(&state);
                }
                let instrument = desc.is_instrument();
                Ok((
                    Box::new(ClapEffect { plugin, instrument, expected: None, long: Vec::new(), presets: None }),
                    FxStatus::Hosted(desc.path),
                ))
            }
            other => Err(format!("{other} plugins are not supported yet")),
        }
    }

    /// The file a VST 2 or JSFX slot's plugin lives in, also looking in REAPER's own plugin
    /// folder for its stock effects.
    pub fn plugin_file(&self, slot: &FxSlot) -> Option<PathBuf> {
        let plugin = slot_plugin(slot)?;
        let args = plugin.args();
        match plugin.name() {
            "VST" if !is_vst3(slot) => {
                let file = args.get(1)?;
                self.find_vst(file)
                    .or_else(|| reaper_plugin_dirs().into_iter().map(|d| d.join(file)).find(|p| p.exists()))
            }
            "JS" => crate::jsfx::find(args.first()?, self.jsfx_paths.get_or_init(crate::jsfx::search_paths)),
            _ => None,
        }
    }

    fn find_vst(&self, file: &str) -> Option<PathBuf> {
        if file.is_empty() {
            return None;
        }
        let direct = PathBuf::from(file);
        if direct.is_absolute() && direct.exists() {
            return Some(direct);
        }
        self.search_paths.iter().map(|d| d.join(file)).find(|p| p.exists())
    }
}

/// REAPER's stock plugin folders (`Plugins/FX` of an installed REAPER), found from the
/// `reaper` executable on the `PATH` and the usual install locations.
fn reaper_plugin_dirs() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).map(|d| d.join("reaper")).filter(|r| r.exists()).collect())
        .unwrap_or_default();
    roots = roots
        .into_iter()
        .flat_map(|r| {
            let real = r.canonicalize().unwrap_or(r.clone());
            [r.parent().map(|d| d.join("../opt/REAPER")), real.parent().map(PathBuf::from)]
        })
        .flatten()
        .collect();
    roots.push(PathBuf::from("/opt/REAPER"));
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("opt/REAPER"));
    }
    roots.into_iter().map(|r| r.join("Plugins/FX")).filter(|d| d.is_dir()).collect()
}

/// `SCYTHE_VST_PATH` and `VST_PATH` (colon separated), then the usual Linux locations.
fn vst_search_paths() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = ["SCYTHE_VST_PATH", "VST_PATH"]
        .iter()
        .filter_map(std::env::var_os)
        .flat_map(|v| std::env::split_paths(&v).collect::<Vec<_>>())
        .collect();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        paths.extend([home.join(".vst"), home.join(".lxvst"), home.join(".nix-profile/lib/vst")]);
    }
    paths.extend(
        ["/usr/lib/vst", "/usr/local/lib/vst", "/usr/lib/lxvst", "/run/current-system/sw/lib/vst"].map(PathBuf::from),
    );
    paths
}

/// FXID when present (stable across moves), otherwise the plugin name and position.
fn slot_key(slot: &FxSlot, index: usize) -> String {
    slot.raw
        .iter()
        .find_map(|n| match n {
            Node::Line(l) if line_key(l) == "FXID" => tokenize(l).get(1).cloned(),
            _ => None,
        })
        .unwrap_or_else(|| format!("{index}:{}", slot.name))
}

fn slot_plugin(slot: &FxSlot) -> Option<&Chunk> {
    slot.raw.iter().find_map(|n| match n {
        Node::Chunk(c) => Some(c),
        Node::Line(_) => None,
    })
}

/// A VST 3 plugin, which REAPER keeps in a `<VST` block naming its bundle.
fn is_vst3(slot: &FxSlot) -> bool {
    slot_plugin(slot).is_some_and(|c| c.name() == "VST" && c.args().first().is_some_and(|d| d.starts_with("VST3")))
}

/// Which track channels an effect's pins connect to, as bitmasks (channel 1 is bit 0). Only
/// the first two input and output pins carry audio here; input pins 3 and 4 are the
/// sidechain.
#[derive(Debug, Clone, PartialEq)]
pub struct PinMap {
    pub inputs: Vec<u64>,
    pub outputs: Vec<u64>,
}

impl PinMap {
    pub fn input(&self, pin: usize) -> u64 {
        self.inputs.get(pin).copied().unwrap_or(0)
    }

    pub fn output(&self, pin: usize) -> u64 {
        self.outputs.get(pin).copied().unwrap_or(0)
    }

    /// Pins 1/2 in and out on channels 1/2, with any sidechain on 3/4: what the graph does
    /// without a map.
    fn is_plain(&self) -> bool {
        self.input(0) == 1
            && self.input(1) == 2
            && self.output(0) == 1
            && self.output(1) == 2
            && self.outputs.iter().skip(2).all(|&m| m == 0)
            && self.inputs.get(2).is_none_or(|&m| m == 4)
            && self.inputs.get(3).is_none_or(|&m| m == 8)
    }
}

/// The pin map REAPER stored for a slot, or `None` when it is plain stereo (or not stored):
/// VST and VST 3 slots keep it in their state header, JSFX in a `<JS_PINMAP` chunk.
pub fn pin_map(slot: &FxSlot) -> Option<PinMap> {
    let plugin = slot_plugin(slot)?;
    let map = match plugin.name() {
        "VST" => {
            let lines: Vec<String> = plugin
                .children
                .iter()
                .filter_map(|n| match n {
                    Node::Line(l) => Some(l.trim().to_owned()),
                    Node::Chunk(_) => None,
                })
                .collect();
            let state = ReaperVstState::decode(&lines)?;
            PinMap { inputs: state.inputs, outputs: state.outputs }
        }
        "JS" => {
            use base64::Engine as _;
            let chunk = slot.raw.iter().find_map(|n| match n {
                Node::Chunk(c) if c.name() == "JS_PINMAP" => Some(c),
                _ => None,
            })?;
            let text: String = chunk
                .children
                .iter()
                .filter_map(|n| match n {
                    Node::Line(l) => Some(l.trim()),
                    Node::Chunk(_) => None,
                })
                .collect();
            let bytes = base64::engine::general_purpose::STANDARD.decode(text).ok()?;
            js_pin_blocks(&bytes)?
        }
        // `<IN_PINS` / `<OUT_PINS` with `PIN <pin> <channel> …` lines (0-based); pins not
        // listed keep their default channel.
        "LV2" | "CLAP" => {
            let pins = |name: &str| -> Option<Vec<u64>> {
                let c = plugin.chunks(name).next()?;
                let mut masks: Vec<u64> = vec![1, 2];
                for node in &c.children {
                    let Node::Line(l) = node else { continue };
                    let t = tokenize(l);
                    let Some(pin) = t.get(1).and_then(|v| v.parse::<usize>().ok()) else { continue };
                    let mask = t[2..]
                        .iter()
                        .filter_map(|v| v.parse::<u32>().ok())
                        .filter(|&c| c < 64)
                        .fold(0u64, |m, c| m | 1 << c);
                    if masks.len() <= pin {
                        masks.extend((masks.len()..=pin).map(|p| 1u64 << p.min(63)));
                    }
                    masks[pin] = mask;
                }
                Some(masks)
            };
            let (inputs, outputs) = (pins("IN_PINS"), pins("OUT_PINS"));
            if inputs.is_none() && outputs.is_none() {
                return None;
            }
            PinMap { inputs: inputs.unwrap_or(vec![1, 2]), outputs: outputs.unwrap_or(vec![1, 2]) }
        }
        _ => return None,
    };
    (!map.is_plain()).then_some(map)
}

/// A JSFX's `<JS_SER` state: base64 of little-endian 32-bit floats.
fn js_serialized(slot: &FxSlot) -> Option<Vec<f64>> {
    use base64::Engine as _;
    let chunk = slot.raw.iter().find_map(|n| match n {
        Node::Chunk(c) if c.name() == "JS_SER" => Some(c),
        _ => None,
    })?;
    let text: String = chunk
        .children
        .iter()
        .filter_map(|n| match n {
            Node::Line(l) => Some(l.trim()),
            Node::Chunk(_) => None,
        })
        .collect();
    let bytes = base64::engine::general_purpose::STANDARD.decode(text).ok()?;
    Some(bytes.as_chunks::<4>().0.iter().map(|&b| f32::from_le_bytes(b) as f64).collect())
}

/// Replaces or adds the `<JS_SER` chunk after the effect's own chunk.
fn set_js_serialized(slot: &mut FxSlot, values: &[f64]) {
    use base64::Engine as _;
    let bytes: Vec<u8> = values.iter().flat_map(|&v| (v as f32).to_le_bytes()).collect();
    let text = base64::engine::general_purpose::STANDARD.encode(bytes);
    let mut chunk = Chunk::new("JS_SER", &[]);
    for line in text.as_bytes().chunks(128) {
        chunk.children.push(Node::Line(String::from_utf8_lossy(line).into_owned()));
    }
    let existing = slot.raw.iter().position(|n| matches!(n, Node::Chunk(c) if c.name() == "JS_SER"));
    match existing {
        Some(i) => slot.raw[i] = Node::Chunk(chunk),
        None => {
            let after =
                slot.raw.iter().position(|n| matches!(n, Node::Chunk(c) if c.name() == "JS")).map_or(0, |i| i + 1);
            slot.raw.insert(after, Node::Chunk(chunk));
        }
    }
}

/// `<JS_PINMAP`: a version word, then an input and an output block, each its byte size
/// followed by two words, the pin count and one 64-bit channel mask per pin.
fn js_pin_blocks(bytes: &[u8]) -> Option<PinMap> {
    let word = |at: usize| bytes.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
    let mut at = 4;
    let mut blocks = Vec::new();
    for _ in 0..2 {
        let size = word(at)?;
        let pins = word(at + 12)?;
        let masks = (0..pins)
            .map(|p| bytes.get(at + 16 + p * 8..at + 24 + p * 8).map(|b| u64::from_le_bytes(b.try_into().unwrap())))
            .collect::<Option<Vec<u64>>>()?;
        blocks.push(masks);
        at += 4 + size;
    }
    let outputs = blocks.pop()?;
    Some(PinMap { inputs: blocks.pop()?, outputs })
}

/// The plugin chunk is what defines the sound; bypass and window lines do not.
fn plugin_state(slot: &FxSlot) -> Vec<Node> {
    slot_plugin(slot).map(|c| vec![Node::Chunk(c.clone())]).unwrap_or_default()
}

/// Plugin state saved in a `<STATE` child chunk of a `<CLAP` slot, base64 encoded.
fn clap_state(c: &Chunk) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let state = c.chunks("STATE").next()?;
    let text: String = state
        .children
        .iter()
        .filter_map(|n| match n {
            Node::Line(l) => Some(l.trim()),
            Node::Chunk(_) => None,
        })
        .collect();
    base64::engine::general_purpose::STANDARD.decode(text).ok()
}

/// A `<CLAP` slot for a plugin, as stored in the FX chain.
pub fn clap_slot(desc: &ClapDescriptor) -> FxSlot {
    let display = format!("CLAP: {} ({})", desc.name, desc.vendor);
    let mut chunk = Chunk::new("CLAP", &[&display, &desc.id, ""]);
    chunk.line(["CFG", "4", "0", "0", ""]);
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

/// A new slot for an LV2 plugin, named the way REAPER names it (`LV2i:` for instruments).
pub fn lv2_slot(info: &crate::lv2::Lv2Info) -> FxSlot {
    let display = format!("{}: {}", if info.instrument { "LV2i" } else { "LV2" }, info.name);
    let mut chunk = Chunk::new("LV2", &[&display, &info.uri, ""]);
    chunk.line(["CFG", "0", "0", "0", ""]);
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

struct ClapEffect {
    plugin: ClapPlugin,
    instrument: bool,
    /// Where the next block should start if playback is continuous.
    expected: Option<i64>,
    /// System exclusive for the next block, kept alive until the plugin has run it.
    long: Vec<(usize, Vec<u8>)>,
    /// The plugin's discovered presets, listed on first use.
    presets: Option<Vec<crate::clap_presets::ClapPreset>>,
}

impl Effect for ClapEffect {
    fn programs(&mut self) -> Vec<String> {
        self.presets.get_or_insert_with(|| self.plugin.presets()).iter().map(|p| p.name.clone()).collect()
    }

    fn set_program(&mut self, index: usize) {
        if let Some(preset) = self.presets.get_or_insert_with(|| self.plugin.presets()).get(index).cloned() {
            self.plugin.load_preset(&preset);
        }
    }

    fn automates_at_block_end(&self) -> bool {
        true
    }

    fn is_instrument(&self) -> bool {
        self.instrument
    }

    fn editor(&mut self) -> Option<Box<dyn crate::editor::Editor>> {
        self.plugin.editor().map(|e| Box::new(e) as Box<dyn crate::editor::Editor>)
    }

    fn latency(&mut self) -> usize {
        self.plugin.latency() as usize
    }

    fn params(&mut self) -> Vec<ParamInfo> {
        self.plugin.params()
    }

    fn param(&mut self, id: u32) -> Option<(f64, String)> {
        let v = self.plugin.param_value(id)?;
        Some((v, self.plugin.param_text(id, v).unwrap_or_else(|| format!("{v:.3}"))))
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.plugin.set_param(id, value);
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        self.plugin.save_state()
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], ctx: &ProcessContext) {
        self.queue_events(left.len() as i64, ctx);
        self.plugin.process(left, right);
    }

    fn pins(&self) -> (usize, usize) {
        self.plugin.pins()
    }

    fn process_pins(&mut self, bufs: &mut [Vec<f32>], range: std::ops::Range<usize>, ctx: &ProcessContext) {
        let (ins, outs) = self.plugin.pins();
        if ins <= 2 && outs <= 2 {
            if let [l, r, ..] = bufs {
                self.process(&mut l[range.clone()], &mut r[range], ctx);
            }
            return;
        }
        self.queue_events(range.len() as i64, ctx);
        self.plugin.process_channels(bufs, range);
    }
}

impl ClapEffect {
    /// Queues the block's notes, controllers and live MIDI for an instrument, resetting it
    /// after a jump in the timeline.
    fn queue_events(&mut self, n: i64, ctx: &ProcessContext) {
        let (start, end) = (ctx.position, ctx.position + n);
        if self.instrument && self.expected.is_some_and(|e| e != start) {
            // A jump in the timeline: drop held notes rather than leave them hanging.
            self.plugin.reset();
        }
        self.expected = Some(end);
        if self.instrument {
            ctx.block_long(n as usize, &mut self.long);
            for (at, data) in &self.long {
                self.plugin.sysex.push(crate::clap_host::sysex_event(*at as u32, data));
            }
        }
        if self.instrument
            && let Some(chain) = ctx.chain_midi(n as usize)
        {
            for (at, bytes) in chain {
                match bytes[0] & 0xf0 {
                    0x90 | 0x80 => {
                        let on = bytes[0] & 0xf0 == 0x90 && bytes[2] > 0;
                        let vel = if on { bytes[2] } else { 0 };
                        let e = crate::clap_host::note_event(on, at as u32, bytes[0] & 0x0f, bytes[1], vel);
                        self.plugin.events.push(e);
                    }
                    _ => self.plugin.midi.push(crate::clap_host::midi_event(at as u32, bytes)),
                }
            }
            return;
        }
        if self.instrument
            && let Some(notes) = ctx.notes
        {
            for note in notes.starting_in(start - notes.max_span()..end) {
                if (start..end).contains(&note.start) {
                    let e = crate::clap_host::note_event(
                        true,
                        (note.start - start) as u32,
                        note.channel,
                        note.key,
                        note.velocity,
                    );
                    self.plugin.events.push(e);
                }
                let off = note.end.min(note.cutoff);
                if (start..end).contains(&off) {
                    let e = crate::clap_host::note_event(false, (off - start) as u32, note.channel, note.key, 0);
                    self.plugin.events.push(e);
                }
            }
            for c in notes.controls_in(start..end) {
                self.plugin.midi.push(crate::clap_host::midi_event((c.frame - start) as u32, c.bytes));
            }
        }
        if self.instrument {
            // Live messages go first, in the order they came in.
            let (mut notes_at, mut midi_at) = (0, 0);
            for m in ctx.live_midi() {
                let (status, key, vel) = (m.bytes[0] & 0xf0, m.bytes[1], m.bytes[2]);
                match status {
                    0x90 | 0x80 => {
                        let on = status == 0x90 && vel > 0;
                        let e = crate::clap_host::note_event(on, 0, m.channel(), key, if on { vel } else { 0 });
                        self.plugin.events.insert(notes_at, e);
                        notes_at += 1;
                    }
                    _ => {
                        self.plugin.midi.insert(midi_at, crate::clap_host::midi_event(0, m.bytes));
                        midi_at += 1;
                    }
                }
            }
            self.plugin
                .events
                .sort_by_key(|e| (e.header.time, e.header.type_ != clap_sys::events::CLAP_EVENT_NOTE_OFF));
        }
    }
}

/// A new slot for a VST 3 plugin in REAPER's layout: `VST3:` name, bundle file, a numeric id
/// with the class id in braces, and the state block.
pub fn vst3_slot(info: &crate::vst3_host::Vst3Info) -> FxSlot {
    let display = format!("{}: {} ({})", if info.instrument { "VST3i" } else { "VST3" }, info.name, info.vendor);
    let id = u32::from_le_bytes([info.cid[0], info.cid[1], info.cid[2], info.cid[3]])
        ^ u32::from_le_bytes([info.cid[12], info.cid[13], info.cid[14], info.cid[15]]);
    let ident = format!("{}{{{}}}", id & 0x7fff_ffff, crate::vst3_host::cid_hex(&info.cid));
    let mut chunk = Chunk::new("VST", &[&display, &info.file_name, "0", "", &ident, ""]);
    // An empty component state until the plugin saves its own.
    let state = [0u32, 1, 0, 0].iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>();
    for line in crate::builtin::vst_lines_with(id & 0x7fff_ffff, &state, 0xffff) {
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

/// Adapts a hosted VST 3 plugin to the stereo in-place interface; parameters are normalised.
struct Vst3Effect {
    plugin: crate::vst3_host::Vst3Plugin,
    notes: Vec<(i32, bool, u8, u8, u8)>,
    controls: Vec<(i32, [u8; 3])>,
}

impl Vst3Effect {
    /// Collects the notes and other MIDI of an `n`-frame block (the track's and live input).
    fn queue_midi(&mut self, n: usize, ctx: &ProcessContext) {
        self.notes.clear();
        self.controls.clear();
        ctx.block_long(n, &mut self.plugin.sysex);
        if let Some(chain) = ctx.chain_midi(n) {
            for (at, bytes) in chain {
                match bytes[0] & 0xf0 {
                    0x90 | 0x80 => {
                        let on = bytes[0] & 0xf0 == 0x90 && bytes[2] > 0;
                        self.notes.push((at as i32, on, bytes[0] & 0x0f, bytes[1], if on { bytes[2] } else { 0 }));
                    }
                    _ => self.controls.push((at as i32, bytes)),
                }
            }
            return;
        }
        if let Some(notes) = ctx.notes {
            let (start, end) = (ctx.position, ctx.position + n as i64);
            for note in notes.starting_in(start - notes.max_span()..end) {
                if (start..end).contains(&note.start) {
                    self.notes.push(((note.start - start) as i32, true, note.channel, note.key, note.velocity));
                }
                let off = note.end.min(note.cutoff);
                if (start..end).contains(&off) {
                    self.notes.push(((off - start) as i32, false, note.channel, note.key, 0));
                }
            }
            self.notes.sort_by_key(|e| (e.0, e.1));
            for c in notes.controls_in(start..end) {
                self.controls.push(((c.frame - start) as i32, c.bytes));
            }
        }
        let (mut notes_at, mut controls_at) = (0, 0);
        for m in ctx.live_midi() {
            let (status, key, vel) = (m.bytes[0] & 0xf0, m.bytes[1], m.bytes[2]);
            match status {
                0x90 | 0x80 => {
                    let on = status == 0x90 && vel > 0;
                    self.notes.insert(notes_at, (0, on, m.channel(), key, if on { vel } else { 0 }));
                    notes_at += 1;
                }
                _ => {
                    self.controls.insert(controls_at, (0, m.bytes));
                    controls_at += 1;
                }
            }
        }
    }

    fn new(plugin: crate::vst3_host::Vst3Plugin) -> Self {
        Self { plugin, notes: Vec::with_capacity(256), controls: Vec::with_capacity(256) }
    }
}

impl Effect for Vst3Effect {
    fn automates_at_block_end(&self) -> bool {
        true
    }

    fn editor(&mut self) -> Option<Box<dyn crate::editor::Editor>> {
        self.plugin.editor().map(|e| Box::new(e) as Box<dyn crate::editor::Editor>)
    }

    fn is_instrument(&self) -> bool {
        self.plugin.instrument
    }

    fn latency(&mut self) -> usize {
        self.plugin.latency() as usize
    }

    fn reset(&mut self) {
        if self.plugin.instrument {
            self.plugin.reset();
        }
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], ctx: &ProcessContext) {
        self.queue_midi(left.len(), ctx);
        self.plugin.process(left, right, &self.notes, &self.controls, ctx.position, ctx.bpm);
    }

    fn default_pins(&self) -> Option<(usize, usize)> {
        Some(self.plugin.default_pins())
    }

    fn pins(&self) -> (usize, usize) {
        self.plugin.pins()
    }

    fn process_pins(&mut self, bufs: &mut [Vec<f32>], range: std::ops::Range<usize>, ctx: &ProcessContext) {
        let (ins, outs) = self.plugin.pins();
        if ins <= 2 && outs <= 2 {
            if let [l, r, ..] = bufs {
                self.process(&mut l[range.clone()], &mut r[range], ctx);
            }
            return;
        }
        self.queue_midi(range.len(), ctx);
        self.plugin.process_channels(bufs, range, &self.notes, &self.controls, ctx.position, ctx.bpm);
    }

    fn params(&mut self) -> Vec<ParamInfo> {
        self.plugin
            .params()
            .into_iter()
            .map(|p| ParamInfo { id: p.id, name: p.name, min: 0.0, max: 1.0, default: p.default, log: false })
            .collect()
    }

    fn param(&mut self, id: u32) -> Option<(f64, String)> {
        self.plugin.param(id)
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.plugin.set_param(id, value);
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        Some(self.plugin.save_state())
    }
}

/// Adapts a hosted LV2 plugin: input control ports are its parameters, MIDI reaches its atom
/// input, and its controls and properties are stored the way REAPER stores them.
struct Lv2Effect {
    plugin: crate::lv2::Lv2Plugin,
    midi: Vec<(u32, [u8; 3])>,
    scratch_midi: Vec<(usize, [u8; 3])>,
    long: Vec<(usize, Vec<u8>)>,
}

impl Lv2Effect {
    fn new(plugin: crate::lv2::Lv2Plugin) -> Self {
        Self { plugin, midi: Vec::with_capacity(256), scratch_midi: Vec::with_capacity(256), long: Vec::new() }
    }

    fn control_ports(&self) -> impl Iterator<Item = &crate::lv2::PortInfo> {
        self.plugin.info.ports.iter().filter(|p| p.kind == crate::lv2::PortKind::Control && p.input)
    }
}

impl Effect for Lv2Effect {
    fn is_instrument(&self) -> bool {
        self.plugin.info.instrument
    }

    fn editor(&mut self) -> Option<Box<dyn crate::editor::Editor>> {
        self.plugin.editor().map(|e| Box::new(e) as Box<dyn crate::editor::Editor>)
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], ctx: &ProcessContext) {
        self.midi.clear();
        let mut events = std::mem::take(&mut self.scratch_midi);
        ctx.block_midi(left.len(), &mut events);
        self.midi.extend(events.iter().map(|&(at, b)| (at as u32, b)));
        self.scratch_midi = events;
        ctx.block_long(left.len(), &mut self.long);
        for (l, r) in left
            .chunks_mut(crate::render::RenderScratch::MAX_BLOCK)
            .zip(right.chunks_mut(crate::render::RenderScratch::MAX_BLOCK))
        {
            self.plugin.process(l, r, &self.midi, &self.long);
        }
    }

    fn params(&mut self) -> Vec<ParamInfo> {
        self.control_ports()
            .map(|p| ParamInfo {
                id: p.index,
                name: p.name.clone(),
                min: p.min as f64,
                max: p.max as f64,
                default: p.default as f64,
                log: p.logarithmic,
            })
            .collect()
    }

    fn param(&mut self, id: u32) -> Option<(f64, String)> {
        let port = self.control_ports().find(|p| p.index == id)?;
        let (toggled, integer) = (port.toggled, port.integer);
        let v = self.plugin.control(id)? as f64;
        let text = if toggled {
            if v > 0.5 { "on".into() } else { "off".into() }
        } else if integer {
            format!("{}", v.round())
        } else {
            format!("{v:.3}")
        };
        Some((v, text))
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.plugin.set_control(id, value as f32);
    }

    fn save_nodes(&mut self) -> Option<Vec<Node>> {
        let mut controls = vec!["P".to_owned()];
        for p in self.control_ports() {
            controls.push(p.symbol.clone());
            controls.push(scythe_core::rpp::fmt_num(self.plugin.control(p.index).unwrap_or(p.default) as f64));
        }
        let mut nodes = vec![Node::Line(scythe_core::rpp::join_tokens(controls))];
        let state = self.plugin.save_state();
        if !state.is_empty() {
            nodes.push(Node::Chunk(crate::lv2::reaper_state(&state)));
        }
        Some(nodes)
    }
}

/// Adapts a hosted VST 2 plugin to the stereo in-place interface.
struct Vst2Effect {
    plugin: Vst2Plugin,
    inputs: Vec<Vec<f32>>,
    outputs: Vec<Vec<f32>>,
    /// MIDI for the next block: frame offset and message.
    midi: Vec<(usize, [u8; 3])>,
    /// System exclusive for the next block, kept alive until the plugin has run it.
    long: Vec<(usize, Vec<u8>)>,
}

impl Vst2Effect {
    /// Collects the block's MIDI (live input, the track's notes and controllers) in `midi`.
    fn queue_midi(&mut self, n: usize, ctx: &ProcessContext) {
        ctx.block_midi(n, &mut self.midi);
    }

    fn new(plugin: Vst2Plugin) -> Self {
        let n = crate::render::RenderScratch::MAX_BLOCK;
        let inputs = (0..plugin.num_inputs()).map(|_| vec![0.0; n]).collect();
        let outputs = (0..plugin.num_outputs()).map(|_| vec![0.0; n]).collect();
        Self { plugin, inputs, outputs, midi: Vec::with_capacity(512), long: Vec::new() }
    }
}

impl Effect for Vst2Effect {
    fn save_state(&mut self) -> Option<Vec<u8>> {
        self.plugin.programs_are_chunks().then(|| self.plugin.chunk())
    }

    fn programs(&mut self) -> Vec<String> {
        self.plugin.programs()
    }

    fn program(&mut self) -> Option<usize> {
        (self.plugin.num_programs() > 0).then(|| self.plugin.program())
    }

    fn set_program(&mut self, index: usize) {
        self.plugin.set_program(index);
    }

    fn automates_at_block_end(&self) -> bool {
        true
    }

    fn is_instrument(&self) -> bool {
        self.plugin.is_synth()
    }

    fn pins(&self) -> (usize, usize) {
        (self.inputs.len(), self.outputs.len())
    }

    fn process_pins(&mut self, bufs: &mut [Vec<f32>], range: std::ops::Range<usize>, ctx: &ProcessContext) {
        let n = range.len().min(crate::render::RenderScratch::MAX_BLOCK);
        if self.pins().0 <= 2 && self.pins().1 <= 2 {
            if let [l, r, ..] = bufs {
                self.process(&mut l[range.clone()], &mut r[range], ctx);
            }
            return;
        }
        self.queue_midi(n, ctx);
        ctx.block_long(n, &mut self.long);
        if !self.midi.is_empty() || !self.long.is_empty() {
            self.plugin.send_midi(self.midi.iter().copied(), &mut self.long);
        }
        for (c, buf) in self.inputs.iter_mut().enumerate() {
            match bufs.get(c) {
                Some(src) => buf[..n].copy_from_slice(&src[range.start..range.start + n]),
                None => buf[..n].fill(0.0),
            }
        }
        let ins: [*const f32; 64] =
            std::array::from_fn(|c| self.inputs.get(c).map_or(std::ptr::null(), |b| b.as_ptr()));
        let mut outs: [*mut f32; 64] =
            std::array::from_fn(|c| self.outputs.get_mut(c).map_or(std::ptr::null_mut(), |b| b.as_mut_ptr()));
        let (ni, no) = (self.inputs.len().min(64), self.outputs.len().min(64));
        self.plugin.process(&ins[..ni], &mut outs[..no], n);
        for (c, out) in self.outputs.iter().enumerate() {
            if let Some(dst) = bufs.get_mut(c) {
                dst[range.start..range.start + n].copy_from_slice(&out[..n]);
            }
        }
    }

    fn editor(&mut self) -> Option<Box<dyn crate::editor::Editor>> {
        self.plugin.editor().map(|e| Box::new(e) as Box<dyn crate::editor::Editor>)
    }

    fn latency(&mut self) -> usize {
        self.plugin.latency()
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], ctx: &ProcessContext) {
        let n = left.len().min(crate::render::RenderScratch::MAX_BLOCK);
        self.queue_midi(n, ctx);
        ctx.block_long(n, &mut self.long);
        if !self.midi.is_empty() || !self.long.is_empty() {
            self.plugin.send_midi(self.midi.iter().copied(), &mut self.long);
        }
        for (c, buf) in self.inputs.iter_mut().enumerate() {
            match c {
                0 => buf[..n].copy_from_slice(&left[..n]),
                1 => buf[..n].copy_from_slice(&right[..n]),
                _ => buf[..n].fill(0.0),
            }
        }
        let ins: [*const f32; 64] =
            std::array::from_fn(|c| self.inputs.get(c).map_or(std::ptr::null(), |b| b.as_ptr()));
        let mut outs: [*mut f32; 64] =
            std::array::from_fn(|c| self.outputs.get_mut(c).map_or(std::ptr::null_mut(), |b| b.as_mut_ptr()));
        let (ni, no) = (self.inputs.len().min(64), self.outputs.len().min(64));
        self.plugin.process(&ins[..ni], &mut outs[..no], n);
        match self.outputs.len() {
            0 => {}
            1 => {
                left[..n].copy_from_slice(&self.outputs[0][..n]);
                right[..n].copy_from_slice(&self.outputs[0][..n]);
            }
            _ => {
                left[..n].copy_from_slice(&self.outputs[0][..n]);
                right[..n].copy_from_slice(&self.outputs[1][..n]);
            }
        }
    }
}

#[cfg(test)]
mod pin_tests {
    use super::*;

    fn js_slot(pinmap: Option<&str>) -> FxSlot {
        let mut text = String::from("<FXCHAIN\n  BYPASS 0 0 0\n  <JS utility/volume \"\"\n    -6 0 -\n  >\n");
        if let Some(b64) = pinmap {
            text.push_str(&format!("  <JS_PINMAP\n    {b64}\n  >\n"));
        }
        text.push_str("  WAK 0 0\n>\n");
        let chain = scythe_core::rpp::parse(&text).unwrap();
        FxSlot { name: "JS".into(), enabled: true, raw: chain.children }
    }

    #[test]
    fn reads_jsfx_pin_maps() {
        // Written by REAPER: inputs from channels 3/4, outputs to 1/2.
        let map = pin_map(&js_slot(Some(
            "6AMAABwAAADoAwAABAAAAAIAAAAEAAAAAAAAAAgAAAAAAAAAHAAAAOgDAAAEAAAAAgAAAAEAAAAAAAAAAgAAAAAAAAA=",
        )))
        .unwrap();
        assert_eq!((map.inputs, map.outputs), (vec![4, 8], vec![1, 2]));
        assert_eq!(pin_map(&js_slot(None)), None, "no map means plain stereo");
    }

    #[test]
    fn reads_clap_pin_maps() {
        // Written by REAPER: inputs from channels 3/4, outputs left at 1/2 (an empty block).
        let text = "<FXCHAIN\n  BYPASS 0 0 0\n  <CLAP \"CLAP: Test Gain\" org.scythe.test-gain \"\"\n    CFG 4 0 0 \"\"\n    <IN_PINS\n      PIN 0 2\n      PIN 1 3\n    >\n    <OUT_PINS\n    >\n    <STATE\n      AAAAPw==\n    >\n  >\n  WAK 0 0\n>\n";
        let chain = scythe_core::rpp::parse(text).unwrap();
        let slot = FxSlot { name: "CLAP".into(), enabled: true, raw: chain.children };
        let map = pin_map(&slot).unwrap();
        assert_eq!((map.inputs, map.outputs), (vec![4, 8], vec![1, 2]));
    }
}
