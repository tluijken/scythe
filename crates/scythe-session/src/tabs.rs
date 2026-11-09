//! Project tabs: several open projects, one of them active. The active one is the session's
//! project; the others wait with their undo history and unsaved changes, stopped, as REAPER's
//! background tabs do unless "Run background projects" is on. REAPER's reaper.ini lists the
//! tabs' files (`projecttab<n>`, `projecttabs`, `lastproject`), and so does Scythe's.

use std::path::Path;
use std::time::Instant;

use scythe_core::Project;
use scythe_core::history::History;
use serde::Serialize;

use crate::Session;

/// A tab that is not active.
#[derive(Debug, Default)]
pub struct ParkedTab {
    project: Project,
    history: History,
    dirty: bool,
    saved_at: Option<Instant>,
}

/// One tab as the tab bar and the API show it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TabInfo {
    pub index: usize,
    /// The project's name, "untitled" before its first save.
    pub name: String,
    pub path: Option<std::path::PathBuf>,
    /// Unsaved changes.
    pub dirty: bool,
    pub active: bool,
}

impl Session {
    /// The open tabs, in order.
    pub fn tabs(&self) -> Vec<TabInfo> {
        (0..self.tab_count())
            .map(|i| {
                let (project, dirty) = match &self.parked_tabs[i] {
                    Some(t) => (&t.project, t.dirty),
                    None => (&self.project, self.dirty),
                };
                TabInfo {
                    index: i,
                    name: if project.path.is_some() { project.name() } else { "untitled".into() },
                    path: project.path.clone(),
                    dirty,
                    active: i == self.active_tab,
                }
            })
            .collect()
    }

    pub fn tab_count(&self) -> usize {
        self.parked_tabs.len()
    }

    pub fn active_tab(&self) -> usize {
        self.active_tab
    }

    /// Opens an empty project in a new tab after the active one and switches to it.
    pub fn new_tab(&mut self) {
        let at = self.active_tab + 1;
        self.parked_tabs.insert(at, Some(ParkedTab::default()));
        self.switch_tab(at);
    }

    /// Opens `path` in a new tab and switches to it; on failure no tab is added.
    pub fn open_in_new_tab(&mut self, path: &Path) -> Result<(), String> {
        let project = scythe_core::rpp::read_project(path).map_err(|e| {
            let e = format!("Could not open {}: {e}", path.display());
            self.notify(e.clone());
            e
        })?;
        let at = self.active_tab + 1;
        self.parked_tabs.insert(at, Some(ParkedTab { project, ..Default::default() }));
        self.switch_tab(at);
        self.notify(format!("Opened {}", path.display()));
        Ok(())
    }

    /// Makes tab `index` the active one; the one it replaces stops and waits as it is.
    pub fn switch_tab(&mut self, index: usize) -> bool {
        if index >= self.tab_count() || index == self.active_tab {
            return false;
        }
        let Some(incoming) = self.parked_tabs[index].take() else { return false };
        let outgoing = ParkedTab {
            project: std::mem::replace(&mut self.project, incoming.project),
            history: std::mem::replace(&mut self.history, incoming.history),
            dirty: std::mem::replace(&mut self.dirty, incoming.dirty),
            saved_at: std::mem::replace(&mut self.saved_at, incoming.saved_at),
        };
        self.parked_tabs[self.active_tab] = Some(outgoing);
        self.active_tab = index;
        self.selected_track = None;
        self.after_load();
        self.remember_tabs();
        true
    }

    /// Closes tab `index`, dropping its unsaved changes. Closing the last tab leaves an empty
    /// project, as REAPER does.
    pub fn close_tab(&mut self, index: usize) -> bool {
        if index >= self.tab_count() {
            return false;
        }
        if self.tab_count() == 1 {
            self.new_project();
            self.remember_tabs();
            return true;
        }
        if index == self.active_tab {
            let next = if index + 1 < self.tab_count() { index + 1 } else { index - 1 };
            self.switch_tab(next);
        }
        self.parked_tabs.remove(index);
        if self.active_tab > index {
            self.active_tab -= 1;
        }
        self.remember_tabs();
        true
    }

    /// Writes the tabs' files to reaper.ini, as REAPER keeps them.
    pub(crate) fn remember_tabs(&mut self) {
        let files: Vec<String> =
            self.tabs().iter().map(|t| t.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default()).collect();
        let last = files.get(self.active_tab).cloned().filter(|f| !f.is_empty());
        self.change_preferences(|p| {
            p.project_tabs = files;
            p.last_project = last;
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use scythe_core::render_settings::ResampleMode;
    use scythe_engine::{Engine, MediaPool};

    use super::*;

    fn session() -> Session {
        let pool = Arc::new(MediaPool::with_resample(ResampleMode::PLAYBACK));
        Session::new(Engine::start_offline(pool, 48_000))
    }

    #[test]
    fn tabs_keep_their_projects_and_history() {
        let mut s = session();
        s.edit("Add track", |p| p.tracks.push(scythe_core::Track::new("first")));
        s.new_tab();
        assert_eq!((s.tab_count(), s.active_tab()), (2, 1));
        assert!(s.project.tracks.is_empty() && !s.dirty);
        s.edit("Add track", |p| p.tracks.push(scythe_core::Track::new("second")));

        assert!(s.switch_tab(0));
        assert_eq!(s.project.tracks[0].name, "first");
        assert!(s.dirty && s.history.undo_label().is_some());
        let tabs = s.tabs();
        assert_eq!(tabs.iter().map(|t| (t.active, t.dirty)).collect::<Vec<_>>(), [(true, true), (false, true)]);

        assert!(s.close_tab(0));
        assert_eq!((s.tab_count(), s.active_tab()), (1, 0));
        assert_eq!(s.project.tracks[0].name, "second");
        assert!(s.close_tab(0));
        assert_eq!(s.tab_count(), 1);
        assert!(s.project.tracks.is_empty());
    }
}
