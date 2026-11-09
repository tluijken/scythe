//! Edits on the project that follow its editing settings the way REAPER 7.73's actions do,
//! as measured with Lua probes:
//!
//! - Moving items moves their group mates (while grouping is on) but never locked items. With
//!   ripple per-track, later items and envelope points on the same tracks (at or after the
//!   earliest moved item's start) move along; with ripple all tracks, those on every track
//!   and markers and regions too.
//! - Deleting items does not follow groups. Ripple deletes remove the time each item covered:
//!   per-track on its own track, all tracks on every track (cutting other items there).
//! - Splitting splits group mates too and puts the right halves in a group of their own.
//! - Locked items (`LOCK 1`, or every item while locking covers full items) are not moved,
//!   split, deleted or rippled.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::{LOCK_ITEM_EDGES, LOCK_ITEM_FADES, LOCK_ITEM_LEFT_RIGHT, LOCK_ITEMS, RippleMode};
use crate::model::MIN_ITEM_LENGTH;
use crate::{Item, ItemId, Project, TimeRange, Track, TrackId};

const EPS: f64 = 1e-9;

/// How "explode takes" lays the takes out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplodeMode {
    /// One new track per take, under the item's track (the item stays).
    AcrossTracks,
    /// Stacked in the item's place, each a slice of the track height.
    InPlace,
    /// One after the other, starting where the item was.
    InOrder,
}

/// Which fade a fade-shape edit changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FadeTarget {
    In,
    Out,
    /// The fade-out of the earlier and the fade-in of the later of two crossfading items.
    Crossfade,
}

/// A razor edit area: a time range on a track's item lane, or on one of its envelopes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RazorArea {
    pub track: TrackId,
    pub range: TimeRange,
    pub envelope: Option<crate::EnvelopeKind>,
}

impl Project {
    pub fn item_mut_anywhere(&mut self, id: ItemId) -> Option<&mut Item> {
        self.tracks.iter_mut().find_map(|t| t.item_mut(id))
    }

    /// `items` with the other members of their groups, while item grouping is on.
    pub fn with_groups(&self, items: &[ItemId]) -> Vec<ItemId> {
        let mut out: Vec<ItemId> = items.to_vec();
        if self.editing.grouping {
            let groups: HashSet<u32> =
                items.iter().filter_map(|id| self.find_item(*id)).map(|(_, it)| it.group).filter(|g| *g > 0).collect();
            out.extend(self.all_items().filter(|it| groups.contains(&it.group)).map(|it| it.id));
        }
        dedup(out)
    }

    fn all_items(&self) -> impl Iterator<Item = &Item> {
        self.tracks.iter().flat_map(|t| t.items.iter())
    }

    /// Whether the item may not move.
    pub fn position_locked(&self, it: &Item) -> bool {
        it.locked || self.editing.locks(LOCK_ITEMS | LOCK_ITEM_LEFT_RIGHT)
    }

    /// Whether the item may not be deleted or split.
    pub fn delete_locked(&self, it: &Item) -> bool {
        it.locked || self.editing.locks(LOCK_ITEMS)
    }

    /// Whether the item's edges may not be dragged.
    pub fn edges_locked(&self, it: &Item) -> bool {
        it.locked || self.editing.locks(LOCK_ITEMS | LOCK_ITEM_EDGES)
    }

    /// Whether the item's fade and volume handles may not be dragged.
    pub fn fades_locked(&self, it: &Item) -> bool {
        it.locked || self.editing.locks(LOCK_ITEMS | LOCK_ITEM_FADES)
    }

    fn movable(&self, items: &[ItemId]) -> Vec<ItemId> {
        self.with_groups(items)
            .into_iter()
            .filter(|id| self.find_item(*id).is_some_and(|(_, it)| !self.position_locked(it)))
            .collect()
    }

    /// Moves the items (with their groups) by `delta` seconds, rippling later material as the
    /// ripple mode says. Answers the items that moved.
    pub fn move_items(&mut self, items: &[ItemId], delta: f64) -> Vec<ItemId> {
        let ids = self.movable(items);
        let Some(min_pos) = ids.iter().filter_map(|id| self.find_item(*id)).map(|(_, it)| it.position).reduce(f64::min)
        else {
            return Vec::new();
        };
        let delta = delta.max(-min_pos);
        if delta.abs() < EPS {
            return ids;
        }
        let moved: HashSet<ItemId> = ids.iter().copied().collect();
        let mut starts: BTreeMap<TrackId, f64> = BTreeMap::new();
        for t in &self.tracks {
            for it in t.items.iter().filter(|it| moved.contains(&it.id)) {
                let s = starts.entry(t.id).or_insert(it.position);
                *s = s.min(it.position);
            }
        }
        match self.editing.ripple {
            RippleMode::Off => {}
            RippleMode::PerTrack => {
                for (&track, &start) in &starts {
                    self.shift_track_after(track, start, delta, &moved);
                }
            }
            RippleMode::AllTracks => {
                let start = starts.values().copied().fold(f64::INFINITY, f64::min);
                let tracks: Vec<TrackId> = self.tracks.iter().map(|t| t.id).collect();
                for track in tracks {
                    self.shift_track_after(track, start, delta, &moved);
                }
                self.shift_markers_after(start, delta);
            }
        }
        for it in self.tracks.iter_mut().flat_map(|t| t.items.iter_mut()).filter(|it| moved.contains(&it.id)) {
            it.position += delta;
        }
        self.crossfade_tracks(starts.keys().copied());
        ids
    }

    /// Moves a track's unlocked items (except `except`) and envelope points at or after
    /// `from` by `delta`.
    fn shift_track_after(&mut self, track: TrackId, from: f64, delta: f64, except: &HashSet<ItemId>) {
        let locked: HashSet<ItemId> = self
            .track(track)
            .map(|t| t.items.iter().filter(|it| self.position_locked(it)).map(|it| it.id).collect())
            .unwrap_or_default();
        let Some(t) = self.track_mut(track) else { return };
        for it in t.items.iter_mut() {
            if it.position >= from - EPS && !except.contains(&it.id) && !locked.contains(&it.id) {
                it.position = (it.position + delta).max(0.0);
            }
        }
        for env in t.envelopes.iter_mut() {
            if delta > 0.0 {
                open_envelope(env, from, delta);
            } else {
                for p in env.points.iter_mut().filter(|p| p.time >= from - EPS) {
                    p.time = (p.time + delta).max(0.0);
                }
            }
        }
    }

    fn shift_markers_after(&mut self, from: f64, delta: f64) {
        for m in self.markers.iter_mut().filter(|m| m.position >= from - EPS) {
            m.position = (m.position + delta).max(0.0);
            if let Some(end) = m.region_end.as_mut() {
                *end = (*end + delta).max(0.0);
            }
        }
        self.markers.sort_by(|a, b| a.position.total_cmp(&b.position));
    }

    /// Recomputes automatic crossfades on the tracks.
    fn crossfade_tracks(&mut self, tracks: impl IntoIterator<Item = TrackId>) {
        for t in tracks.into_iter().collect::<Vec<_>>() {
            self.auto_crossfade(t);
        }
    }

    /// Deletes the items (not their group mates, as REAPER's "Remove items"), closing the
    /// gaps as the ripple mode says. Answers how many went.
    pub fn delete_items(&mut self, items: &[ItemId]) -> usize {
        let doomed: HashSet<ItemId> = items
            .iter()
            .copied()
            .filter(|id| self.find_item(*id).is_some_and(|(_, it)| !self.delete_locked(it)))
            .collect();
        let mut ranges: BTreeMap<Option<TrackId>, Vec<TimeRange>> = BTreeMap::new();
        for t in &self.tracks {
            for it in t.items.iter().filter(|it| doomed.contains(&it.id)) {
                let key = (self.editing.ripple == RippleMode::PerTrack).then_some(t.id);
                ranges.entry(key).or_default().push(TimeRange::new(it.position, it.end()));
            }
        }
        for t in self.tracks.iter_mut() {
            t.items.retain(|it| !doomed.contains(&it.id));
        }
        if self.editing.ripple != RippleMode::Off {
            for (track, list) in ranges {
                for range in merge(list).into_iter().rev() {
                    self.remove_time(range, track);
                }
            }
        }
        let touched: Vec<TrackId> = self.tracks.iter().map(|t| t.id).collect();
        self.crossfade_tracks(touched);
        doomed.len()
    }

    /// Removes the time `range` from one track, or from every track with markers and regions
    /// when `track` is `None`: what lies inside goes, items crossing the edges are cut, and
    /// later items and envelope points move left. Locked items stay where they are.
    pub fn remove_time(&mut self, range: TimeRange, track: Option<TrackId>) {
        if range.is_empty() {
            return;
        }
        let len = range.length();
        let scope: Vec<TrackId> = match track {
            Some(t) => vec![t],
            None => self.tracks.iter().map(|t| t.id).collect(),
        };
        for id in &scope {
            let locked: HashSet<ItemId> = self
                .track(*id)
                .map(|t| t.items.iter().filter(|it| self.delete_locked(it)).map(|it| it.id).collect())
                .unwrap_or_default();
            let Some(t) = self.track_mut(*id) else { continue };
            let mut added = Vec::new();
            for it in t.items.iter_mut().filter(|it| !locked.contains(&it.id)) {
                for at in [range.start, range.end] {
                    if let Some(right) = it.split_at(at) {
                        added.push(right);
                    }
                }
            }
            // A piece split off at the start may need its own cut at the end.
            let mut more = Vec::new();
            for it in added.iter_mut() {
                more.extend(it.split_at(range.end));
            }
            t.items.extend(added);
            t.items.extend(more);
            t.items.retain(|it| {
                locked.contains(&it.id) || it.end() <= range.start + EPS || it.position >= range.end - EPS
            });
            for it in t.items.iter_mut().filter(|it| !locked.contains(&it.id) && it.position >= range.end - EPS) {
                it.position = (it.position - len).max(0.0);
            }
            t.items.sort_by(|a, b| a.position.total_cmp(&b.position));
            for env in t.envelopes.iter_mut() {
                close_envelope(env, range);
            }
        }
        if track.is_none() {
            self.markers.retain(|m| {
                let inside = |t: f64| t > range.start + EPS && t < range.end - EPS;
                match m.region_end {
                    None => !inside(m.position),
                    Some(end) => !(m.position >= range.start - EPS && end <= range.end + EPS),
                }
            });
            for m in self.markers.iter_mut() {
                let squeeze = |t: f64| {
                    if t >= range.end - EPS {
                        t - len
                    } else if t > range.start {
                        range.start
                    } else {
                        t
                    }
                };
                m.position = squeeze(m.position);
                if let Some(end) = m.region_end.as_mut() {
                    *end = squeeze(*end);
                }
            }
        }
    }

    /// Makes room for `len` seconds of new material at `at` as the ripple mode says: on
    /// `tracks` per-track, on every track (markers too) for all tracks. `except` are items
    /// already placed there. Nothing happens with ripple off.
    pub fn ripple_insert(&mut self, tracks: &[TrackId], at: f64, len: f64, except: &[ItemId]) {
        let except: HashSet<ItemId> = except.iter().copied().collect();
        let scope: Vec<TrackId> = match self.editing.ripple {
            RippleMode::Off => return,
            RippleMode::PerTrack => tracks.to_vec(),
            RippleMode::AllTracks => {
                self.shift_markers_after(at, len);
                self.tracks.iter().map(|t| t.id).collect()
            }
        };
        for t in scope {
            self.shift_track_after(t, at, len, &except);
        }
    }

    /// Splits the items (with their groups) at `at`; the right halves of grouped items form
    /// new groups. Answers the right halves.
    pub fn split_items(&mut self, items: &[ItemId], at: f64) -> Vec<ItemId> {
        let ids: HashSet<ItemId> = self
            .with_groups(items)
            .into_iter()
            .filter(|id| self.find_item(*id).is_some_and(|(_, it)| !self.delete_locked(it)))
            .collect();
        let mut next_group = self.max_group() + 1;
        let mut regroup: HashMap<u32, u32> = HashMap::new();
        let mut out = Vec::new();
        for t in self.tracks.iter_mut() {
            let mut added = Vec::new();
            for it in t.items.iter_mut().filter(|it| ids.contains(&it.id)) {
                if let Some(mut right) = it.split_at(at) {
                    if right.group > 0 {
                        right.group = *regroup.entry(right.group).or_insert_with(|| {
                            next_group += 1;
                            next_group - 1
                        });
                    }
                    out.push(right.id);
                    added.push(right);
                }
            }
            t.items.extend(added);
            t.items.sort_by(|a, b| a.position.total_cmp(&b.position));
        }
        out
    }

    fn max_group(&self) -> u32 {
        self.all_items().map(|it| it.group).max().unwrap_or(0)
    }

    /// Puts the items in a new group. Answers its number.
    pub fn group_items(&mut self, items: &[ItemId]) -> u32 {
        let id = self.max_group() + 1;
        for it in self.tracks.iter_mut().flat_map(|t| t.items.iter_mut()).filter(|it| items.contains(&it.id)) {
            it.group = id;
        }
        id
    }

    /// Takes the items out of their groups; a group left with one item is dissolved.
    pub fn ungroup_items(&mut self, items: &[ItemId]) {
        for it in self.tracks.iter_mut().flat_map(|t| t.items.iter_mut()).filter(|it| items.contains(&it.id)) {
            it.group = 0;
        }
        let mut count: HashMap<u32, usize> = HashMap::new();
        for it in self.all_items().filter(|it| it.group > 0) {
            *count.entry(it.group).or_default() += 1;
        }
        for it in self.tracks.iter_mut().flat_map(|t| t.items.iter_mut()) {
            if count.get(&it.group) == Some(&1) {
                it.group = 0;
            }
        }
    }

    /// Locks or unlocks the items; toggles each when `locked` is `None`.
    pub fn set_items_locked(&mut self, items: &[ItemId], locked: Option<bool>) {
        for it in self.tracks.iter_mut().flat_map(|t| t.items.iter_mut()).filter(|it| items.contains(&it.id)) {
            it.locked = locked.unwrap_or(!it.locked);
        }
    }

    /// Lays each item's takes out as separate items. Answers the new items.
    pub fn explode_takes(&mut self, items: &[ItemId], mode: ExplodeMode) -> Vec<ItemId> {
        let mut out = Vec::new();
        if mode == ExplodeMode::AcrossTracks {
            for ti in (0..self.tracks.len()).rev() {
                let chosen: Vec<Item> = self.tracks[ti]
                    .items
                    .iter()
                    .filter(|it| items.contains(&it.id) && it.takes.len() > 1)
                    .cloned()
                    .collect();
                let Some(n) = chosen.iter().map(|it| it.takes.len()).max() else { continue };
                let mut new_tracks: Vec<Track> = (0..n).map(|_| Track::new("")).collect();
                for it in &chosen {
                    for (k, take) in it.takes.iter().enumerate() {
                        let piece = single_take(it, take.clone());
                        out.push(piece.id);
                        new_tracks[k].items.push(piece);
                    }
                }
                for (k, t) in new_tracks.into_iter().enumerate() {
                    self.tracks.insert(ti + 1 + k, t);
                }
            }
            return out;
        }
        for t in self.tracks.iter_mut() {
            let mut added = Vec::new();
            t.items.retain(|it| {
                if !items.contains(&it.id) || it.takes.len() < 2 {
                    return true;
                }
                let n = it.takes.len();
                for (k, take) in it.takes.iter().enumerate() {
                    let mut piece = single_take(it, take.clone());
                    match mode {
                        ExplodeMode::InPlace => piece.ypos = Some((k as f64 / n as f64, 1.0 / n as f64)),
                        _ => piece.position = it.position + k as f64 * it.length,
                    }
                    out.push(piece.id);
                    added.push(piece);
                }
                false
            });
            t.items.extend(added);
            t.items.sort_by(|a, b| a.position.total_cmp(&b.position));
        }
        out
    }

    /// Turns items on different tracks that overlap in time into one item whose takes they
    /// are, on the topmost track and in the place of its item there; each take keeps its
    /// timing. Answers the resulting items.
    pub fn implode_across_tracks(&mut self, items: &[ItemId]) -> Vec<ItemId> {
        let mut chosen: Vec<(usize, Item)> = Vec::new();
        for (ti, t) in self.tracks.iter().enumerate() {
            chosen.extend(t.items.iter().filter(|it| items.contains(&it.id)).map(|it| (ti, it.clone())));
        }
        chosen.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.position.total_cmp(&b.1.position)));
        let clusters = overlap_clusters(&chosen);
        let mut out = Vec::new();
        for cluster in clusters {
            let (base_track, base) = chosen[cluster[0]].clone();
            if cluster.len() < 2 {
                out.push(base.id);
                continue;
            }
            let mut merged = base.clone();
            let gone: HashSet<ItemId> = cluster[1..].iter().map(|&i| chosen[i].1.id).collect();
            for &i in &cluster[1..] {
                let it = &chosen[i].1;
                for take in &it.takes {
                    let mut take = take.clone();
                    let shift = (base.position - it.position) * take.playrate;
                    take.source_offset += shift;
                    take.shift_markers(-shift);
                    merged.takes.push(take);
                }
            }
            for t in self.tracks.iter_mut() {
                t.items.retain(|it| !gone.contains(&it.id));
            }
            if let Some(slot) = self.tracks[base_track].item_mut(base.id) {
                *slot = merged;
            }
            out.push(base.id);
        }
        out
    }

    /// Turns the items on each track into one item, the earliest, whose takes they are (in
    /// time order, each keeping its own start offset). Answers the resulting items.
    pub fn implode_on_track(&mut self, items: &[ItemId]) -> Vec<ItemId> {
        let mut out = Vec::new();
        for t in self.tracks.iter_mut() {
            let mut chosen: Vec<Item> = t.items.iter().filter(|it| items.contains(&it.id)).cloned().collect();
            if chosen.len() < 2 {
                out.extend(chosen.iter().map(|it| it.id));
                continue;
            }
            chosen.sort_by(|a, b| a.position.total_cmp(&b.position));
            let mut merged = chosen[0].clone();
            merged.takes = chosen.iter().flat_map(|it| it.takes.iter().cloned()).collect();
            merged.active_take = 0;
            let gone: HashSet<ItemId> = chosen[1..].iter().map(|it| it.id).collect();
            t.items.retain(|it| !gone.contains(&it.id));
            if let Some(slot) = t.item_mut(merged.id) {
                *slot = merged;
            }
            out.push(chosen[0].id);
        }
        out
    }

    /// Keeps only the active take of each item.
    pub fn crop_to_active_take(&mut self, items: &[ItemId]) {
        for it in self.tracks.iter_mut().flat_map(|t| t.items.iter_mut()).filter(|it| items.contains(&it.id)) {
            if let Some(take) = it.takes.get(it.active_take).cloned() {
                it.takes = vec![take];
                it.active_take = 0;
            }
        }
    }

    /// Deletes each item's active take; the take before it becomes active. An item's only
    /// take stays.
    pub fn delete_active_take(&mut self, items: &[ItemId]) {
        for it in self.tracks.iter_mut().flat_map(|t| t.items.iter_mut()).filter(|it| items.contains(&it.id)) {
            if it.takes.len() > 1 {
                it.takes.remove(it.active_take.min(it.takes.len() - 1));
                it.active_take = it.active_take.saturating_sub(1);
            }
        }
    }

    /// Steps each item's active take forwards or backwards, wrapping.
    pub fn cycle_takes(&mut self, items: &[ItemId], forward: bool) {
        for it in self.tracks.iter_mut().flat_map(|t| t.items.iter_mut()).filter(|it| items.contains(&it.id)) {
            let n = it.takes.len().max(1);
            it.active_take = if forward { (it.active_take + 1) % n } else { (it.active_take + n - 1) % n };
        }
    }

    /// Crossfades every overlapping pair of the items over their overlap (equal-power, as
    /// REAPER's default crossfade shape).
    pub fn crossfade_overlapping(&mut self, items: &[ItemId]) {
        for t in self.tracks.iter_mut() {
            for (a, b) in overlapping_pairs(t, items) {
                let overlap = t.items[a].end().min(t.items[b].end()) - t.items[b].position;
                set_crossfade(&mut t.items[a].fade_out, overlap);
                set_crossfade(&mut t.items[b].fade_in, overlap);
            }
        }
    }

    /// Extends neighbouring items so they overlap over `range` and crossfades them there.
    pub fn crossfade_in_range(&mut self, items: &[ItemId], range: TimeRange) {
        if range.is_empty() {
            return;
        }
        for t in self.tracks.iter_mut() {
            let mut order: Vec<usize> = (0..t.items.len()).filter(|&i| items.contains(&t.items[i].id)).collect();
            order.sort_by(|&a, &b| t.items[a].position.total_cmp(&t.items[b].position));
            for w in order.windows(2) {
                let (a, b) = (w[0], w[1]);
                let (ia, ib) = (&t.items[a], &t.items[b]);
                let joins = ia.position < range.start
                    && ib.end() > range.end
                    && ia.end() >= range.start - EPS
                    && ib.position <= range.end + EPS;
                if !joins {
                    continue;
                }
                t.items[a].trim_end(range.end);
                t.items[b].trim_start(range.start);
                set_crossfade(&mut t.items[a].fade_out, range.length());
                set_crossfade(&mut t.items[b].fade_in, range.length());
            }
        }
    }

    /// Gives the items' fades one of REAPER's seven fade types (0-based).
    pub fn set_fade_type(&mut self, items: &[ItemId], target: FadeTarget, kind: usize) {
        for t in self.tracks.iter_mut() {
            if target == FadeTarget::Crossfade {
                for (a, b) in overlapping_pairs(t, items) {
                    if t.items[a].fade_out.effective_length() > 0.0 {
                        t.items[a].fade_out.set_type(kind, true, true);
                        t.items[b].fade_in.set_type(kind, false, true);
                    }
                }
                continue;
            }
            for it in t.items.iter_mut().filter(|it| items.contains(&it.id)) {
                match target {
                    FadeTarget::In => it.fade_in.set_type(kind, false, false),
                    _ => it.fade_out.set_type(kind, true, false),
                }
            }
        }
    }

    /// The next of the seven fade types for each item's fade-in or fade-out.
    pub fn cycle_fade_type(&mut self, items: &[ItemId], fade_out: bool) {
        for it in self.tracks.iter_mut().flat_map(|t| t.items.iter_mut()).filter(|it| items.contains(&it.id)) {
            let fade = if fade_out { &mut it.fade_out } else { &mut it.fade_in };
            let next = fade.kind(fade_out).map_or(0, |k| (k + 1) % 7);
            fade.set_type(next, fade_out, false);
        }
    }

    /// Removes what lies inside razor areas: item pieces on item lanes, points on envelope
    /// lanes. With ripple per-track an item-lane area closes its gap on its track; with ripple
    /// all tracks, on every track.
    pub fn remove_razor(&mut self, areas: &[RazorArea]) {
        let mut ripple: Vec<(Option<TrackId>, TimeRange)> = Vec::new();
        for a in areas {
            match a.envelope {
                Some(kind) => {
                    if let Some(env) = self.track_mut(a.track).and_then(|t| t.envelope_mut_if(kind)) {
                        clear_envelope(env, a.range);
                    }
                }
                None => match self.editing.ripple {
                    RippleMode::Off => self.remove_range(a.range, Some(&[a.track]), false),
                    RippleMode::PerTrack => ripple.push((Some(a.track), a.range)),
                    RippleMode::AllTracks => ripple.push((None, a.range)),
                },
            }
        }
        ripple.sort_by(|a, b| b.1.start.total_cmp(&a.1.start));
        ripple.dedup_by(|a, b| a.0.is_none() && b.0.is_none() && a.1 == b.1);
        for (track, range) in ripple {
            self.remove_time(range, track);
        }
    }
}

impl Track {
    /// The envelope of `kind` if the track has one.
    pub fn envelope_mut_if(&mut self, kind: crate::EnvelopeKind) -> Option<&mut crate::Envelope> {
        self.envelopes.iter_mut().find(|e| e.kind == kind)
    }
}

/// How far before an opened gap REAPER puts the edge point that holds the shape there.
const EDGE: f64 = 0.0005;

fn insert_point(env: &mut crate::Envelope, time: f64, value: f64) {
    let at = env.points.partition_point(|p| p.time <= time);
    env.points.insert(at, crate::EnvPoint::new(time, value));
}

/// Opens `len` seconds at `at`: later points move right, and edge points keep the envelope's
/// value up to the gap and across it, as REAPER's "add envelope edge points when ripple
/// editing" (on by default) does.
fn open_envelope(env: &mut crate::Envelope, at: f64, len: f64) {
    let (Some(before), Some(here)) = (env.value_at(at - EDGE), env.value_at(at)) else { return };
    for p in env.points.iter_mut().filter(|p| p.time >= at - EPS) {
        p.time += len;
    }
    if at - EDGE > 0.0 {
        insert_point(env, at - EDGE, before);
    }
    insert_point(env, at + len, here);
}

/// Removes `range` from the envelope: points inside go, later ones move left, and edge
/// points keep the values at both ends of the cut.
fn close_envelope(env: &mut crate::Envelope, range: TimeRange) {
    let (Some(start), Some(end)) = (env.value_at(range.start), env.value_at(range.end)) else { return };
    let had_end = env.points.iter().any(|p| (p.time - range.end).abs() < EPS);
    env.points.retain(|p| p.time < range.start - EPS || p.time >= range.end - EPS);
    for p in env.points.iter_mut().filter(|p| p.time >= range.end - EPS) {
        p.time = (p.time - range.length()).max(0.0);
    }
    let at = env.points.partition_point(|p| p.time < range.start - EPS);
    env.points.insert(at, crate::EnvPoint::new(range.start, start));
    if !had_end {
        env.points.insert(at + 1, crate::EnvPoint::new(range.start, end));
    }
}

/// Removes the points inside `range` keeping the envelope's shape outside it: the value at
/// the start holds to the end, where the old value comes back, as a razor delete does.
fn clear_envelope(env: &mut crate::Envelope, range: TimeRange) {
    let (Some(start), Some(end)) = (env.value_at(range.start), env.value_at(range.end)) else { return };
    env.points.retain(|p| p.time < range.start - EPS || p.time > range.end + EPS);
    insert_point(env, range.start, start);
    insert_point(env, range.end, start);
    insert_point(env, range.end + 1e-6, end);
}

fn dedup(mut ids: Vec<ItemId>) -> Vec<ItemId> {
    let mut seen = HashSet::new();
    ids.retain(|id| seen.insert(*id));
    ids
}

/// Sorted, overlapping ranges joined.
fn merge(mut list: Vec<TimeRange>) -> Vec<TimeRange> {
    list.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out: Vec<TimeRange> = Vec::new();
    for r in list {
        match out.last_mut() {
            Some(last) if r.start <= last.end + EPS => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

/// A copy of `it` with `take` as its only take.
fn single_take(it: &Item, take: crate::Take) -> Item {
    let mut piece = it.duplicate();
    piece.takes = vec![take];
    piece.active_take = 0;
    piece.group = 0;
    piece
}

/// Groups of indices into `items` (sorted by track, then time) that overlap in time, through
/// any chain of overlaps; each group starts with its topmost, earliest item.
fn overlap_clusters(items: &[(usize, Item)]) -> Vec<Vec<usize>> {
    let n = items.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    for i in 0..n {
        for j in i + 1..n {
            let (a, b) = (&items[i].1, &items[j].1);
            if a.position < b.end() - EPS && b.position < a.end() - EPS {
                let (ri, rj) = (root(&mut parent, i), root(&mut parent, j));
                parent[ri.max(rj)] = ri.min(rj);
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let r = root(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    groups.into_values().collect()
}

/// Pairs (earlier, later) of the chosen items on a track that overlap, by index.
fn overlapping_pairs(t: &Track, items: &[ItemId]) -> Vec<(usize, usize)> {
    let mut order: Vec<usize> = (0..t.items.len()).filter(|&i| items.contains(&t.items[i].id)).collect();
    order.sort_by(|&a, &b| t.items[a].position.total_cmp(&t.items[b].position));
    let mut out = Vec::new();
    for (k, &a) in order.iter().enumerate() {
        for &b in &order[k + 1..] {
            let (ia, ib) = (&t.items[a], &t.items[b]);
            if ib.position < ia.end() - EPS && ia.ypos == ib.ypos && ib.position >= ia.position {
                out.push((a, b));
            }
        }
    }
    out
}

/// REAPER's default crossfade: equal power over `length`.
fn set_crossfade(fade: &mut crate::Fade, length: f64) {
    fade.auto_length = length.max(MIN_ITEM_LENGTH);
    fade.set_api_shape(7);
    fade.curve = 0.0;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EnvPoint, EnvelopeKind, Take};

    fn item(pos: f64, len: f64, name: &str) -> Item {
        let mut it = Item::new(pos, len, Take::from_file(name.into()));
        it.fade_in = crate::Fade::new(0.0);
        it.fade_out = crate::Fade::new(0.0);
        it
    }

    /// Two tracks with items at 0, 2 and 4 (one second each), a volume envelope on each
    /// with points at 1, 3.5 and 6, a marker at 3.5 and a region from 6 to 7, as the Lua
    /// probes built them.
    fn scene() -> (Project, Vec<Vec<ItemId>>) {
        let mut p = Project::default();
        let mut ids = Vec::new();
        for n in 0..2 {
            let t = p.add_track(format!("T{n}"));
            let tr = p.track_mut(t).unwrap();
            let mut row = Vec::new();
            for (k, pos) in [0.0, 2.0, 4.0].into_iter().enumerate() {
                let it = item(pos, 1.0, &format!("tone{k}.wav"));
                row.push(it.id);
                tr.items.push(it);
            }
            let env = tr.envelope_mut(EnvelopeKind::Volume);
            env.points = [1.0, 3.5, 6.0].into_iter().map(|t| EnvPoint::new(t, 0.5)).collect();
            ids.push(row);
        }
        p.add_marker(3.5, "m1");
        p.markers.push(crate::Marker { region_end: Some(7.0), ..p.markers[0].clone() });
        p.markers[1].position = 6.0;
        (p, ids)
    }

    fn positions(p: &Project, track: usize) -> Vec<f64> {
        let mut v: Vec<f64> = p.tracks[track].items.iter().map(|i| i.position).collect();
        v.sort_by(f64::total_cmp);
        v
    }

    fn points(p: &Project, track: usize) -> Vec<f64> {
        p.tracks[track].envelopes[0].points.iter().map(|p| p.time).collect()
    }

    fn close(a: &[f64], b: &[f64]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9)
    }

    #[test]
    fn ripple_delete_follows_the_mode() {
        let (mut p, ids) = scene();
        p.delete_items(&[ids[0][1]]);
        assert!(close(&positions(&p, 0), &[0.0, 4.0]));

        let (mut p, ids) = scene();
        p.editing.ripple = RippleMode::PerTrack;
        p.delete_items(&[ids[0][1]]);
        assert!(close(&positions(&p, 0), &[0.0, 3.0]));
        assert!(close(&positions(&p, 1), &[0.0, 2.0, 4.0]));
        assert!(close(&points(&p, 0), &[1.0, 2.0, 2.0, 2.5, 5.0]));
        assert!(close(&points(&p, 1), &[1.0, 3.5, 6.0]));
        assert_eq!(p.markers[0].position, 3.5);

        let (mut p, ids) = scene();
        p.editing.ripple = RippleMode::AllTracks;
        p.delete_items(&[ids[0][1]]);
        // REAPER removes the deleted item's time on every track, other items there included.
        assert!(close(&positions(&p, 1), &[0.0, 3.0]));
        assert!(close(&points(&p, 1), &[1.0, 2.0, 2.0, 2.5, 5.0]));
        assert_eq!((p.markers[0].position, p.markers[1].position, p.markers[1].region_end), (2.5, 5.0, Some(6.0)));
    }

    #[test]
    fn ripple_moves_later_material() {
        let (mut p, ids) = scene();
        p.editing.ripple = RippleMode::PerTrack;
        p.move_items(&[ids[0][1]], 0.5);
        assert!(close(&positions(&p, 0), &[0.0, 2.5, 4.5]));
        assert!(close(&points(&p, 0), &[1.0, 1.9995, 2.5, 4.0, 6.5]));
        assert!(close(&positions(&p, 1), &[0.0, 2.0, 4.0]));

        let (mut p, ids) = scene();
        p.editing.ripple = RippleMode::AllTracks;
        p.move_items(&[ids[0][1]], 0.5);
        assert!(close(&positions(&p, 1), &[0.0, 2.5, 4.5]));
        assert_eq!(p.markers[0].position, 4.0);
    }

    #[test]
    fn groups_move_and_split_together_and_locks_hold() {
        let (mut p, ids) = scene();
        p.group_items(&[ids[0][0], ids[1][0]]);
        p.move_items(&[ids[0][0]], 0.5);
        assert!(close(&positions(&p, 1), &[0.5, 2.0, 4.0]));
        // Deleting does not follow the group.
        p.delete_items(&[ids[0][0]]);
        assert_eq!(p.tracks[1].items.len(), 3);

        let (mut p, ids) = scene();
        p.group_items(&[ids[0][0], ids[1][0]]);
        let right = p.split_items(&[ids[0][0]], 0.5);
        assert_eq!(right.len(), 2);
        let groups: HashSet<u32> = right.iter().map(|id| p.find_item(*id).unwrap().1.group).collect();
        assert_eq!(groups.len(), 1);
        assert_ne!(groups.into_iter().next(), Some(p.find_item(ids[0][0]).unwrap().1.group));

        let (mut p, ids) = scene();
        p.editing.ripple = RippleMode::PerTrack;
        p.set_items_locked(&[ids[0][2]], Some(true));
        p.delete_items(&[ids[0][1]]);
        assert!(close(&positions(&p, 0), &[0.0, 4.0]));
        p.move_items(&[ids[0][2]], 1.0);
        assert!(close(&positions(&p, 0), &[0.0, 4.0]));

        let (mut p, ids) = scene();
        p.editing.lock = super::super::LOCK_ENABLED | LOCK_ITEMS;
        assert_eq!(p.delete_items(&[ids[0][1]]), 0);
    }

    #[test]
    fn ungrouping_dissolves_single_member_groups() {
        let (mut p, ids) = scene();
        p.group_items(&[ids[0][0], ids[1][0]]);
        p.ungroup_items(&[ids[0][0]]);
        assert_eq!(p.find_item(ids[1][0]).unwrap().1.group, 0);
        p.group_items(&[ids[0][0], ids[0][1], ids[1][0]]);
        p.ungroup_items(&[ids[0][0]]);
        assert!(p.find_item(ids[1][0]).unwrap().1.group > 0);
    }

    fn multi_take() -> (Project, ItemId) {
        let mut p = Project::default();
        let t = p.add_track("T");
        let mut it = item(1.0, 2.0, "tone0.wav");
        it.takes[0].source_offset = 0.5;
        for (name, offs) in [("tone1.wav", 1.0), ("tone2.wav", 0.0)] {
            let mut take = Take::from_file(name.into());
            take.source_offset = offs;
            it.takes.push(take);
        }
        it.active_take = 1;
        let id = it.id;
        p.track_mut(t).unwrap().items.push(it);
        (p, id)
    }

    #[test]
    fn takes_explode_implode_crop_and_delete() {
        let (mut p, id) = multi_take();
        let new = p.explode_takes(&[id], ExplodeMode::AcrossTracks);
        assert_eq!((p.tracks.len(), new.len()), (4, 3));
        assert_eq!(p.tracks[0].items[0].takes.len(), 3);
        assert_eq!(p.tracks[2].items[0].takes[0].source_offset, 1.0);

        let (mut p, id) = multi_take();
        p.explode_takes(&[id], ExplodeMode::InOrder);
        assert!(close(&positions(&p, 0), &[1.0, 3.0, 5.0]));

        let (mut p, id) = multi_take();
        p.explode_takes(&[id], ExplodeMode::InPlace);
        let ypos: Vec<_> = p.tracks[0].items.iter().map(|i| i.ypos.unwrap().0).collect();
        assert!(close(&ypos, &[0.0, 1.0 / 3.0, 2.0 / 3.0]));

        let (mut p, id) = multi_take();
        p.crop_to_active_take(&[id]);
        assert_eq!(p.tracks[0].items[0].takes[0].source_offset, 1.0);

        let (mut p, id) = multi_take();
        p.tracks[0].items[0].active_take = 2;
        p.delete_active_take(&[id]);
        assert_eq!((p.tracks[0].items[0].takes.len(), p.tracks[0].items[0].active_take), (2, 1));
    }

    #[test]
    fn implode_keeps_timing_across_tracks_and_offsets_on_one_track() {
        let mut p = Project::default();
        let mut ids = Vec::new();
        for (pos, offs) in [(1.0, 0.0), (1.2, 0.3), (0.8, 0.0)] {
            let t = p.add_track("");
            let mut it = item(pos, 2.0, "x.wav");
            it.takes[0].source_offset = offs;
            ids.push(it.id);
            p.track_mut(t).unwrap().items.push(it);
        }
        p.implode_across_tracks(&ids);
        let it = &p.tracks[0].items[0];
        let offs: Vec<f64> = it.takes.iter().map(|t| t.source_offset).collect();
        assert!(close(&offs, &[0.0, 0.1, 0.2]) && p.tracks[1].items.is_empty());

        let mut p = Project::default();
        let t = p.add_track("");
        let mut ids = Vec::new();
        for (pos, offs) in [(1.0, 0.0), (1.5, 0.3), (4.0, 0.0), (0.5, 0.0)] {
            let mut it = item(pos, 1.0, "x.wav");
            it.takes[0].source_offset = offs;
            ids.push(it.id);
            p.track_mut(t).unwrap().items.push(it);
        }
        p.implode_on_track(&ids);
        let it = &p.tracks[0].items[0];
        assert_eq!((p.tracks[0].items.len(), it.position, it.takes.len()), (1, 0.5, 4));
        assert_eq!(it.takes[2].source_offset, 0.3);
    }

    #[test]
    fn crossfades_and_fade_types() {
        let mut p = Project::default();
        let t = p.add_track("");
        let (a, b) = (item(0.0, 1.0, "a.wav"), item(1.0, 1.0, "b.wav"));
        let ids = [a.id, b.id];
        p.track_mut(t).unwrap().items.extend([a, b]);
        p.crossfade_in_range(&ids, TimeRange::new(0.8, 1.3));
        let (a, b) = (&p.tracks[0].items[0], &p.tracks[0].items[1]);
        assert!((a.end() - 1.3).abs() < 1e-9 && (b.position - 0.8).abs() < 1e-9);
        assert!((a.fade_out.auto_length - 0.5).abs() < 1e-9 && a.fade_out.api_shape() == 7);
        p.set_fade_type(&ids, FadeTarget::Crossfade, 2);
        let (a, b) = (&p.tracks[0].items[0], &p.tracks[0].items[1]);
        assert_eq!((a.fade_out.shape, a.fade_out.curve, b.fade_in.shape, b.fade_in.curve), (2, -1.0, 2, 1.0));
        p.set_fade_type(&ids[..1], FadeTarget::In, 3);
        assert_eq!((p.tracks[0].items[0].fade_in.shape, p.tracks[0].items[0].fade_in.curve), (3, -1.0));
        p.cycle_fade_type(&ids[..1], false);
        assert_eq!(p.tracks[0].items[0].fade_in.kind(false), Some(4));
    }

    #[test]
    fn razor_on_envelopes_removes_points_and_ripples() {
        let (mut p, _) = scene();
        let track = p.tracks[0].id;
        let range = TimeRange::new(3.0, 4.0);
        p.remove_razor(&[RazorArea { track, range, envelope: Some(EnvelopeKind::Volume) }]);
        assert!(close(&points(&p, 0), &[1.0, 3.0, 4.0, 4.000001, 6.0]));
        assert_eq!(p.tracks[0].items.len(), 3);

        let (mut p, _) = scene();
        p.editing.ripple = RippleMode::PerTrack;
        let track = p.tracks[0].id;
        p.remove_razor(&[RazorArea { track, range: TimeRange::new(2.5, 3.5), envelope: None }]);
        assert!(close(&positions(&p, 0), &[0.0, 2.0, 3.0]));
        assert!(close(&points(&p, 0), &[1.0, 2.5, 2.5, 5.0]));
    }
}
