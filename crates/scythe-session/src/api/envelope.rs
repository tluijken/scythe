//! Envelopes: points and their shapes, automation items and their pools, lanes, arming, and the
//! global automation override. An envelope is named on its track by kind (`volume`, `pan`,
//! `mute`, `width`, …, with `send` for a send's), or as `fx:<effect>:<parameter>` for an FX
//! parameter envelope, which `envelope.set` creates.

use schemars::JsonSchema;
use scythe_core::automation_item::{AutomationItem, AutomationPool, Lfo, LfoShape, capture};
use scythe_core::fx_chain::ParamEnvelope;
use scythe_core::model::GlobalAutomation;
use scythe_core::{AutoMode, EnvPoint, Envelope, EnvelopeKind, ItemId, PointShape, Project};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ApiError, Target, TrackRef, done, json, time};
use crate::{PreviewSpan, Session};

/// Which envelope of a track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EnvRef {
    pub track: TrackRef,
    /// `volume`, `volume_pre_fx`, `trim`, `pan`, `mute`, `width`, `dual_pan_left`,
    /// `dual_pan_right`, `fx:<effect index>:<parameter index>` for an FX parameter, or `tempo`
    /// for the project's tempo map (REAPER's master tempo envelope; any track).
    pub envelope: String,
    /// A send's envelope (`volume`, `pan` or `mute`): the send's index in `send.list`.
    #[serde(default)]
    pub send: Option<usize>,
    /// A take envelope (`volume`, `pan`, `mute` or `pitch`) of this item's active take, its
    /// points given in project seconds.
    #[serde(default)]
    pub item: Option<u64>,
}

/// A point as the API gives and takes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PointSpec {
    /// Seconds (quarter notes from the pool's start for pool points).
    pub time: f64,
    /// In the envelope's units (0..1 of its range for pool points).
    pub value: f64,
    /// `linear`, `square`, `slow_start_end`, `fast_start`, `fast_end` or `bezier`; the shape
    /// of the segment to the next point.
    #[serde(default)]
    pub shape: Option<String>,
    /// Bezier tension, -1 to 1.
    #[serde(default)]
    pub tension: Option<f64>,
}

/// Settings that generate a pool's points as an LFO.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LfoSpec {
    /// `sine`, `triangle`, `square`, `saw_up` or `saw_down`.
    pub shape: String,
    /// Cycles over the pool's length.
    pub cycles: f64,
    /// Where the first cycle starts, 0 to 1.
    #[serde(default)]
    pub phase: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum EnvelopeCommand {
    /// Every envelope of `track`: its own, its sends' and its effects' parameter envelopes.
    #[serde(rename = "envelope.list")]
    List { track: TrackRef },
    /// One envelope with its points and automation items.
    #[serde(rename = "envelope.get")]
    Get {
        #[serde(flatten)]
        at: EnvRef,
    },
    /// Creates the envelope when missing and changes what is given.
    #[serde(rename = "envelope.set")]
    Set {
        #[serde(flatten)]
        at: EnvRef,
        /// Off bypasses the envelope.
        #[serde(default)]
        active: Option<bool>,
        #[serde(default)]
        visible: Option<bool>,
        /// In a lane of its own rather than over the track.
        #[serde(default)]
        in_lane: Option<bool>,
        /// Armed envelopes are the ones automation writing records into.
        #[serde(default)]
        armed: Option<bool>,
    },
    /// Removes the envelope with its points and items.
    #[serde(rename = "envelope.remove")]
    Remove {
        #[serde(flatten)]
        at: EnvRef,
    },
    /// Adds a point (creating the envelope) and answers with the envelope.
    #[serde(rename = "envelope.point_add")]
    PointAdd {
        #[serde(flatten)]
        at: EnvRef,
        #[serde(flatten)]
        point: PointSpec,
    },
    /// Changes point `index` (its position in `envelope.get`).
    #[serde(rename = "envelope.point_edit")]
    PointEdit {
        #[serde(flatten)]
        at: EnvRef,
        index: usize,
        #[serde(default)]
        time: Option<f64>,
        #[serde(default)]
        value: Option<f64>,
        /// See `PointSpec::shape`.
        #[serde(default)]
        shape: Option<String>,
        #[serde(default)]
        tension: Option<f64>,
    },
    #[serde(rename = "envelope.point_delete")]
    PointDelete {
        #[serde(flatten)]
        at: EnvRef,
        index: usize,
    },
    /// Sets the shape of every point from `from` to `to` seconds (all points when left out).
    #[serde(rename = "envelope.shape")]
    Shape {
        #[serde(flatten)]
        at: EnvRef,
        /// See `PointSpec::shape`.
        shape: String,
        #[serde(default)]
        tension: Option<f64>,
        #[serde(default)]
        from: Option<f64>,
        #[serde(default)]
        to: Option<f64>,
    },
    /// The envelope's value at `time` seconds, automation items included.
    #[serde(rename = "envelope.evaluate")]
    Evaluate {
        #[serde(flatten)]
        at: EnvRef,
        time: f64,
    },
    /// Inserts an automation item. With `pool` it is a pooled copy of that pool; with `lfo` a
    /// new pool shaped by the LFO; otherwise a new pool holding the envelope as it is there.
    #[serde(rename = "envelope.item_add")]
    ItemAdd {
        #[serde(flatten)]
        at: EnvRef,
        /// Seconds.
        position: f64,
        /// Seconds.
        length: f64,
        #[serde(default)]
        pool: Option<i64>,
        #[serde(default)]
        lfo: Option<LfoSpec>,
    },
    /// Changes automation item `index` (its position in `envelope.get`).
    #[serde(rename = "envelope.item_edit")]
    ItemEdit {
        #[serde(flatten)]
        at: EnvRef,
        index: usize,
        #[serde(default)]
        position: Option<f64>,
        #[serde(default)]
        length: Option<f64>,
        /// Seconds into the pool where the item starts.
        #[serde(default)]
        start_offset: Option<f64>,
        /// Play rate, above 0.
        #[serde(default)]
        rate: Option<f64>,
        /// 0 to 1: where the middle of the pool's range plays.
        #[serde(default)]
        baseline: Option<f64>,
        /// -1 to 1: how far the pool's values reach from the baseline.
        #[serde(default)]
        amplitude: Option<f64>,
        /// Repeat the pool over the item's length.
        #[serde(default)]
        looped: Option<bool>,
        /// Seconds of the ramps at the item's edges.
        #[serde(default)]
        transition: Option<f64>,
        #[serde(default)]
        selected: Option<bool>,
    },
    /// Removes automation item `index`; with `keep_points` its values stay as envelope points.
    #[serde(rename = "envelope.item_delete")]
    ItemDelete {
        #[serde(flatten)]
        at: EnvRef,
        index: usize,
        #[serde(default)]
        keep_points: bool,
    },
    /// Copies automation item `index` to right after it, pooled (sharing its points) or with
    /// a pool of its own.
    #[serde(rename = "envelope.item_duplicate")]
    ItemDuplicate {
        #[serde(flatten)]
        at: EnvRef,
        index: usize,
        #[serde(default)]
        pooled: bool,
    },
    /// Gives automation item `index` a pool of its own (REAPER's unpool).
    #[serde(rename = "envelope.item_unpool")]
    ItemUnpool {
        #[serde(flatten)]
        at: EnvRef,
        index: usize,
    },
    /// A pool's points, or sets them (every pooled item changes): times in quarter notes from
    /// the pool's start, values 0..1 of the envelope's range.
    #[serde(rename = "envelope.pool")]
    Pool {
        pool: i64,
        #[serde(default)]
        points: Option<Vec<PointSpec>>,
        /// Generates the points as an LFO instead.
        #[serde(default)]
        lfo: Option<LfoSpec>,
        /// Source length in quarter notes.
        #[serde(default)]
        length_qn: Option<f64>,
        #[serde(default)]
        name: Option<String>,
    },
    /// A track's automation mode, or sets it (on every track when `track` is left out):
    /// `trim_read`, `read`, `touch`, `write`, `latch` or `latch_preview`. With `send`, the mode of
    /// that send's envelopes (REAPER's per-send mode), where `track` means the send's own mode
    /// is dropped and it follows the track again.
    #[serde(rename = "envelope.mode")]
    Mode {
        #[serde(default)]
        track: Option<TrackRef>,
        #[serde(default)]
        mode: Option<String>,
        /// A send of `track`, by its index in `send.list`.
        #[serde(default)]
        send: Option<usize>,
    },
    /// Writes the values the last latch preview pass remembered: over the time selection
    /// (`time_selection`), from the edit cursor on (`to_end`) or up to it (`to_start`).
    #[serde(rename = "envelope.latch_preview_apply")]
    LatchPreviewApply { span: String },
    /// The global automation override, or sets it: `none` (every track its own mode),
    /// `trim_read`, `read`, `touch`, `write`, `latch`, `latch_preview` or `bypass`.
    #[serde(rename = "envelope.global_override")]
    GlobalOverride {
        #[serde(default)]
        mode: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemInfo {
    pub index: usize,
    pub pool: i64,
    pub position: f64,
    pub length: f64,
    pub start_offset: f64,
    pub rate: f64,
    pub baseline: f64,
    pub amplitude: f64,
    pub looped: bool,
    pub transition: f64,
    pub selected: bool,
    /// Other items playing the same pool, anywhere in the project.
    pub pooled_copies: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvelopeInfo {
    pub envelope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub send: Option<usize>,
    pub label: String,
    pub active: bool,
    pub visible: bool,
    pub in_lane: bool,
    pub armed: bool,
    pub range: (f64, f64),
    pub fader_scaled: bool,
    pub points: Vec<PointSpec>,
    pub items: Vec<ItemInfo>,
}

enum Which {
    Kind(EnvelopeKind),
    Take(ItemId, EnvelopeKind),
    Tempo,
    Send(usize, EnvelopeKind),
    Param(usize, usize),
}

fn which(at: &EnvRef) -> Result<Which, ApiError> {
    if at.envelope == "tempo" {
        return Ok(Which::Tempo);
    }
    if let Some(item) = at.item {
        let kind = match at.envelope.as_str() {
            "pitch" => EnvelopeKind::Pitch,
            name => EnvelopeKind::from_name(name)
                .filter(|k| matches!(k, EnvelopeKind::Volume | EnvelopeKind::Pan | EnvelopeKind::Mute))
                .ok_or_else(|| ApiError::invalid("a take envelope is volume, pan, mute or pitch"))?,
        };
        return Ok(Which::Take(ItemId(item), kind));
    }
    if let Some(rest) = at.envelope.strip_prefix("fx:") {
        let (fx, param) =
            rest.split_once(':').ok_or_else(|| ApiError::invalid("an FX envelope is \"fx:<effect>:<parameter>\""))?;
        let parse = |s: &str| s.parse::<usize>().map_err(|_| ApiError::invalid(format!("\"{s}\" is not an index")));
        return Ok(Which::Param(parse(fx)?, parse(param)?));
    }
    let kind = EnvelopeKind::from_name(&at.envelope).ok_or_else(|| {
        ApiError::invalid(format!("unknown envelope \"{}\"; see envelope.list for the names", at.envelope))
    })?;
    match at.send {
        Some(_) if kind.send_chunk_name().is_none() => {
            Err(ApiError::invalid(format!("sends have no {} envelope", kind.name())))
        }
        Some(s) => Ok(Which::Send(s, kind)),
        None => Ok(Which::Kind(kind)),
    }
}

fn shape(name: Option<&str>, tension: Option<f64>, old: PointShape) -> Result<PointShape, ApiError> {
    let tension = tension.or(match old {
        PointShape::Bezier(t) => Some(t),
        _ => None,
    });
    match name {
        None => Ok(match (old, tension) {
            (PointShape::Bezier(_), Some(t)) => PointShape::Bezier(t.clamp(-1.0, 1.0)),
            _ => old,
        }),
        Some(n) => PointShape::from_name(n, tension.unwrap_or(0.0)).ok_or_else(|| {
            ApiError::invalid(format!("unknown shape \"{n}\"; one of {}", PointShape::NAMES.join(", ")))
        }),
    }
}

fn point_spec(p: &EnvPoint) -> PointSpec {
    PointSpec {
        time: p.time,
        value: p.value,
        shape: Some(p.shape.name().to_owned()),
        tension: match p.shape {
            PointShape::Bezier(t) => Some(t),
            _ => None,
        },
    }
}

fn point(p: &PointSpec) -> Result<EnvPoint, ApiError> {
    if !p.time.is_finite() || !p.value.is_finite() {
        return Err(ApiError::invalid("points need finite times and values"));
    }
    Ok(EnvPoint { shape: shape(p.shape.as_deref(), p.tension, PointShape::Linear)?, ..EnvPoint::new(p.time, p.value) })
}

fn lfo(spec: &LfoSpec) -> Result<Lfo, ApiError> {
    let shape = LfoShape::from_name(&spec.shape)
        .ok_or_else(|| ApiError::invalid("the LFO shape is sine, triangle, square, saw_up or saw_down"))?;
    if !(spec.cycles.is_finite() && spec.cycles > 0.0 && spec.cycles <= 1024.0) {
        return Err(ApiError::invalid("cycles run from above 0 to 1024"));
    }
    Ok(Lfo { shape, cycles: spec.cycles, phase: spec.phase.rem_euclid(1.0) })
}

/// The running effect's view of parameter `param`: name, range and current value.
pub fn param_info(
    session: &Session,
    target: Target,
    fx: usize,
    param: usize,
) -> Result<(String, f64, f64, f64), ApiError> {
    let t = target.get(&session.project).ok_or_else(|| ApiError::not_found("the track is gone"))?;
    let slot = t.fx.get(fx).ok_or_else(|| ApiError::not_found(format!("the track has no effect {fx}")))?;
    let (name, _) = super::modulation::param_state(session, target, fx, slot, param);
    let owner = match target {
        Target::Master => None,
        Target::Track(id) => Some(id),
    };
    let inst = session.engine.fx().instance(owner, slot, fx);
    let (lo, hi, v) = inst
        .and_then(|i| {
            i.with(|e| {
                let (lo, hi) = scythe_engine::modulation::native_range(e, param);
                let n = scythe_engine::modulation::normalized(e, param).unwrap_or(0.0);
                (lo, hi, lo + n * (hi - lo))
            })
        })
        .unwrap_or((0.0, 1.0, 0.0));
    Ok((format!("{name} / {}", slot.name), lo, hi, v))
}

/// A new envelope for parameter `param`, as REAPER writes it: over the parameter's range in its
/// own units, named by index and, for a CLAP plugin, its id, with the middle of the range as
/// its default. A CLAP plugin no instance runs (offline) is asked through one made for the
/// question, so the envelope is not stored on 0..1, which marks one from an older Scythe.
pub fn new_parameter_envelope(
    session: &Session,
    target: Target,
    fx: usize,
    param: usize,
) -> Result<ParamEnvelope, ApiError> {
    let (label, mut lo, mut hi, _) = param_info(session, target, fx, param)?;
    let t = target.get(&session.project).ok_or_else(|| ApiError::not_found("the track is gone"))?;
    let slot = t.fx.get(fx).ok_or_else(|| ApiError::not_found(format!("the track has no effect {fx}")))?;
    let owner = match target {
        Target::Master => None,
        Target::Track(id) => Some(id),
    };
    let registry = session.engine.fx();
    let params = match registry.instance(owner, slot, fx) {
        Some(inst) => inst.with(|e| e.params()),
        None => {
            let offline = scythe_engine::fx::offline_clap_params(slot, || registry.made_params(slot));
            if let Some(range) = offline.as_deref().and_then(|p| scythe_engine::fx::clap_param_range(p, param)) {
                (lo, hi) = range;
            }
            offline
        }
    };
    let mut pe = ParamEnvelope::new(param, lo, hi, (lo + hi) / 2.0, &label);
    pe.spec = scythe_engine::fx::envelope_spec(slot, params.as_deref(), param).unwrap_or(pe.spec);
    Ok(pe)
}

/// The envelope as it is, or a new empty one (not stored) when the track has none yet.
fn read(project: &Project, target: Target, at: &EnvRef) -> Result<Envelope, ApiError> {
    let t = target.get(project).ok_or_else(|| ApiError::not_found("the track is gone"))?;
    let mut env = match which(at)? {
        Which::Kind(kind) => t.envelope(kind).cloned().unwrap_or_else(|| Envelope::new(kind)),
        Which::Tempo => return Ok(tempo_envelope(project)),
        Which::Take(item, kind) => {
            let (_, it) = project.find_item(item).ok_or_else(|| ApiError::not_found(format!("no item {}", item.0)))?;
            let mut env = it
                .take()
                .and_then(|tk| tk.envelopes.iter().find(|e| e.kind == kind))
                .cloned()
                .unwrap_or_else(|| Envelope::new(kind));
            env.points.iter_mut().for_each(|p| p.time += it.position);
            return Ok(env);
        }
        Which::Send(s, kind) => {
            let send = t.sends.get(s).ok_or_else(|| ApiError::not_found(format!("the track has no send {s}")))?;
            send.envelopes.iter().find(|e| e.kind == kind).cloned().unwrap_or_else(|| Envelope::new(kind))
        }
        Which::Param(fx, param) => {
            let slot = t.fx.get(fx).ok_or_else(|| ApiError::not_found(format!("the track has no effect {fx}")))?;
            match slot.parameter_envelope(&param.to_string()) {
                Some(p) => p.envelope,
                None => ParamEnvelope::new(param, 0.0, 1.0, 0.0, "").envelope,
            }
        }
    };
    project.automation_context().resolve(&mut env);
    Ok(env)
}

/// Runs `f` on the envelope as one undoable edit, creating the envelope when missing.
fn edit<R>(
    session: &mut Session,
    at: &EnvRef,
    label: &str,
    f: impl FnOnce(&mut Envelope, &mut Project) -> Result<R, ApiError>,
) -> Result<R, ApiError> {
    change(session, at, label, false, f)
}

/// The envelope `at` names as it plays (automation items resolved), or a new empty one when
/// the track has none yet.
pub fn envelope(project: &Project, at: &EnvRef) -> Result<Envelope, ApiError> {
    read(project, at.track.resolve(project)?, at)
}

/// Runs `f` on the envelope `at` names (and the project, for its pools), creating it when
/// missing: as one undo step, or as part of the gesture under way (a drag) when `gesture`.
pub fn change<R>(
    session: &mut Session,
    at: &EnvRef,
    label: &str,
    gesture: bool,
    f: impl FnOnce(&mut Envelope, &mut Project) -> Result<R, ApiError>,
) -> Result<R, ApiError> {
    let target = at.track.resolve(&session.project)?;
    let which = which(at)?;
    let param = match which {
        Which::Param(fx, param) => Some(new_parameter_envelope(session, target, fx, param)?),
        _ => None,
    };
    let mut project = session.project.clone();
    let mut env = read(&project, target, at)?;
    let out = f(&mut env, &mut project)?;
    match which {
        Which::Tempo => apply_tempo(&mut project, &env),
        Which::Take(item, kind) => {
            let it = project
                .tracks
                .iter_mut()
                .find_map(|t| t.item_mut(item))
                .ok_or_else(|| ApiError::not_found(format!("no item {}", item.0)))?;
            let position = it.position;
            let active = it.active_take;
            let tk = it.takes.get_mut(active).ok_or_else(|| ApiError::not_found("the item has no take"))?;
            let mut env = env.clone();
            env.points.iter_mut().for_each(|p| p.time = (p.time - position).max(0.0));
            match tk.envelopes.iter_mut().find(|e| e.kind == kind) {
                Some(e) => *e = env,
                None => tk.envelopes.push(env),
            }
        }
        _ => {}
    }
    let t = target.get_mut(&mut project).ok_or_else(|| ApiError::not_found("the track is gone"))?;
    match which {
        Which::Tempo | Which::Take(..) => {}
        Which::Kind(kind) => {
            *t.envelope_mut(kind) = env;
        }
        Which::Send(s, kind) => {
            let send = t.sends.get_mut(s).ok_or_else(|| ApiError::not_found(format!("the track has no send {s}")))?;
            match send.envelopes.iter_mut().find(|e| e.kind == kind) {
                Some(e) => *e = env,
                None => send.envelopes.push(env),
            }
        }
        Which::Param(fx, p) => {
            let slot = t.fx.get_mut(fx).ok_or_else(|| ApiError::not_found(format!("the track has no effect {fx}")))?;
            let mut pe = slot
                .parameter_envelope(&p.to_string())
                .or(param)
                .unwrap_or_else(|| ParamEnvelope::new(p, 0.0, 1.0, 0.0, ""));
            pe.envelope = Envelope { range: Some((pe.min, pe.max)), ..env };
            slot.set_parameter_envelope(&pe);
        }
    }
    project.resolve_automation_items();
    if gesture {
        session.gesture(label, |p| *p = project);
    } else {
        session.edit(label, |p| *p = project);
    }
    Ok(out)
}

fn pooled_copies(project: &Project, pool: i64) -> usize {
    let in_fx = std::iter::once(&project.master)
        .chain(&project.tracks)
        .flat_map(|t| &t.fx)
        .flat_map(|s| s.parameter_envelopes())
        .flat_map(|p| p.envelope.items)
        .filter(|i| i.pool == pool)
        .count();
    let in_tracks = std::iter::once(&project.master)
        .chain(&project.tracks)
        .flat_map(|t| t.envelopes.iter().chain(t.sends.iter().flat_map(|s| &s.envelopes)))
        .flat_map(|e| &e.items)
        .filter(|i| i.pool == pool)
        .count();
    in_fx + in_tracks
}

fn info(project: &Project, at: &EnvRef, env: &Envelope, label: String) -> EnvelopeInfo {
    EnvelopeInfo {
        envelope: at.envelope.clone(),
        send: at.send,
        label,
        active: env.active,
        visible: env.visible,
        in_lane: env.in_lane,
        armed: env.armed,
        range: env.range.unwrap_or_else(|| env.kind.range()),
        fader_scaled: env.fader_scaled,
        points: env.points.iter().map(point_spec).collect(),
        items: env
            .items
            .iter()
            .enumerate()
            .map(|(index, i)| ItemInfo {
                index,
                pool: i.pool,
                position: i.position,
                length: i.length,
                start_offset: i.start_offset,
                rate: i.rate,
                baseline: i.baseline,
                amplitude: i.amplitude,
                looped: i.looped,
                transition: i.transition,
                selected: i.selected,
                pooled_copies: pooled_copies(project, i.pool).saturating_sub(1),
            })
            .collect(),
    }
}

fn get(session: &Session, at: &EnvRef) -> Result<Value, ApiError> {
    let target = at.track.resolve(&session.project)?;
    let env = read(&session.project, target, at)?;
    let label = match which(at)? {
        Which::Param(fx, param) => target
            .get(&session.project)
            .and_then(|t| t.fx.get(fx))
            .and_then(|s| s.parameter_envelope(&param.to_string()))
            .map(|p| p.label)
            .unwrap_or_default(),
        Which::Kind(k) | Which::Send(_, k) => k.label().to_owned(),
        Which::Take(_, k) => format!("Take {}", k.label().to_lowercase()),
        Which::Tempo => "Tempo map".to_owned(),
    };
    Ok(json(info(&session.project, at, &env, label)))
}

fn item_mut(env: &mut Envelope, index: usize) -> Result<&mut AutomationItem, ApiError> {
    let n = env.items.len();
    env.items
        .get_mut(index)
        .ok_or_else(|| ApiError::not_found(format!("the envelope has {n} automation item(s); no item {index}")))
}

fn new_pool(project: &mut Project, length_qn: f64, points: Vec<EnvPoint>, lfo: Option<Lfo>) -> i64 {
    let id = project.next_pool_id();
    project.automation_pools.push(AutomationPool { points, lfo, ..AutomationPool::new(id, length_qn) });
    id
}

pub fn execute(session: &mut Session, command: EnvelopeCommand) -> Result<Value, ApiError> {
    use EnvelopeCommand as C;
    match command {
        C::List { track } => {
            let target = track.resolve(&session.project)?;
            let t = target.get(&session.project).ok_or_else(|| ApiError::not_found("the track is gone"))?;
            let mut out = Vec::new();
            for e in &t.envelopes {
                let at = EnvRef { track: track.clone(), envelope: e.kind.name().into(), send: None, item: None };
                out.push(info(&session.project, &at, e, e.kind.label().into()));
            }
            for (s, send) in t.sends.iter().enumerate() {
                for e in &send.envelopes {
                    let at = EnvRef { track: track.clone(), envelope: e.kind.name().into(), send: Some(s), item: None };
                    out.push(info(&session.project, &at, e, format!("Send {} {}", s + 1, e.kind.label())));
                }
            }
            for (fx, slot) in t.fx.iter().enumerate() {
                for p in slot.parameter_envelopes() {
                    let Some(param) = p.param() else { continue };
                    let at =
                        EnvRef { track: track.clone(), envelope: format!("fx:{fx}:{param}"), send: None, item: None };
                    out.push(info(&session.project, &at, &p.envelope, p.label.clone()));
                }
            }
            Ok(json(out))
        }
        C::Get { at } => get(session, &at),
        C::Set { at, active, visible, in_lane, armed } => {
            edit(session, &at, "Envelope settings", |env, _| {
                if let Some(v) = active {
                    env.active = v;
                }
                if let Some(v) = visible {
                    env.visible = v;
                }
                if let Some(v) = in_lane {
                    env.in_lane = v;
                }
                if let Some(v) = armed {
                    env.armed = v;
                }
                Ok(())
            })?;
            get(session, &at)
        }
        C::Remove { at } => {
            let target = at.track.resolve(&session.project)?;
            let which = which(&at)?;
            session.edit("Remove envelope", |p| {
                match which {
                    Which::Tempo => p.tempo.points.clear(),
                    Which::Take(item, kind) => {
                        if let Some(tk) = p.tracks.iter_mut().find_map(|t| t.item_mut(item)).and_then(|i| {
                            let k = i.active_take;
                            i.takes.get_mut(k)
                        }) {
                            tk.envelopes.retain(|e| e.kind != kind);
                        }
                    }
                    _ => {}
                }
                let Some(t) = target.get_mut(p) else { return };
                match which {
                    Which::Tempo | Which::Take(..) => {}
                    Which::Kind(kind) => t.envelopes.retain(|e| e.kind != kind),
                    Which::Send(s, kind) => {
                        if let Some(send) = t.sends.get_mut(s) {
                            send.envelopes.retain(|e| e.kind != kind);
                        }
                    }
                    Which::Param(fx, param) => {
                        if let Some(slot) = t.fx.get_mut(fx) {
                            slot.remove_parameter_envelope(&param.to_string());
                        }
                    }
                }
            });
            Ok(done())
        }
        C::PointAdd { at, point: spec } => {
            let pt = point(&spec)?;
            time(pt.time, "time")?;
            edit(session, &at, "Add envelope point", |env, _| {
                env.points.retain(|p| (p.time - pt.time).abs() > 1e-9);
                env.points.push(pt);
                env.sort();
                Ok(())
            })?;
            get(session, &at)
        }
        C::PointEdit { at, index, time: t, value, shape: s, tension } => {
            edit(session, &at, "Edit envelope point", |env, _| {
                let n = env.points.len();
                let p = env
                    .points
                    .get_mut(index)
                    .ok_or_else(|| ApiError::not_found(format!("the envelope has {n} point(s); no point {index}")))?;
                if let Some(t) = t {
                    p.time = time(t, "time")?;
                }
                if let Some(v) = value.filter(|v| v.is_finite()) {
                    p.value = v;
                }
                p.shape = shape(s.as_deref(), tension, p.shape)?;
                env.sort();
                Ok(())
            })?;
            get(session, &at)
        }
        C::PointDelete { at, index } => {
            edit(session, &at, "Delete envelope point", |env, _| {
                if index >= env.points.len() {
                    return Err(ApiError::not_found(format!("no point {index}")));
                }
                env.points.remove(index);
                Ok(())
            })?;
            get(session, &at)
        }
        C::Shape { at, shape: s, tension, from, to } => {
            let new = shape(Some(&s), tension, PointShape::Linear)?;
            let (from, to) = (from.unwrap_or(f64::NEG_INFINITY), to.unwrap_or(f64::INFINITY));
            edit(session, &at, "Set envelope point shape", |env, _| {
                for p in env.points.iter_mut().filter(|p| p.time >= from && p.time <= to) {
                    p.shape = new;
                }
                Ok(())
            })?;
            get(session, &at)
        }
        C::Evaluate { at, time: t } => {
            let target = at.track.resolve(&session.project)?;
            let env = read(&session.project, target, &at)?;
            Ok(serde_json::json!({ "value": env.value_at(t) }))
        }
        C::ItemAdd { at, position, length, pool, lfo: spec } => {
            let position = time(position, "position")?;
            if !(length.is_finite() && length > 0.0) {
                return Err(ApiError::invalid("length must be above 0 seconds"));
            }
            if let Some(id) = pool
                && !session.project.automation_pools.iter().any(|p| p.id == id)
            {
                return Err(ApiError::not_found(format!("no automation item pool {id}")));
            }
            let lfo = spec.as_ref().map(lfo).transpose()?;
            edit(session, &at, "Insert automation item", |env, project| {
                let timeline = project.tempo.timeline();
                let id = match pool {
                    Some(id) => id,
                    None => {
                        let (length_qn, points) = capture(env, position, length, &timeline);
                        match lfo {
                            Some(l) => new_pool(project, length_qn, l.points(length_qn), Some(l)),
                            None => new_pool(project, length_qn, points, None),
                        }
                    }
                };
                env.items.push(AutomationItem::new(id, position, length));
                if env.points.is_empty() {
                    env.points.push(EnvPoint::new(0.0, env.range.map_or_else(|| env.kind.neutral(), |(lo, _)| lo)));
                }
                Ok(())
            })?;
            get(session, &at)
        }
        C::ItemEdit {
            at,
            index,
            position,
            length,
            start_offset,
            rate,
            baseline,
            amplitude,
            looped,
            transition,
            selected,
        } => {
            edit(session, &at, "Edit automation item", |env, _| {
                let i = item_mut(env, index)?;
                if let Some(v) = position {
                    i.position = time(v, "position")?;
                }
                if let Some(v) = length.filter(|v| v.is_finite() && *v > 0.0) {
                    i.length = v;
                }
                if let Some(v) = start_offset.filter(|v| v.is_finite()) {
                    i.start_offset = v;
                }
                if let Some(v) = rate {
                    if !(v.is_finite() && v > 0.0) {
                        return Err(ApiError::invalid("rate must be above 0"));
                    }
                    i.rate = v;
                }
                if let Some(v) = baseline {
                    i.baseline = v.clamp(0.0, 1.0);
                }
                if let Some(v) = amplitude {
                    i.amplitude = v.clamp(-1.0, 1.0);
                }
                if let Some(v) = looped {
                    i.looped = v;
                }
                if let Some(v) = transition {
                    i.transition = v.clamp(0.0, 10.0);
                }
                if let Some(v) = selected {
                    i.selected = v;
                }
                Ok(())
            })?;
            get(session, &at)
        }
        C::ItemDelete { at, index, keep_points } => {
            edit(session, &at, "Delete automation item", |env, _| {
                item_mut(env, index)?;
                if keep_points {
                    let item = env.items[index].clone();
                    let mut times: Vec<f64> = vec![item.position, item.end() - 1e-6];
                    times.extend(item.point_times());
                    let values: Vec<(f64, f64)> = times.iter().map(|&t| (t, env.value_at(t).unwrap_or(0.0))).collect();
                    env.points.retain(|p| p.time < item.position || p.time >= item.end());
                    env.points.extend(values.into_iter().map(|(t, v)| EnvPoint::new(t, v)));
                    env.sort();
                }
                env.items.remove(index);
                Ok(())
            })?;
            get(session, &at)
        }
        C::ItemDuplicate { at, index, pooled } => {
            edit(session, &at, "Duplicate automation item", |env, project| {
                let mut copy = item_mut(env, index)?.clone();
                copy.position = copy.end();
                copy.selected = false;
                copy.raw.clear();
                if !pooled {
                    let source = project.automation_pools.iter().find(|p| p.id == copy.pool).cloned();
                    let source = source.unwrap_or_else(|| AutomationPool::new(0, 0.0));
                    copy.pool = new_pool(project, source.length_qn, source.points, source.lfo);
                }
                env.items.push(copy);
                Ok(())
            })?;
            get(session, &at)
        }
        C::ItemUnpool { at, index } => {
            edit(session, &at, "Unpool automation item", |env, project| {
                let pool = item_mut(env, index)?.pool;
                let source = project
                    .automation_pools
                    .iter()
                    .find(|p| p.id == pool)
                    .cloned()
                    .unwrap_or_else(|| AutomationPool::new(0, 0.0));
                let id = new_pool(project, source.length_qn, source.points, source.lfo);
                env.items[index].pool = id;
                Ok(())
            })?;
            get(session, &at)
        }
        C::Pool { pool, points, lfo: spec, length_qn, name } => {
            let Some(current) = session.project.automation_pools.iter().find(|p| p.id == pool).cloned() else {
                return Err(ApiError::not_found(format!("no automation item pool {pool}")));
            };
            if points.is_some() || spec.is_some() || length_qn.is_some() || name.is_some() {
                let pts = points.as_ref().map(|p| p.iter().map(point).collect::<Result<Vec<_>, _>>()).transpose()?;
                let lfo = spec.as_ref().map(lfo).transpose()?;
                if let Some(l) = length_qn
                    && !(l.is_finite() && l > 0.0)
                {
                    return Err(ApiError::invalid("length_qn must be above 0"));
                }
                session.edit("Edit automation item pool", |p| {
                    let Some(pl) = p.automation_pools.iter_mut().find(|pl| pl.id == pool) else { return };
                    if let Some(l) = length_qn {
                        pl.length_qn = l;
                    }
                    if let Some(mut pts) = pts {
                        pts.sort_by(|a, b| a.time.total_cmp(&b.time));
                        pl.points = pts;
                        pl.lfo = None;
                    }
                    if let Some(l) = lfo {
                        pl.points = l.points(pl.length_qn);
                        pl.lfo = Some(l);
                    }
                    if let Some(n) = name {
                        pl.name = n;
                    }
                    p.resolve_automation_items();
                });
            }
            let pl = session.project.automation_pools.iter().find(|p| p.id == pool).unwrap_or(&current);
            Ok(serde_json::json!({
                "pool": pl.id,
                "name": pl.name,
                "length_qn": pl.length_qn,
                "lfo": pl.lfo.map(|l| serde_json::json!({ "shape": l.shape.name(), "cycles": l.cycles, "phase": l.phase })),
                "points": pl.points.iter().map(point_spec).collect::<Vec<_>>(),
                "items": pooled_copies(&session.project, pool),
            }))
        }
        C::Mode { track: Some(track), mode, send: Some(send) } => {
            let id = track.track(&session.project)?;
            if session.project.track(id).is_none_or(|t| send >= t.sends.len()) {
                return Err(ApiError::not_found(format!("the track has no send {send}")));
            }
            if let Some(m) = mode {
                let m = match m.as_str() {
                    "track" => None,
                    _ => Some(AutoMode::from_name(&m).ok_or_else(|| {
                        ApiError::invalid("the mode is track, trim_read, read, touch, write, latch or latch_preview")
                    })?),
                };
                session.edit("Send automation mode", |p| {
                    if let Some(s) = p.track_mut(id).and_then(|t| t.sends.get_mut(send)) {
                        s.set_auto_mode(m);
                    }
                });
            }
            let t = session.project.track(id).ok_or_else(|| ApiError::not_found("the track is gone"))?;
            let own = t.sends[send].auto_mode().map_or("track", |m| m.name());
            Ok(
                serde_json::json!({ "track": id.0, "send": send, "mode": own, "effective": session.project.send_auto_mode(t, send).name() }),
            )
        }
        C::Mode { send: Some(_), .. } => Err(ApiError::invalid("a send's mode needs its track")),
        C::Mode { track, mode, send: None } => {
            let mode = mode
                .map(|m| {
                    AutoMode::from_name(&m).ok_or_else(|| {
                        ApiError::invalid("the mode is trim_read, read, touch, write, latch or latch_preview")
                    })
                })
                .transpose()?;
            let ids = match &track {
                Some(t) => vec![t.track(&session.project)?],
                None => session.project.tracks.iter().map(|t| t.id).collect(),
            };
            if let Some(m) = mode {
                session.edit("Automation mode", |p| {
                    for id in &ids {
                        if let Some(t) = p.track_mut(*id) {
                            t.auto_mode = m;
                        }
                    }
                });
            }
            let modes: Vec<Value> = ids
                .iter()
                .filter_map(|id| session.project.track(*id))
                .map(|t| serde_json::json!({ "track": t.id.0, "mode": t.auto_mode.name(), "effective": session.project.auto_mode_of(t).name() }))
                .collect();
            Ok(json(modes))
        }
        C::LatchPreviewApply { span } => {
            let span = match span.as_str() {
                "time_selection" => PreviewSpan::TimeSelection,
                "to_end" => PreviewSpan::CursorToEnd,
                "to_start" => PreviewSpan::StartToCursor,
                _ => return Err(ApiError::invalid("the span is time_selection, to_end or to_start")),
            };
            Ok(serde_json::json!({ "written": session.apply_latch_preview(span) }))
        }
        C::GlobalOverride { mode } => {
            if let Some(m) = mode {
                let over = GlobalAutomation::from_name(&m).ok_or_else(|| {
                    ApiError::invalid(
                        "the override is none, trim_read, read, touch, write, latch, latch_preview or bypass",
                    )
                })?;
                session.edit("Global automation override", |p| p.global_automation = over);
            }
            Ok(serde_json::json!({ "mode": GlobalAutomation::name(session.project.global_automation) }))
        }
    }
}

/// The tempo map as an envelope, as REAPER shows it on the master track: a point per tempo
/// change (square, or linear when it ramps to the next), over 40-250 BPM or wider when the
/// map needs it.
fn tempo_envelope(project: &Project) -> Envelope {
    let tempo = &project.tempo;
    let mut env = Envelope::new(EnvelopeKind::Volume);
    env.points = if tempo.points.is_empty() {
        vec![EnvPoint { shape: PointShape::Square, ..EnvPoint::new(0.0, tempo.bpm) }]
    } else {
        tempo
            .points
            .iter()
            .map(|p| EnvPoint {
                shape: if p.linear { PointShape::Linear } else { PointShape::Square },
                ..EnvPoint::new(p.time, p.bpm)
            })
            .collect()
    };
    let lo = env.points.iter().map(|p| p.value).fold(40.0, f64::min);
    let hi = env.points.iter().map(|p| p.value).fold(250.0, f64::max);
    env.range = Some((lo, hi));
    env.fader_scaled = false;
    env
}

/// Makes the tempo map hold the envelope's points: their times and tempos, a linear shape
/// ramping; time signatures stay with points that stay at their time.
fn apply_tempo(project: &mut Project, env: &Envelope) {
    let old = std::mem::take(&mut project.tempo.points);
    let mut points: Vec<scythe_core::tempo::TempoPoint> = env
        .points
        .iter()
        .map(|p| {
            let kept = old.iter().find(|o| (o.time - p.time).abs() < 1e-6);
            scythe_core::tempo::TempoPoint {
                time: p.time.max(0.0),
                bpm: p.value.clamp(1.0, 960.0),
                linear: matches!(p.shape, PointShape::Linear),
                time_sig: kept.and_then(|o| o.time_sig),
                raw: kept.map(|o| o.raw.clone()).unwrap_or_default(),
            }
        })
        .collect();
    points.sort_by(|a, b| a.time.total_cmp(&b.time));
    if points.len() == 1 && points[0].time <= 0.0 && points[0].time_sig.is_none() && old.is_empty() {
        project.tempo.bpm = points[0].bpm;
        points.clear();
    }
    project.tempo.points = points;
    project.tempo.settle();
}
