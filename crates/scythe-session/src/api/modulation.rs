use schemars::JsonSchema;
use scythe_core::modulation::{
    self as m, LEARN_FOCUSED_FX, LEARN_LFO_RETRIGGER, LEARN_SELECTED_TRACK, LEARN_SOFT_TAKEOVER, LEARN_VISIBLE_FX,
    LFO_SHAPES, LearnMode, LinkSource, ParamLearn, ParamLink, ParamModulation,
};
use scythe_core::{FxSlot, Project};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ApiError, Target, TrackRef, done, json};
use crate::Session;
use crate::learn::{ActionBinding, Heard, Trigger, describe_midi};

/// Parameter modulation (baseline, LFO, audio follower, links) and MIDI/OSC learn for effect
/// parameters, and controls bound to actions, as REAPER keeps them (`<PROGRAMENV`,
/// `PARMLEARN` in the FX chain; `KEY` lines in reaper-kb.ini). Parameters count in REAPER's
/// order from 0 (a JSFX's sliders in order).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum ModulationCommand {
    /// The modulated and learned parameters of an effect.
    #[serde(rename = "fx.modulation.list")]
    List {
        track: TrackRef,
        /// Position in the chain, from 0.
        index: usize,
    },
    /// Changes a parameter's modulation; fields left out keep their value. Creates it (with
    /// the parameter's current value as baseline) when the parameter has none.
    #[serde(rename = "fx.modulation.set")]
    Set {
        track: TrackRef,
        /// Position in the chain, from 0.
        index: usize,
        /// The parameter, from 0 in REAPER's order.
        param: usize,
        /// Switches the whole modulation off without losing its settings.
        #[serde(default)]
        enabled: Option<bool>,
        /// Value without modulation, in the parameter's own units (a JSFX slider's range,
        /// else 0..1).
        #[serde(default)]
        base: Option<f64>,
        #[serde(default)]
        lfo: Option<Box<LfoSpec>>,
        /// Following the level of the effect's input.
        #[serde(default)]
        audio: Option<Box<AudioSpec>>,
        #[serde(default)]
        link: Option<Box<LinkSpec>>,
    },
    /// Removes a parameter's modulation.
    #[serde(rename = "fx.modulation.remove")]
    Remove {
        track: TrackRef,
        /// Position in the chain, from 0.
        index: usize,
        /// The parameter, from 0 in REAPER's order.
        param: usize,
    },
    /// Binds a MIDI message or OSC address to a parameter, replacing its binding. Give
    /// `midi`, `osc`, or `heard: true` for what `fx.learn.listen` caught.
    #[serde(rename = "fx.learn.set")]
    Learn {
        track: TrackRef,
        /// Position in the chain, from 0.
        index: usize,
        /// The parameter, from 0 in REAPER's order.
        param: usize,
        #[serde(default)]
        midi: Option<MidiSpec>,
        /// An OSC address; its first argument (0..1) sets the parameter.
        #[serde(default)]
        osc: Option<String>,
        /// Use the message `fx.learn.listen` caught.
        #[serde(default)]
        heard: bool,
        /// `absolute` (default), `relative1` (127=-1, 1=+1), `relative2` (63=-1, 65=+1),
        /// `relative3` (65=-1, 1=+1) or `toggle`.
        #[serde(default)]
        mode: Option<String>,
        /// Ignore an absolute control until it reaches the parameter's value.
        #[serde(default)]
        soft_takeover: bool,
        /// Only while the effect's track is selected.
        #[serde(default)]
        selected_track_only: bool,
        /// Only while the effect's window is in front.
        #[serde(default)]
        focused_fx_only: bool,
        /// Only while the effect's window is open.
        #[serde(default)]
        visible_fx_only: bool,
        /// The message restarts the parameter's LFO instead of setting it.
        #[serde(default)]
        lfo_retrigger: bool,
    },
    /// Removes a parameter's MIDI/OSC binding.
    #[serde(rename = "fx.learn.remove")]
    Unlearn {
        track: TrackRef,
        /// Position in the chain, from 0.
        index: usize,
        /// The parameter, from 0 in REAPER's order.
        param: usize,
    },
    /// Waits for the next MIDI message (from an input enabled for control) or OSC message
    /// (from a surface that allows learning), as the Learn buttons do; `fx.learn.heard`
    /// tells what came. `on: false` stops waiting.
    #[serde(rename = "fx.learn.listen")]
    Listen {
        #[serde(default = "yes")]
        on: bool,
    },
    /// Whether it still listens, and the message it caught.
    #[serde(rename = "fx.learn.heard")]
    Heard,
    /// The MIDI and OSC controls bound to actions.
    #[serde(rename = "action.learn.list")]
    Actions,
    /// Binds a control to an action (a REAPER command id such as `40364` or a named command
    /// such as `_SCYTHE_SETLIST_NEXT`; see `surface.action_list`). Give `midi`, `osc` or
    /// `heard: true`.
    #[serde(rename = "action.learn.add")]
    BindAction {
        action: String,
        #[serde(default)]
        midi: Option<MidiSpec>,
        #[serde(default)]
        osc: Option<String>,
        #[serde(default)]
        heard: bool,
    },
    /// Removes bindings: those of `action`, or of the control given as `midi` or `osc`.
    #[serde(rename = "action.learn.remove")]
    UnbindAction {
        #[serde(default)]
        action: Option<String>,
        #[serde(default)]
        midi: Option<MidiSpec>,
        #[serde(default)]
        osc: Option<String>,
    },
}

fn yes() -> bool {
    true
}

/// An LFO's settings; fields left out keep their value.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LfoSpec {
    #[serde(default)]
    pub active: Option<bool>,
    /// `sine`, `square`, `saw_down`, `saw_up`, `triangle` or `random`.
    #[serde(default)]
    pub shape: Option<String>,
    /// Share of the parameter's range it sweeps, 0..1.
    #[serde(default)]
    pub strength: Option<f64>,
    /// 1 sweeps up from the baseline, -1 down, 0 around it.
    #[serde(default)]
    pub direction: Option<i32>,
    /// Rate in cycles per second (0.0039..8) when not tempo synced.
    #[serde(default)]
    pub hz: Option<f64>,
    /// Quarter notes per cycle (0.25..8) when tempo synced.
    #[serde(default)]
    pub period_qn: Option<f64>,
    #[serde(default)]
    pub tempo_sync: Option<bool>,
    /// Phase runs from the start of playback rather than following the project position.
    #[serde(default)]
    pub free: Option<bool>,
    /// Start phase, 0..1 of a cycle.
    #[serde(default)]
    pub phase: Option<f64>,
}

/// Audio follower settings; fields left out keep their value.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AudioSpec {
    #[serde(default)]
    pub active: Option<bool>,
    /// First input channel it listens to, from 0 (2 for a sidechain on 3/4).
    #[serde(default)]
    pub channel: Option<i32>,
    /// Listens to `channel` and the next one.
    #[serde(default)]
    pub stereo: Option<bool>,
    #[serde(default)]
    pub attack_ms: Option<f64>,
    #[serde(default)]
    pub release_ms: Option<f64>,
    /// Level that gives 0.
    #[serde(default)]
    pub min_db: Option<f64>,
    /// Level that gives 1.
    #[serde(default)]
    pub max_db: Option<f64>,
    /// Share of the parameter's range it moves, 0..1.
    #[serde(default)]
    pub strength: Option<f64>,
    /// 1 raises the parameter as the level rises, -1 lowers it, 0 moves it both ways.
    #[serde(default)]
    pub direction: Option<i32>,
    /// Control point of the level curve, 0..1 (0.5, 0.5 is straight).
    #[serde(default)]
    pub curve_x: Option<f64>,
    #[serde(default)]
    pub curve_y: Option<f64>,
}

/// Parameter link settings; fields left out keep their value.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LinkSpec {
    #[serde(default)]
    pub active: Option<bool>,
    /// The effect (position in the same chain) whose parameter it follows.
    #[serde(default)]
    pub fx: Option<usize>,
    /// That effect's parameter, from 0.
    #[serde(default)]
    pub param: Option<usize>,
    /// Follows a MIDI message on the effect's input instead.
    #[serde(default)]
    pub midi: Option<MidiSpec>,
    /// value = baseline + scale * (source + offset), all in 0..1.
    #[serde(default)]
    pub scale: Option<f64>,
    #[serde(default)]
    pub offset: Option<f64>,
}

/// A MIDI message: `cc` with `number`, `note` with `number`, `pitch`, `aftertouch` with
/// `number`, `pressure` or `program`, on `channel` 1-16 (0: any, for links only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MidiSpec {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub number: u8,
    #[serde(default = "first_channel")]
    pub channel: u8,
}

fn first_channel() -> u8 {
    1
}

impl MidiSpec {
    fn status_kind(&self) -> Result<u8, ApiError> {
        Ok(match self.kind.as_str() {
            "cc" => 0xb0,
            "note" => 0x90,
            "pitch" => 0xe0,
            "aftertouch" => 0xa0,
            "pressure" => 0xd0,
            "program" => 0xc0,
            other => return Err(ApiError::invalid(format!("unknown MIDI type \"{other}\""))),
        })
    }

    /// Status and first data byte; channel 0 is not a status.
    fn bytes(&self) -> Result<(u8, u8), ApiError> {
        if !(1..=16).contains(&self.channel) {
            return Err(ApiError::invalid("channel runs from 1 to 16"));
        }
        if self.number > 127 {
            return Err(ApiError::invalid("number runs from 0 to 127"));
        }
        let kind = self.status_kind()?;
        let data1 = if matches!(kind, 0xc0 | 0xd0 | 0xe0) { 0 } else { self.number };
        Ok((kind | (self.channel - 1), data1))
    }

    fn from_bytes(status: u8, data1: u8) -> Self {
        let kind = match status & 0xf0 {
            0x90 | 0x80 => "note",
            0xa0 => "aftertouch",
            0xc0 => "program",
            0xd0 => "pressure",
            0xe0 => "pitch",
            _ => "cc",
        };
        Self { kind: kind.into(), number: data1, channel: (status & 0x0f) + 1 }
    }
}

/// A parameter's modulation and binding, as `fx.modulation.list` shows them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamControlInfo {
    pub param: usize,
    pub name: String,
    pub modulation: Option<ModulationInfo>,
    pub learn: Option<LearnInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModulationInfo {
    pub enabled: bool,
    pub base: f64,
    pub lfo: LfoInfo,
    pub audio: AudioInfo,
    pub link: Option<LinkInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LfoInfo {
    pub active: bool,
    pub shape: String,
    pub strength: f64,
    pub direction: i32,
    pub tempo_sync: bool,
    pub hz: f64,
    pub period_qn: f64,
    pub free: bool,
    pub phase: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioInfo {
    pub active: bool,
    pub channel: i32,
    pub stereo: bool,
    pub attack_ms: f64,
    pub release_ms: f64,
    pub min_db: f64,
    pub max_db: f64,
    pub strength: f64,
    pub direction: i32,
    pub curve_x: f64,
    pub curve_y: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkInfo {
    pub active: bool,
    pub fx: Option<usize>,
    pub param: Option<i64>,
    pub midi: Option<MidiSpec>,
    pub scale: f64,
    pub offset: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LearnInfo {
    pub midi: Option<MidiSpec>,
    /// "CC 7, ch 1" and the like.
    pub describe: String,
    pub osc: Option<String>,
    pub mode: String,
    pub soft_takeover: bool,
    pub selected_track_only: bool,
    pub focused_fx_only: bool,
    pub visible_fx_only: bool,
    pub lfo_retrigger: bool,
}

pub fn shape_name(shape: u8) -> String {
    LFO_SHAPES.get(shape as usize).map_or("sine".into(), |s| s.to_lowercase().replace(' ', "_"))
}

fn shape_index(name: &str) -> Result<u8, ApiError> {
    LFO_SHAPES
        .iter()
        .position(|s| s.to_lowercase().replace(' ', "_") == name.to_lowercase())
        .map(|i| i as u8)
        .ok_or_else(|| ApiError::invalid(format!("unknown LFO shape \"{name}\"")))
}

pub fn mode_name(mode: LearnMode) -> &'static str {
    match mode {
        LearnMode::Absolute | LearnMode::Other(_) => "absolute",
        LearnMode::Relative1 => "relative1",
        LearnMode::Relative2 => "relative2",
        LearnMode::Relative3 => "relative3",
        LearnMode::Toggle => "toggle",
    }
}

fn mode_from(name: &str) -> Result<LearnMode, ApiError> {
    LearnMode::CHOICES
        .into_iter()
        .find(|m| mode_name(*m) == name)
        .ok_or_else(|| ApiError::invalid(format!("unknown learn mode \"{name}\"")))
}

pub fn modulation_info(m: &ParamModulation, own: usize) -> ModulationInfo {
    let l = &m.lfo;
    let a = &m.acs;
    ModulationInfo {
        enabled: !m.disabled,
        base: m.base,
        lfo: LfoInfo {
            active: l.active,
            shape: shape_name(l.shape),
            strength: l.strength,
            direction: l.direction,
            tempo_sync: l.tempo_sync,
            hz: l.hz(),
            period_qn: l.period_qn(),
            free: l.free,
            phase: l.phase,
        },
        audio: AudioInfo {
            active: a.active,
            channel: a.channel,
            stereo: a.stereo,
            attack_ms: a.attack_ms,
            release_ms: a.release_ms,
            min_db: a.db_low,
            max_db: a.db_high,
            strength: a.strength,
            direction: a.direction,
            curve_x: a.x2,
            curve_y: a.y2,
        },
        link: m.link.as_ref().map(|k| match &k.source {
            LinkSource::Fx { param, .. } => LinkInfo {
                active: m.link_active,
                fx: k.source_fx(own),
                param: Some(*param),
                midi: None,
                scale: k.scale,
                offset: k.offset,
            },
            LinkSource::Midi { channel, status, data1, .. } => LinkInfo {
                active: m.link_active,
                fx: None,
                param: None,
                midi: Some(MidiSpec { channel: *channel as u8, ..MidiSpec::from_bytes(*status as u8, *data1 as u8) }),
                scale: k.scale,
                offset: k.offset,
            },
        }),
    }
}

pub fn learn_info(l: &ParamLearn) -> LearnInfo {
    let midi = (l.status() >= 0x80).then(|| MidiSpec::from_bytes(l.status(), l.data1()));
    let osc = (!l.osc.is_empty()).then(|| l.osc.clone());
    let describe = match (&midi, &osc) {
        (Some(_), _) => describe_midi(l.status(), l.data1()),
        (None, Some(o)) => format!("OSC {o}"),
        (None, None) => String::new(),
    };
    LearnInfo {
        midi,
        describe,
        osc,
        mode: mode_name(l.mode()).into(),
        soft_takeover: l.flags & LEARN_SOFT_TAKEOVER != 0,
        selected_track_only: l.flags & LEARN_SELECTED_TRACK != 0,
        focused_fx_only: l.flags & LEARN_FOCUSED_FX != 0,
        visible_fx_only: l.flags & LEARN_VISIBLE_FX != 0,
        lfo_retrigger: l.flags & LEARN_LFO_RETRIGGER != 0,
    }
}

fn slot(project: &Project, target: Target, index: usize) -> Result<FxSlot, ApiError> {
    let t = target.get(project).ok_or_else(|| ApiError::not_found("the track is gone"))?;
    t.fx.get(index).cloned().ok_or_else(|| {
        ApiError::not_found(format!("the track has {} effect(s); there is no effect {index}", t.fx.len()))
    })
}

fn owner(target: Target) -> Option<scythe_core::TrackId> {
    match target {
        Target::Master => None,
        Target::Track(id) => Some(id),
    }
}

/// The name of parameter `param` (REAPER's order) of a running effect, and its current value
/// in its own units.
pub fn param_state(session: &Session, target: Target, index: usize, slot: &FxSlot, param: usize) -> (String, f64) {
    let inst = session.engine.fx().instance(owner(target), slot, index);
    let js = is_js(slot);
    inst.and_then(|i| {
        i.with(|e| {
            let params = e.params();
            let info =
                if js { params.into_iter().find(|p| p.id as usize == param) } else { params.into_iter().nth(param) };
            let name = info.map(|p| p.name).unwrap_or_else(|| format!("Parameter {}", param + 1));
            let (lo, hi) = scythe_engine::modulation::native_range(e, param);
            let v = scythe_engine::modulation::normalized(e, param).unwrap_or(0.0);
            (name, lo + v * (hi - lo))
        })
    })
    .unwrap_or_else(|| (format!("Parameter {}", param + 1), 0.0))
}

pub fn is_js(slot: &FxSlot) -> bool {
    slot.raw.iter().any(|n| matches!(n, scythe_core::rpp::Node::Chunk(c) if c.name() == "JS"))
}

/// How REAPER names parameter `param` beside the slot: the spelling of an envelope or
/// binding it already has, else as a new envelope names it: `<index>:<id>` for a CLAP plugin,
/// the index for others.
fn param_spec(session: &Session, target: Target, index: usize, slot: &FxSlot, param: usize) -> String {
    let from_env = slot.raw.iter().find_map(|n| match n {
        scythe_core::rpp::Node::Chunk(c) if c.name() == "PARMENV" => {
            c.args().first().filter(|s| m::param_index(s) == Some(param)).cloned()
        }
        _ => None,
    });
    from_env
        .or_else(|| m::learn(slot, param).map(|l| l.param))
        .or_else(|| m::modulation(slot, param).map(|m| m.param))
        .or_else(|| {
            let (params, _) = super::envelope::listed_params(session, target, index, slot);
            scythe_engine::fx::envelope_spec(slot, params.as_deref(), param)
        })
        .unwrap_or_else(|| param.to_string())
}

pub fn list(session: &Session, target: Target, index: usize) -> Result<Vec<ParamControlInfo>, ApiError> {
    let slot = slot(&session.project, target, index)?;
    let mods = m::modulations(&slot);
    let learns = m::learns(&slot);
    let mut params: Vec<usize> =
        mods.iter().filter_map(|m| m.index()).chain(learns.iter().filter_map(|l| l.index())).collect();
    params.sort_unstable();
    params.dedup();
    Ok(params
        .into_iter()
        .map(|p| ParamControlInfo {
            param: p,
            name: param_state(session, target, index, &slot, p).0,
            modulation: mods.iter().find(|m| m.index() == Some(p)).map(|m| modulation_info(m, index)),
            learn: learns.iter().find(|l| l.index() == Some(p)).map(learn_info),
        })
        .collect())
}

fn apply_lfo(l: &mut m::Lfo, s: &LfoSpec) -> Result<(), ApiError> {
    if let Some(v) = s.active {
        l.active = v;
    }
    if let Some(v) = &s.shape {
        l.shape = shape_index(v)?;
    }
    if let Some(v) = s.strength {
        l.strength = v.clamp(0.0, 1.0);
    }
    if let Some(v) = s.direction {
        l.direction = v.signum();
    }
    if let Some(v) = s.tempo_sync {
        l.tempo_sync = v;
    }
    if let Some(v) = s.free {
        l.free = v;
    }
    if let Some(v) = s.phase {
        l.phase = v.rem_euclid(1.0);
    }
    match (s.hz, s.period_qn) {
        (Some(_), Some(_)) => return Err(ApiError::invalid("give either \"hz\" or \"period_qn\"")),
        (Some(hz), None) => {
            if !(m::LFO_MIN_HZ..=m::LFO_MAX_HZ).contains(&hz) {
                return Err(ApiError::invalid("hz runs from 0.0039 to 8"));
            }
            l.speed = m::Lfo::speed_for_hz(hz);
            l.tempo_sync = s.tempo_sync.unwrap_or(false);
        }
        (None, Some(qn)) => {
            if !(0.25..=8.0).contains(&qn) {
                return Err(ApiError::invalid("period_qn runs from 0.25 to 8"));
            }
            l.speed = m::Lfo::speed_for_period_qn(qn);
            l.tempo_sync = s.tempo_sync.unwrap_or(true);
        }
        (None, None) => {}
    }
    Ok(())
}

fn apply_audio(a: &mut m::AudioControl, s: &AudioSpec) {
    let set = |dst: &mut f64, v: Option<f64>| {
        if let Some(v) = v.filter(|v| v.is_finite()) {
            *dst = v;
        }
    };
    if let Some(v) = s.active {
        a.active = v;
    }
    if let Some(v) = s.channel {
        a.channel = v.max(-1);
    }
    if let Some(v) = s.stereo {
        a.stereo = v;
    }
    set(&mut a.attack_ms, s.attack_ms.map(|v| v.max(0.0)));
    set(&mut a.release_ms, s.release_ms.map(|v| v.max(0.0)));
    set(&mut a.db_low, s.min_db);
    set(&mut a.db_high, s.max_db);
    set(&mut a.strength, s.strength.map(|v| v.clamp(0.0, 1.0)));
    set(&mut a.x2, s.curve_x.map(|v| v.clamp(0.0, 1.0)));
    set(&mut a.y2, s.curve_y.map(|v| v.clamp(0.0, 1.0)));
    if let Some(v) = s.direction {
        a.direction = v.signum();
    }
}

/// Gives a link from a CLAP or VST3 parameter that parameter's id, as REAPER names it
/// (`PLINK 1 0:-1 3:7 0 ""`), when the source effect's parameters are known.
fn name_link_param(session: &Session, target: Target, own: usize, link: &mut ParamLink) {
    let Some(fx) = link.source_fx(own) else { return };
    let LinkSource::Fx { param, id, .. } = &mut link.source else { return };
    if id.is_some() {
        return;
    }
    let Some(source) = target.get(&session.project).and_then(|t| t.fx.get(fx)) else { return };
    let Ok(index) = usize::try_from(*param) else { return };
    let (params, _) = super::envelope::listed_params(session, target, fx, source);
    *id = scythe_engine::fx::envelope_spec(source, params.as_deref(), index)
        .and_then(|spec| spec.split_once(':').and_then(|(_, id)| id.parse().ok()));
}

fn apply_link(md: &mut ParamModulation, s: &LinkSpec, own: usize) -> Result<(), ApiError> {
    let link = md.link.get_or_insert_with(|| ParamLink {
        scale: 1.0,
        offset: 0.0,
        source: LinkSource::Fx { fx: "-1".into(), param: -1, id: None },
        tail: String::new(),
    });
    if let Some(v) = s.scale {
        link.scale = v;
    }
    if let Some(v) = s.offset {
        link.offset = v;
    }
    match (&s.midi, s.fx) {
        (Some(_), Some(_)) => return Err(ApiError::invalid("a link follows either an effect or MIDI")),
        (Some(midi), None) => {
            let kind = midi.status_kind()?;
            if midi.channel > 16 {
                return Err(ApiError::invalid("channel runs from 0 (any) to 16"));
            }
            link.source = LinkSource::Midi {
                bus: 0,
                channel: midi.channel as i32,
                status: kind as i32,
                data1: midi.number as i32,
            };
        }
        (None, Some(fx)) => {
            let param = s.param.map_or(0, |p| p as i64);
            link.source = LinkSource::Fx { fx: ParamLink::fx_token(fx, own), param, id: None };
        }
        (None, None) => {
            if let (Some(p), LinkSource::Fx { param, id, .. }) = (s.param, &mut link.source) {
                *param = p as i64;
                *id = None;
            }
        }
    }
    md.link_active = s.active.unwrap_or(true);
    Ok(())
}

fn learn_from(
    session: &Session,
    midi: &Option<MidiSpec>,
    osc: &Option<String>,
    heard: bool,
) -> Result<Trigger, ApiError> {
    match (midi, osc, heard) {
        (Some(m), None, false) => {
            let (status, data1) = m.bytes()?;
            Ok(Trigger::Midi { status, data1 })
        }
        (None, Some(o), false) if o.starts_with('/') => Ok(Trigger::Osc { address: o.clone() }),
        (None, Some(_), false) => Err(ApiError::invalid("an OSC address starts with /")),
        (None, None, true) => session
            .learn
            .heard
            .as_ref()
            .map(Trigger::from)
            .ok_or_else(|| ApiError::failed("nothing heard yet; send a message after fx.learn.listen")),
        _ => Err(ApiError::invalid("give one of \"midi\", \"osc\" or \"heard\": true")),
    }
}

fn binding_info(b: &ActionBinding) -> Value {
    let (midi, osc) = match &b.trigger {
        Trigger::Midi { status, data1 } => (Some(MidiSpec::from_bytes(*status, *data1)), None),
        Trigger::Osc { address } => (None, Some(address.clone())),
    };
    serde_json::json!({ "action": b.action, "midi": midi, "osc": osc, "describe": b.trigger.describe() })
}

pub fn execute(session: &mut Session, command: ModulationCommand) -> Result<Value, ApiError> {
    match command {
        ModulationCommand::List { track, index } => {
            let target = track.resolve(&session.project)?;
            Ok(json(list(session, target, index)?))
        }
        ModulationCommand::Set { track, index, param, enabled, base, lfo, audio, link } => {
            let target = track.resolve(&session.project)?;
            let current = slot(&session.project, target, index)?;
            let mut md = match m::modulation(&current, param) {
                Some(md) => md,
                None => {
                    let (name, value) = param_state(session, target, index, &current, param);
                    let fx_name = current.name.split_once(": ").map_or(current.name.as_str(), |(_, n)| n);
                    let fx_name = fx_name.rsplit_once(" (").map_or(fx_name, |(n, _)| n);
                    ParamModulation::new(
                        param_spec(session, target, index, &current, param),
                        format!("{name} / {fx_name}"),
                        value,
                    )
                }
            };
            if let Some(v) = enabled {
                md.disabled = !v;
            }
            if let Some(v) = base {
                if !v.is_finite() {
                    return Err(ApiError::invalid("the baseline must be a number"));
                }
                md.base = v;
                if let Some(inst) = session.engine.fx().instance(owner(target), &current, index) {
                    inst.with_mods(|s| s.clear_base(param));
                }
            }
            if let Some(s) = &lfo {
                apply_lfo(&mut md.lfo, s)?;
            }
            if let Some(s) = &audio {
                apply_audio(&mut md.acs, s);
            }
            if let Some(s) = &link {
                apply_link(&mut md, s, index)?;
                if let Some(l) = md.link.as_mut() {
                    name_link_param(session, target, index, l);
                }
            }
            let info = modulation_info(&md, index);
            session.edit("Parameter modulation", |p| {
                if let Some(s) = target.get_mut(p).and_then(|t| t.fx.get_mut(index)) {
                    m::set_modulation(s, &md);
                }
            });
            Ok(json(info))
        }
        ModulationCommand::Remove { track, index, param } => {
            let target = track.resolve(&session.project)?;
            let current = slot(&session.project, target, index)?;
            if m::modulation(&current, param).is_none() {
                return Err(ApiError::not_found(format!("parameter {param} has no modulation")));
            }
            session.edit("Remove parameter modulation", |p| {
                if let Some(s) = target.get_mut(p).and_then(|t| t.fx.get_mut(index)) {
                    m::remove_modulation(s, param);
                }
            });
            Ok(done())
        }
        ModulationCommand::Learn {
            track,
            index,
            param,
            midi,
            osc,
            heard,
            mode,
            soft_takeover,
            selected_track_only,
            focused_fx_only,
            visible_fx_only,
            lfo_retrigger,
        } => {
            let target = track.resolve(&session.project)?;
            let current = slot(&session.project, target, index)?;
            let mode = mode.as_deref().map(mode_from).transpose()?.unwrap_or(LearnMode::Absolute);
            let trigger = learn_from(session, &midi, &osc, heard)?;
            let spec = param_spec(session, target, index, &current, param);
            let mut l = match &trigger {
                Trigger::Midi { status, data1 } => ParamLearn::midi(spec, *status, *data1, mode),
                Trigger::Osc { address } => {
                    let mut l = ParamLearn::midi(spec, 0, 0, mode);
                    l.osc = address.clone();
                    l
                }
            };
            for (on, bit) in [
                (soft_takeover, LEARN_SOFT_TAKEOVER),
                (selected_track_only, LEARN_SELECTED_TRACK),
                (focused_fx_only, LEARN_FOCUSED_FX),
                (visible_fx_only, LEARN_VISIBLE_FX),
                (lfo_retrigger, LEARN_LFO_RETRIGGER),
            ] {
                if on {
                    l.flags |= bit;
                }
            }
            let info = learn_info(&l);
            session.edit("Learn parameter", |p| {
                if let Some(s) = target.get_mut(p).and_then(|t| t.fx.get_mut(index)) {
                    m::set_learn(s, &l);
                }
            });
            session.sync();
            Ok(json(info))
        }
        ModulationCommand::Unlearn { track, index, param } => {
            let target = track.resolve(&session.project)?;
            let current = slot(&session.project, target, index)?;
            if m::learn(&current, param).is_none() {
                return Err(ApiError::not_found(format!("parameter {param} is not learned")));
            }
            session.edit("Remove parameter learn", |p| {
                if let Some(s) = target.get_mut(p).and_then(|t| t.fx.get_mut(index)) {
                    m::remove_learn(s, param);
                }
            });
            Ok(done())
        }
        ModulationCommand::Listen { on } => {
            session.listen_for_control(on);
            Ok(heard_state(session))
        }
        ModulationCommand::Heard => Ok(heard_state(session)),
        ModulationCommand::Actions => Ok(Value::Array(session.learn.bindings.iter().map(binding_info).collect())),
        ModulationCommand::BindAction { action, midi, osc, heard } => {
            let action = action.trim().to_owned();
            if action.is_empty() {
                return Err(ApiError::invalid("name the action"));
            }
            let trigger = learn_from(session, &midi, &osc, heard)?;
            session.bind_action(trigger.clone(), action.clone());
            Ok(binding_info(&ActionBinding { trigger, action }))
        }
        ModulationCommand::UnbindAction { action, midi, osc } => {
            let trigger = match (&midi, &osc) {
                (None, None) => None,
                _ => Some(learn_from(session, &midi, &osc, false)?),
            };
            if action.is_none() && trigger.is_none() {
                return Err(ApiError::invalid("give \"action\", \"midi\" or \"osc\""));
            }
            let removed = session.unbind_actions(|b| {
                let hit =
                    action.as_deref().is_none_or(|a| a == b.action) && trigger.as_ref().is_none_or(|t| *t == b.trigger);
                !hit
            });
            Ok(serde_json::json!({ "removed": removed }))
        }
    }
}

fn heard_state(session: &Session) -> Value {
    let heard: Option<&Heard> = session.learn.heard.as_ref();
    serde_json::json!({
        "listening": session.learn.listening,
        "heard": heard,
        "describe": heard.map(Heard::describe),
    })
}
