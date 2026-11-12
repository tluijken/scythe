//! Folder editing as REAPER 7.73 does it. The structure lives in the per-track depth deltas
//! (`ISBUS`, `I_FOLDERDEPTH`); moves work on nesting levels and write the deltas back, so every
//! folder stays closed by its last child. Moving a folder takes its children along, as REAPER's
//! track drag and `ReorderSelectedTracks` do.

use crate::model::{Project, Track, TrackId};

/// How the track view shows a track, from the folder compaction (`I_FOLDERCOMPACT`) of the
/// folders around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowShow {
    Normal,
    /// Inside a folder set to small: the minimum height.
    Small,
    /// Inside a collapsed folder.
    Hidden,
}

/// Where a track drag drops, as REAPER's track view decides it from the hovered track: its
/// top third drops before it at its level, its middle third makes it a folder holding the
/// dragged tracks (as its first children), its bottom third drops after it at its level (so
/// dropping on the last child of a folder extends the folder).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Drop {
    /// The track index the dragged tracks go before (the track count for the end).
    pub before: usize,
    /// Nesting level of the dragged tracks (of the top ones, when they hold children).
    pub level: usize,
}

impl Project {
    /// Index of the last track inside folder `i` (`i` itself for a track without children).
    pub fn folder_end(&self, i: usize) -> usize {
        let levels = self.folder_levels();
        let Some(&own) = levels.get(i) else { return i };
        let mut end = i;
        while end + 1 < levels.len() && levels[end + 1] > own {
            end += 1;
        }
        end
    }

    /// The indices `ids` stand for in a move, with every folder's children: sorted, unique.
    pub fn with_children(&self, ids: &[TrackId]) -> Vec<usize> {
        let mut out: Vec<usize> = Vec::new();
        for i in ids.iter().filter_map(|id| self.track_index(*id)) {
            out.extend(i..=self.folder_end(i));
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The nesting levels tracks moved before track `before` can take: from the level of the
    /// track after them (so its folder stays as it is) to one deeper than the track before them
    /// (which then becomes their folder). `moving` (sorted indices) are left out.
    pub fn drop_levels(&self, moving: &[usize], before: usize) -> (usize, usize) {
        let levels = self.folder_levels();
        let rest = |i: &usize| moving.binary_search(i).is_err();
        let prev = (0..before.min(levels.len())).rev().find(rest);
        let next = (before..levels.len()).find(rest);
        let max = prev.map_or(0, |p| levels[p] + 1);
        let min = next.map_or(0, |n| levels[n]).min(max);
        (min, max)
    }

    /// The drop for a drag over track `i` at `frac` (0 top, 1 bottom) of its row; `None` over a
    /// dragged track. Below a collapsed folder the tracks go after its hidden children.
    pub fn drop_at(&self, moving: &[usize], i: usize, frac: f32) -> Option<Drop> {
        if moving.binary_search(&i).is_ok() {
            return None;
        }
        let levels = self.folder_levels();
        let own = *levels.get(i)?;
        let (before, want) = if frac < 1.0 / 3.0 {
            (i, own)
        } else if frac < 2.0 / 3.0 {
            (i + 1, own + 1)
        } else if self.tracks[i].folder_depth > 0 && self.tracks[i].folder_compact == 2 {
            (self.folder_end(i) + 1, own)
        } else {
            (i + 1, own)
        };
        let (min, max) = self.drop_levels(moving, before);
        Some(Drop { before, level: want.clamp(min, max) })
    }

    /// Moves `ids` (with their children) before track `before`, the top ones at nesting `level`
    /// (clamped to [`Project::drop_levels`]); the tracks keep their order and their nesting
    /// below each other. Answers whether anything changed.
    pub fn move_tracks(&mut self, ids: &[TrackId], before: usize, level: usize) -> bool {
        let moving = self.with_children(ids);
        if moving.is_empty() {
            return false;
        }
        let (min, max) = self.drop_levels(&moving, before);
        self.place(&moving, before, level.clamp(min, max), None)
    }

    /// Puts `moving` before `before` at `level`, after `head` (a new folder for them) when
    /// given, and rewrites the depths.
    fn place(&mut self, moving: &[usize], before: usize, level: usize, head: Option<Track>) -> bool {
        let old: Vec<(TrackId, i32)> = self.tracks.iter().map(|t| (t.id, t.folder_depth)).collect();
        let levels = self.folder_levels();
        let parents = self.folder_parents();
        let mut rel = vec![0usize; self.tracks.len()];
        for &i in moving {
            rel[i] = match parents[i] {
                Some(p) if moving.binary_search(&p).is_ok() => rel[p] + 1,
                _ => 0,
            };
        }
        let at = (0..before.min(self.tracks.len())).filter(|i| moving.binary_search(i).is_err()).count();
        let base = level + usize::from(head.is_some());
        let (mut rest, mut block) = (Vec::new(), Vec::new());
        block.extend(head.map(|t| (t, level)));
        for (i, t) in std::mem::take(&mut self.tracks).into_iter().enumerate() {
            if moving.binary_search(&i).is_ok() {
                block.push((t, base + rel[i]));
            } else {
                rest.push((t, levels[i]));
            }
        }
        rest.splice(at..at, block);
        let (tracks, levels): (Vec<Track>, Vec<usize>) = rest.into_iter().unzip();
        self.tracks = tracks;
        self.set_folder_levels(&levels);
        self.tracks.iter().map(|t| (t.id, t.folder_depth)).ne(old)
    }

    /// "Track: Move tracks up/down" (43647/43648): up goes before the track above at its level
    /// (into the end of a folder above), down after the track below (into a folder below as its
    /// first children).
    pub fn step_tracks(&mut self, ids: &[TrackId], up: bool) -> bool {
        let moving = self.with_children(ids);
        let (Some(&first), Some(&last)) = (moving.first(), moving.last()) else { return false };
        let levels = self.folder_levels();
        let rest = |i: &usize| moving.binary_search(i).is_err();
        let target = if up { (0..first).rev().find(rest) } else { (last + 1..self.tracks.len()).find(rest) };
        let Some(t) = target else { return false };
        let before = if up { t } else { t + 1 };
        self.move_tracks(ids, before, levels[t])
    }

    /// Moves a track one place up or down, as "Track: Move tracks up/down".
    pub fn move_track(&mut self, id: TrackId, up: bool) -> bool {
        self.step_tracks(&[id], up)
    }

    /// "Track: Move tracks to new folder" (42785): a new folder track goes where the first of
    /// `ids` was, at its level, and `ids` (with their children) become its children. Answers
    /// the folder.
    pub fn move_to_new_folder(&mut self, ids: &[TrackId], folder: Track) -> Option<TrackId> {
        let moving = self.with_children(ids);
        let &first = moving.first()?;
        let level = self.folder_levels()[first];
        let id = folder.id;
        self.place(&moving, first, level, Some(folder));
        Some(id)
    }

    /// "Track: Cycle track folder state" (1041): normal, folder parent, then (inside a folder)
    /// last in the folder. REAPER sets the raw depth, so a folder can close early.
    pub fn cycle_folder_state(&mut self, id: TrackId) {
        let Some(i) = self.track_index(id) else { return };
        let inside = self.folder_levels()[i] > 0;
        let t = &mut self.tracks[i];
        t.folder_depth = match t.folder_depth {
            0 => 1,
            d if d > 0 && inside => -1,
            _ => 0,
        };
    }

    /// "Track: Cycle folder collapsed state" (1042): normal, small, collapsed; folders only.
    pub fn cycle_folder_compact(&mut self, id: TrackId) {
        if let Some(t) = self.track_mut(id).filter(|t| t.folder_depth > 0) {
            t.folder_compact = (t.folder_compact + 1) % 3;
        }
    }

    /// Removes `ids` with their children, as "Track: Remove tracks" (40005) does; a folder
    /// left without children becomes a plain track.
    pub fn remove_tracks(&mut self, ids: &[TrackId]) -> Vec<Track> {
        let gone = self.with_children(ids);
        let levels = self.folder_levels();
        let mut kept = Vec::new();
        let mut removed = Vec::new();
        for (i, t) in std::mem::take(&mut self.tracks).into_iter().enumerate() {
            if gone.binary_search(&i).is_ok() {
                removed.push(t);
            } else {
                kept.push((t, levels[i]));
            }
        }
        let (tracks, levels): (Vec<Track>, Vec<usize>) = kept.into_iter().unzip();
        self.tracks = tracks;
        self.set_folder_levels(&levels);
        for t in &mut self.tracks {
            t.sends.retain(|s| removed.iter().all(|r| r.id != s.dest));
        }
        removed
    }

    /// How the track view shows each track.
    pub fn row_shows(&self) -> Vec<RowShow> {
        let parents = self.folder_parents();
        let mut out: Vec<RowShow> = Vec::with_capacity(parents.len());
        for parent in &parents {
            out.push(match *parent {
                None => RowShow::Normal,
                Some(p) => match (out[p], self.tracks[p].folder_compact) {
                    (RowShow::Hidden, _) | (_, 2) => RowShow::Hidden,
                    (RowShow::Small, _) | (_, 1) => RowShow::Small,
                    _ => RowShow::Normal,
                },
            });
        }
        out
    }

    /// The tracks the mixer hides: inside a folder collapsed there.
    pub fn mixer_hidden(&self) -> Vec<bool> {
        let parents = self.folder_parents();
        let mut out: Vec<bool> = Vec::with_capacity(parents.len());
        for parent in parents {
            out.push(parent.is_some_and(|p| out[p] || self.tracks[p].mixer_collapsed));
        }
        out
    }

    /// Moves `ids` (with their children) into folder `parent` (the top level for `None`) at
    /// position `index` among its other children, the end when `index` is past the last one or
    /// left out. A plain track becomes a folder. `None` when `parent` is among the moved tracks.
    pub fn move_into(&mut self, ids: &[TrackId], parent: Option<TrackId>, index: Option<usize>) -> Option<bool> {
        let moving = self.with_children(ids);
        let parent = match parent {
            Some(id) => Some(self.track_index(id).filter(|p| moving.binary_search(p).is_err())?),
            None => None,
        };
        let parents = self.folder_parents();
        let rest = |i: &usize| moving.binary_search(i).is_err();
        let children: Vec<usize> = (0..self.tracks.len()).filter(|i| parents[*i] == parent && rest(i)).collect();
        let before = match index.and_then(|k| children.get(k)) {
            Some(&c) => c,
            None => match parent {
                Some(p) => self.folder_end(p) + 1,
                None => self.tracks.len(),
            },
        };
        let level = parent.map_or(0, |p| self.folder_levels()[p] + 1);
        Some(self.move_tracks(ids, before, level))
    }

    /// Moves `ids` (with their children) so the first lands at `index` in the track list,
    /// keeping its nesting where the tracks around it allow.
    pub fn move_to_index(&mut self, ids: &[TrackId], index: usize) -> bool {
        let moving = self.with_children(ids);
        let Some(&first) = moving.first() else { return false };
        let level = self.folder_levels()[first];
        let rest: Vec<usize> = (0..self.tracks.len()).filter(|i| moving.binary_search(i).is_err()).collect();
        let before = rest.get(index).copied().unwrap_or(self.tracks.len());
        self.move_tracks(ids, before, level)
    }
}

/// The track selection as REAPER saves it: `SEL 1` in each selected track.
impl Project {
    /// The tracks the file saved as selected.
    pub fn saved_selection(&self) -> Vec<TrackId> {
        self.tracks.iter().filter(|t| t.saved_selected()).map(|t| t.id).collect()
    }

    /// Writes `tracks` as the selection the file keeps.
    pub fn store_selection(&mut self, tracks: &[TrackId]) {
        for t in &mut self.tracks {
            let on = tracks.contains(&t.id);
            t.set_saved_selected(on);
        }
    }
}

impl Track {
    fn sel_line(&self) -> Option<usize> {
        self.extra.iter().position(|n| matches!(n, crate::rpp::Node::Line(l) if crate::rpp::line_key(l) == "SEL"))
    }

    pub fn saved_selected(&self) -> bool {
        self.sel_line().is_some_and(|i| match &self.extra[i] {
            crate::rpp::Node::Line(l) => crate::rpp::tokenize(l).get(1).is_some_and(|v| v != "0"),
            _ => false,
        })
    }

    pub fn set_saved_selected(&mut self, on: bool) {
        let line = crate::rpp::Node::Line(format!("SEL {}", on as u8));
        match self.sel_line() {
            Some(i) => self.extra[i] = line,
            None => self.extra.push(line),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Spec = &'static [(&'static str, i32)];

    /// Tracks named by letters with their depths; `names` reads them back with the depths.
    fn project(spec: &[(&str, i32)]) -> Project {
        let mut p = Project::default();
        for &(name, depth) in spec {
            let id = p.add_track(name);
            p.track_mut(id).unwrap().folder_depth = depth;
        }
        p
    }

    fn state(p: &Project) -> Vec<(String, i32)> {
        p.tracks.iter().map(|t| (t.name.clone(), t.folder_depth)).collect()
    }

    fn ids(p: &Project, names: &[&str]) -> Vec<TrackId> {
        names.iter().map(|n| p.tracks.iter().find(|t| t.name == *n).unwrap().id).collect()
    }

    fn expect(p: &Project, spec: &[(&str, i32)]) {
        let want: Vec<(String, i32)> = spec.iter().map(|(n, d)| ((*n).to_owned(), *d)).collect();
        assert_eq!(state(p), want);
    }

    const F: &[(&str, i32)] = &[("F", 1), ("A", 0), ("B", -1), ("C", 0), ("D", 0)];

    // Drags in REAPER 7.73's track view (xdotool on its window) of D over the tracks of `F`.
    #[test]
    fn drags_match_reaper() {
        let cases: &[(usize, f32, Spec)] = &[
            (2, 0.30, &[("F", 1), ("A", 0), ("D", 0), ("B", -1), ("C", 0)]),
            (2, 0.42, &[("F", 1), ("A", 0), ("B", 1), ("D", -2), ("C", 0)]),
            (2, 0.65, &[("F", 1), ("A", 0), ("B", 1), ("D", -2), ("C", 0)]),
            (2, 0.73, &[("F", 1), ("A", 0), ("B", 0), ("D", -1), ("C", 0)]),
            (3, 0.05, &[("F", 1), ("A", 0), ("B", -1), ("D", 0), ("C", 0)]),
            (0, 0.5, &[("F", 1), ("D", 0), ("A", 0), ("B", -1), ("C", 0)]),
            (0, 0.88, &[("F", 1), ("D", 0), ("A", 0), ("B", -1), ("C", 0)]),
        ];
        for &(over, frac, want) in cases {
            let mut p = project(F);
            let d = ids(&p, &["D"]);
            let moving = p.with_children(&d);
            let drop = p.drop_at(&moving, over, frac).unwrap();
            p.move_tracks(&d, drop.before, drop.level);
            expect(&p, want);
        }
        let p = project(F);
        let moving = p.with_children(&ids(&p, &["D"]));
        assert_eq!(p.drop_at(&moving, 4, 0.5), None);

        // C and D together into the middle of A: A becomes their folder, B stays in F.
        let mut p = project(F);
        let cd = ids(&p, &["C", "D"]);
        let drop = p.drop_at(&p.with_children(&cd), 1, 0.51).unwrap();
        p.move_tracks(&cd, drop.before, drop.level);
        expect(&p, &[("F", 1), ("A", 1), ("C", 0), ("D", -1), ("B", -1)]);

        // Below a collapsed folder the tracks go after its children.
        let mut p = project(F);
        p.tracks[0].folder_compact = 2;
        let d = ids(&p, &["D"]);
        let drop = p.drop_at(&p.with_children(&d), 0, 0.9).unwrap();
        assert_eq!((drop.before, drop.level), (3, 0));

        // A folder takes its children along.
        let mut p = project(F);
        let f = ids(&p, &["F"]);
        p.move_tracks(&f, 5, 0);
        expect(&p, &[("C", 0), ("D", 0), ("F", 1), ("A", 0), ("B", -1)]);
    }

    // `ReorderSelectedTracks` results in REAPER 7.73 (its three folder modes are levels here).
    #[test]
    fn reorders_match_reaper() {
        let f4 = &[("T1", 1), ("T2", 0), ("T3", -1), ("T4", 0)];
        let mut p = project(f4);
        p.move_tracks(&ids(&p, &["T4"]), 3, 0);
        expect(&p, &[("T1", 1), ("T2", 0), ("T3", -1), ("T4", 0)]);
        p.move_tracks(&ids(&p, &["T4"]), 3, 2);
        expect(&p, &[("T1", 1), ("T2", 0), ("T3", 1), ("T4", -2)]);
        let mut p = project(f4);
        p.move_tracks(&ids(&p, &["T4"]), 3, 1);
        expect(&p, &[("T1", 1), ("T2", 0), ("T3", 0), ("T4", -1)]);
        let mut p = project(f4);
        p.move_tracks(&ids(&p, &["T4"]), 1, 0);
        expect(&p, &[("T1", 1), ("T4", 0), ("T2", 0), ("T3", -1)]);

        let f5 = &[("T1", 1), ("T2", 0), ("T3", -1), ("T4", 0), ("T5", 0)];
        let mut p = project(f5);
        p.move_tracks(&ids(&p, &["T3"]), 5, 0);
        expect(&p, &[("T1", 1), ("T2", -1), ("T4", 0), ("T5", 0), ("T3", 0)]);
        let mut p = project(f5);
        p.move_tracks(&ids(&p, &["T1"]), 5, 0);
        expect(&p, &[("T4", 0), ("T5", 0), ("T1", 1), ("T2", 0), ("T3", -1)]);
        let mut p = project(&[("T1", 0), ("T2", 0), ("T3", 0), ("T4", 0), ("T5", 0)]);
        p.move_tracks(&ids(&p, &["T1", "T4"]), 3, 1);
        expect(&p, &[("T2", 0), ("T3", 1), ("T1", 0), ("T4", -1), ("T5", 0)]);
    }

    // "Track: Move tracks up/down" (43647/43648) in REAPER 7.73.
    #[test]
    fn steps_match_reaper() {
        let f4 = &[("T1", 1), ("T2", 0), ("T3", -1), ("T4", 0)];
        let cases: &[(&[&str], bool, Spec)] = &[
            (&["T2", "T3"], true, &[("T2", 0), ("T3", 0), ("T1", 0), ("T4", 0)]),
            (&["T4"], true, &[("T1", 1), ("T2", 0), ("T4", 0), ("T3", -1)]),
            (&["T1"], false, &[("T4", 0), ("T1", 1), ("T2", 0), ("T3", -1)]),
            (&["T3"], false, &[("T1", 1), ("T2", -1), ("T4", 0), ("T3", 0)]),
            (&["T2"], true, &[("T2", 0), ("T1", 1), ("T3", -1), ("T4", 0)]),
        ];
        for &(sel, up, want) in cases {
            let mut p = project(f4);
            assert!(p.step_tracks(&ids(&p, sel), up));
            expect(&p, want);
        }
        let mut p = project(f4);
        assert!(!p.step_tracks(&ids(&p, &["T1"]), true));
    }

    // "Track: Move tracks to new folder" (42785) in REAPER 7.73.
    #[test]
    fn new_folder_matches_reaper() {
        let cases: &[(Spec, &[&str], Spec)] = &[
            (
                &[("T1", 0), ("T2", 0), ("T3", 0), ("T4", 0), ("T5", 0)],
                &["T2", "T4"],
                &[("T1", 0), ("N", 1), ("T2", 0), ("T4", -1), ("T3", 0), ("T5", 0)],
            ),
            (
                &[("T1", 1), ("T2", 0), ("T3", -1), ("T4", 0), ("T5", 0)],
                &["T2", "T3"],
                &[("T1", 1), ("N", 1), ("T2", 0), ("T3", -2), ("T4", 0), ("T5", 0)],
            ),
            (
                &[("T1", 1), ("T2", 0), ("T3", -1), ("T4", 0)],
                &["T1"],
                &[("N", 1), ("T1", 1), ("T2", 0), ("T3", -2), ("T4", 0)],
            ),
            (
                &[("T1", 1), ("T2", 0), ("T3", -1), ("T4", 0), ("T5", 0)],
                &["T3", "T4"],
                &[("T1", 1), ("T2", 0), ("N", 1), ("T3", 0), ("T4", -2), ("T5", 0)],
            ),
        ];
        for &(spec, sel, want) in cases {
            let mut p = project(spec);
            let sel = ids(&p, sel);
            assert!(p.move_to_new_folder(&sel, Track::new("N")).is_some());
            expect(&p, want);
        }
    }

    // "Track: Cycle track folder state" (1041) and "Cycle folder collapsed state" (1042).
    #[test]
    fn cycles_match_reaper() {
        let cycle = |spec: &[(&str, i32)], name: &str| {
            let mut p = project(spec);
            let id = ids(&p, &[name])[0];
            (0..4)
                .map(|_| {
                    p.cycle_folder_state(id);
                    p.track(id).unwrap().folder_depth
                })
                .collect::<Vec<_>>()
        };
        let f5 = &[("T1", 1), ("T2", 0), ("T3", 0), ("T4", -1), ("T5", 0)];
        assert_eq!(cycle(&[("T1", 0), ("T2", 0), ("T3", 0)], "T2"), [1, 0, 1, 0]);
        assert_eq!(cycle(f5, "T2"), [1, -1, 0, 1]);
        assert_eq!(cycle(f5, "T4"), [0, 1, -1, 0]);

        let mut p = project(f5);
        let [t1, t2] = ids(&p, &["T1", "T2"])[..] else { unreachable!() };
        let compact: Vec<u8> = (0..4)
            .map(|_| {
                p.cycle_folder_compact(t1);
                p.track(t1).unwrap().folder_compact
            })
            .collect();
        assert_eq!(compact, [1, 2, 0, 1]);
        p.cycle_folder_compact(t2);
        assert_eq!(p.track(t2).unwrap().folder_compact, 0);
    }

    // "Track: Remove tracks" (40005) in REAPER 7.73.
    #[test]
    fn removing_keeps_folders_closed() {
        let f5 = &[("T1", 1), ("T2", 0), ("T3", -1), ("T4", 0), ("T5", 0)];
        let mut p = project(f5);
        assert_eq!(p.remove_tracks(&ids(&p, &["T1"])).len(), 3);
        expect(&p, &[("T4", 0), ("T5", 0)]);
        let mut p = project(f5);
        p.remove_tracks(&ids(&p, &["T3"]));
        expect(&p, &[("T1", 1), ("T2", -1), ("T4", 0), ("T5", 0)]);
        let mut p = project(&[("T1", 1), ("T2", 1), ("T3", 0), ("T4", -2), ("T5", 0), ("T6", 0)]);
        p.remove_tracks(&ids(&p, &["T2"]));
        expect(&p, &[("T1", 0), ("T5", 0), ("T6", 0)]);
    }

    #[test]
    fn removing_one_track_keeps_the_tree_around_it() {
        let f6 = &[("T1", 1), ("T2", 1), ("T3", 0), ("T4", -2), ("T5", 1), ("T6", -1)];
        let mut p = project(f6);
        p.remove_track(ids(&p, &["T4"])[0]);
        expect(&p, &[("T1", 1), ("T2", 1), ("T3", -2), ("T5", 1), ("T6", -1)]);
        let mut p = project(f6);
        p.remove_track(ids(&p, &["T2"])[0]);
        expect(&p, &[("T1", 1), ("T3", 0), ("T4", -1), ("T5", 1), ("T6", -1)]);
        let mut p = project(f6);
        p.remove_track(ids(&p, &["T6"])[0]);
        expect(&p, &[("T1", 1), ("T2", 1), ("T3", 0), ("T4", -2), ("T5", 0)]);
        let mut p = project(f6);
        p.remove_track(ids(&p, &["T1"])[0]);
        expect(&p, &[("T2", 1), ("T3", 0), ("T4", -1), ("T5", 1), ("T6", -1)]);
    }

    #[test]
    fn collapsed_folders_hide_and_shrink() {
        let mut p = project(&[("F", 1), ("G", 1), ("A", -1), ("B", -1), ("C", 0)]);
        assert!(p.row_shows().iter().all(|s| *s == RowShow::Normal));
        p.tracks[1].folder_compact = 1;
        assert_eq!(p.row_shows()[2], RowShow::Small);
        p.tracks[0].folder_compact = 2;
        use RowShow::*;
        assert_eq!(p.row_shows(), [Normal, Hidden, Hidden, Hidden, Normal]);
        p.tracks[0].folder_compact = 1;
        assert_eq!(p.row_shows(), [Normal, Small, Small, Small, Normal]);
        p.tracks[1].mixer_collapsed = true;
        assert_eq!(p.mixer_hidden(), [false, false, true, false, false]);
    }

    #[test]
    fn moves_into_folders_by_position() {
        let mut p = project(F);
        let [f, a, b, c, d] = ids(&p, &["F", "A", "B", "C", "D"])[..] else { unreachable!() };
        assert_eq!(p.move_into(&[d], Some(f), Some(1)), Some(true));
        expect(&p, &[("F", 1), ("A", 0), ("D", 0), ("B", -1), ("C", 0)]);
        assert_eq!(p.move_into(&[a], Some(f), None), Some(true));
        expect(&p, &[("F", 1), ("D", 0), ("B", 0), ("A", -1), ("C", 0)]);
        assert_eq!(p.move_into(&[d], Some(c), None), Some(true));
        expect(&p, &[("F", 1), ("B", 0), ("A", -1), ("C", 1), ("D", -1)]);
        assert_eq!(p.move_into(&[c], None, Some(0)), Some(true));
        expect(&p, &[("C", 1), ("D", -1), ("F", 1), ("B", 0), ("A", -1)]);
        assert_eq!(p.move_into(&[f], Some(b), None), None, "not into itself");
        assert!(p.move_to_index(&[b], 0));
        expect(&p, &[("B", 0), ("C", 1), ("D", -1), ("F", 1), ("A", -1)]);
        assert!(p.move_to_index(&[f], 0));
        expect(&p, &[("F", 1), ("A", -1), ("B", 0), ("C", 1), ("D", -1)]);
    }
}

#[cfg(test)]
mod selection_tests {
    use crate::rpp::{project_from_rpp, project_to_rpp};

    #[test]
    fn the_selection_is_the_tracks_sel_lines() {
        let text = "<REAPER_PROJECT 0.1 \"7.73\"\n  <TRACK\n    NAME a\n    SEL 0\n  >\n  <TRACK\n    NAME b\n    SEL 1\n  >\n  <TRACK\n    NAME c\n  >\n>\n";
        let mut p = project_from_rpp(text).unwrap();
        assert_eq!(p.saved_selection(), [p.tracks[1].id]);
        let (a, c) = (p.tracks[0].id, p.tracks[2].id);
        p.store_selection(&[a, c]);
        let back = project_from_rpp(&project_to_rpp(&p)).unwrap();
        let names: Vec<&str> = back.tracks.iter().filter(|t| t.saved_selected()).map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["a", "c"]);
        assert_eq!(project_to_rpp(&back).matches("SEL ").count(), 3);
    }
}
