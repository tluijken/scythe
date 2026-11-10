use crate::model::Project;

/// One undo step: its label, the project to return to, a number no other step of the history
/// has, and who made it when not the user at this machine (an AI agent's name).
#[derive(Debug)]
struct Step {
    label: String,
    before: Project,
    serial: u64,
    by: Option<String>,
}

/// Snapshot-based undo. Projects are small compared to the media they reference, so storing
/// whole copies keeps every edit trivially undoable without per-command inverse logic.
#[derive(Debug)]
pub struct History {
    undo: Vec<Step>,
    redo: Vec<Step>,
    limit: usize,
    next_serial: u64,
}

impl Default for History {
    fn default() -> Self {
        Self::new(200)
    }
}

impl History {
    pub fn new(limit: usize) -> Self {
        Self { undo: Vec::new(), redo: Vec::new(), limit, next_serial: 1 }
    }

    /// Keeps at most `limit` undo steps (the oldest are dropped first).
    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit.max(1);
        let excess = self.undo.len().saturating_sub(self.limit);
        self.undo.drain(..excess);
    }

    /// Records `before` as the state to return to when undoing `label`.
    pub fn record(&mut self, label: impl Into<String>, before: Project) {
        self.push(label.into(), before, None);
        self.redo.clear();
    }

    fn push(&mut self, label: String, before: Project, by: Option<String>) {
        let serial = self.next_serial;
        self.next_serial += 1;
        self.undo.push(Step { label, before, serial, by });
        if self.undo.len() > self.limit {
            self.undo.remove(0);
        }
    }

    pub fn undo(&mut self, current: &mut Project) -> Option<String> {
        let mut step = self.undo.pop()?;
        step.before = std::mem::replace(current, step.before);
        let label = step.label.clone();
        self.redo.push(step);
        Some(label)
    }

    pub fn redo(&mut self, current: &mut Project) -> Option<String> {
        let mut step = self.redo.pop()?;
        step.before = std::mem::replace(current, step.before);
        let label = step.label.clone();
        self.undo.push(step);
        Some(label)
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|s| s.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|s| s.label.as_str())
    }

    /// Who made the step undo would revert, when it was not the user at this machine.
    pub fn undo_by(&self) -> Option<&str> {
        self.undo.last().and_then(|s| s.by.as_deref())
    }

    pub fn redo_by(&self) -> Option<&str> {
        self.redo.last().and_then(|s| s.by.as_deref())
    }

    /// The serial number the next recorded step gets: steps numbered from it on were recorded
    /// after asking.
    pub fn next_serial(&self) -> u64 {
        self.next_serial
    }

    /// Marks the undo steps recorded since [`Self::next_serial`] answered `since` as made by
    /// `by`, and answers their labels, oldest first. The labels stay as REAPER names the steps.
    pub fn attribute_since(&mut self, since: u64, by: &str) -> Vec<String> {
        self.undo
            .iter_mut()
            .filter(|s| s.serial >= since)
            .map(|s| {
                s.by = Some(by.to_owned());
                s.label.clone()
            })
            .collect()
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
        let mut folded = self.undo.drain(since..).collect::<Vec<_>>().into_iter();
        let first = folded.next().expect("non-empty");
        let by = folded.fold(first.by, |by, s| by.or(s.by));
        self.push(label.to_owned(), first.before, by);
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

    #[test]
    fn steps_remember_who_made_them() {
        let mut h = History::default();
        let mut p = Project::default();
        h.record("by hand", p.clone());
        let since = h.next_serial();
        h.record("Track volume", p.clone());
        p.add_track("a");
        assert_eq!(h.attribute_since(since, "Claude"), vec!["Track volume".to_owned()]);
        assert_eq!((h.undo_label(), h.undo_by()), (Some("Track volume"), Some("Claude")));
        h.undo(&mut p);
        assert_eq!((h.undo_by(), h.redo_by()), (None, Some("Claude")));
        h.redo(&mut p);
        assert_eq!(h.undo_by(), Some("Claude"));
    }
}
