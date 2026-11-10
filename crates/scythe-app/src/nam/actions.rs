//! What the NAM panel, the shortcuts and dropped files do to the project's NAM slots, on their
//! own or as the amp of a guitar rig.

use std::path::{Path, PathBuf};

use scythe_core::{FxSlot, Project, TrackId};
use scythe_engine::builtin::{Builtin, reverb_state};
use scythe_engine::fx::{
    FxOwner, FxStatus, clear_nam_model, is_nam, nam_settings, set_nam_levels, set_nam_model, slot_id,
};
use scythe_engine::rig::{self, RigSettings, Stage};
use scythe_session::rig::{RigAt, auto_cab};

use super::library::{Filter, Kind};
use super::preview::Ending;
use super::snapshots::{self, Snapshot};
use super::tone3000::{Model, Tone};
use super::{Outcome, Purpose, Target, is_model_file};
use crate::app::ScytheApp;

/// Changes the NAM slot at `path` of the track's chain (inside its rig when the path says so).
fn edit_slot(p: &mut Project, track: TrackId, path: &[usize], f: impl FnOnce(&mut FxSlot)) {
    if let Some(t) = p.track_mut(track) {
        rig::edit_at(&mut t.fx, path, f);
    }
}

impl ScytheApp {
    /// The NAM slot at `index` of the track's chain, or the amp of the rig there.
    pub fn nam_target(&self, track: TrackId, index: usize) -> Option<Target> {
        let slot = self.project.track(track)?.fx.get(index)?;
        if is_nam(slot) {
            return Some(Target { track, fx_id: slot_id(slot)? });
        }
        if rig::is_rig(slot) {
            let children = slot.container_children();
            let amp = rig::layout(&children)?.amp;
            return Some(Target { track, fx_id: slot_id(&children[amp])? });
        }
        None
    }

    /// Where the slot sits: its index in the track's chain, or its rig's and its own inside.
    pub fn nam_path(&self, target: &Target) -> Option<Vec<usize>> {
        rig::find_nam(&self.project.track(target.track)?.fx, &target.fx_id)
    }

    /// The index in the track's chain of the slot, or of the rig holding it.
    pub fn nam_index(&self, target: &Target) -> Option<usize> {
        self.nam_path(target).map(|p| p[0])
    }

    pub fn nam_slot(&self, target: &Target) -> Option<FxSlot> {
        rig::slot_at(&self.project.track(target.track)?.fx, &self.nam_path(target)?)
    }

    /// The rig the slot is the amp of.
    pub fn nam_rig(&self, target: &Target) -> Option<RigAt> {
        let path = self.nam_path(target)?;
        (path.len() == 2).then(|| RigAt { track: target.track, index: path[0] })
    }

    pub fn nam_status(&self, target: &Target) -> Option<FxStatus> {
        let fx = &self.project.track(target.track)?.fx;
        self.engine.fx().status_at(&FxOwner::Track(target.track), fx, &self.nam_path(target)?).cloned()
    }

    /// The model a slot plays, with a path relative to the project resolved.
    pub fn nam_model_path(&self, target: &Target) -> Option<PathBuf> {
        let model = nam_settings(&self.nam_slot(target)?)?.model?;
        Some(self.nam_resolve(&model))
    }

    /// The slot the NAM shortcuts act on: the one in the panel, else the selected track's first
    /// NAM slot or rig.
    pub fn nam_focus(&self) -> Option<Target> {
        if let Some(t) = self.nam.panel.clone().filter(|t| self.nam_path(t).is_some()) {
            return Some(t);
        }
        let track = self.sel.track?;
        let index = self.project.track(track)?.fx.iter().position(|s| is_nam(s) || rig::is_rig(s))?;
        self.nam_target(track, index)
    }

    pub fn toggle_nam_panel(&mut self, track: TrackId, index: usize) {
        let target = self.nam_target(track, index);
        if target.is_some() && self.nam.panel == target {
            self.nam.panel = None;
            return;
        }
        if let Some(t) = &target
            && self.nam_rig(t).is_some()
            && self.nam.panel.as_ref() != Some(t)
        {
            self.nam.section = Stage::Amp;
        }
        self.nam.panel = target;
        self.nam.ensure_scanned();
    }

    /// Adds an empty NAM slot to the end of the track's chain and opens it in the panel.
    pub fn add_nam_slot(&mut self, track: TrackId) {
        let slot = scythe_engine::fx::nam_slot(None);
        self.edit("Add Neural Amp Modeler", |p| p.track_mut(track).unwrap().fx.push(slot));
        if let Some(index) = self.project.track(track).map(|t| t.fx.len() - 1) {
            self.toggle_nam_panel(track, index);
        }
    }

    /// Adds a guitar rig to the end of the track's chain and opens it.
    pub fn add_guitar_rig(&mut self, track: TrackId) {
        match self.session.add_rig(track, &RigSettings::default()) {
            Ok(index) => {
                self.toggle_nam_panel(track, index);
                self.nam.section = Stage::Amp;
            }
            Err(e) => self.notify(e.to_string()),
        }
    }

    /// Turns the slot's NAM (with the ReaVerb after it) into a rig, keeping the panel on it.
    pub fn nam_make_rig(&mut self, target: &Target) {
        let Some(path) = self.nam_path(target).filter(|p| p.len() == 1) else { return };
        match self.session.convert_to_rig(target.track, path[0]) {
            Ok(_) => {
                self.nam.panel = Some(target.clone());
                self.nam.section = Stage::Amp;
                self.notify("Made a guitar rig: gate, amp, cab, EQ and output in one slot");
            }
            Err(e) => self.notify(e.to_string()),
        }
    }

    /// Loads a model into the slot (read in the background, then crossfaded in) or, for an
    /// impulse response, into the cab: the rig's, or the convolution slot after a bare NAM.
    pub fn nam_open(&mut self, target: &Target, path: PathBuf) {
        if is_model_file(&path) {
            self.nam.load_model(target.clone(), path, None);
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav")) {
            self.nam_load_ir(target, &path);
        } else {
            self.notify(format!("{} is neither a NAM model nor an impulse response", path.display()));
        }
    }

    /// Puts an impulse response in the rig's cab (switching it on), or in the convolution
    /// slot right after a bare NAM slot, adding one (a ReaVerb, fully wet like a cabinet) when
    /// there is none.
    pub fn nam_load_ir(&mut self, target: &Target, path: &Path) {
        if let Some(at) = self.nam_rig(target) {
            self.rig_set_ir(at, path, false);
            return;
        }
        let Some(index) = self.nam_index(target) else { return };
        let slot = Builtin::Reverb.slot_with(&reverb_state(Some(path), 1.0, 0.0));
        let track = target.track;
        self.edit("Load impulse response", |p| {
            let fx = &mut p.track_mut(track).unwrap().fx;
            match fx.get(index + 1) {
                Some(next) if next.name.contains("ReaVerb") => fx[index + 1] = slot,
                _ => fx.insert(index + 1, slot),
            }
        });
        self.nam.library.touch(path);
        self.nam.library_dirty = true;
    }

    /// Loads `path` as the rig's first (or `second`) impulse response. A cab the amp model
    /// switched off comes on, and then stays as set by hand.
    pub fn rig_set_ir(&mut self, at: RigAt, path: &Path, second: bool) {
        let Some(mut s) = self.rig_settings(at) else { return };
        let ir = Some(path.display().to_string());
        if second {
            s.cab.ir2 = ir;
            if s.cab.blend == 0.0 {
                s.cab.blend = 0.5;
            }
        } else {
            s.cab.ir = ir;
        }
        if !s.cab.enabled {
            s.cab.enabled = true;
            s.cab.auto = false;
        }
        self.apply_rig(at, "Load cab impulse response", &s);
        self.engine.wake_effects();
        self.nam.library.touch(path);
        self.nam.library_dirty = true;
    }

    /// Changes the rig's settings; `gesture` folds a knob drag into one undo step.
    pub fn rig_change(&mut self, at: RigAt, label: &str, gesture: bool, f: impl FnOnce(&mut RigSettings)) {
        let Some(mut s) = self.rig_settings(at) else { return };
        f(&mut s);
        if gesture {
            self.gesture(label, |p| {
                if let Some(slot) = p.track_mut(at.track).and_then(|t| t.fx.get_mut(at.index)) {
                    rig::apply(slot, &s);
                }
            });
        } else {
            self.apply_rig(at, label, &s);
        }
        self.engine.wake_effects();
    }

    /// The model `step` places away from the current one among those the library browser shows.
    pub fn nam_step(&mut self, step: isize) {
        let Some(target) = self.nam_focus() else {
            self.notify("Select a track with a Neural Amp Modeler slot first");
            return;
        };
        if self.nam.preview.as_ref().is_some_and(|p| p.target == target) {
            self.nam.step_preview(step);
            return;
        }
        self.nam.ensure_scanned();
        let current = self.nam.loading.get(&target).cloned().or_else(|| self.nam_model_path(&target));
        let filter = Filter { kind: Some(Kind::Model), ..self.nam.filter.clone() };
        let next = self.nam.library.neighbour(current.as_deref(), step, &filter).map(|e| e.path.clone());
        match next {
            Some(path) => self.nam_open(&target, path),
            None => self.notify("No models in the library (add a folder in the NAM panel)"),
        }
    }

    pub fn nam_snapshots(&self, target: &Target) -> snapshots::SlotExtras {
        self.project.track(target.track).map(|t| snapshots::read(t, &target.fx_id)).unwrap_or_default()
    }

    /// Stores the slot's model and levels (a rig's whole settings) as snapshot `n`.
    pub fn nam_store_snapshot(&mut self, target: &Target, n: usize) {
        if let Some(at) = self.nam_rig(target) {
            self.store_rig_snapshot(at, n);
            self.notify(format!("Stored rig snapshot {}", scythe_session::rig::SNAPSHOT_NAMES[n]));
            return;
        }
        let Some(settings) = self.nam_slot(target).as_ref().and_then(nam_settings) else { return };
        let mut extras = self.nam_snapshots(target);
        extras.set(
            n,
            Some(Snapshot { model: settings.model, input_db: settings.input_db, output_db: settings.output_db }),
        );
        let fx_id = target.fx_id.clone();
        self.edit("Store NAM snapshot", |p| snapshots::write(p.track_mut(target.track).unwrap(), &fx_id, &extras));
        self.notify(format!("Stored snapshot {}", n + 1));
    }

    pub fn nam_clear_snapshot(&mut self, target: &Target, n: usize) {
        if let Some(at) = self.nam_rig(target) {
            self.clear_rig_snapshot(at, n);
            return;
        }
        let mut extras = self.nam_snapshots(target);
        extras.set(n, None);
        let fx_id = target.fx_id.clone();
        self.edit("Clear NAM snapshot", |p| snapshots::write(p.track_mut(target.track).unwrap(), &fx_id, &extras));
    }

    /// Switches the slot to snapshot `n`: its levels at once, its model once it is read. A
    /// rig switches as a whole once its model is read.
    pub fn nam_recall_snapshot(&mut self, target: &Target, n: usize) {
        if let Some(at) = self.nam_rig(target) {
            match self.rig_extras(at).snapshot(n).cloned() {
                Some(s) => {
                    self.rig_switch(target, s, format!("Rig snapshot {}", scythe_session::rig::SNAPSHOT_NAMES[n]))
                }
                None => self.notify(format!(
                    "Snapshot {} is empty (Shift+click it to store the rig)",
                    scythe_session::rig::SNAPSHOT_NAMES[n]
                )),
            }
            return;
        }
        let Some(snap) = self.nam_snapshots(target).get(n).cloned() else {
            self.notify(format!("Snapshot {} is empty (Shift+click it to store the current model)", n + 1));
            return;
        };
        let current = self.nam_slot(target).as_ref().and_then(nam_settings);
        match &snap.model {
            Some(model) if current.as_ref().and_then(|c| c.model.as_ref()) != Some(model) => {
                self.nam.load_model(target.clone(), PathBuf::from(model), Some((snap.input_db, snap.output_db)));
            }
            _ => self.nam_set_levels(target, snap.input_db, snap.output_db, false),
        }
    }

    /// Switches the rig to `settings`, reading a new amp model off the UI thread first so the
    /// switch does not stall.
    pub fn rig_switch(&mut self, target: &Target, settings: RigSettings, label: String) {
        let Some(at) = self.nam_rig(target) else { return };
        let current = self.rig_settings(at).and_then(|s| s.amp.model);
        match &settings.amp.model {
            Some(m) if current.as_ref() != Some(m) => {
                let path = self.nam_resolve(m);
                self.nam.read_model(target.clone(), path, None, Purpose::Rig(Box::new(settings), label));
            }
            _ => {
                self.apply_rig(at, &label, &settings);
                self.engine.wake_effects();
            }
        }
    }

    /// Loads a rig preset into the rig.
    pub fn rig_load_preset(&mut self, target: &Target, preset: &scythe_session::rig::Preset) {
        let Some(at) = self.nam_rig(target) else { return };
        self.rig_mark_preset(at, &preset.name);
        self.rig_switch(target, preset.settings.clone(), format!("Load rig preset {}", preset.name));
    }

    /// Remembers `name` as the rig's preset, for stepping to the next one.
    pub fn rig_mark_preset(&mut self, at: RigAt, name: &str) {
        let Some(id) = self.project.track(at.track).and_then(|t| t.fx.get(at.index)).and_then(slot_id) else { return };
        let mut extras = self.rig_extras(at);
        extras.preset = Some(name.to_owned());
        self.edit("Rig preset", |p| {
            if let Some(t) = p.track_mut(at.track) {
                scythe_session::rig::write_extras(t, &id, &extras);
            }
        });
    }

    pub fn nam_recall_focused(&mut self, n: usize) {
        match self.nam_focus() {
            Some(target) => self.nam_recall_snapshot(&target, n),
            None => self.notify("Select a track with a Neural Amp Modeler slot first"),
        }
    }

    /// Sets the slot's levels; `gesture` folds a knob drag into one undo step.
    pub fn nam_set_levels(&mut self, target: &Target, input_db: f64, output_db: f64, gesture: bool) {
        let Some(path) = self.nam_path(target) else { return };
        let track = target.track;
        let f = |p: &mut Project| edit_slot(p, track, &path, |s| set_nam_levels(s, input_db, output_db));
        if gesture {
            self.gesture("NAM levels", f);
        } else {
            self.edit("NAM levels", f);
        }
    }

    /// Model files dropped on a track: into its NAM slot or rig, or a new NAM slot.
    pub fn drop_nam_files(&mut self, track: TrackId, files: &[PathBuf]) {
        let Some(path) = files.iter().find(|p| is_model_file(p)).cloned() else { return };
        let found = self.project.track(track).and_then(|t| t.fx.iter().position(|s| is_nam(s) || rig::is_rig(s)));
        let target = match found {
            Some(index) => self.nam_target(track, index),
            None => {
                let slot = scythe_engine::fx::nam_slot(None);
                self.edit("Add Neural Amp Modeler", |p| p.track_mut(track).unwrap().fx.push(slot));
                let index = self.project.track(track).map_or(0, |t| t.fx.len() - 1);
                self.nam_target(track, index)
            }
        };
        if let Some(target) = target {
            self.nam_open(&target, path);
        }
    }

    /// The gear type of a model: what the library knows (TONE3000 or the file's metadata).
    fn model_gear(&self, path: &Path) -> Option<String> {
        self.nam.library.entry(path).and_then(|e| e.gear_type.clone()).or_else(|| scythe_session::rig::model_gear(path))
    }

    /// Applies finished NAM work: loaded models, downloads, messages.
    pub fn poll_nam(&mut self) {
        for outcome in self.nam.poll() {
            match outcome {
                Outcome::Load { target, path, levels, purpose, model } => {
                    let Some(slot_path) = self.nam_path(&target) else { continue };
                    self.engine.fx().preload_nam(path.clone(), *model);
                    if let Purpose::Rig(settings, label) = &purpose {
                        if let Some(at) = self.nam_rig(&target) {
                            self.apply_rig(at, label, settings);
                        }
                        self.engine.wake_effects();
                        continue;
                    }
                    let track = target.track;
                    let model_name = match &purpose {
                        Purpose::Restore(name) => PathBuf::from(name),
                        _ => path.clone(),
                    };
                    let gear = match &purpose {
                        Purpose::Preview(_) => self.nam.preview.as_ref().and_then(|p| p.tone.gear.clone()),
                        _ => self.model_gear(&path),
                    };
                    let follow = self.nam_rig(&target).filter(|at| self.rig_extras(*at).cab_auto);
                    let cab_before = match purpose {
                        Purpose::Restore(_) => self.nam.preview.as_ref().and_then(|p| p.cab_before),
                        _ => None,
                    };
                    let change = |p: &mut Project| {
                        edit_slot(p, track, &slot_path, |slot| {
                            set_nam_model(slot, &model_name);
                            if let Some((input, output)) = levels {
                                set_nam_levels(slot, input, output);
                            }
                        });
                        if let Some(at) = follow
                            && let Some(rig_slot) = p.track_mut(at.track).and_then(|t| t.fx.get_mut(at.index))
                            && let Some(mut s) = rig::read(rig_slot)
                        {
                            match cab_before {
                                Some(on) => s.cab.enabled = on,
                                None => s = auto_cab(s, gear.as_deref()),
                            }
                            rig::apply(rig_slot, &s);
                        }
                    };
                    match purpose {
                        Purpose::Load => {
                            if self.nam.preview.as_ref().is_some_and(|p| p.target == target) {
                                self.nam.preview = None;
                            }
                            self.edit("Load NAM model", change);
                        }
                        Purpose::Preview(_) | Purpose::Restore(_) => {
                            let fold = self.nam.preview.as_ref().is_none_or(|p| p.recorded);
                            self.session.edit_folding(PREVIEW_STEP, fold, change);
                            if let Some(p) = self.nam.preview.as_mut() {
                                p.recorded = true;
                            }
                        }
                        Purpose::Rig(..) => {}
                    }
                    self.engine.wake_effects();
                }
                Outcome::PreviewIr { target, path } => {
                    let fold = self.nam.preview.as_ref().is_none_or(|p| p.recorded);
                    self.rig_preview_ir(&target, path.as_deref(), fold);
                }
                Outcome::Open { target, path } => self.nam_open(&target, path),
                Outcome::Notify(msg) => self.notify(msg),
            }
        }
    }

    /// Plays `path` in the rig's cab while previewing (or the cab's earlier response when
    /// comparing or reverting), as part of the preview's undo step.
    fn rig_preview_ir(&mut self, target: &Target, path: Option<&Path>, fold: bool) {
        let Some(at) = self.nam_rig(target) else { return };
        let Some(mut s) = self.rig_settings(at) else { return };
        s.cab.ir = path.map(|p| p.display().to_string());
        self.session.edit_folding(PREVIEW_STEP, fold, |p| {
            if let Some(slot) = p.track_mut(at.track).and_then(|t| t.fx.get_mut(at.index)) {
                rig::apply(slot, &s);
            }
        });
        if let Some(p) = self.nam.preview.as_mut() {
            p.recorded = true;
        }
        self.engine.wake_effects();
    }

    /// Plays `model` of `tone` in the slot (or an impulse response in the rig's cab) without
    /// adding it to the library yet.
    pub fn nam_preview(&mut self, target: &Target, tone: &Tone, models: &[Model], model: &Model) {
        let ir = tone.is_ir();
        let previous = if ir {
            self.nam_rig(target).and_then(|at| self.rig_settings(at)).and_then(|s| s.cab.ir)
        } else {
            self.nam_slot(target).as_ref().and_then(nam_settings).and_then(|s| s.model)
        };
        if ir {
            self.nam.section = Stage::Cab;
        }
        let cab = self.nam_rig(target).and_then(|at| self.rig_settings(at)).map(|s| s.cab.enabled);
        self.nam.start_preview(target.clone(), previous, tone, models, model.id);
        if let Some(p) = self.nam.preview.as_mut().filter(|p| p.target == *target && p.cab_before.is_none()) {
            p.cab_before = cab;
        }
    }

    /// Ends a preview: keeps the model (into the library, the slot pointing at it there) or
    /// puts the slot's earlier model back.
    pub fn nam_end_preview(&mut self, keep: bool) {
        let recorded = self.nam.preview.as_ref().is_some_and(|p| p.recorded);
        let ir = self.nam.preview.as_ref().is_some_and(|p| p.ir);
        let in_rig = self.nam.preview.as_ref().is_some_and(|p| self.nam_rig(&p.target).is_some());
        if !keep && in_rig && recorded && self.session.history.undo_label() == Some(PREVIEW_STEP) {
            // The preview's one undo step holds everything it changed in the rig (the model, the
            // impulse response and the cab it switched), so undoing it puts the rig back whole.
            self.nam.end_preview(false);
            self.undo();
            self.engine.wake_effects();
            return;
        }
        match self.nam.end_preview(keep) {
            Some(Ending::Keep { target, tone, model }) => match self.nam.keep_previewed(&tone, &model) {
                Ok(path) => {
                    self.nam.selected = Some(path.clone());
                    if ir {
                        self.rig_preview_ir(&target, Some(&path), recorded);
                    } else {
                        self.nam.load_model(target, path, None);
                    }
                    self.notify(format!("Kept {} in the library", model.name));
                }
                Err(e) => self.notify(format!("Could not keep {}: {e}", model.name)),
            },
            Some(Ending::Revert { target, previous }) if ir => {
                self.rig_preview_ir(&target, previous.as_deref().map(Path::new), recorded);
            }
            Some(Ending::Revert { target, previous }) => match previous {
                Some(name) => {
                    let path = self.nam_resolve(&name);
                    self.nam.read_model(target, path, None, Purpose::Restore(name));
                }
                None => {
                    let Some(path) = self.nam_path(&target) else { return };
                    let track = target.track;
                    self.session.edit_folding(PREVIEW_STEP, recorded, |p| edit_slot(p, track, &path, clear_nam_model));
                    self.engine.wake_effects();
                }
            },
            None => {}
        }
    }

    /// A/B: plays the slot's model from before the preview, or the previewed one again.
    pub fn nam_toggle_compare(&mut self) {
        let dir = self.project.path.as_deref().and_then(Path::parent).map(Path::to_owned);
        self.nam.toggle_compare(|name| resolve(dir.as_deref(), name));
    }

    /// A model name as a slot keeps it, with a path relative to the project resolved.
    pub fn nam_resolve(&self, name: &str) -> PathBuf {
        resolve(self.project.path.as_deref().and_then(Path::parent), name)
    }

    /// Adds the folder of the slot's model to the library, so it is listed with what is
    /// known about it.
    pub fn nam_add_model_folder(&mut self, target: &Target) {
        let Some(dir) = self.nam_model_path(target).and_then(|p| p.parent().map(Path::to_owned)) else { return };
        if self.nam.library.add_folder(dir.clone()) {
            self.nam.rescan();
            self.notify(format!("Added {} to the library", dir.display()));
        }
    }
}

/// The undo step a preview's changes to the slot fold into.
const PREVIEW_STEP: &str = "Preview NAM model";

fn resolve(project_dir: Option<&Path>, name: &str) -> PathBuf {
    let path = PathBuf::from(name);
    match (path.is_absolute(), project_dir) {
        (false, Some(dir)) => dir.join(path),
        _ => path,
    }
}
