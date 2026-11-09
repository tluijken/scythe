use schemars::JsonSchema;
use scythe_core::FxSlot;
use scythe_engine::builtin::Builtin;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ApiError, Target, TrackRef, json, snapshot};
use crate::Session;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum FxCommand {
    /// The effects on `track` (`"master"` for the master).
    #[serde(rename = "fx.list")]
    List { track: TrackRef },
    /// Effects that `fx.add` can add: the built-in ones and the installed CLAP, LV2 and VST 3
    /// plugins.
    #[serde(rename = "fx.catalog")]
    Catalog {
        /// Only entries whose name, id or format contain this, ignoring case.
        #[serde(default)]
        query: Option<String>,
    },
    /// Adds the effect `plugin` names (a catalog `id` or exact `name`) at `index`, at the end
    /// of the chain when left out.
    #[serde(rename = "fx.add")]
    Add {
        track: TrackRef,
        /// A catalog id such as `builtin:compressor`, or an exact name.
        plugin: String,
        /// Position in the chain, from 0.
        #[serde(default)]
        index: Option<usize>,
    },
    /// Removes the effect at `index` in the chain.
    #[serde(rename = "fx.remove")]
    Remove {
        track: TrackRef,
        /// Position in the chain, from 0.
        index: usize,
    },
    /// Bypasses the effect, or lets it process again; toggles when `bypassed` is left out.
    #[serde(rename = "fx.bypass")]
    Bypass {
        track: TrackRef,
        /// Position in the chain, from 0.
        index: usize,
        #[serde(default)]
        bypassed: Option<bool>,
    },
    /// The parameters of a running effect with their values.
    #[serde(rename = "fx.params")]
    Params {
        track: TrackRef,
        /// Position in the chain, from 0.
        index: usize,
    },
    /// Sets parameter `param` (an id from `fx.params`) to `value`, within its range.
    #[serde(rename = "fx.param")]
    SetParam {
        track: TrackRef,
        /// Position in the chain, from 0.
        index: usize,
        /// A parameter id from `fx.params`.
        param: u32,
        /// Clamped to the parameter's range.
        value: f64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// What `fx.add` takes: `builtin:<name>`, `clap:<id>`, `lv2:<uri>` or `vst3:<name>`.
    pub id: String,
    pub name: String,
    pub format: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamValue {
    pub id: u32,
    pub name: String,
    pub value: f64,
    /// The value as the plugin shows it.
    pub text: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
}

fn catalog(session: &Session) -> Vec<(CatalogEntry, FxSlot)> {
    let fx = session.engine.fx();
    let mut out: Vec<(CatalogEntry, FxSlot)> = Builtin::ALL
        .iter()
        .map(|b| {
            let entry = CatalogEntry {
                id: format!("builtin:{}", b.label().to_lowercase().replace(' ', "-")),
                name: b.label().into(),
                format: "built in".into(),
            };
            (entry, b.slot())
        })
        .collect();
    out.extend(fx.clap_plugins().iter().map(|d| {
        let entry = CatalogEntry { id: format!("clap:{}", d.id), name: d.name.clone(), format: "CLAP".into() };
        (entry, scythe_engine::fx::clap_slot(d))
    }));
    out.extend(fx.lv2_plugins().iter().map(|i| {
        let entry = CatalogEntry { id: format!("lv2:{}", i.uri), name: i.name.clone(), format: "LV2".into() };
        (entry, scythe_engine::fx::lv2_slot(i))
    }));
    out.extend(fx.vst3_plugins().iter().map(|i| {
        let entry = CatalogEntry { id: format!("vst3:{}", i.name), name: i.name.clone(), format: "VST3".into() };
        (entry, scythe_engine::fx::vst3_slot(i))
    }));
    out
}

/// The built-in effects are found without scanning for plugins, which can take a while.
fn find_plugin(session: &Session, wanted: &str) -> Option<FxSlot> {
    let builtin = Builtin::ALL.iter().find(|b| {
        let id = format!("builtin:{}", b.label().to_lowercase().replace(' ', "-"));
        id == wanted || b.label().eq_ignore_ascii_case(wanted)
    });
    if let Some(b) = builtin {
        return Some(b.slot());
    }
    let all = catalog(session);
    let found = all.iter().find(|(e, _)| e.id == wanted).or_else(|| all.iter().find(|(e, _)| e.name == wanted));
    found.map(|(_, slot)| slot.clone())
}

fn list(session: &Session, target: Target) -> Value {
    target.get(&session.project).map_or(Value::Null, |t| json(snapshot::fx_list(session, target, t)))
}

fn slot(session: &Session, target: Target, index: usize) -> Result<FxSlot, ApiError> {
    let t = target.get(&session.project).ok_or_else(|| ApiError::not_found("the track is gone"))?;
    t.fx.get(index).cloned().ok_or_else(|| {
        ApiError::not_found(format!("the track has {} effect(s); there is no effect {index}", t.fx.len()))
    })
}

fn instance(
    session: &Session,
    target: Target,
    index: usize,
) -> Result<std::sync::Arc<scythe_engine::fx::FxInstance>, ApiError> {
    let slot = slot(session, target, index)?;
    let track = match target {
        Target::Master => None,
        Target::Track(id) => Some(id),
    };
    session
        .engine
        .fx()
        .instance(track, &slot, index)
        .ok_or_else(|| ApiError::failed(format!("{} is not running", slot.name)))
}

fn params(session: &Session, target: Target, index: usize) -> Result<Vec<ParamValue>, ApiError> {
    let inst = instance(session, target, index)?;
    let values = inst
        .with(|e| {
            e.params()
                .into_iter()
                .map(|p| {
                    let (value, text) = e.param(p.id).unwrap_or((p.default, String::new()));
                    ParamValue { id: p.id, name: p.name, value, text, min: p.min, max: p.max, default: p.default }
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(values)
}

pub fn execute(session: &mut Session, command: FxCommand) -> Result<Value, ApiError> {
    match command {
        FxCommand::List { track } => {
            let target = track.resolve(&session.project)?;
            Ok(list(session, target))
        }
        FxCommand::Catalog { query } => {
            let query = query.map(|q| q.to_lowercase());
            let matches = |e: &CatalogEntry| {
                query
                    .as_deref()
                    .is_none_or(|q| [&e.name, &e.id, &e.format].iter().any(|field| field.to_lowercase().contains(q)))
            };
            Ok(json(catalog(session).into_iter().map(|(e, _)| e).filter(matches).collect::<Vec<_>>()))
        }
        FxCommand::Add { track, plugin, index } => {
            let target = track.resolve(&session.project)?;
            let count = target.get(&session.project).map_or(0, |t| t.fx.len());
            if index.is_some_and(|i| i > count) {
                return Err(ApiError::invalid(format!("index runs from 0 to {count}")));
            }
            let slot = find_plugin(session, &plugin)
                .ok_or_else(|| ApiError::not_found(format!("no effect \"{plugin}\"; see fx.catalog")))?;
            session.edit("Add plugin", |p| {
                if let Some(t) = target.get_mut(p) {
                    t.fx.insert(index.unwrap_or(t.fx.len()), slot);
                }
            });
            Ok(list(session, target))
        }
        FxCommand::Remove { track, index } => {
            let target = track.resolve(&session.project)?;
            slot(session, target, index)?;
            session.edit("Remove plugin", |p| {
                if let Some(t) = target.get_mut(p) {
                    t.fx.remove(index);
                }
            });
            Ok(list(session, target))
        }
        FxCommand::Bypass { track, index, bypassed } => {
            let target = track.resolve(&session.project)?;
            let current = slot(session, target, index)?;
            let enabled = !bypassed.unwrap_or(current.enabled);
            session.edit("Toggle FX bypass", |p| {
                if let Some(s) = target.get_mut(p).and_then(|t| t.fx.get_mut(index)) {
                    s.enabled = enabled;
                }
            });
            Ok(list(session, target))
        }
        FxCommand::Params { track, index } => {
            let target = track.resolve(&session.project)?;
            Ok(json(params(session, target, index)?))
        }
        FxCommand::SetParam { track, index, param, value } => {
            let target = track.resolve(&session.project)?;
            let inst = instance(session, target, index)?;
            let info = inst.with(|e| e.params().into_iter().find(|p| p.id == param)).flatten();
            let Some(info) = info else { return Err(ApiError::not_found(format!("no parameter {param}"))) };
            if !value.is_finite() {
                return Err(ApiError::invalid("the value must be a number"));
            }
            inst.with(|e| e.set_param(param, value.clamp(info.min, info.max)));
            // Plugin state is not part of the undoable model; it reaches the project on save.
            session.engine.wake_effects();
            session.dirty = true;
            let values = params(session, target, index)?;
            Ok(values.into_iter().find(|p| p.id == param).map_or(Value::Null, json))
        }
    }
}
