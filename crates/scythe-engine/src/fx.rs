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
}

impl ProcessContext<'_> {
    /// Live MIDI messages for this track.
    pub fn live_midi(&self) -> impl Iterator<Item = &LiveMidi> {
        self.live.iter().filter(|m| self.live_channel.is_none_or(|c| m.channel() == c))
    }
}

/// An audio effect working in place on a stereo block.
pub trait Effect: Send {
    fn process(&mut self, left: &mut [f32], right: &mut [f32], ctx: &ProcessContext);
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

pub struct FxInstance {
    effect: Mutex<Box<dyn Effect>>,
    pub instrument: bool,
}

impl FxInstance {
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
        if !bypassed {
            fx.process(left, right, ctx);
        }
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
        self.chain_with_automation(track, slots).0
    }

    /// Like `chain`, with each effect's parameter envelopes.
    pub fn chain_with_automation(
        &mut self,
        track: Option<TrackId>,
        slots: &[FxSlot],
    ) -> (Vec<Arc<FxInstance>>, Vec<Vec<ParamAutomation>>) {
        let owner = track.map_or(Owner::Master, Owner::Track);
        let mut out = Vec::new();
        let mut automation = Vec::new();
        for (i, slot) in slots.iter().enumerate() {
            let key = (owner.clone(), slot_key(slot, i));
            let state = plugin_state(slot);
            let stale = self.entries.get(&key).is_none_or(|e| e.state != state);
            if stale {
                let (instance, status) = match self.create(slot) {
                    Ok((fx, status)) => {
                        (Some(Arc::new(FxInstance { instrument: fx.is_instrument(), effect: Mutex::new(fx) })), status)
                    }
                    Err(reason) => (None, FxStatus::Unavailable(reason)),
                };
                self.entries.insert(key.clone(), Entry { state, instance, status });
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
                    if ReaperVstState::decode(&lines).is_some_and(|old| old.chunk == state) {
                        continue;
                    }
                    let new_lines = ReaperVstState::reencode(&lines, &state)
                        .unwrap_or_else(|| crate::builtin::vst_lines(id, &state));
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

    fn create(&self, slot: &FxSlot) -> Result<(Box<dyn Effect>, FxStatus), String> {
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
                let fx = crate::jsfx::JsfxEffect::new(script, &values, self.sample_rate)?;
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
                let nam = NamEffect::new(&path, level("input_level"), level("output_level"))
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
                Ok((Box::new(ClapEffect { plugin, instrument, expected: None }), FxStatus::Hosted(desc.path)))
            }
            other => Err(format!("{other} plugins are not supported yet")),
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
        _ => return None,
    };
    (!map.is_plain()).then_some(map)
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
}

impl Effect for ClapEffect {
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
        let n = left.len() as i64;
        let (start, end) = (ctx.position, ctx.position + n);
        if self.instrument && self.expected.is_some_and(|e| e != start) {
            // A jump in the timeline: drop held notes rather than leave them hanging.
            self.plugin.reset();
        }
        self.expected = Some(end);
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
        self.plugin.process(left, right);
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
    fn new(plugin: crate::vst3_host::Vst3Plugin) -> Self {
        Self { plugin, notes: Vec::with_capacity(256), controls: Vec::with_capacity(256) }
    }
}

impl Effect for Vst3Effect {
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
        self.notes.clear();
        self.controls.clear();
        if let Some(notes) = ctx.notes {
            let (start, end) = (ctx.position, ctx.position + left.len() as i64);
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
        self.plugin.process(left, right, &self.notes, &self.controls, ctx.position, ctx.bpm);
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
}

impl Lv2Effect {
    fn new(plugin: crate::lv2::Lv2Plugin) -> Self {
        Self { plugin, midi: Vec::with_capacity(256) }
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
        if let Some(notes) = ctx.notes {
            let (start, end) = (ctx.position, ctx.position + left.len() as i64);
            for note in notes.starting_in(start - notes.max_span()..end) {
                if (start..end).contains(&note.start) {
                    self.midi.push(((note.start - start) as u32, [0x90 | note.channel, note.key, note.velocity]));
                }
                let off = note.end.min(note.cutoff);
                if (start..end).contains(&off) {
                    self.midi.push(((off - start) as u32, [0x80 | note.channel, note.key, 0]));
                }
            }
            for c in notes.controls_in(start..end) {
                self.midi.push(((c.frame - start) as u32, c.bytes));
            }
            self.midi.sort_by_key(|e| (e.0, e.1[0] & 0xf0 == 0x90));
        }
        for (i, m) in ctx.live_midi().enumerate() {
            self.midi.insert(i, (0, m.bytes));
        }
        for (l, r) in left
            .chunks_mut(crate::render::RenderScratch::MAX_BLOCK)
            .zip(right.chunks_mut(crate::render::RenderScratch::MAX_BLOCK))
        {
            self.plugin.process(l, r, &self.midi);
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
}

impl Vst2Effect {
    fn new(plugin: Vst2Plugin) -> Self {
        let n = crate::render::RenderScratch::MAX_BLOCK;
        let inputs = (0..plugin.num_inputs()).map(|_| vec![0.0; n]).collect();
        let outputs = (0..plugin.num_outputs()).map(|_| vec![0.0; n]).collect();
        Self { plugin, inputs, outputs, midi: Vec::with_capacity(512) }
    }
}

impl Effect for Vst2Effect {
    fn is_instrument(&self) -> bool {
        self.plugin.is_synth()
    }

    fn editor(&mut self) -> Option<Box<dyn crate::editor::Editor>> {
        self.plugin.editor().map(|e| Box::new(e) as Box<dyn crate::editor::Editor>)
    }

    fn latency(&mut self) -> usize {
        self.plugin.latency()
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], ctx: &ProcessContext) {
        let n = left.len().min(crate::render::RenderScratch::MAX_BLOCK);
        self.midi.clear();
        self.midi.extend(ctx.live_midi().map(|m| (0, m.bytes)));
        if let Some(notes) = ctx.notes {
            let (start, end) = (ctx.position, ctx.position + n as i64);
            for note in notes.starting_in(start - notes.max_span()..end) {
                if (start..end).contains(&note.start) {
                    self.midi.push(((note.start - start) as usize, [0x90 | note.channel, note.key, note.velocity]));
                }
                let off = note.end.min(note.cutoff);
                if (start..end).contains(&off) {
                    self.midi.push(((off - start) as usize, [0x80 | note.channel, note.key, 0]));
                }
            }
            for c in notes.controls_in(start..end) {
                self.midi.push(((c.frame - start) as usize, c.bytes));
            }
            // Offsets in order, note-offs before note-ons at the same frame.
            self.midi.sort_by_key(|e| (e.0, e.1[0] & 0xf0 == 0x90));
        }
        if !self.midi.is_empty() {
            self.plugin.send_midi(self.midi.iter().copied());
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
}
