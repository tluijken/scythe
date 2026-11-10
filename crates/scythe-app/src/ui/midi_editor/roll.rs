//! The piano roll and the named-notes (drum map) view: notes on rows of keys over the grid.
//!
//! Click on the grid adds a note, drag on empty grid draws one; a note is selected by a click
//! (Shift adds to the selection), dragged to move the selection, dragged by its right edge to
//! resize, right-clicked to delete and double-clicked for its properties. Right-drag selects
//! with a rectangle. The modifiers of a left drag are REAPER 7.73's defaults (its
//! `MM_CTX_MIDI_NOTE` and `MM_CTX_MIDI_PIANOROLL` contexts, tried in its editor):
//!
//! - on a note: Shift moves ignoring snap, Ctrl copies, Ctrl+Shift moves on one axis only,
//!   Alt edits the velocity (a step per pixel), Ctrl+Shift+Alt moves vertically ignoring the
//!   scale, Shift+Alt and Ctrl+Alt move;
//! - on empty grid: Shift draws ignoring snap, Ctrl copies the selected notes by the drag,
//!   Alt erases the notes it passes, Ctrl+Alt paints a straight line of notes and Shift+Alt
//!   one ignoring snap.
//!
//! A looped item's later passes show dimmed after the first, and edit the same notes. Notes of
//! other items show behind at their play rate and loops; clicking one makes its item the
//! edited one.

use egui::{Align2, Color32, CornerRadius, Id, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use scythe_core::midi::edit::{EditNote, Grid};
use scythe_core::midi::view::EditorMode;

use super::{EditorState, Take, apply_notes};
use crate::app::ScytheApp;
use crate::theme::{self, ACCENT, BG, LANE, RAISED, RAISED_HI, TEXT, TEXT_DIM, with_alpha};
use crate::ui::window::{WindowKind, scythe_window};

const KEYS_W: f32 = 44.0;
const NAMES_W: f32 = 120.0;
const ROW_H: f32 = 10.0;
const EDGE: f32 = 5.0;

/// The time axis the roll and the lanes share, in ticks of the edited take.
#[derive(Clone, Copy)]
pub(crate) struct Axis {
    pub left: f32,
    pub start_tick: f64,
    pub px_per_tick: f64,
    pub ppq: u32,
}

impl Axis {
    pub fn x(&self, tick: f64) -> f32 {
        self.left + ((tick - self.start_tick) * self.px_per_tick) as f32
    }

    pub fn tick(&self, x: f32) -> f64 {
        (self.start_tick + (x - self.left) as f64 / self.px_per_tick).max(0.0)
    }

    /// The grid line at or before `tick`.
    pub fn floor(&self, grid: &Grid, tick: f64) -> u64 {
        let lines = grid.lines_between(self.ppq, (tick - 2.0 * grid.qn * self.ppq as f64).max(-1.0), tick + 1e-6);
        lines.into_iter().filter(|l| *l <= tick + 1e-6).fold(0.0, f64::max).round() as u64
    }
}

pub(crate) fn is_black(key: u8) -> bool {
    matches!(key % 12, 1 | 3 | 6 | 8 | 10)
}

pub(crate) fn key_name(key: u8) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    format!("{}{}", NAMES[key as usize % 12], key as i32 / 12 - 1)
}

/// How a note drag moves the notes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MoveKind {
    /// Time and key, snapped (REAPER's "Move note").
    Free,
    IgnoreSnap,
    /// Only time or only key, whichever the pointer has moved further along.
    OneAxis,
    /// Only the key, ignoring key snap.
    VerticalIgnoringScale,
}

#[derive(Clone, Copy)]
enum Drag {
    Move {
        /// The row and point where the drag started.
        from_key: i32,
        from: Pos2,
        anchor: usize,
        kind: MoveKind,
        /// The moved notes are copies; the originals stay.
        copy: bool,
    },
    Resize {
        anchor: usize,
        /// The loop pass the grabbed note was drawn in.
        off: f64,
    },
    /// Alt-drag on a note: its velocity (the selection's, when it is selected) follows the
    /// pointer up and down.
    Velocity { from_y: f32, anchor: usize },
    /// Alt-drag on empty grid: notes the pointer passes are erased.
    Erase,
    /// Ctrl+Alt (Shift+Alt ignoring snap) drag on empty grid: a line of grid-long notes from
    /// the start to the pointer.
    Paint {
        start: f64,
        key: i32,
        snap: bool,
        /// The loop pass the drag started in.
        off: f64,
    },
    /// Right-drag marquee (REAPER's "MIDI editor" right drag): Shift adds to the selection,
    /// Ctrl toggles it.
    Select { from: Pos2, add: bool, toggle: bool },
    /// Left-drag on empty grid (REAPER's piano roll default): "Insert note, drag to extend or
    /// change pitch".
    Draw { start: u64, off: f64 },
}

/// The end of a note drawn from `start`: the snapped pointer, but at least one grid step on.
fn drawn_end(start: u64, step: u64, at: f64) -> u64 {
    (at.round().max(0.0) as u64).max(start + step.max(1))
}

/// The notes of a painted line from `start` on `key` to `end` on `to_key`: one per `step`, the
/// keys following the line. Snapped lines start on the grid, so their notes stay on it.
fn painted_line(start: f64, key: i32, end: f64, to_key: i32, step: u64, snapped: bool) -> Vec<(u64, i32)> {
    let step = step.max(1) as f64;
    let first = if snapped { start.round() } else { start };
    let n = ((end - first) / step).floor().max(0.0) as usize + 1;
    (0..n.min(4096))
        .map(|k| {
            let at = first + k as f64 * step;
            let f = if end > first { (at - first) / (end - first) } else { 0.0 };
            (at.round().max(0.0) as u64, (key as f64 + (to_key - key) as f64 * f).round() as i32)
        })
        .collect()
}

/// Which rows show and where: the piano roll shows every key from `top`, named notes one row
/// per named (or used) key.
struct Rows {
    keys: Option<Vec<u8>>,
    top: i32,
    rect: Rect,
}

impl Rows {
    fn y(&self, key: u8) -> Option<f32> {
        match &self.keys {
            Some(keys) => keys.iter().position(|&k| k == key).map(|i| self.rect.top() + i as f32 * ROW_H),
            None => Some(self.rect.top() + (self.top - key as i32) as f32 * ROW_H),
        }
    }

    fn key_at(&self, y: f32) -> Option<u8> {
        let i = ((y - self.rect.top()) / ROW_H).floor() as i32;
        match &self.keys {
            Some(keys) => keys.get(usize::try_from(i).ok()?).copied(),
            None => u8::try_from(self.top - i).ok().filter(|k| *k <= 127),
        }
    }

    /// The key of row `row`, from the top.
    fn key(&self, row: i32) -> Option<u8> {
        match &self.keys {
            Some(keys) => keys.get(usize::try_from(row).ok()?).copied(),
            None => u8::try_from(self.top - row).ok().filter(|k| *k <= 127),
        }
    }
}

pub(crate) fn shown(take: &Take, channel: u8) -> bool {
    take.view.channel_filter == 0 || take.view.channel_filter & (1 << channel) != 0
}

pub fn show(app: &mut ScytheApp, ui: &mut Ui, rect: Rect, take: &Take, state: &mut EditorState) -> Axis {
    let painter = ui.painter().clone();
    let named = take.view.mode == EditorMode::NamedNotes;
    let keys_w = if named { NAMES_W } else { KEYS_W };
    let roll = Rect::from_min_max(pos2(rect.left() + keys_w, rect.top()), rect.max);
    let keys = Rect::from_min_max(rect.min, pos2(roll.left() - 2.0, roll.bottom()));
    let ppq = take.src.ticks_per_qn;
    let mut notes = take.notes.clone();
    let snap = state.key_snap();

    let visible_rows = (roll.height() / ROW_H).floor().max(1.0) as i32;
    let rows = if named {
        let mut list: Vec<u8> = take.names.notes.iter().map(|n| n.1).collect();
        list.extend(notes.iter().map(|n| n.key));
        list.sort_unstable_by(|a, b| b.cmp(a));
        list.dedup();
        if list.is_empty() {
            list = (35..=81).rev().collect();
        }
        Rows { keys: Some(list), top: 127, rect: roll }
    } else {
        let top = state.top_key.unwrap_or_else(|| {
            let hi = notes.iter().map(|n| n.key).max().unwrap_or(72) as i32;
            let lo = notes.iter().map(|n| n.key).min().unwrap_or(48) as i32;
            ((hi + lo) / 2 + visible_rows / 2).clamp(visible_rows - 1, 127) as u8
        }) as i32;
        Rows { keys: None, top, rect: roll }
    };

    let end_tick = notes.iter().map(|n| n.end).max().unwrap_or(0).max(take.src.length_ticks) as f64;
    let end_tick = if take.passes.len() > 1 { end_tick.max(take.end_tick) } else { end_tick };
    let length_qn = (end_tick / ppq as f64).max(4.0);
    let fit = roll.width() / length_qn as f32;
    let px_per_qn = state.zoom.unwrap_or(fit).max(2.0);
    let max_scroll = (length_qn - roll.width() as f64 / px_per_qn as f64).max(0.0);
    state.scroll_qn = state.scroll_qn.clamp(0.0, max_scroll);
    let axis = Axis {
        left: roll.left(),
        start_tick: state.scroll_qn * ppq as f64,
        px_per_tick: px_per_qn as f64 / ppq as f64,
        ppq,
    };
    let grid = Grid { qn: take.view.grid_qn, swing: take.view.swing.unwrap_or(0.0) };

    // Background: rows, keyboard, grid.
    let clip = painter.with_clip_rect(roll);
    for row in 0..=visible_rows {
        let Some(key) = rows.key(row) else { continue };
        let y = roll.top() + row as f32 * ROW_H;
        let black = !named && is_black(key);
        let in_key = !take.view.key_snap || snap.contains(key);
        let lane = Rect::from_min_size(pos2(roll.left(), y), vec2(roll.width(), ROW_H));
        clip.rect_filled(lane, CornerRadius::ZERO, if black || !in_key { BG } else { LANE });
        let k = Rect::from_min_size(pos2(keys.left(), y), vec2(keys.width(), ROW_H - 1.0)).intersect(keys);
        if k.height() <= 0.0 {
            continue;
        }
        let (kb, fg) = if black { (BG, TEXT_DIM) } else { (RAISED_HI, TEXT) };
        painter.rect_filled(k, CornerRadius::same(2), kb);
        let label = match (named, take.names.note(take.view.channel, key)) {
            (true, Some(n)) => format!("{key} {n}"),
            (true, None) => format!("{key} {}", key_name(key)),
            (false, _) if key % 12 == 0 => key_name(key),
            _ => String::new(),
        };
        if !label.is_empty() {
            painter.text(k.left_center() + vec2(4.0, 0.0), Align2::LEFT_CENTER, label, theme::mono(8.0), fg);
        }
    }
    let (t0, t1) = (axis.tick(roll.left()), axis.tick(roll.right()));
    for line in grid.lines_between(ppq, t0 - 1.0, t1 + 1.0) {
        let beat = (line / ppq as f64).fract().abs() < 1e-6;
        clip.vline(axis.x(line), roll.y_range(), Stroke::new(1.0, if beat { RAISED_HI } else { RAISED }));
    }
    if state.step {
        clip.vline(axis.x(state.step_at as f64), roll.y_range(), Stroke::new(2.0, theme::GREEN));
    }

    // Other items, behind, where they play: at their take's play rate and in every pass of a
    // looped item. A click on one of their notes edits that item.
    let mut switch_to = None;
    let clicked_at = ui.input(|i| if i.pointer.primary_clicked() { i.pointer.interact_pos() } else { None });
    let tl = app.project.tempo.timeline();
    for &other in &state.others {
        let Some(src) = app.project.midi_source(other) else { continue };
        let Some((_, item)) = app.project.find_item(other) else { continue };
        let Some(model) = item.take() else { continue };
        for n in src.place(item, model, &tl) {
            let s = take.tick(tl.qn_at(n.start));
            let e = take.tick(tl.qn_at(n.end));
            let Some(y) = rows.y(n.key) else { continue };
            let r = Rect::from_min_max(pos2(axis.x(s), y + 2.0), pos2(axis.x(e).max(axis.x(s) + 3.0), y + ROW_H - 2.0));
            if !r.intersects(roll) {
                continue;
            }
            clip.rect_filled(r, CornerRadius::same(2), with_alpha(TEXT_DIM, 60));
            if clicked_at.is_some_and(|p| r.contains(p) && roll.contains(p)) {
                switch_to = Some(other);
            }
        }
    }

    let resp = ui.interact(roll, Id::unique(("midi-roll", take.id)), Sense::click_and_drag());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Canvas, true, "Piano roll"));
    let rect_at = |n: &EditNote, off: f64| {
        let x0 = axis.x(n.start as f64 + off);
        rows.y(n.key)
            .map(|y| Rect::from_min_max(pos2(x0, y), pos2(axis.x(n.end as f64 + off).max(x0 + 3.0), y + ROW_H)))
    };
    // The note under `p` in any pass: its index, whether on its right edge, and the pass.
    let hit = |notes: &[EditNote], p: Pos2| {
        take.passes.iter().find_map(|&off| {
            notes.iter().enumerate().rev().filter(|(_, n)| shown(take, n.channel)).find_map(|(i, n)| {
                let r = rect_at(n, off)?;
                r.contains(p).then_some((i, p.x > r.right() - EDGE, off))
            })
        })
    };
    let mods = ui.input(|i| i.modifiers);
    let (shift_held, ctrl_held, alt_held) = (mods.shift, mods.command, mods.alt);
    let secondary = ui.input(|i| i.pointer.button_down(egui::PointerButton::Secondary));
    let step = (grid.qn * ppq as f64).round().max(1.0) as u64;
    let snap_key = |key: i32| {
        let k = key.clamp(0, 127) as u8;
        if take.view.key_snap { snap.nearest(k) } else { k }
    };
    let in_source = |tick: f64| match take.passes.get(1) {
        Some(&len) => tick.rem_euclid(len),
        None => tick,
    };
    let drag_id = Id::unique(("midi-drag", take.id));
    if resp.drag_started()
        && let Some(p) = crate::ui::widgets::drag_origin(ui, &resp)
    {
        let from_key = |fallback: u8| rows.key_at(p.y).map_or(fallback as i32, |k| k as i32);
        let drag = match hit(&notes, p) {
            _ if secondary => Drag::Select { from: p, add: shift_held, toggle: ctrl_held },
            Some((i, _, _)) if alt_held && !ctrl_held && !shift_held => Drag::Velocity { from_y: p.y, anchor: i },
            Some((i, edge, off)) => {
                if !notes[i].selected {
                    if !shift_held {
                        notes.iter_mut().for_each(|n| n.selected = false);
                    }
                    notes[i].selected = true;
                }
                let (kind, copy) = match (ctrl_held, shift_held, alt_held) {
                    (false, true, false) => (MoveKind::IgnoreSnap, false),
                    (true, false, false) => (MoveKind::Free, true),
                    (true, true, false) => (MoveKind::OneAxis, false),
                    (true, true, true) => (MoveKind::VerticalIgnoringScale, false),
                    _ => (MoveKind::Free, false),
                };
                if edge && !ctrl_held && !alt_held {
                    Drag::Resize { anchor: i, off }
                } else {
                    Drag::Move { from_key: from_key(notes[i].key), from: p, anchor: i, kind, copy }
                }
            }
            None if ctrl_held && !alt_held && !shift_held => match notes.iter().position(|n| n.selected) {
                Some(anchor) => Drag::Move {
                    from_key: from_key(notes[anchor].key),
                    from: p,
                    anchor,
                    kind: MoveKind::Free,
                    copy: true,
                },
                None => Drag::Select { from: p, add: false, toggle: false },
            },
            None if alt_held && !ctrl_held && !shift_held => Drag::Erase,
            None if alt_held && (ctrl_held ^ shift_held) => {
                let at = in_source(axis.tick(p.x));
                let start = if shift_held { at.round() } else { axis.floor(&grid, at) as f64 };
                Drag::Paint { start, key: from_key(60), snap: !shift_held, off: axis.tick(p.x) - at }
            }
            None => match rows.key_at(p.y) {
                Some(key) => {
                    let at = in_source(axis.tick(p.x));
                    let start = if shift_held && !ctrl_held { at.round() as u64 } else { axis.floor(&grid, at) };
                    notes.iter_mut().for_each(|n| n.selected = false);
                    let before = notes.clone();
                    let mut n = EditNote::new(start, start + step, take.view.channel, snap_key(key as i32), 100);
                    n.selected = true;
                    notes.push(n);
                    notes.sort_by_key(|n| (n.start, n.key, n.channel));
                    apply_notes(app, take.id, &notes, "Insert note", true);
                    let draw = Drag::Draw { start, off: axis.tick(p.x) - at };
                    ui.data_mut(|d| d.insert_temp(drag_id, (draw, before)));
                    draw
                }
                None => Drag::Select { from: p, add: shift_held, toggle: ctrl_held },
            },
        };
        if !matches!(drag, Drag::Draw { .. }) {
            ui.data_mut(|d| d.insert_temp(drag_id, (drag, notes.clone())));
        }
    }
    let drag: Option<(Drag, Vec<EditNote>)> = ui.data(|d| d.get_temp(drag_id));
    if resp.dragged()
        && let (Some(p), Some((drag, before))) = (resp.interact_pointer_pos(), drag.clone())
    {
        match drag {
            Drag::Move { from_key, from, anchor, kind, copy } => {
                let a = &before[anchor];
                let moved = axis.tick(p.x) - axis.tick(from.x);
                let target = match kind {
                    MoveKind::IgnoreSnap => (a.start as f64 + moved).max(0.0).round() as i64,
                    _ => grid.nearest(ppq, a.start as f64 + moved).max(0.0).round() as i64,
                };
                let key = rows.key_at(p.y).map_or(from_key, |k| k as i32);
                let raw_dk = key - from_key;
                let mut dt = target - a.start as i64;
                let mut dk = match kind {
                    MoveKind::VerticalIgnoringScale => raw_dk,
                    _ => snap_key(a.key as i32 + raw_dk) as i32 - a.key as i32,
                };
                match kind {
                    MoveKind::OneAxis if (p.x - from.x).abs() >= (p.y - from.y).abs() => dk = 0,
                    MoveKind::OneAxis | MoveKind::VerticalIgnoringScale => dt = 0,
                    _ => {}
                }
                notes = before.clone();
                let shift = |n: &mut EditNote| {
                    let len = n.length();
                    n.start = (n.start as i64 + dt).max(0) as u64;
                    n.end = n.start + len;
                    n.key = (n.key as i32 + dk).clamp(0, 127) as u8;
                };
                if copy {
                    if dt != 0 || dk != 0 {
                        let mut copies: Vec<EditNote> = notes.iter().filter(|n| n.selected).cloned().collect();
                        copies.iter_mut().for_each(shift);
                        notes.iter_mut().for_each(|n| n.selected = false);
                        notes.extend(copies);
                        notes.sort_by_key(|n| (n.start, n.key, n.channel));
                    }
                    apply_notes(app, take.id, &notes, "Copy notes", true);
                } else {
                    notes.iter_mut().filter(|n| n.selected).for_each(shift);
                    apply_notes(app, take.id, &notes, "Move notes", true);
                }
            }
            Drag::Resize { anchor, off } => {
                let a = &before[anchor];
                let g = (grid.qn * ppq as f64).round().max(1.0) as i64;
                let end = (grid.nearest(ppq, axis.tick(p.x) - off).round() as i64).max(a.start as i64 + g);
                let de = end - a.end as i64;
                notes = before.clone();
                for n in notes.iter_mut().filter(|n| n.selected) {
                    n.end = (n.end as i64 + de).max(n.start as i64 + 1) as u64;
                }
                apply_notes(app, take.id, &notes, "Resize notes", true);
            }
            Drag::Velocity { from_y, anchor } => {
                let dv = (from_y - p.y).round() as i32;
                notes = before.clone();
                let all = before[anchor].selected;
                for (i, n) in notes.iter_mut().enumerate() {
                    if (all && n.selected) || i == anchor {
                        n.velocity = (n.velocity as i32 + dv).clamp(1, 127) as u8;
                    }
                }
                apply_notes(app, take.id, &notes, "Edit note velocity", true);
            }
            Drag::Erase => {
                if let Some((i, _, _)) = hit(&notes, p) {
                    notes.remove(i);
                    apply_notes(app, take.id, &notes, "Erase notes", true);
                }
            }
            Drag::Paint { start, key, snap: snapped, off } => {
                let now = (axis.tick(p.x) - off).max(start);
                let to_key = rows.key_at(p.y).map_or(key, |k| k as i32);
                notes = before.clone();
                notes.iter_mut().for_each(|n| n.selected = false);
                for (s, k) in painted_line(start, key, now, to_key, step, snapped) {
                    let mut n = EditNote::new(s, s + step, take.view.channel, snap_key(k), 100);
                    n.selected = true;
                    notes.push(n);
                }
                notes.sort_by_key(|n| (n.start, n.key, n.channel));
                apply_notes(app, take.id, &notes, "Insert notes", true);
            }
            Drag::Draw { start, off } => {
                let at = (axis.tick(p.x) - off).max(start as f64);
                let at = if shift_held { at } else { grid.nearest(ppq, at) };
                let end = drawn_end(start, if shift_held { 1 } else { step }, at);
                let key = rows.key_at(p.y).map_or(60, |k| snap_key(k as i32));
                notes = before.clone();
                let mut n = EditNote::new(start, end, take.view.channel, key, 100);
                n.selected = true;
                notes.push(n);
                notes.sort_by_key(|n| (n.start, n.key, n.channel));
                apply_notes(app, take.id, &notes, "Insert note", true);
            }
            Drag::Select { from, .. } => {
                let r = Rect::from_two_pos(from, p);
                painter.rect_filled(r, CornerRadius::same(2), with_alpha(ACCENT, 30));
                painter.rect_stroke(r, CornerRadius::same(2), Stroke::new(1.0, ACCENT), StrokeKind::Inside);
            }
        }
    }
    if resp.drag_stopped() {
        if let (Some((Drag::Select { from, add, toggle }, _)), Some(p)) = (drag, resp.interact_pointer_pos()) {
            let r = Rect::from_two_pos(from, p);
            for n in notes.iter_mut() {
                let inside = shown(take, n.channel)
                    && take.passes.iter().any(|&off| rect_at(n, off).is_some_and(|nr| nr.intersects(r)));
                n.selected = match (inside, toggle, add) {
                    (true, true, _) => !n.selected,
                    (true, false, _) => true,
                    (false, _, keep) => (keep || toggle) && n.selected,
                };
            }
            apply_notes(app, take.id, &notes, "Select notes", false);
        }
        ui.data_mut(|d| d.remove::<(Drag, Vec<EditNote>)>(drag_id));
        app.end_gesture();
    }
    if resp.clicked()
        && switch_to.is_none()
        && let Some(p) = resp.interact_pointer_pos()
    {
        match hit(&notes, p) {
            Some((i, _, _)) => {
                let on = !(shift_held && notes[i].selected);
                if !shift_held {
                    notes.iter_mut().for_each(|n| n.selected = false);
                }
                notes[i].selected = on;
                apply_notes(app, take.id, &notes, "Select notes", false);
            }
            None => {
                if let Some(key) = rows.key_at(p.y) {
                    let start = axis.floor(&grid, in_source(axis.tick(p.x)));
                    let len = (grid.qn * ppq as f64).round().max(1.0) as u64;
                    notes.iter_mut().for_each(|n| n.selected = false);
                    let mut n = EditNote::new(start, start + len, take.view.channel, snap_key(key as i32), 100);
                    n.selected = true;
                    notes.push(n);
                    notes.sort_by_key(|n| (n.start, n.key, n.channel));
                    apply_notes(app, take.id, &notes, "Insert note", false);
                }
            }
        }
    }
    if resp.secondary_clicked()
        && let Some(p) = resp.interact_pointer_pos()
        && let Some((i, _, _)) = hit(&notes, p)
    {
        if notes[i].selected {
            notes.retain(|n| !n.selected);
        } else {
            notes.remove(i);
        }
        apply_notes(app, take.id, &notes, "Delete notes", false);
    }
    let props_id = Id::unique(("midi-props", take.id));
    if resp.double_clicked()
        && let Some(p) = resp.interact_pointer_pos()
        && let Some((i, _, _)) = hit(&notes, p)
    {
        ui.data_mut(|d| d.insert_temp(props_id, i));
    }
    if let Some(p) = resp.hover_pos()
        && let Some((_, edge, _)) = hit(&notes, p)
    {
        ui.ctx().set_cursor_icon(if edge { egui::CursorIcon::ResizeHorizontal } else { egui::CursorIcon::Grab });
    }
    if resp.hovered() && ui.input(|i| i.key_pressed(egui::Key::Delete)) && notes.iter().any(|n| n.selected) {
        notes.retain(|n| !n.selected);
        apply_notes(app, take.id, &notes, "Delete notes", false);
    }

    for (k, &off) in take.passes.iter().enumerate() {
        if k > 0 {
            clip.vline(axis.x(off), roll.y_range(), Stroke::new(1.5, with_alpha(TEXT, 70)));
        }
        let x_range = (axis.x(off), axis.x(take.passes.get(k + 1).copied().unwrap_or(f64::MAX).min(end_tick)));
        if x_range.1 < roll.left() || x_range.0 > roll.right() {
            continue;
        }
        let pass = clip.with_clip_rect(Rect::from_x_y_ranges(x_range.0..=x_range.1, roll.y_range()).intersect(roll));
        for n in &notes {
            let Some(r) = rect_at(n, off) else { continue };
            let r = r.shrink2(vec2(0.0, 1.0));
            let shade = 0.45 + 0.55 * n.velocity as f32 / 127.0;
            let base = if n.muted { TEXT_DIM.gamma_multiply(0.5) } else { ACCENT.gamma_multiply(shade) };
            let base = if shown(take, n.channel) { base } else { with_alpha(base, 50) };
            let base = if k > 0 { with_alpha(base, 110) } else { base };
            pass.rect_filled(r, CornerRadius::same(2), base);
            let border =
                if n.selected { Stroke::new(1.5, TEXT) } else { Stroke::new(1.0, with_alpha(Color32::BLACK, 90)) };
            pass.rect_stroke(r, CornerRadius::same(2), border, StrokeKind::Inside);
        }
    }

    note_properties(app, ui, take, &mut notes, props_id);

    // The keyboard plays its keys on the virtual keyboard; with step input they go in.
    let kresp = ui.interact(keys, Id::unique(("midi-keys", take.id)), Sense::click());
    if kresp.clicked()
        && let Some(key) = kresp.interact_pointer_pos().and_then(|p| rows.key_at(p.y))
    {
        app.session.virtual_keyboard([0x90 | take.view.channel, key, 100]);
        app.session.virtual_keyboard([0x80 | take.view.channel, key, 0]);
    }

    // Wheel: keys up and down; Shift scrolls time, Ctrl zooms.
    if resp.hovered() || kresp.hovered() {
        let (dx, dy, mods) = ui.input(|i| (i.smooth_scroll_delta.x, i.smooth_scroll_delta.y, i.modifiers));
        if mods.ctrl && dy.abs() > 0.5 {
            state.zoom = Some((px_per_qn * (1.0 + dy * 0.002)).clamp(4.0, 2000.0));
        } else if (mods.shift || dx.abs() > dy.abs()) && (dx.abs() > 0.5 || dy.abs() > 0.5) {
            let d = if dx.abs() > 0.5 { dx } else { dy };
            state.scroll_qn = (state.scroll_qn - d as f64 / px_per_qn as f64).clamp(0.0, max_scroll);
        } else if !named && dy.abs() > 0.5 {
            state.top_key = Some((rows.top + (dy / ROW_H).round() as i32).clamp(visible_rows - 1, 127) as u8);
        } else if state.top_key.is_none() && !named {
            state.top_key = Some(rows.top as u8);
        }
    }
    if let Some(other) = switch_to {
        state.others.retain(|o| *o != other);
        state.others.push(take.id);
        app.view.midi_editor = Some(other);
        app.sel.item = Some(other);
    }
    axis
}

/// The properties of a double-clicked note: start, length, key, velocities, channel, mute.
fn note_properties(app: &mut ScytheApp, ui: &mut Ui, take: &Take, notes: &mut [EditNote], id: Id) {
    let Some(i) = ui.data(|d| d.get_temp::<usize>(id)) else { return };
    let Some(mut n) = notes.get(i).cloned() else {
        ui.data_mut(|d| d.remove::<usize>(id));
        return;
    };
    let ppq = take.src.ticks_per_qn as f64;
    let mut open = true;
    let mut changed = false;
    scythe_window("note-properties", format!("Note {}", key_name(n.key)), WindowKind::Dialog)
        .id(id.with("window"))
        .anchored()
        .show(ui.ctx(), &mut open, |ui| {
            egui::Grid::new(("note-props", i)).num_columns(2).show(ui, |ui| {
                let mut start = n.start as f64 / ppq;
                let mut length = n.length() as f64 / ppq;
                let mut channel = n.channel + 1;
                ui.label("Start (quarter notes)");
                changed |= ui.add(egui::DragValue::new(&mut start).speed(0.01).range(0.0..=f64::MAX)).changed();
                ui.end_row();
                ui.label("Length (quarter notes)");
                changed |= ui.add(egui::DragValue::new(&mut length).speed(0.01).range(1.0 / ppq..=f64::MAX)).changed();
                ui.end_row();
                ui.label("Key");
                changed |= ui.add(egui::DragValue::new(&mut n.key).range(0..=127)).changed();
                ui.end_row();
                ui.label("Velocity");
                changed |= ui.add(egui::DragValue::new(&mut n.velocity).range(1..=127)).changed();
                ui.end_row();
                ui.label("Off velocity");
                changed |= ui.add(egui::DragValue::new(&mut n.off_velocity).range(0..=127)).changed();
                ui.end_row();
                ui.label("Channel");
                changed |= ui.add(egui::DragValue::new(&mut channel).range(1..=16)).changed();
                ui.end_row();
                ui.label("Muted");
                changed |= ui.checkbox(&mut n.muted, "").changed();
                ui.end_row();
                n.start = (start * ppq).round() as u64;
                n.end = n.start + (length * ppq).round().max(1.0) as u64;
                n.channel = channel - 1;
            });
        });
    if changed {
        notes[i] = n;
        apply_notes(app, take.id, notes, "Note properties", true);
    }
    if !open {
        ui.data_mut(|d| d.remove::<usize>(id));
        app.end_gesture();
    }
}

#[cfg(test)]
mod tests {
    use super::{drawn_end, painted_line};

    #[test]
    fn a_drawn_note_follows_the_pointer_and_keeps_one_grid_step() {
        assert_eq!(drawn_end(960, 240, 1920.0), 1920);
        assert_eq!(drawn_end(960, 240, 900.0), 1200, "dragged left of its start");
        assert_eq!(drawn_end(960, 0, 960.0), 961);
    }

    #[test]
    fn a_painted_line_steps_down_the_keys_like_reapers() {
        // REAPER 7.73, Ctrl+Alt+drag on a 1/32 grid from C4 down to A#3.
        assert_eq!(painted_line(480.0, 60, 840.0, 58, 120, true), [(480, 60), (600, 59), (720, 59), (840, 58)]);
        assert_eq!(painted_line(480.0, 60, 470.0, 58, 120, true), [(480, 60)]);
    }
}
