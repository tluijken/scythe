//! Guitar rigs on the session: finding a track's rig, changing it as one, its four snapshots
//! (kept in the track's `<EXT` extension state, which REAPER keeps, under `scythe_rig:<FXID>`)
//! and named presets in Scythe's data folder that load on any track.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use scythe_core::rpp::{Chunk, Node, line_key, tokenize};
use scythe_core::{FxSlot, Track, TrackId};
use scythe_engine::fx::slot_id;
use scythe_engine::rig::{self, RigSettings, Stage};
use serde::{Deserialize, Serialize};

use crate::Session;
use crate::api::ApiError;

pub const SNAPSHOTS: usize = 4;
pub const SNAPSHOT_NAMES: [&str; SNAPSHOTS] = ["A", "B", "C", "D"];

/// What Scythe keeps about a rig beside its effects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RigExtras {
    pub snapshots: Vec<Option<RigSettings>>,
    /// The cab follows the amp model's gear type (see [`RigSettings::cab`]).
    pub cab_auto: bool,
    /// The preset last loaded, for stepping to the next one.
    pub preset: Option<String>,
}

impl Default for RigExtras {
    fn default() -> Self {
        Self { snapshots: Vec::new(), cab_auto: true, preset: None }
    }
}

impl RigExtras {
    pub fn snapshot(&self, n: usize) -> Option<&RigSettings> {
        self.snapshots.get(n).and_then(Option::as_ref)
    }

    pub fn set_snapshot(&mut self, n: usize, s: Option<RigSettings>) {
        if self.snapshots.len() <= n {
            self.snapshots.resize(n + 1, None);
        }
        self.snapshots[n] = s;
        while self.snapshots.last().is_some_and(Option::is_none) {
            self.snapshots.pop();
        }
    }

    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

fn ext_key(fx_id: &str) -> String {
    format!("scythe_rig:{fx_id}")
}

/// A value kept under `key` in the track's `<EXT` block.
pub fn ext_value(track: &Track, key: &str) -> Option<String> {
    track.extra.iter().find_map(|n| match n {
        Node::Chunk(c) if c.name() == "EXT" => c.children.iter().find_map(|n| match n {
            Node::Line(l) if line_key(l) == key => tokenize(l).get(1).cloned(),
            _ => None,
        }),
        _ => None,
    })
}

/// Keeps `value` under `key` in the track's `<EXT` block (removing it for `None`), leaving
/// other extensions' values alone.
pub fn set_ext_value(track: &mut Track, key: &str, value: Option<&str>) {
    let at = track.extra.iter().position(|n| matches!(n, Node::Chunk(c) if c.name() == "EXT"));
    let at = match (at, value) {
        (Some(at), _) => at,
        (None, None) => return,
        (None, Some(_)) => {
            track.extra.push(Node::Chunk(Chunk::new("EXT", &[])));
            track.extra.len() - 1
        }
    };
    let Node::Chunk(chunk) = &mut track.extra[at] else { return };
    chunk.children.retain(|n| !matches!(n, Node::Line(l) if line_key(l) == key));
    if let Some(v) = value {
        chunk.children.push(Node::Line(format!("{key} {v}")));
    }
    if chunk.children.is_empty() {
        track.extra.remove(at);
    }
}

pub fn read_extras(track: &Track, fx_id: &str) -> RigExtras {
    ext_value(track, &ext_key(fx_id))
        .and_then(|v| base64::engine::general_purpose::STANDARD.decode(v).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn write_extras(track: &mut Track, fx_id: &str, extras: &RigExtras) {
    let value = (!extras.is_default())
        .then(|| base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(extras).unwrap_or_default()));
    set_ext_value(track, &ext_key(fx_id), value.as_deref());
}

/// A rig on a track: the track and the rig's index in its chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RigAt {
    pub track: TrackId,
    pub index: usize,
}

/// The rigs of a track, by index in its chain.
pub fn rigs(track: &Track) -> Vec<usize> {
    track.fx.iter().enumerate().filter(|(_, s)| rig::is_rig(s)).map(|(i, _)| i).collect()
}

/// `$XDG_DATA_HOME/scythe/rigs`.
pub fn presets_dir() -> PathBuf {
    crate::data_dir().join("rigs")
}

/// A preset as its file holds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub settings: RigSettings,
}

fn preset_file(dir: &Path, name: &str) -> PathBuf {
    let safe: String =
        name.trim().chars().map(|c| if c.is_alphanumeric() || " -_+().,'&".contains(c) { c } else { '_' }).collect();
    dir.join(format!("{safe}.json"))
}

/// The presets in `dir`, sorted by name.
pub fn list_presets(dir: &Path) -> Vec<Preset> {
    let Ok(read) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<Preset> = read
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok())
        .collect();
    out.sort_by_key(|p: &Preset| p.name.to_lowercase());
    out
}

pub fn save_preset(dir: &Path, name: &str, settings: &RigSettings) -> std::io::Result<PathBuf> {
    let name = name.trim();
    if name.is_empty() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "a preset needs a name"));
    }
    std::fs::create_dir_all(dir)?;
    let path = preset_file(dir, name);
    let preset = Preset { name: name.to_owned(), settings: settings.clone() };
    std::fs::write(&path, serde_json::to_vec_pretty(&preset).map_err(std::io::Error::other)?)?;
    Ok(path)
}

pub fn find_preset(dir: &Path, name: &str) -> Option<Preset> {
    list_presets(dir).into_iter().find(|p| p.name.eq_ignore_ascii_case(name.trim()))
}

pub fn delete_preset(dir: &Path, name: &str) -> std::io::Result<()> {
    let p = find_preset(dir, name).ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
    std::fs::remove_file(preset_file(dir, &p.name))
}

/// The gear type a `.nam` file states, if any.
pub fn model_gear(path: &Path) -> Option<String> {
    scythe_engine::nam::NamInfo::read(path).ok()?.gear_type
}

/// `settings` with the cab switched as the amp model's gear wants, when the rig follows it.
pub fn auto_cab(mut settings: RigSettings, gear: Option<&str>) -> RigSettings {
    if settings.cab.auto
        && let Some(wanted) = gear.and_then(rig::cab_wanted)
    {
        settings.cab.enabled = wanted;
    }
    settings
}

impl Session {
    /// The rig `index` names on `track`, or the track's first rig.
    pub fn rig_at(&self, track: TrackId, index: Option<usize>) -> Result<RigAt, ApiError> {
        let t = self.project.track(track).ok_or_else(|| ApiError::not_found("no such track"))?;
        let found = match index {
            Some(i) => t.fx.get(i).filter(|s| rig::is_rig(s)).map(|_| i),
            None => rigs(t).first().copied(),
        };
        found.map(|index| RigAt { track, index }).ok_or_else(|| match index {
            Some(i) => ApiError::not_found(format!("effect {i} of {} is not a rig", t.name)),
            None => ApiError::not_found(format!("{} has no rig", t.name)),
        })
    }

    /// The rig the actions switch: the selected track's first, else the first in the project.
    pub fn focused_rig(&self) -> Option<RigAt> {
        if let Some(t) = self.selected_track
            && let Ok(at) = self.rig_at(t, None)
        {
            return Some(at);
        }
        self.project.tracks.iter().find_map(|t| rigs(t).first().map(|&index| RigAt { track: t.id, index }))
    }

    fn rig_slot(&self, at: RigAt) -> Option<&FxSlot> {
        self.project.track(at.track)?.fx.get(at.index)
    }

    pub fn rig_extras(&self, at: RigAt) -> RigExtras {
        let (Some(t), Some(id)) = (self.project.track(at.track), self.rig_slot(at).and_then(slot_id)) else {
            return RigExtras::default();
        };
        read_extras(t, &id)
    }

    /// The rig's settings, with whether its cab follows the amp.
    pub fn rig_settings(&self, at: RigAt) -> Option<RigSettings> {
        let mut s = rig::read(self.rig_slot(at)?)?;
        s.cab.auto = self.rig_extras(at).cab_auto;
        Some(s)
    }

    /// Makes the rig hold `settings`, as one undo step.
    pub fn apply_rig(&mut self, at: RigAt, label: &str, settings: &RigSettings) {
        self.apply_rig_with(at, label, settings, |_| {});
    }

    fn apply_rig_with(&mut self, at: RigAt, label: &str, settings: &RigSettings, extra: impl FnOnce(&mut RigExtras)) {
        let Some(id) = self.rig_slot(at).and_then(slot_id) else { return };
        let mut extras = self.rig_extras(at);
        extras.cab_auto = settings.cab.auto;
        extra(&mut extras);
        self.edit(label, |p| {
            let Some(t) = p.track_mut(at.track) else { return };
            if let Some(slot) = t.fx.get_mut(at.index) {
                rig::apply(slot, settings);
            }
            write_extras(t, &id, &extras);
        });
    }

    /// Adds a rig at the end of the track's chain; returns its index.
    pub fn add_rig(&mut self, track: TrackId, settings: &RigSettings) -> Result<usize, ApiError> {
        let t = self.project.track(track).ok_or_else(|| ApiError::not_found("no such track"))?;
        let index = t.fx.len();
        let slot = rig::new_rig(settings);
        let id = slot_id(&slot).unwrap_or_default();
        let extras = RigExtras { cab_auto: settings.cab.auto, ..RigExtras::default() };
        self.edit("Add guitar rig", |p| {
            if let Some(t) = p.track_mut(track) {
                t.fx.push(slot);
                write_extras(t, &id, &extras);
            }
        });
        Ok(index)
    }

    /// Wraps the NAM slot at `index` (and the gate, cab and EQ around it) in a rig.
    pub fn convert_to_rig(&mut self, track: TrackId, index: usize) -> Result<usize, ApiError> {
        let t = self.project.track(track).ok_or_else(|| ApiError::not_found("no such track"))?;
        let mut chain = t.fx.clone();
        let at = rig::convert(&mut chain, index)
            .ok_or_else(|| ApiError::invalid(format!("effect {index} is not a Neural Amp Modeler slot")))?;
        self.edit("Convert to guitar rig", |p| {
            if let Some(t) = p.track_mut(track) {
                t.fx = chain;
            }
        });
        Ok(at)
    }

    /// Points the rig's amp at `model`, switching the cab as the model's gear type wants when
    /// the rig follows it.
    pub fn set_rig_amp(&mut self, at: RigAt, model: &Path, gear: Option<&str>) {
        let Some(mut s) = self.rig_settings(at) else { return };
        s.amp.model = Some(model.display().to_string());
        let gear = gear.map(str::to_owned).or_else(|| model_gear(&self.resolve_path(model)));
        let s = auto_cab(s, gear.as_deref());
        self.apply_rig(at, "Load rig amp", &s);
    }

    fn resolve_path(&self, p: &Path) -> PathBuf {
        match (p.is_absolute(), self.project.path.as_deref().and_then(Path::parent)) {
            (false, Some(dir)) => dir.join(p),
            _ => p.to_path_buf(),
        }
    }

    /// Switches a stage on or off; switching the cab by hand stops it following the amp.
    pub fn set_rig_stage(&mut self, at: RigAt, stage: Stage, on: bool) {
        let Some(mut s) = self.rig_settings(at) else { return };
        match stage {
            Stage::Gate => s.gate.enabled = on,
            Stage::Amp => s.amp.enabled = on,
            Stage::Cab => {
                s.cab.enabled = on;
                s.cab.auto = false;
            }
            Stage::Eq => s.eq.enabled = on,
            Stage::Output => {}
        }
        self.apply_rig(at, &format!("{} {}", stage.label(), if on { "on" } else { "off" }), &s);
    }

    pub fn store_rig_snapshot(&mut self, at: RigAt, n: usize) {
        let Some(s) = self.rig_settings(at) else { return };
        let Some(id) = self.rig_slot(at).and_then(slot_id) else { return };
        let mut extras = self.rig_extras(at);
        extras.set_snapshot(n, Some(s));
        self.edit("Store rig snapshot", |p| {
            if let Some(t) = p.track_mut(at.track) {
                write_extras(t, &id, &extras);
            }
        });
    }

    pub fn clear_rig_snapshot(&mut self, at: RigAt, n: usize) {
        let Some(id) = self.rig_slot(at).and_then(slot_id) else { return };
        let mut extras = self.rig_extras(at);
        extras.set_snapshot(n, None);
        self.edit("Clear rig snapshot", |p| {
            if let Some(t) = p.track_mut(at.track) {
                write_extras(t, &id, &extras);
            }
        });
    }

    /// Switches the rig to snapshot `n`; false when it is empty.
    pub fn recall_rig_snapshot(&mut self, at: RigAt, n: usize) -> bool {
        let Some(s) = self.rig_extras(at).snapshot(n).cloned() else { return false };
        self.apply_rig(at, &format!("Rig snapshot {}", SNAPSHOT_NAMES.get(n).unwrap_or(&"?")), &s);
        true
    }

    /// Loads preset `name` into the rig.
    pub fn load_rig_preset(&mut self, at: RigAt, preset: &Preset) {
        let name = preset.name.clone();
        self.apply_rig_with(at, &format!("Load rig preset {name}"), &preset.settings, |x| x.preset = Some(name));
    }

    /// Loads the preset `by` places from the one last loaded, in name order.
    pub fn step_rig_preset(&mut self, at: RigAt, by: isize) -> Result<String, ApiError> {
        let presets = list_presets(&presets_dir());
        if presets.is_empty() {
            return Err(ApiError::not_found("there are no rig presets yet"));
        }
        let n = presets.len() as isize;
        let current = self.rig_extras(at).preset;
        let next = match current.and_then(|c| presets.iter().position(|p| p.name.eq_ignore_ascii_case(&c))) {
            Some(i) => (i as isize + by).rem_euclid(n),
            None if by < 0 => n - 1,
            None => 0,
        };
        let preset = presets[next as usize].clone();
        self.load_rig_preset(at, &preset);
        Ok(preset.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scythe_engine::rig::{CabSettings, RigSettings};

    #[test]
    fn extras_round_trip_beside_other_extensions() {
        let mut project = scythe_core::Project::default();
        let id = project.add_track("Guitar");
        let track = project.track_mut(id).unwrap();
        set_ext_value(track, "other_ext:key", Some("value"));
        let mut extras = RigExtras::default();
        let mut a = RigSettings::default();
        a.amp.model = Some("/m/a.nam".into());
        extras.set_snapshot(1, Some(a));
        extras.cab_auto = false;
        write_extras(track, "{RIG}", &extras);
        let text = scythe_core::rpp::project_to_rpp(&project);
        let back = scythe_core::rpp::project_from_rpp(&text).unwrap();
        let track = back.tracks.iter().find(|t| t.name == "Guitar").unwrap();
        assert_eq!(read_extras(track, "{RIG}"), extras);
        assert_eq!(ext_value(track, "other_ext:key").as_deref(), Some("value"));
        let mut track = track.clone();
        write_extras(&mut track, "{RIG}", &RigExtras::default());
        assert_eq!(read_extras(&track, "{RIG}"), RigExtras::default());
        assert_eq!(ext_value(&track, "other_ext:key").as_deref(), Some("value"));
    }

    #[test]
    fn presets_save_list_load_and_delete() {
        let dir = std::env::temp_dir().join(format!("scythe-rig-presets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = RigSettings::default();
        s.cab = CabSettings { ir: Some("/irs/v30.wav".into()), level_db: -4.0, ..CabSettings::default() };
        save_preset(&dir, "Lead / Solo", &s).unwrap();
        save_preset(&dir, "clean", &RigSettings::default()).unwrap();
        let names: Vec<String> = list_presets(&dir).into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["clean", "Lead / Solo"]);
        assert_eq!(find_preset(&dir, "lead / solo").unwrap().settings, s);
        assert!(save_preset(&dir, "  ", &s).is_err());
        delete_preset(&dir, "clean").unwrap();
        assert_eq!(list_presets(&dir).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_cab_follows_the_capture_until_switched_by_hand() {
        let s = RigSettings::default();
        assert!(!auto_cab(s.clone(), Some("amp_cab")).cab.enabled);
        assert!(auto_cab(s.clone(), Some("amp")).cab.enabled);
        assert!(auto_cab(s.clone(), Some("pedal")).cab.enabled, "a pedal leaves the cab as it is");
        let mut manual = s.clone();
        manual.cab.auto = false;
        assert!(auto_cab(manual, Some("full-rig")).cab.enabled);

        let pool = std::sync::Arc::new(scythe_engine::MediaPool::with_resample(
            scythe_core::render_settings::ResampleMode::PLAYBACK,
        ));
        let mut session = Session::new(scythe_engine::Engine::start_offline(pool, 48_000));
        let t = session.project.add_track("Guitar");
        let index = session.add_rig(t, &RigSettings::default()).unwrap();
        let at = session.rig_at(t, None).unwrap();
        assert_eq!(at.index, index);
        session.set_rig_amp(at, Path::new("/m/full.nam"), Some("full-rig"));
        let now = session.rig_settings(at).unwrap();
        assert!(!now.cab.enabled && now.cab.auto && now.amp.model.as_deref() == Some("/m/full.nam"));
        session.set_rig_amp(at, Path::new("/m/head.nam"), Some("amp"));
        assert!(session.rig_settings(at).unwrap().cab.enabled);
        session.set_rig_stage(at, Stage::Cab, false);
        assert!(!session.rig_settings(at).unwrap().cab.auto);
        session.set_rig_amp(at, Path::new("/m/head2.nam"), Some("amp"));
        assert!(!session.rig_settings(at).unwrap().cab.enabled, "the override holds");

        session.store_rig_snapshot(at, 0);
        let mut other = session.rig_settings(at).unwrap();
        other.gate.enabled = true;
        other.output_db = -3.0;
        session.apply_rig(at, "x", &other);
        session.store_rig_snapshot(at, 2);
        assert!(session.recall_rig_snapshot(at, 0));
        let back = session.rig_settings(at).unwrap();
        assert!(!back.gate.enabled && back.output_db == 0.0);
        assert!(session.recall_rig_snapshot(at, 2));
        assert_eq!(session.rig_settings(at).unwrap().output_db, -3.0);
        assert!(!session.recall_rig_snapshot(at, 3));
    }
}
