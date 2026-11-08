use std::path::Path;
use std::time::{Duration, Instant};

use scythe_core::Project;
use scythe_core::rpp::{autosave_path, read_project, save_project, write_project};

use crate::Session;

impl Session {
    pub fn new_project(&mut self) {
        self.engine.pool().clear();
        self.project = Project::default();
        self.history.clear();
        self.dirty = false;
        self.saved_at = None;
        self.after_load();
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
                Ok(())
            }
            Err(e) => {
                let e = format!("Could not open {}: {e}", path.display());
                self.notify(e.clone());
                Err(e)
            }
        }
    }

    /// Every few minutes, writes unsaved changes to a timestamped file in the project's
    /// `Backups` folder. The project file itself is only written by an explicit save.
    pub(crate) fn autosave(&mut self) {
        const INTERVAL: Duration = Duration::from_secs(300);
        if !self.dirty || self.autosaved_at.elapsed() < INTERVAL || self.gesture.is_some() {
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
        if let Err(e) = result {
            self.notify(format!("Auto-save failed: {e}"));
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
