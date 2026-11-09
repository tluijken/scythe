//! What the NAM panel, the shortcuts and dropped files do to the project's NAM slots.

use std::path::{Path, PathBuf};

use scythe_core::{FxSlot, TrackId};
use scythe_engine::builtin::{Builtin, reverb_state};
use scythe_engine::fx::{is_nam, nam_settings, set_nam_levels, set_nam_model, slot_id};

use super::library::{Filter, Kind};
use super::snapshots::{self, Snapshot};
use super::{Outcome, Target, is_model_file};
use crate::app::ScytheApp;

impl ScytheApp {
    pub fn nam_target(&self, track: TrackId, index: usize) -> Option<Target> {
        let slot = self.project.track(track)?.fx.get(index).filter(|s| is_nam(s))?;
        Some(Target { track, fx_id: slot_id(slot)? })
    }

    pub fn nam_index(&self, target: &Target) -> Option<usize> {
        let fx = &self.project.track(target.track)?.fx;
        fx.iter().position(|s| is_nam(s) && slot_id(s).as_deref() == Some(target.fx_id.as_str()))
    }

    pub fn nam_slot(&self, target: &Target) -> Option<&FxSlot> {
        let index = self.nam_index(target)?;
        self.project.track(target.track)?.fx.get(index)
    }

    /// The model a slot plays, with a path relative to the project resolved.
    pub fn nam_model_path(&self, target: &Target) -> Option<PathBuf> {
        let model = nam_settings(self.nam_slot(target)?)?.model?;
        let path = PathBuf::from(model);
        Some(match (path.is_absolute(), self.project.path.as_deref().and_then(Path::parent)) {
            (false, Some(dir)) => dir.join(path),
            _ => path,
        })
    }

    /// The slot the NAM shortcuts act on: the one in the panel, else the selected track's first.
    pub fn nam_focus(&self) -> Option<Target> {
        if let Some(t) = self.nam.panel.clone().filter(|t| self.nam_index(t).is_some()) {
            return Some(t);
        }
        let track = self.sel.track?;
        let index = self.project.track(track)?.fx.iter().position(is_nam)?;
        self.nam_target(track, index)
    }

    pub fn toggle_nam_panel(&mut self, track: TrackId, index: usize) {
        let target = self.nam_target(track, index);
        if target.is_some() && self.nam.panel == target {
            self.nam.panel = None;
            return;
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

    /// Loads a model into the slot (read in the background, then crossfaded in) or, for an
    /// impulse response, into the convolution slot after it.
    pub fn nam_open(&mut self, target: &Target, path: PathBuf) {
        if is_model_file(&path) {
            self.nam.load_model(target.clone(), path, None);
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav")) {
            self.nam_load_ir(target, &path);
        } else {
            self.notify(format!("{} is neither a NAM model nor an impulse response", path.display()));
        }
    }

    /// Puts an impulse response in the convolution slot right after the NAM slot, adding one
    /// (a ReaVerb, fully wet like a cabinet) when there is none.
    pub fn nam_load_ir(&mut self, target: &Target, path: &Path) {
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

    /// The model `step` places away from the current one among those the library browser shows.
    pub fn nam_step(&mut self, step: isize) {
        let Some(target) = self.nam_focus() else {
            self.notify("Select a track with a Neural Amp Modeler slot first");
            return;
        };
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

    /// Stores the slot's model and levels as snapshot `n`.
    pub fn nam_store_snapshot(&mut self, target: &Target, n: usize) {
        let Some(settings) = self.nam_slot(target).and_then(nam_settings) else { return };
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
        let mut extras = self.nam_snapshots(target);
        extras.set(n, None);
        let fx_id = target.fx_id.clone();
        self.edit("Clear NAM snapshot", |p| snapshots::write(p.track_mut(target.track).unwrap(), &fx_id, &extras));
    }

    /// Switches the slot to snapshot `n`: its levels at once, its model once it is read.
    pub fn nam_recall_snapshot(&mut self, target: &Target, n: usize) {
        let Some(snap) = self.nam_snapshots(target).get(n).cloned() else {
            self.notify(format!("Snapshot {} is empty (Shift+click it to store the current model)", n + 1));
            return;
        };
        let current = self.nam_slot(target).and_then(nam_settings);
        match &snap.model {
            Some(model) if current.as_ref().and_then(|c| c.model.as_ref()) != Some(model) => {
                self.nam.load_model(target.clone(), PathBuf::from(model), Some((snap.input_db, snap.output_db)));
            }
            _ => self.nam_set_levels(target, snap.input_db, snap.output_db, false),
        }
    }

    pub fn nam_recall_focused(&mut self, n: usize) {
        match self.nam_focus() {
            Some(target) => self.nam_recall_snapshot(&target, n),
            None => self.notify("Select a track with a Neural Amp Modeler slot first"),
        }
    }

    /// Sets the slot's levels; `gesture` folds a knob drag into one undo step.
    pub fn nam_set_levels(&mut self, target: &Target, input_db: f64, output_db: f64, gesture: bool) {
        let Some(index) = self.nam_index(target) else { return };
        let track = target.track;
        let f = |p: &mut scythe_core::Project| {
            set_nam_levels(&mut p.track_mut(track).unwrap().fx[index], input_db, output_db)
        };
        if gesture {
            self.gesture("NAM levels", f);
        } else {
            self.edit("NAM levels", f);
        }
    }

    /// Model files dropped on a track: into its NAM slot, or a new one.
    pub fn drop_nam_files(&mut self, track: TrackId, files: &[PathBuf]) {
        let Some(path) = files.iter().find(|p| is_model_file(p)).cloned() else { return };
        let target = match self.project.track(track).and_then(|t| t.fx.iter().position(is_nam)) {
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

    /// Applies finished NAM work: loaded models, downloads, messages.
    pub fn poll_nam(&mut self) {
        for outcome in self.nam.poll() {
            match outcome {
                Outcome::Load { target, path, levels, model } => {
                    let Some(index) = self.nam_index(&target) else { continue };
                    self.engine.fx().preload_nam(path.clone(), model);
                    let track = target.track;
                    self.edit("Load NAM model", |p| {
                        let slot = &mut p.track_mut(track).unwrap().fx[index];
                        set_nam_model(slot, &path);
                        if let Some((input, output)) = levels {
                            set_nam_levels(slot, input, output);
                        }
                    });
                    self.engine.wake_effects();
                }
                Outcome::Open { target, path } => self.nam_open(&target, path),
                Outcome::Notify(msg) => self.notify(msg),
            }
        }
    }
}
