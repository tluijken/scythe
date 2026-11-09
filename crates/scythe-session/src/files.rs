use std::path::Path;
use std::time::{Duration, Instant};

use scythe_core::Project;
use scythe_core::rpp::{autosave_path, prune_backups, read_project, save_project, write_project};

use crate::Session;

impl Session {
    pub fn new_project(&mut self) {
        self.engine.pool().clear();
        self.project = Project::default();
        self.history.clear();
        self.dirty = false;
        self.saved_at = None;
        self.after_load();
        self.remember_tabs();
    }

    /// Replaces the project with one built elsewhere (a generated demo, a template).
    pub fn load(&mut self, project: Project) {
        self.project = project;
        self.history.clear();
        self.history.set_limit(200);
        self.dirty = false;
        self.saved_at = None;
        self.after_load();
    }

    pub fn open_path(&mut self, path: &Path) -> Result<(), String> {
        match read_project(path) {
            Ok(project) => {
                self.project = project;
                self.history.clear();
                // Undo keeps whole snapshots: bound them to roughly 256 MB for big projects.
                let bytes = std::fs::metadata(path).map_or(0, |m| m.len() as usize).max(1);
                self.history.set_limit((256_000_000 / (bytes * 3)).clamp(30, 200));
                self.dirty = false;
                self.saved_at = None;
                self.after_load();
                self.notify(format!("Opened {}", path.display()));
                self.remember_tabs();
                Ok(())
            }
            Err(e) => {
                let e = format!("Could not open {}: {e}", path.display());
                self.notify(e.clone());
                Err(e)
            }
        }
    }

    /// A project used as a subproject (it has a `-PROX` proxy next to it, as REAPER keeps one)
    /// renders its proxy again in the background when it is saved, so the projects that hold it
    /// play the new version.
    fn render_subproject_proxy(&mut self, path: &Path) {
        if !scythe_engine::media::subproject_proxy(path).exists() {
            return;
        }
        let (path, pool, tx) = (path.to_owned(), self.engine.pool().clone(), self.loader_tx.clone());
        std::thread::spawn(move || {
            let result = scythe_engine::media::render_subproject(&path).map_err(|e| e.to_string());
            pool.forget(&path);
            let _ = tx.send(crate::LoaderMsg::SubprojectRendered(path, result));
        });
    }

    /// Every few minutes, writes unsaved changes to a timestamped file in the project's
    /// `Backups` folder (see [`autosave_due`]). The project file itself is only written by an
    /// explicit save.
    pub(crate) fn autosave(&mut self) {
        let due = autosave_due(
            self.dirty && self.gesture.is_none(),
            self.autosaved_at.elapsed(),
            self.is_playing(),
            self.is_recording(),
        );
        if !due {
            return;
        }
        self.autosaved_at = Instant::now();
        self.write_backup();
    }

    /// Writes the project, unsaved changes included, to a new file in its `Backups` folder.
    pub fn write_backup(&mut self) {
        let Some(path) = self.project.path.clone() else { return };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let target = autosave_path(&path, now);
        let result = target
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .map_err(Into::into)
            .and_then(|()| write_project(&self.project, &target));
        match result {
            Ok(()) => prune_backups(&path, BACKUPS_KEPT),
            Err(e) => self.notify(format!("Auto-save failed: {e}")),
        }
    }

    pub fn save_to(&mut self, path: &Path) -> Result<(), String> {
        self.project.path = Some(path.to_owned());
        self.engine.store_plugin_states(&mut self.project);
        match save_project(&self.project, path) {
            Ok(()) => {
                self.dirty = false;
                self.autosaved_at = Instant::now();
                self.saved_at = Some(Instant::now());
                self.notify(format!("Saved {}", path.display()));
                self.remember_tabs();
                self.render_subproject_proxy(path);
                Ok(())
            }
            Err(e) => {
                let e = format!("Could not save: {e}");
                self.notify(e.clone());
                Err(e)
            }
        }
    }
}

/// How often unsaved changes are backed up.
const AUTOSAVE_INTERVAL: Duration = Duration::from_secs(300);
/// While the transport runs, a backup waits for it to stop (between songs) at most this long.
const AUTOSAVE_HOLD: Duration = Duration::from_secs(900);
/// Backups kept per project; a session running for days would otherwise fill the disk.
const BACKUPS_KEPT: usize = 50;

/// Whether to write a backup now: `pending` changes, `since` the last one. Never while recording,
/// which is streaming to the same disk; while playing only once it has waited too long.
fn autosave_due(pending: bool, since: Duration, playing: bool, recording: bool) -> bool {
    pending && !recording && since >= if playing { AUTOSAVE_HOLD } else { AUTOSAVE_INTERVAL }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backups_wait_for_the_song_to_end() {
        let min = |m: u64| Duration::from_secs(m * 60);
        assert!(!autosave_due(true, min(4), false, false));
        assert!(autosave_due(true, min(5), false, false));
        assert!(!autosave_due(false, min(60), false, false), "nothing to back up");
        assert!(!autosave_due(true, min(10), true, false), "held while a song plays");
        assert!(autosave_due(true, min(15), true, false), "but not for ever");
        assert!(!autosave_due(true, min(60), true, true), "never while recording");
    }
}
