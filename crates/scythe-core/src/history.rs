use crate::model::Project;

/// Snapshot-based undo. Projects are small compared to the media they reference, so storing
/// whole copies keeps every edit trivially undoable without per-command inverse logic.
#[derive(Debug)]
pub struct History {
    undo: Vec<(String, Project)>,
    redo: Vec<(String, Project)>,
    limit: usize,
}

impl Default for History {
    fn default() -> Self {
        Self::new(200)
    }
}

impl History {
    pub fn new(limit: usize) -> Self {
        Self { undo: Vec::new(), redo: Vec::new(), limit }
    }

    /// Keeps at most `limit` undo steps (the oldest are dropped first).
    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit.max(1);
        let excess = self.undo.len().saturating_sub(self.limit);
        self.undo.drain(..excess);
    }

    /// Records `before` as the state to return to when undoing `label`.
    pub fn record(&mut self, label: impl Into<String>, before: Project) {
        self.undo.push((label.into(), before));
        if self.undo.len() > self.limit {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn undo(&mut self, current: &mut Project) -> Option<String> {
        let (label, prev) = self.undo.pop()?;
        let now = std::mem::replace(current, prev);
        self.redo.push((label.clone(), now));
        Some(label)
    }

    pub fn redo(&mut self, current: &mut Project) -> Option<String> {
        let (label, next) = self.redo.pop()?;
        let now = std::mem::replace(current, next);
        self.undo.push((label.clone(), now));
        Some(label)
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|(l, _)| l.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|(l, _)| l.as_str())
    }

    /// How many undo steps there are.
    pub fn depth(&self) -> usize {
        self.undo.len()
    }

    /// Folds the undo steps recorded since the depth was `since` into one named `label`, as a
    /// custom action with "consolidate undo" leaves them. Answers whether there was any.
    pub fn consolidate(&mut self, since: usize, label: &str) -> bool {
        if self.undo.len() <= since {
            return false;
        }
        let before = self.undo.drain(since..).next().map(|(_, p)| p).expect("non-empty");
        self.undo.push((label.to_owned(), before));
        true
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_redo_cycle() {
        let mut h = History::default();
        let mut p = Project::default();
        h.record("add track", p.clone());
        p.add_track("a");
        assert_eq!(h.undo(&mut p).as_deref(), Some("add track"));
        assert!(p.tracks.is_empty());
        assert_eq!(h.redo(&mut p).as_deref(), Some("add track"));
        assert_eq!(p.tracks.len(), 1);
    }

    #[test]
    fn new_edit_clears_redo() {
        let mut h = History::default();
        let mut p = Project::default();
        h.record("a", p.clone());
        p.add_track("a");
        h.undo(&mut p);
        h.record("b", p.clone());
        assert!(h.redo_label().is_none());
    }

    #[test]
    fn consolidate_folds_steps() {
        let mut h = History::default();
        let mut p = Project::default();
        h.record("first", p.clone());
        let mark = h.depth();
        h.record("a", p.clone());
        p.add_track("a");
        h.record("b", p.clone());
        p.add_track("b");
        assert!(h.consolidate(mark, "Custom: two"));
        assert!(!h.consolidate(h.depth(), "none"));
        assert_eq!(h.undo_label(), Some("Custom: two"));
        assert_eq!(h.undo(&mut p).as_deref(), Some("Custom: two"));
        assert!(p.tracks.is_empty());
        assert_eq!(h.undo_label(), Some("first"));
    }

    #[test]
    fn limit_drops_oldest() {
        let mut h = History::new(2);
        let p = Project::default();
        for l in ["1", "2", "3"] {
            h.record(l, p.clone());
        }
        let mut cur = p;
        assert_eq!(h.undo(&mut cur).as_deref(), Some("3"));
        assert_eq!(h.undo(&mut cur).as_deref(), Some("2"));
        assert!(h.undo(&mut cur).is_none());
    }
}
