//! Docked piano roll for one MIDI item: add notes by clicking, drag to move, drag the right
//! edge to resize, right-click to delete. The lane below edits velocities or, picked with its
//! label, a controller lane: click or drag paints values on the grid, right-click deletes.

use egui::{Align2, Color32, CornerRadius, Id, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use scythe_core::midi::{ControlLane, ControlPoint, MidiSource, Note};
use scythe_core::{ItemId, Source};

use crate::app::ScytheApp;
use crate::theme::{self, ACCENT, BG, LANE, PANEL, RAISED, RAISED_HI, TEXT, TEXT_DIM, with_alpha};
use crate::ui::widgets::text_pill;

const KEYS_W: f32 = 44.0;
const ROW_H: f32 = 10.0;
const VEL_H: f32 = 56.0;
const EDGE: f32 = 5.0;

/// Lanes below the roll: label, full name and what they edit (`None` for velocity).
const LANES: [(&str, &str, Option<ControlLane>); 7] = [
    ("Vel", "Velocity", None),
    ("Mod", "CC 1 modulation", Some(ControlLane::Cc(1))),
    ("Vol", "CC 7 volume", Some(ControlLane::Cc(7))),
    ("Pan", "CC 10 pan", Some(ControlLane::Cc(10))),
    ("Expr", "CC 11 expression", Some(ControlLane::Cc(11))),
    ("Sus", "CC 64 sustain", Some(ControlLane::Cc(64))),
    ("Pitch", "Pitch bend", Some(ControlLane::PitchBend)),
];

/// Grid choices in quarter notes.
const GRIDS: [(&str, f64); 4] = [("1/4", 1.0), ("1/8", 0.5), ("1/16", 0.25), ("1/32", 0.125)];

#[derive(Debug, Clone, Copy, Default)]
pub struct EditorState {
    pub grid: usize,
    /// Index into `LANES`.
    pub lane: usize,
    /// Highest visible key.
    pub top_key: Option<u8>,
}

#[derive(Clone, Copy)]
enum Drag {
    Move { note: usize, grab_qn: f64, grab_key: i32 },
    Resize { note: usize },
}

fn source_of(app: &ScytheApp, id: ItemId) -> Option<(MidiSource, String)> {
    let (_, item) = app.project.find_item(id)?;
    let Source::Raw(chunk) = &item.take()?.source else { return None };
    Some((MidiSource::parse(chunk)?, item.name().to_owned()))
}

fn apply(app: &mut ScytheApp, id: ItemId, notes: Vec<Note>, gesture: bool) {
    let label = "Edit MIDI";
    let f = move |p: &mut scythe_core::Project| {
        let Some(item) = p.tracks.iter_mut().find_map(|t| t.item_mut(id)) else { return };
        let active = item.active_take;
        let Some(take) = item.takes.get_mut(active) else { return };
        let Source::Raw(chunk) = &take.source else { return };
        let Some(src) = MidiSource::parse(chunk) else { return };
        let chunk = src.with_notes(&notes).write_chunk(chunk);
        take.source = Source::Raw(chunk);
    };
    if gesture { app.gesture(label, f) } else { app.edit(label, f) }
}

fn apply_controls(app: &mut ScytheApp, id: ItemId, lane: ControlLane, points: Vec<ControlPoint>, gesture: bool) {
    let f = move |p: &mut scythe_core::Project| {
        let Some(item) = p.tracks.iter_mut().find_map(|t| t.item_mut(id)) else { return };
        let active = item.active_take;
        let Some(take) = item.takes.get_mut(active) else { return };
        let Source::Raw(chunk) = &take.source else { return };
        let Some(src) = MidiSource::parse(chunk) else { return };
        let chunk = src.with_controls(lane, &points).write_chunk(chunk);
        take.source = Source::Raw(chunk);
    };
    if gesture { app.gesture("Edit MIDI controllers", f) } else { app.edit("Edit MIDI controllers", f) }
}

fn is_black(key: u8) -> bool {
    matches!(key % 12, 1 | 3 | 6 | 8 | 10)
}

fn key_name(key: u8) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    format!("{}{}", NAMES[key as usize % 12], key as i32 / 12 - 1)
}

pub fn show(app: &mut ScytheApp, ui: &mut Ui, rect: Rect, id: ItemId) {
    let painter = ui.painter().clone();
    painter.rect_filled(rect, CornerRadius::same(18), PANEL);
    let Some((source, name)) = source_of(app, id) else {
        app.view.midi_editor = None;
        return;
    };
    let mut notes = source.notes();
    let mut state = app.view.midi_state;

    // Header: title, grid choice, close.
    let head = Rect::from_min_size(rect.min + vec2(16.0, 8.0), vec2(rect.width() - 32.0, 32.0));
    painter.text(head.left_center(), Align2::LEFT_CENTER, format!("MIDI · {name}"), theme::semibold(15.0), TEXT);
    let mut x = head.right() - 32.0;
    let close = Rect::from_min_size(pos2(x, head.top()), vec2(32.0, 30.0));
    if text_pill(ui, close, "×", theme::semibold(15.0), RAISED, TEXT_DIM).on_hover_text("Close (Esc)").clicked() {
        app.view.midi_editor = None;
        return;
    }
    for (i, (label, _)) in GRIDS.iter().enumerate().rev() {
        x -= 52.0;
        let r = Rect::from_min_size(pos2(x, head.top()), vec2(48.0, 30.0));
        let (bg, fg) = if state.grid == i { (RAISED_HI, TEXT) } else { (RAISED, TEXT_DIM) };
        if text_pill(ui, r, label, theme::mono(11.0), bg, fg).clicked() {
            state.grid = i;
        }
    }
    let grid = GRIDS[state.grid.min(GRIDS.len() - 1)].1;

    let body = Rect::from_min_max(
        pos2(rect.left() + 12.0, head.bottom() + 8.0),
        pos2(rect.right() - 12.0, rect.bottom() - 12.0),
    );
    let roll =
        Rect::from_min_max(pos2(body.left() + KEYS_W, body.top()), pos2(body.right(), body.bottom() - VEL_H - 6.0));
    let keys = Rect::from_min_max(body.left_top(), pos2(roll.left() - 2.0, roll.bottom()));
    let vel = Rect::from_min_max(pos2(roll.left(), roll.bottom() + 6.0), body.right_bottom());

    let rows = (roll.height() / ROW_H).floor().max(1.0) as i32;
    let top_key = state.top_key.unwrap_or_else(|| {
        let hi = notes.iter().map(|n| n.key).max().unwrap_or(72) as i32;
        let lo = notes.iter().map(|n| n.key).min().unwrap_or(48) as i32;
        ((hi + lo) / 2 + rows / 2).clamp(rows - 1, 127) as u8
    }) as i32;
    let key_y = |key: i32| roll.top() + (top_key - key) as f32 * ROW_H;
    let key_at = |y: f32| top_key - ((y - roll.top()) / ROW_H).floor() as i32;

    let length = source.length_qn().max(notes.iter().map(|n| n.end_qn).fold(0.0, f64::max)).max(4.0);
    let px_per_qn = roll.width() as f64 / length;
    let qn_x = |qn: f64| roll.left() + (qn * px_per_qn) as f32;
    let x_qn = |x: f32| ((x - roll.left()) as f64 / px_per_qn).clamp(0.0, length);
    let snap = |qn: f64| (qn / grid).round() * grid;

    // Background rows and keyboard.
    let clip = painter.with_clip_rect(roll);
    for row in 0..=rows {
        let key = top_key - row;
        if !(0..=127).contains(&key) {
            continue;
        }
        let r = Rect::from_min_size(pos2(roll.left(), key_y(key)), vec2(roll.width(), ROW_H));
        clip.rect_filled(r, CornerRadius::ZERO, if is_black(key as u8) { BG } else { LANE });
        let k = Rect::from_min_size(pos2(keys.left(), key_y(key)), vec2(keys.width(), ROW_H - 1.0)).intersect(keys);
        if k.height() > 0.0 {
            let (bg, fg) = if is_black(key as u8) { (BG, TEXT_DIM) } else { (RAISED_HI, TEXT) };
            painter.rect_filled(k, CornerRadius::same(2), bg);
            if key % 12 == 0 {
                painter.text(
                    k.left_center() + vec2(4.0, 0.0),
                    Align2::LEFT_CENTER,
                    key_name(key as u8),
                    theme::mono(8.0),
                    fg,
                );
            }
        }
    }
    let mut q = 0.0;
    while q <= length {
        let strong = (q % 1.0).abs() < 1e-9;
        clip.vline(qn_x(q), roll.y_range(), Stroke::new(1.0, if strong { RAISED_HI } else { RAISED }));
        q += grid;
    }

    // Interaction on the grid.
    let resp = ui.interact(roll, Id::unique(("midi-roll", id)), Sense::click_and_drag());
    let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
    let hit = |notes: &[Note], p: egui::Pos2| {
        notes.iter().enumerate().rev().find_map(|(i, n)| {
            let r = Rect::from_min_max(
                pos2(qn_x(n.start_qn), key_y(n.key as i32)),
                pos2(qn_x(n.end_qn), key_y(n.key as i32) + ROW_H),
            );
            r.contains(p).then_some((i, p.x > r.right() - EDGE))
        })
    };
    let drag_id = Id::unique(("midi-drag", id));
    if resp.drag_started()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let drag = match hit(&notes, p) {
            Some((i, true)) => Some(Drag::Resize { note: i }),
            Some((i, false)) => Some(Drag::Move {
                note: i,
                grab_qn: x_qn(p.x) - notes[i].start_qn,
                grab_key: key_at(p.y) - notes[i].key as i32,
            }),
            None => None,
        };
        ui.data_mut(|d| d.insert_temp(drag_id, drag));
    }
    let drag: Option<Drag> = ui.data(|d| d.get_temp(drag_id)).flatten();
    if resp.dragged()
        && let (Some(p), Some(drag)) = (resp.interact_pointer_pos(), drag)
    {
        match drag {
            Drag::Move { note, grab_qn, grab_key } => {
                let len = notes[note].end_qn - notes[note].start_qn;
                let start = snap(x_qn(p.x) - grab_qn).clamp(0.0, (length - len).max(0.0));
                notes[note].start_qn = start;
                notes[note].end_qn = start + len;
                notes[note].key = (key_at(p.y) - grab_key).clamp(0, 127) as u8;
            }
            Drag::Resize { note } => {
                let end = snap(x_qn(p.x)).max(notes[note].start_qn + grid);
                notes[note].end_qn = end;
            }
        }
        apply(app, id, notes.clone(), true);
    }
    if resp.drag_stopped() {
        ui.data_mut(|d| d.remove::<Option<Drag>>(drag_id));
        app.end_gesture();
    }
    if resp.clicked()
        && let Some(p) = resp.interact_pointer_pos()
        && hit(&notes, p).is_none()
    {
        let start = (x_qn(p.x) / grid).floor() * grid;
        let key = key_at(p.y).clamp(0, 127) as u8;
        let channel = notes.first().map_or(0, |n| n.channel);
        notes.push(Note { start_qn: start, end_qn: start + grid, channel, key, velocity: 100 });
        apply(app, id, notes.clone(), false);
    }
    if resp.secondary_clicked()
        && let Some(p) = resp.interact_pointer_pos()
        && let Some((i, _)) = hit(&notes, p)
    {
        notes.remove(i);
        apply(app, id, notes.clone(), false);
    }
    if let Some(p) = pointer
        && let Some((_, edge)) = hit(&notes, p)
    {
        ui.ctx().set_cursor_icon(if edge { egui::CursorIcon::ResizeHorizontal } else { egui::CursorIcon::Grab });
    }

    // Notes.
    for n in &notes {
        let r = Rect::from_min_max(
            pos2(qn_x(n.start_qn), key_y(n.key as i32) + 1.0),
            pos2(qn_x(n.end_qn).max(qn_x(n.start_qn) + 3.0), key_y(n.key as i32) + ROW_H - 1.0),
        );
        let shade = 0.45 + 0.55 * n.velocity as f32 / 127.0;
        clip.rect_filled(r, CornerRadius::same(2), ACCENT.gamma_multiply(shade));
        clip.rect_stroke(
            r,
            CornerRadius::same(2),
            Stroke::new(1.0, with_alpha(Color32::BLACK, 90)),
            StrokeKind::Inside,
        );
    }

    // Lane picker, then the velocity or controller lane.
    let (short, full, lane) = LANES[state.lane.min(LANES.len() - 1)];
    let pick = Rect::from_min_size(pos2(body.left(), vel.top()), vec2(KEYS_W - 4.0, 24.0));
    if text_pill(ui, pick, short, theme::mono(10.0), RAISED, TEXT)
        .on_hover_text(format!("{full} (click for the next lane)"))
        .clicked()
    {
        state.lane = (state.lane + 1) % LANES.len();
    }
    painter.rect_filled(vel, CornerRadius::same(8), LANE);
    let vresp = ui.interact(vel, Id::unique(("midi-vel", id)), Sense::click_and_drag());
    if let Some(lane) = lane {
        control_lane(app, &painter, vel, &vresp, id, lane, &source, &notes, grid, &qn_x, &x_qn);
    }
    for n in notes.iter().filter(|_| lane.is_none()) {
        let x = qn_x(n.start_qn);
        let h = (vel.height() - 6.0) * n.velocity as f32 / 127.0;
        let bar = Rect::from_min_max(pos2(x, vel.bottom() - 3.0 - h), pos2(x + 3.0, vel.bottom() - 3.0));
        painter.rect_filled(bar, CornerRadius::same(1), ACCENT.gamma_multiply(0.8));
    }
    if lane.is_none()
        && (vresp.dragged() || vresp.clicked())
        && let Some(p) = vresp.interact_pointer_pos()
    {
        let near = notes
            .iter()
            .enumerate()
            .min_by(|a, b| (qn_x(a.1.start_qn) - p.x).abs().total_cmp(&(qn_x(b.1.start_qn) - p.x).abs()))
            .filter(|(_, n)| (qn_x(n.start_qn) - p.x).abs() < 8.0)
            .map(|(i, _)| i);
        if let Some(i) = near {
            let v = ((vel.bottom() - 3.0 - p.y) / (vel.height() - 6.0) * 127.0).round().clamp(1.0, 127.0) as u8;
            if notes[i].velocity != v {
                notes[i].velocity = v;
                apply(app, id, notes.clone(), true);
            }
        }
    }
    if vresp.drag_stopped() || vresp.clicked() {
        app.end_gesture();
    }

    // Scroll the keyboard with the mouse wheel over the roll.
    if resp.hovered() || vresp.hovered() {
        let dy = ui.input(|i| i.smooth_scroll_delta.y);
        if dy.abs() > 0.5 {
            state.top_key = Some((top_key + (dy / ROW_H).round() as i32).clamp(rows - 1, 127) as u8);
        } else if state.top_key.is_none() {
            state.top_key = Some(top_key as u8);
        }
    }
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.view.midi_editor = None;
    }
    app.view.midi_state = state;
}

/// Draws a controller lane as bars held until the next value, and paints or deletes values.
#[allow(clippy::too_many_arguments)]
fn control_lane(
    app: &mut ScytheApp,
    painter: &egui::Painter,
    lane_rect: Rect,
    resp: &egui::Response,
    id: ItemId,
    lane: ControlLane,
    source: &MidiSource,
    notes: &[Note],
    grid: f64,
    qn_x: &dyn Fn(f64) -> f32,
    x_qn: &dyn Fn(f32) -> f64,
) {
    let max = lane.max() as f32;
    let inner = lane_rect.shrink2(vec2(0.0, 3.0));
    let value_y = |v: u16| inner.bottom() - inner.height() * v as f32 / max;
    let mut points = source.controls(lane);
    if lane == ControlLane::PitchBend {
        painter.hline(inner.x_range(), value_y(8192), Stroke::new(1.0, RAISED_HI));
    }
    for (i, p) in points.iter().enumerate() {
        let x = qn_x(p.qn);
        let next = points.get(i + 1).map_or(lane_rect.right(), |n| qn_x(n.qn));
        let y = value_y(p.value);
        painter.hline(x..=next, y, Stroke::new(1.5, ACCENT.gamma_multiply(0.6)));
        let bar = Rect::from_min_max(pos2(x, y), pos2(x + 3.0, inner.bottom()));
        painter.rect_filled(bar, CornerRadius::same(1), ACCENT.gamma_multiply(0.8));
    }
    let Some(p) = resp.interact_pointer_pos() else { return };
    if resp.secondary_clicked() {
        let near = points
            .iter()
            .enumerate()
            .min_by(|a, b| (qn_x(a.1.qn) - p.x).abs().total_cmp(&(qn_x(b.1.qn) - p.x).abs()))
            .filter(|(_, c)| (qn_x(c.qn) - p.x).abs() < 8.0)
            .map(|(i, _)| i);
        if let Some(i) = near {
            points.remove(i);
            apply_controls(app, id, lane, points, false);
        }
        return;
    }
    if resp.dragged() || resp.clicked() {
        let qn = (x_qn(p.x) / grid).floor() * grid;
        let value = (((inner.bottom() - p.y) / inner.height()).clamp(0.0, 1.0) * max).round() as u16;
        let channel = points.first().map(|c| c.channel).or(notes.first().map(|n| n.channel)).unwrap_or(0);
        let changed = match points.iter_mut().find(|c| (c.qn - qn).abs() < 1e-9) {
            Some(c) => std::mem::replace(&mut c.value, value) != value,
            None => {
                points.push(ControlPoint { qn, value, channel });
                points.sort_by(|a, b| a.qn.total_cmp(&b.qn));
                true
            }
        };
        if changed {
            apply_controls(app, id, lane, points, true);
        }
    }
    if resp.drag_stopped() || resp.clicked() {
        app.end_gesture();
    }
}
