//! Track and master FX chains. Instances outlive render graph rebuilds: the registry keeps them
//! keyed by slot identity and recreates one only when its stored state changes.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use scythe_core::rpp::{Chunk, Node, line_key, tokenize};
use scythe_core::{FxSlot, Project, TrackId};

use crate::clap_host::{ClapDescriptor, ClapLibrary, ClapPlugin, ParamInfo};
use crate::delay::{Limiter, ReaDelayCompat, ReaDelaySettings};
use crate::dynamics::{Compressor, Gate};
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
    /// Project tempo at the block start, in quarter notes per minute.
    pub bpm: f64,
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

#[derive(Debug, Clone)]
pub struct ParamAutomation {
    pub target: Target,
    pub envelope: scythe_core::Envelope,
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
            let spec = c.args().into_iter().next()?;
            let name = spec.split_once(':').map_or(spec.as_str(), |(_, n)| n).to_owned();
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
            (envelope.active && !envelope.points.is_empty()).then_some(ParamAutomation { target, envelope })
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
                    fx.set_named_param(name, v);
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
    /// Relative file references in plugin state (impulse responses) resolve against this.
    pub project_dir: Option<PathBuf>,
    clap_cache: std::sync::OnceLock<Vec<ClapDescriptor>>,
}

impl FxRegistry {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            entries: HashMap::new(),
            sample_rate,
            search_paths: vst_search_paths(),
            project_dir: None,
            clap_cache: Default::default(),
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
                automation.push(parameter_envelopes(slot));
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
                let Some(state) = inst.with(|e| e.save_state()).flatten() else { continue };
                let Some(Node::Chunk(chunk)) =
                    slot.raw.iter_mut().find(|n| matches!(n, Node::Chunk(c) if matches!(c.name(), "CLAP" | "VST")))
                else {
                    continue;
                };
                if chunk.name() == "VST" {
                    // REAPER's layout: header, plugin chunk and program, as base64 lines.
                    let id = chunk.args().get(4).and_then(|a| a.split('<').next()?.parse::<u32>().ok());
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
                    return Ok((Box::new(limiter), FxStatus::Native("ReaLimit-compatible limiter (approximate)")));
                }
                if display.contains("ReaGate (Cockos)") {
                    let state = state.ok_or("unreadable ReaGate state")?;
                    let gate =
                        Gate::from_reagate(&state.chunk, self.sample_rate).ok_or("unreadable ReaGate settings")?;
                    return Ok((Box::new(gate), FxStatus::Native("ReaGate-compatible gate (approximate)")));
                }
                if display.contains("ReaComp (Cockos)") {
                    let state = state.ok_or("unreadable ReaComp state")?;
                    let comp = Compressor::from_reacomp(&state.chunk, self.sample_rate)
                        .ok_or("unreadable ReaComp settings")?;
                    return Ok((Box::new(comp), FxStatus::Native("ReaComp-compatible compressor (approximate)")));
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
            self.plugin
                .events
                .sort_by_key(|e| (e.header.time, e.header.type_ != clap_sys::events::CLAP_EVENT_NOTE_OFF));
        }
        self.plugin.process(left, right);
    }
}

/// Adapts a hosted VST 2 plugin to the stereo in-place interface.
struct Vst2Effect {
    plugin: Vst2Plugin,
    inputs: Vec<Vec<f32>>,
    outputs: Vec<Vec<f32>>,
}

impl Vst2Effect {
    fn new(plugin: Vst2Plugin) -> Self {
        let n = crate::render::RenderScratch::MAX_BLOCK;
        let inputs = (0..plugin.num_inputs()).map(|_| vec![0.0; n]).collect();
        let outputs = (0..plugin.num_outputs()).map(|_| vec![0.0; n]).collect();
        Self { plugin, inputs, outputs }
    }
}

impl Effect for Vst2Effect {
    fn process(&mut self, left: &mut [f32], right: &mut [f32], _ctx: &ProcessContext) {
        let n = left.len().min(crate::render::RenderScratch::MAX_BLOCK);
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
