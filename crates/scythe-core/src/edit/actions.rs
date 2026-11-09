//! REAPER's editing actions by command id, run on a project and an item selection. The
//! desktop app, ReaScript's `Main_OnCommand` and the session (for the ones that need no
//! selection) all run them through [`run`].

use super::nudge::{Nudge, NudgeContext};
use super::ops::{ExplodeMode, FadeTarget, RazorArea};
use super::{GridKind, LOCK_ALL, LOCK_ENABLED, LOCK_MODES, RippleMode};
use crate::{ItemId, Project, TimeRange};

/// What an editing action works on besides the project; it changes the selection, the
/// cursor and the razor areas the way REAPER's action does.
pub struct EditContext<'a> {
    pub items: Vec<ItemId>,
    pub cursor: f64,
    /// Seconds per pixel, for "a bit" and the pixel nudges (REAPER's default zoom is 100
    /// pixels a second).
    pub pixel: f64,
    pub razor: Vec<RazorArea>,
    /// The last nudge dialog settings.
    pub nudge: Nudge,
    /// The transients of an item, in project time; only tab-to-transient and dynamic split
    /// ask.
    pub transients: &'a dyn Fn(&Project, ItemId) -> Vec<f64>,
}

impl<'a> EditContext<'a> {
    pub fn new(items: Vec<ItemId>, cursor: f64, transients: &'a dyn Fn(&Project, ItemId) -> Vec<f64>) -> Self {
        Self { items, cursor, pixel: 0.01, razor: Vec::new(), nudge: Nudge::default(), transients }
    }
}

/// No transients, for callers without media.
pub fn no_transients(_: &Project, _: ItemId) -> Vec<f64> {
    Vec::new()
}

/// Grid divisions of REAPER's "Grid: Set to …" actions, in whole notes.
const GRID_SETS: [(u32, f64); 22] = [
    (41047, 1.0 / 128.0),
    (40774, 1.0 / 64.0),
    (41212, 1.0 / 48.0),
    (40775, 1.0 / 32.0),
    (41213, 1.0 / 24.0),
    (42001, 1.0 / 18.0),
    (40776, 1.0 / 16.0),
    (40777, 1.0 / 12.0),
    (42002, 1.0 / 10.0),
    (42003, 1.0 / 9.0),
    (40778, 1.0 / 8.0),
    (42004, 1.0 / 7.0),
    (41214, 1.0 / 6.0),
    (42005, 1.0 / 5.0),
    (40779, 1.0 / 4.0),
    (42000, 1.0 / 3.0),
    (40780, 1.0 / 2.0),
    (42007, 2.0 / 3.0),
    (40781, 1.0),
    (41210, 2.0),
    (42006, 3.0),
    (41211, 4.0),
];

/// The actions here that need no item selection, so the session runs them without a window.
pub const SESSION_IDS: &[u32] = &[
    1155, 40309, 40310, 40311, 41990, 41991, 1135, 40569, 40570, 40567, 40568, 40571, 40572, 40573, 40574, 40575,
    40576, 40577, 40578, 40579, 40580, 40581, 40582, 40583, 40584, 40585, 40586, 40587, 40588, 40589, 40590, 40591,
    40592, 40593, 40594, 40595, 40596, 40597, 40598, 40599, 40600, 40627, 40628, 40629, 1156, 40041, 41118, 41119, 1157, 40753, 40754, 40145, 40923, 42304,
    40725, 40783, 40786, 41047, 40774, 41212, 40775, 41213, 42001, 40776, 40777, 42002, 42003, 40778, 42004, 41214,
    42005, 40779, 42000, 40780, 42007, 40781, 41210, 42006, 41211,
];

/// The actions here that work on the selected items or razor areas.
pub const SELECTION_IDS: &[u32] = &[
    40006, 40687, 40688, 40689, 40032, 40033, 40034, 40224, 40642, 40643, 40438, 40543, 40131, 40129, 40126, 41059,
    40916, 41514, 41515, 41516, 41517, 41518, 41519, 41836, 41520, 41521, 41522, 41523, 41524, 41525, 41526, 41837,
    41527, 41528, 41529, 41530, 41531, 41532, 41533, 41838, 41534, 40119, 40120, 40793, 40794, 41249, 41250, 40225,
    40226, 40227, 40228, 40375, 40376, 42399, 42400, 42402, 42403, 42406,
];

/// Whether `run` handles `id`.
pub fn handles(id: u32) -> bool {
    SESSION_IDS.contains(&id) || SELECTION_IDS.contains(&id)
}

/// The on/off state of a toggle action here, `None` for one without a state.
pub fn toggle_state(p: &Project, id: u32) -> Option<bool> {
    let e = &p.editing;
    Some(match id {
        1155 => e.ripple != RippleMode::Off,
        40309 => e.ripple == RippleMode::Off,
        40310 | 41990 => e.ripple == RippleMode::PerTrack,
        40311 | 41991 => e.ripple == RippleMode::AllTracks,
        1135 => e.lock & LOCK_ENABLED != 0,
        1156 => e.grouping,
        40041 => e.auto_crossfade,
        1157 => e.grid.snap,
        40145 => e.grid.lines,
        42304 => e.grid.kind == GridKind::Swing,
        40725 | 40923 => e.grid.kind == GridKind::Measure,
        _ => {
            let mode = LOCK_MODES.iter().find(|m| m.2 == id)?;
            e.lock & mode.0 != 0
        }
    })
}

fn set_grid(p: &mut Project, whole: f64) {
    let g = &mut p.editing.grid;
    g.qn = whole * 4.0;
    if g.kind == GridKind::Measure {
        g.kind = GridKind::Normal;
    }
}

fn lock_mode(p: &mut Project, id: u32) -> bool {
    // "Set", "Clear" and "Toggle" come in threes ending in the toggle's id.
    let Some(&(bit, _, toggle)) = LOCK_MODES.iter().find(|m| (m.2 - 2..=m.2).contains(&id)) else { return false };
    let l = &mut p.editing.lock;
    *l = match id + 2 - toggle {
        0 => *l | bit,
        1 => *l & !bit,
        _ => *l ^ bit,
    };
    true
}

fn fade_action(id: u32) -> Option<(FadeTarget, Option<usize>)> {
    let kind = |first: u32, last7: u32| match id {
        _ if (first..first + 6).contains(&id) => Some((id - first) as usize),
        _ if id == last7 => Some(6),
        _ => None,
    };
    if let Some(k) = kind(41514, 41836) {
        return Some((FadeTarget::In, Some(k)));
    }
    if let Some(k) = kind(41521, 41837) {
        return Some((FadeTarget::Out, Some(k)));
    }
    if let Some(k) = kind(41528, 41838) {
        return Some((FadeTarget::Crossfade, Some(k)));
    }
    match id {
        41520 => Some((FadeTarget::In, None)),
        41527 => Some((FadeTarget::Out, None)),
        41534 => Some((FadeTarget::Crossfade, None)),
        _ => None,
    }
}

/// Runs the actions that change only the cursor or the razor areas (tab-to-transient,
/// moving razor areas). False for any other action.
pub fn run_view(p: &Project, ctx: &mut EditContext, id: u32) -> bool {
    match id {
        40375 | 40376 => {
            let mut all: Vec<f64> = ctx.items.iter().flat_map(|i| (ctx.transients)(p, *i)).collect();
            all.sort_by(f64::total_cmp);
            let next = if id == 40375 {
                all.into_iter().find(|t| *t > ctx.cursor + 1e-6)
            } else {
                all.into_iter().rev().find(|t| *t < ctx.cursor - 1e-6)
            };
            if let Some(t) = next {
                ctx.cursor = t;
            }
            true
        }
        42399 | 42400 => {
            for a in ctx.razor.iter_mut() {
                let len = a.range.length();
                let d = if id == 42399 { len } else { -len.min(a.range.start) };
                a.range = TimeRange::new(a.range.start + d, a.range.end + d);
            }
            true
        }
        42402 | 42403 => {
            let order: Vec<_> = p.tracks.iter().map(|t| t.id).collect();
            for a in ctx.razor.iter_mut() {
                let Some(i) = order.iter().position(|t| *t == a.track) else { continue };
                let j = if id == 42403 { (i + 1).min(order.len() - 1) } else { i.saturating_sub(1) };
                a.track = order[j];
                a.envelope = None;
            }
            true
        }
        42406 => {
            ctx.razor.clear();
            true
        }
        _ => false,
    }
}

/// Runs editing action `id` on the project. Answers the undo label, or `None` when `id` is
/// not one of these actions.
pub fn run(p: &mut Project, ctx: &mut EditContext, id: u32) -> Option<&'static str> {
    if run_view(p, ctx, id) {
        return Some("");
    }
    if let Some(&(_, whole)) = GRID_SETS.iter().find(|g| g.0 == id) {
        set_grid(p, whole);
        return Some("Set grid");
    }
    if lock_mode(p, id) {
        return Some("Locking mode");
    }
    if let Some((target, kind)) = fade_action(id) {
        match kind {
            Some(k) => p.set_fade_type(&ctx.items, target, k),
            None if target == FadeTarget::Crossfade => {
                let next = first_crossfade_kind(p, &ctx.items).map_or(0, |k| (k + 1) % 7);
                p.set_fade_type(&ctx.items, target, next);
            }
            None => p.cycle_fade_type(&ctx.items, target == FadeTarget::Out),
        }
        return Some("Set fade shape");
    }
    let items = ctx.items.clone();
    let step = |p: &Project| {
        let at = items.iter().filter_map(|i| p.find_item(*i)).map(|(_, it)| it.position).fold(f64::INFINITY, f64::min);
        p.editing.grid.step_seconds(&p.tempo, if at.is_finite() { at } else { 0.0 })
    };
    let e = &mut p.editing;
    let label = match id {
        1155 => {
            e.ripple = e.ripple.cycled();
            "Cycle ripple editing mode"
        }
        40309 => {
            e.ripple = RippleMode::Off;
            "Set ripple editing off"
        }
        40310 => {
            e.ripple = RippleMode::PerTrack;
            "Set ripple editing per-track"
        }
        40311 => {
            e.ripple = RippleMode::AllTracks;
            "Set ripple editing all tracks"
        }
        41990 | 41991 => {
            let mode = if id == 41990 { RippleMode::PerTrack } else { RippleMode::AllTracks };
            e.ripple = if e.ripple == mode { RippleMode::Off } else { mode };
            "Toggle ripple editing"
        }
        1135 => {
            e.lock ^= LOCK_ENABLED;
            "Toggle locking"
        }
        40569 | 40570 => {
            e.lock = if id == 40569 { e.lock | LOCK_ENABLED } else { e.lock & !LOCK_ENABLED };
            "Locking"
        }
        40567 => {
            e.lock = 0;
            "Clear all lock modes"
        }
        40568 => {
            e.lock = LOCK_ALL | LOCK_ENABLED;
            "Set all lock modes"
        }
        1156 => {
            e.grouping ^= true;
            "Toggle item grouping"
        }
        40041 | 41118 | 41119 => {
            e.auto_crossfade = match id {
                41118 => true,
                41119 => false,
                _ => !e.auto_crossfade,
            };
            "Auto-crossfade"
        }
        1157 | 40753 | 40754 => {
            e.grid.snap = match id {
                40753 => false,
                40754 => true,
                _ => !e.grid.snap,
            };
            "Snapping"
        }
        40145 => {
            e.grid.lines ^= true;
            "Toggle grid lines"
        }
        40923 => {
            e.grid.kind = GridKind::Measure;
            "Set measure grid"
        }
        40725 | 42304 => {
            let kind = if id == 40725 { GridKind::Measure } else { GridKind::Swing };
            e.grid.kind = if e.grid.kind == kind { GridKind::Normal } else { kind };
            "Grid"
        }
        40783 | 40786 => {
            e.grid.qn = if id == 40783 { e.grid.qn / 2.0 } else { e.grid.qn * 2.0 }.clamp(4.0 / 256.0, 64.0);
            "Adjust grid"
        }
        40006 => {
            if ctx.razor.is_empty() {
                p.delete_items(&items);
                ctx.items.clear();
            } else {
                p.remove_razor(&std::mem::take(&mut ctx.razor));
            }
            "Remove items"
        }
        40687..=40689 => {
            p.set_items_locked(&items, match id {
                40688 => Some(true),
                40689 => Some(false),
                _ => None,
            });
            "Item lock"
        }
        40032 => {
            p.group_items(&items);
            "Group items"
        }
        40033 => {
            p.ungroup_items(&items);
            "Remove items from group"
        }
        40034 => {
            let grouping = std::mem::replace(&mut p.editing.grouping, true);
            ctx.items = p.with_groups(&items);
            p.editing.grouping = grouping;
            "Select all items in groups"
        }
        40224 | 40642 | 40643 => {
            let mode = match id {
                40224 => ExplodeMode::AcrossTracks,
                40642 => ExplodeMode::InPlace,
                _ => ExplodeMode::InOrder,
            };
            let new = p.explode_takes(&items, mode);
            if mode == ExplodeMode::AcrossTracks {
                ctx.items = new;
            } else {
                ctx.items.retain(|i| p.find_item(*i).is_some());
            }
            "Explode takes"
        }
        40438 => {
            ctx.items = p.implode_across_tracks(&items);
            "Implode items across tracks into takes"
        }
        40543 => {
            ctx.items = p.implode_on_track(&items);
            "Implode items on same track into takes"
        }
        40131 => {
            p.crop_to_active_take(&items);
            "Crop to active take"
        }
        40129 => {
            p.delete_active_take(&items);
            "Delete active take"
        }
        40126 => {
            p.cycle_takes(&items, false);
            "Previous take"
        }
        41059 => {
            p.crossfade_overlapping(&items);
            "Crossfade overlapping items"
        }
        40916 => {
            if let Some(range) = p.time_selection {
                p.crossfade_in_range(&items, range);
            }
            "Crossfade items within time selection"
        }
        40119 | 40120 => {
            p.move_items(&items, if id == 40119 { ctx.pixel } else { -ctx.pixel });
            "Move items"
        }
        40793 | 40794 => {
            let s = step(p);
            p.move_items(&items, if id == 40794 { s } else { -s });
            "Move items by grid size"
        }
        41249 | 41250 => {
            let nudge = Nudge { reverse: id == 41250, ..ctx.nudge };
            let mut n = NudgeContext { cursor: ctx.cursor, pixel: ctx.pixel };
            ctx.items = nudge.apply(p, &items, &mut n);
            ctx.cursor = n.cursor;
            "Nudge items"
        }
        40225..=40228 => {
            let d = ctx.pixel;
            for id_ in &items {
                let locked = p.find_item(*id_).is_some_and(|(_, it)| p.edges_locked(it));
                let Some(it) = p.item_mut_anywhere(*id_).filter(|_| !locked) else { continue };
                match id {
                    40225 => it.trim_start(it.position - d),
                    40226 => it.trim_start(it.position + d),
                    40227 => it.trim_end(it.end() - d),
                    _ => it.trim_end(it.end() + d),
                }
            }
            "Trim items"
        }
        _ => return None,
    };
    Some(label)
}

fn first_crossfade_kind(p: &Project, items: &[ItemId]) -> Option<usize> {
    items.iter().filter_map(|i| p.find_item(*i)).find_map(|(_, it)| {
        (it.fade_in.auto_length > 0.0).then(|| it.fade_in.kind(false)).flatten()
    })
}

/// Splits each item at the given project times (dynamic split), keeping pieces at least
/// `min_length` long. Answers every resulting piece.
pub fn split_at_times(p: &mut Project, item: ItemId, times: &[f64], min_length: f64) -> Vec<ItemId> {
    let mut pieces = vec![item];
    let mut last = p.find_item(item).map_or(0.0, |(_, it)| it.position);
    let mut current = item;
    for &t in times {
        if t - last < min_length.max(crate::model::MIN_ITEM_LENGTH) {
            continue;
        }
        let Some(end) = p.find_item(current).map(|(_, it)| it.end()) else { break };
        if end - t < min_length.max(crate::model::MIN_ITEM_LENGTH) {
            break;
        }
        let right = p.split_items(&[current], t);
        let Some(&r) = right.iter().find(|r| p.find_item(**r).is_some()) else { continue };
        pieces.push(r);
        current = r;
        last = t;
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggles_and_grid_sets() {
        let mut p = Project::default();
        let none = no_transients;
        let mut ctx = EditContext::new(Vec::new(), 0.0, &none);
        run(&mut p, &mut ctx, 40310);
        assert_eq!(toggle_state(&p, 41990), Some(true));
        run(&mut p, &mut ctx, 41990);
        assert_eq!(p.editing.ripple, RippleMode::Off);
        run(&mut p, &mut ctx, 41213);
        assert!((p.editing.grid.qn - 1.0 / 6.0).abs() < 1e-12);
        run(&mut p, &mut ctx, 1135);
        assert_eq!(p.editing.lock, 16385);
        run(&mut p, &mut ctx, 40576);
        assert_eq!(p.editing.lock, 16387);
        run(&mut p, &mut ctx, 40575);
        assert_eq!(p.editing.lock, 16385);
        assert_eq!(toggle_state(&p, 40576), Some(false));
        run(&mut p, &mut ctx, 1156);
        assert!(!p.editing.grouping);
        for id in SESSION_IDS.iter().chain(SELECTION_IDS) {
            assert!(run(&mut p, &mut ctx, *id).is_some(), "{id}");
        }
    }
}
