use std::path::Path;

use schemars::JsonSchema;
use scythe_engine::rig::{RigSettings, Stage};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{ApiError, TrackRef};
use crate::Session;
use crate::rig::{self as rigs, RigAt, SNAPSHOT_NAMES, SNAPSHOTS};

/// Guitar rigs: a gate, a Neural Amp Modeler amp, a cabinet impulse response (with an optional
/// second one to blend), an EQ with low and high cut and an output level, kept as one REAPER
/// FX container named "Rig: <amp> + <cab>". Rigs are addressed by track and, when a track has
/// more than one, by the rig's position in the track's effect chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum RigCommand {
    /// The rigs in the project (track, position, name, settings) and the saved presets.
    #[serde(rename = "rig.list")]
    List,
    /// One rig's settings and which of its snapshots A-D are stored.
    #[serde(rename = "rig.get")]
    Get {
        track: TrackRef,
        /// The rig's position in the track's effect chain; the track's first rig when left out.
        #[serde(default)]
        index: Option<usize>,
    },
    /// Adds a rig at the end of the track's chain, from a preset or with default settings.
    #[serde(rename = "rig.create")]
    Create {
        track: TrackRef,
        /// A preset's name.
        #[serde(default)]
        preset: Option<String>,
    },
    /// Turns the Neural Amp Modeler slot at `index` into a rig, together with a ReaGate right
    /// before it and the ReaVerb (impulse response) and ReaEQ right after it. The stages it
    /// lacks are added switched off, so the sound stays the same.
    #[serde(rename = "rig.convert")]
    Convert {
        track: TrackRef,
        /// Position of the NAM slot in the track's chain, from 0.
        index: usize,
    },
    /// Loads an amp model (a `.nam` file) and sets the amp's levels. A capture that includes a
    /// cabinet (gear type amp-cab or full rig) switches the cab stage off, an amp-only one
    /// switches it on, unless the cab was switched by hand.
    #[serde(rename = "rig.set_amp")]
    SetAmp {
        track: TrackRef,
        #[serde(default)]
        index: Option<usize>,
        /// Path of the `.nam` file.
        #[serde(default)]
        model: Option<String>,
        /// The capture's gear type when the file does not say (amp, amp-cab, full-rig, pedal).
        #[serde(default)]
        gear: Option<String>,
        #[serde(default)]
        input_db: Option<f64>,
        #[serde(default)]
        output_db: Option<f64>,
    },
    /// Sets the cabinet: its impulse response (a WAV file), a second one blended in, the blend,
    /// the level, whether it plays and whether it follows the amp model's gear type.
    #[serde(rename = "rig.set_cab")]
    SetCab {
        track: TrackRef,
        #[serde(default)]
        index: Option<usize>,
        /// Path of the impulse response.
        #[serde(default)]
        ir: Option<String>,
        /// Path of a second impulse response; an empty string removes it.
        #[serde(default)]
        ir2: Option<String>,
        /// The second response's share, 0-1.
        #[serde(default)]
        blend: Option<f64>,
        #[serde(default)]
        level_db: Option<f64>,
        /// Switches the cab stage on or off by hand (which stops it following the amp).
        #[serde(default)]
        enabled: Option<bool>,
        /// The cab follows the amp model: off for captures with a cabinet, on for amp-only ones.
        #[serde(default)]
        auto: Option<bool>,
    },
    /// Switches a stage on or off: gate, amp, cab or eq.
    #[serde(rename = "rig.stage")]
    Stage {
        track: TrackRef,
        #[serde(default)]
        index: Option<usize>,
        /// gate, amp, cab or eq.
        stage: String,
        enabled: bool,
    },
    /// Changes any settings at once: `settings` is merged into the rig's settings as `rig.get`
    /// shows them (gate, amp, cab, eq, output_db).
    #[serde(rename = "rig.set")]
    Set {
        track: TrackRef,
        #[serde(default)]
        index: Option<usize>,
        settings: Value,
    },
    /// Switches the rig to snapshot A-D (amp, impulse responses, gate, EQ and levels), or with
    /// `store` keeps the rig's current settings there. Without `track`, the selected track's
    /// rig.
    #[serde(rename = "rig.snapshot")]
    Snapshot {
        #[serde(default)]
        track: Option<TrackRef>,
        #[serde(default)]
        index: Option<usize>,
        /// A, B, C or D.
        snapshot: String,
        #[serde(default)]
        store: bool,
    },
    /// Tries an amp model (`model`, a `.nam` file) or a cab impulse response (`ir`, a WAV
    /// file) in the rig without making it final: the first try remembers the rig's settings,
    /// later ones in the same stage fold into one undo step. A model switches the cab as
    /// `rig.set_amp` does. End it with `rig.keep` or `rig.revert`.
    #[serde(rename = "rig.preview")]
    Preview {
        track: TrackRef,
        #[serde(default)]
        index: Option<usize>,
        /// Path of a `.nam` file to try in the amp.
        #[serde(default)]
        model: Option<String>,
        /// Path of an impulse response to try in the cab.
        #[serde(default)]
        ir: Option<String>,
    },
    /// Ends the running preview keeping what it plays.
    #[serde(rename = "rig.keep")]
    Keep,
    /// Ends the running preview, putting back the rig's settings from before it.
    #[serde(rename = "rig.revert")]
    Revert,
    /// The saved rig presets.
    #[serde(rename = "rig.presets")]
    Presets,
    /// Saves the rig's settings as a preset, replacing one of the same name.
    #[serde(rename = "rig.save_preset")]
    SavePreset {
        track: TrackRef,
        #[serde(default)]
        index: Option<usize>,
        name: String,
    },
    /// Loads a preset into the rig (a new rig when the track has none). Without `track`, the
    /// selected track's rig. `name` may also be `next` or `previous` to step through the
    /// presets in name order.
    #[serde(rename = "rig.load_preset")]
    LoadPreset {
        #[serde(default)]
        track: Option<TrackRef>,
        #[serde(default)]
        index: Option<usize>,
        name: String,
    },
    /// Deletes a saved preset.
    #[serde(rename = "rig.delete_preset")]
    DeletePreset { name: String },
}

fn at(session: &Session, track: &TrackRef, index: Option<usize>) -> Result<RigAt, ApiError> {
    session.rig_at(track.track(&session.project)?, index)
}

fn at_or_focused(session: &Session, track: &Option<TrackRef>, index: Option<usize>) -> Result<RigAt, ApiError> {
    match track {
        Some(t) => at(session, t, index),
        None => session.focused_rig().ok_or_else(|| ApiError::not_found("no rig on the selected track")),
    }
}

fn settings(session: &Session, at: RigAt) -> Result<RigSettings, ApiError> {
    session.rig_settings(at).ok_or_else(|| ApiError::not_found("not a rig"))
}

fn snapshot_index(name: &str) -> Result<usize, ApiError> {
    let n = name.trim();
    SNAPSHOT_NAMES
        .iter()
        .position(|s| s.eq_ignore_ascii_case(n))
        .or_else(|| n.parse::<usize>().ok().filter(|i| (1..=SNAPSHOTS).contains(i)).map(|i| i - 1))
        .ok_or_else(|| ApiError::invalid("snapshots are A, B, C and D"))
}

fn describe(session: &Session, at: RigAt) -> Value {
    let slot = session.project.track(at.track).and_then(|t| t.fx.get(at.index));
    let extras = session.rig_extras(at);
    json!({
        "track": at.track.0,
        "track_name": session.project.track(at.track).map(|t| t.name.clone()),
        "index": at.index,
        "name": slot.and_then(scythe_engine::rig::container_name),
        "settings": session.rig_settings(at),
        "snapshots": (0..SNAPSHOTS).map(|n| json!({"name": SNAPSHOT_NAMES[n], "stored": extras.snapshot(n).is_some()})).collect::<Vec<_>>(),
        "preset": extras.preset,
    })
}

/// Merges `patch` into `base` (objects key by key, anything else replaced).
fn merge(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                merge(b.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
        (b, p) => *b = p.clone(),
    }
}

fn preset_names() -> Vec<String> {
    rigs::list_presets(&rigs::presets_dir()).into_iter().map(|p| p.name).collect()
}

pub fn execute(session: &mut Session, command: RigCommand) -> Result<Value, ApiError> {
    match command {
        RigCommand::List => {
            let all: Vec<RigAt> = session
                .project
                .tracks
                .iter()
                .flat_map(|t| rigs::rigs(t).into_iter().map(|index| RigAt { track: t.id, index }))
                .collect();
            Ok(json!({
                "rigs": all.into_iter().map(|a| describe(session, a)).collect::<Vec<_>>(),
                "presets": preset_names(),
            }))
        }
        RigCommand::Get { track, index } => {
            let at = at(session, &track, index)?;
            Ok(describe(session, at))
        }
        RigCommand::Create { track, preset } => {
            let track = track.track(&session.project)?;
            let preset = match preset {
                Some(name) => Some(
                    rigs::find_preset(&rigs::presets_dir(), &name)
                        .ok_or_else(|| ApiError::not_found(format!("no rig preset \"{name}\"")))?,
                ),
                None => None,
            };
            let settings = preset.as_ref().map(|p| p.settings.clone()).unwrap_or_default();
            let index = session.add_rig(track, &settings)?;
            let at = RigAt { track, index };
            if let Some(p) = &preset {
                session.load_rig_preset(at, p);
            }
            Ok(describe(session, at))
        }
        RigCommand::Convert { track, index } => {
            let track = track.track(&session.project)?;
            let index = session.convert_to_rig(track, index)?;
            Ok(describe(session, RigAt { track, index }))
        }
        RigCommand::SetAmp { track, index, model, gear, input_db, output_db } => {
            let at = at(session, &track, index)?;
            if let Some(m) = &model {
                session.set_rig_amp(at, Path::new(m), gear.as_deref());
            }
            if input_db.is_some() || output_db.is_some() {
                let mut s = settings(session, at)?;
                s.amp.input_db = input_db.unwrap_or(s.amp.input_db).clamp(-40.0, 40.0);
                s.amp.output_db = output_db.unwrap_or(s.amp.output_db).clamp(-40.0, 40.0);
                session.apply_rig(at, "Rig amp levels", &s);
            }
            Ok(describe(session, at))
        }
        RigCommand::SetCab { track, index, ir, ir2, blend, level_db, enabled, auto } => {
            let at = at(session, &track, index)?;
            let mut s = settings(session, at)?;
            if let Some(ir) = ir {
                s.cab.ir = (!ir.is_empty()).then_some(ir);
            }
            if let Some(ir2) = ir2 {
                s.cab.ir2 = (!ir2.is_empty()).then_some(ir2);
            }
            if let Some(b) = blend {
                s.cab.blend = b.clamp(0.0, 1.0);
            }
            if let Some(l) = level_db {
                s.cab.level_db = l.clamp(-60.0, 24.0);
            }
            if let Some(on) = enabled {
                s.cab.enabled = on;
                s.cab.auto = false;
            }
            if let Some(a) = auto {
                s.cab.auto = a;
            }
            session.apply_rig(at, "Rig cab", &s);
            Ok(describe(session, at))
        }
        RigCommand::Stage { track, index, stage, enabled } => {
            let at = at(session, &track, index)?;
            let stage = Stage::parse(&stage)
                .filter(|s| *s != Stage::Output)
                .ok_or_else(|| ApiError::invalid("stages are gate, amp, cab and eq"))?;
            session.set_rig_stage(at, stage, enabled);
            Ok(describe(session, at))
        }
        RigCommand::Set { track, index, settings: patch } => {
            let at = at(session, &track, index)?;
            let mut value =
                serde_json::to_value(settings(session, at)?).map_err(|e| ApiError::invalid(e.to_string()))?;
            merge(&mut value, &patch);
            let s: RigSettings = serde_json::from_value(value).map_err(|e| ApiError::invalid(e.to_string()))?;
            session.apply_rig(at, "Rig settings", &s);
            Ok(describe(session, at))
        }
        RigCommand::Snapshot { track, index, snapshot, store } => {
            let at = at_or_focused(session, &track, index)?;
            let n = snapshot_index(&snapshot)?;
            if store {
                session.store_rig_snapshot(at, n);
            } else if !session.recall_rig_snapshot(at, n) {
                return Err(ApiError::not_found(format!("snapshot {} is empty", SNAPSHOT_NAMES[n])));
            }
            Ok(describe(session, at))
        }
        RigCommand::Preview { track, index, model, ir } => {
            let at = at(session, &track, index)?;
            let (path, is_ir) = match (model, ir) {
                (Some(m), None) => (m, false),
                (None, Some(i)) => (i, true),
                _ => return Err(ApiError::invalid("give either model or ir")),
            };
            let preview = session.preview_rig(at, Path::new(&path), is_ir).map(super::nam::describe)?;
            Ok(json!({ "preview": preview, "rig": describe(session, at) }))
        }
        RigCommand::Keep | RigCommand::Revert => {
            let p = session.end_preview(command == RigCommand::Keep)?;
            let rig = match p.slot {
                rigs::PreviewSlot::Amp(at) | rigs::PreviewSlot::Cab(at) => describe(session, at),
                rigs::PreviewSlot::Nam { .. } => Value::Null,
            };
            Ok(json!({ "preview": super::nam::describe(&p), "rig": rig }))
        }
        RigCommand::Presets => Ok(json!(preset_names())),
        RigCommand::SavePreset { track, index, name } => {
            let at = at(session, &track, index)?;
            let s = settings(session, at)?;
            rigs::save_preset(&rigs::presets_dir(), &name, &s).map_err(|e| ApiError::invalid(e.to_string()))?;
            Ok(json!({ "saved": name.trim(), "presets": preset_names() }))
        }
        RigCommand::LoadPreset { track, index, name } => {
            let at = match (&track, at_or_focused(session, &track, index)) {
                (_, Ok(at)) => at,
                (Some(t), Err(_)) if index.is_none() => {
                    let id = t.track(&session.project)?;
                    let index = session.add_rig(id, &RigSettings::default())?;
                    RigAt { track: id, index }
                }
                (_, Err(e)) => return Err(e),
            };
            match name.trim().to_ascii_lowercase().as_str() {
                "next" => {
                    session.step_rig_preset(at, 1)?;
                }
                "previous" => {
                    session.step_rig_preset(at, -1)?;
                }
                _ => {
                    let p = rigs::find_preset(&rigs::presets_dir(), &name)
                        .ok_or_else(|| ApiError::not_found(format!("no rig preset \"{name}\"")))?;
                    session.load_rig_preset(at, &p);
                }
            }
            Ok(describe(session, at))
        }
        RigCommand::DeletePreset { name } => {
            rigs::delete_preset(&rigs::presets_dir(), &name)
                .map_err(|_| ApiError::not_found(format!("no rig preset \"{name}\"")))?;
            Ok(json!(preset_names()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::execute_json;

    fn session() -> Session {
        let pool = std::sync::Arc::new(scythe_engine::MediaPool::with_resample(
            scythe_core::render_settings::ResampleMode::PLAYBACK,
        ));
        Session::new(scythe_engine::Engine::start_offline(pool, 48_000))
    }

    #[test]
    fn rigs_are_built_switched_and_snapshotted_over_the_api() {
        let mut s = session();
        let t = s.project.add_track("Guitar");
        s.edit("nam", |p| p.track_mut(t).unwrap().fx.push(scythe_engine::fx::nam_slot(None)));
        let made = execute_json(&mut s, json!({"cmd": "rig.convert", "track": "Guitar", "index": 0})).unwrap();
        assert_eq!(made["index"], 0);
        assert_eq!(made["settings"]["cab"]["enabled"], false);
        execute_json(&mut s, json!({"cmd": "rig.set_cab", "track": "Guitar", "ir": "/irs/v30.wav", "level_db": -6.0}))
            .unwrap();
        execute_json(&mut s, json!({"cmd": "rig.stage", "track": "Guitar", "stage": "cab", "enabled": true})).unwrap();
        let got = execute_json(&mut s, json!({"cmd": "rig.get", "track": "Guitar"})).unwrap();
        assert_eq!(got["name"], "Rig: v30");
        assert_eq!(got["settings"]["cab"]["auto"], false);
        execute_json(&mut s, json!({"cmd": "rig.snapshot", "track": "Guitar", "snapshot": "B", "store": true}))
            .unwrap();
        execute_json(
            &mut s,
            json!({"cmd": "rig.set", "track": "Guitar", "settings": {"output_db": -4.0, "gate": {"enabled": true}}}),
        )
        .unwrap();
        let changed = execute_json(&mut s, json!({"cmd": "rig.get", "track": "Guitar"})).unwrap();
        assert_eq!(
            (changed["settings"]["output_db"].as_f64(), changed["settings"]["gate"]["enabled"].as_bool()),
            (Some(-4.0), Some(true))
        );
        s.selected_track = Some(t);
        crate::actions::run(&mut s, "_SCYTHE_RIG_SNAPSHOT_B").unwrap();
        let back = execute_json(&mut s, json!({"cmd": "rig.get", "track": "Guitar"})).unwrap();
        assert_eq!(
            (back["settings"]["output_db"].as_f64(), back["settings"]["gate"]["enabled"].as_bool()),
            (Some(0.0), Some(false))
        );
        assert!(crate::actions::run(&mut s, "_SCYTHE_RIG_SNAPSHOT_C").is_err(), "an empty snapshot");
        let list = execute_json(&mut s, json!({"cmd": "rig.list"})).unwrap();
        assert_eq!(list["rigs"].as_array().unwrap().len(), 1);
        assert!(
            execute_json(&mut s, json!({"cmd": "rig.stage", "track": "Guitar", "stage": "reverb", "enabled": true}))
                .is_err()
        );
    }

    #[test]
    fn previews_fold_into_one_step_and_keep_or_revert() {
        let mut s = session();
        let t = s.project.add_track("Guitar");
        s.edit("nam", |p| p.track_mut(t).unwrap().fx.push(scythe_engine::fx::nam_slot(None)));
        execute_json(&mut s, json!({"cmd": "rig.convert", "track": "Guitar", "index": 0})).unwrap();
        execute_json(&mut s, json!({"cmd": "rig.set_cab", "track": "Guitar", "ir": "/irs/v30.wav"})).unwrap();
        let steps = s.history.depth();
        let amp = |s: &Session| s.rig_settings(s.rig_at(t, None).unwrap()).unwrap().amp.model;
        let cab = |s: &Session| s.rig_settings(s.rig_at(t, None).unwrap()).unwrap().cab.ir;
        for m in ["/m/a.nam", "/m/b.nam", "/m/c.nam"] {
            let r = execute_json(&mut s, json!({"cmd": "rig.preview", "track": "Guitar", "model": m})).unwrap();
            assert_eq!((r["preview"]["slot"].as_str(), r["preview"]["path"].as_str()), (Some("rig_amp"), Some(m)));
        }
        assert_eq!(amp(&s).as_deref(), Some("/m/c.nam"));
        assert_eq!(s.history.depth(), steps + 1, "every try is one undo step");
        let state = execute_json(&mut s, json!({"cmd": "nam.previewing"})).unwrap();
        assert_eq!(state["before"]["rig"]["amp"]["model"], Value::Null);
        execute_json(&mut s, json!({"cmd": "rig.revert"})).unwrap();
        assert_eq!(amp(&s), None);
        assert_eq!(s.history.depth(), steps);
        assert!(execute_json(&mut s, json!({"cmd": "rig.keep"})).is_err(), "nothing previews any more");

        execute_json(&mut s, json!({"cmd": "rig.preview", "track": "Guitar", "ir": "/irs/greenback.wav"})).unwrap();
        assert_eq!(cab(&s).as_deref(), Some("/irs/greenback.wav"));
        execute_json(&mut s, json!({"cmd": "rig.keep"})).unwrap();
        assert_eq!(cab(&s).as_deref(), Some("/irs/greenback.wav"));
        assert_eq!(execute_json(&mut s, json!({"cmd": "nam.previewing"})).unwrap(), Value::Null);
        // Reverting after another edit puts the old response back as a step of its own.
        execute_json(&mut s, json!({"cmd": "rig.preview", "track": "Guitar", "ir": "/irs/other.wav"})).unwrap();
        execute_json(&mut s, json!({"cmd": "rig.set", "track": "Guitar", "settings": {"output_db": -2.0}})).unwrap();
        execute_json(&mut s, json!({"cmd": "rig.revert"})).unwrap();
        assert_eq!(cab(&s).as_deref(), Some("/irs/greenback.wav"));
        assert!(
            execute_json(&mut s, json!({"cmd": "rig.preview", "track": "Guitar", "model": "/m/a.nam", "ir": "/i.wav"}))
                .is_err()
        );

        let u = s.project.add_track("Bass");
        s.edit("nam", |p| p.track_mut(u).unwrap().fx.push(scythe_engine::fx::nam_slot(Some(Path::new("/m/old.nam")))));
        let model =
            |s: &Session| scythe_engine::fx::nam_settings(&s.project.track(u).unwrap().fx[0]).and_then(|n| n.model);
        for m in ["/m/x.nam", "/m/y.nam"] {
            execute_json(&mut s, json!({"cmd": "nam.preview", "track": "Bass", "index": 0, "model": m})).unwrap();
        }
        assert_eq!(model(&s).as_deref(), Some("/m/y.nam"));
        let ended = execute_json(&mut s, json!({"cmd": "nam.revert"})).unwrap();
        assert_eq!((ended["slot"].as_str(), ended["before"]["model"].as_str()), (Some("nam"), Some("/m/old.nam")));
        assert_eq!(model(&s).as_deref(), Some("/m/old.nam"));
        execute_json(&mut s, json!({"cmd": "nam.preview", "track": "Bass", "index": 0, "model": "/m/x.nam"})).unwrap();
        execute_json(&mut s, json!({"cmd": "nam.keep"})).unwrap();
        assert_eq!(model(&s).as_deref(), Some("/m/x.nam"));
        let not_nam =
            execute_json(&mut s, json!({"cmd": "nam.preview", "track": "Guitar", "index": 0, "model": "/m/x.nam"}));
        assert!(not_nam.is_err(), "a rig is not a NAM slot");
    }
}
