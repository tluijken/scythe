//! The inline MIDI editor: a small piano roll inside the item in the arrange view (REAPER's
//! "Item: Open item inline editors", 40847). Click adds a note on the take's grid, drag moves
//! one, right-click deletes; the rows span the take's notes with some room around them.

use egui::{Color32, CornerRadius, Id, Rect, Sense, Stroke, StrokeKind, Ui, pos2};
use scythe_core::ItemId;
use scythe_core::midi::edit::{EditNote, Grid};

use super::{apply_notes, is_midi, load};
use crate::app::ScytheApp;
use crate::theme::{ACCENT, TEXT, with_alpha};

/// Opens the item's inline editor, or closes it when open; true when it opened.
pub fn toggle_inline(app: &mut ScytheApp, id: ItemId) -> bool {
    if let Some(i) = app.view.inline_midi.iter().position(|&o| o == id) {
        app.view.inline_midi.remove(i);
        return false;
    }
    if !is_midi(app, id) {
        return false;
    }
    app.view.inline_midi.push(id);
    true
}

/// Draws and edits the item's notes in `area`; `x` maps project seconds to the screen and `t`
/// back.
pub fn inline_editor(
    app: &mut ScytheApp,
    ui: &mut Ui,
    area: Rect,
    id: ItemId,
    x: &dyn Fn(f64) -> f32,
    t: &dyn Fn(f32) -> f64,
) {
    let Some(take) = load(app, id) else { return };
    let tl = app.project.tempo.timeline();
    let ppq = take.src.ticks_per_qn as f64;
    let tick_x = |tick: f64| x(tl.time_at_qn(take.origin_qn + tick / ppq));
    let x_tick = |px: f32| ((tl.qn_at(t(px)) - take.origin_qn) * ppq).max(0.0);
    let passes = loop_passes(app, id, &take);
    let in_source = |tick: f64| match passes.get(1) {
        Some(&len) => tick.rem_euclid(len),
        None => tick,
    };
    let lo = take.notes.iter().map(|n| n.key).min().unwrap_or(60).saturating_sub(2);
    let hi = take.notes.iter().map(|n| n.key).max().unwrap_or(72).saturating_add(2).min(127).max(lo + 12);
    let rows = (hi - lo) as f32 + 1.0;
    let row_h = area.height() / rows;
    let key_y = |k: u8| area.bottom() - (k - lo + 1) as f32 * row_h;
    let key_at = |y: f32| (lo as f32 + ((area.bottom() - y) / row_h).floor()).clamp(lo as f32, hi as f32) as u8;
    let painter = ui.painter().with_clip_rect(area);
    painter.rect_stroke(area, CornerRadius::same(4), Stroke::new(1.0, with_alpha(ACCENT, 120)), StrokeKind::Inside);
    for k in lo..=hi {
        if k % 12 == 0 {
            painter.hline(area.x_range(), key_y(k) + row_h, Stroke::new(1.0, with_alpha(TEXT, 30)));
        }
    }
    let rect_of = |n: &EditNote, off: f64| {
        let (s, e) = (n.start as f64 + off, n.end as f64 + off);
        Rect::from_min_max(pos2(tick_x(s), key_y(n.key)), pos2(tick_x(e).max(tick_x(s) + 2.0), key_y(n.key) + row_h))
    };
    for (k, &off) in passes.iter().enumerate() {
        if k > 0 {
            painter.vline(tick_x(off), area.y_range(), Stroke::new(1.0, with_alpha(TEXT, 40)));
        }
        for n in &take.notes {
            let fill = if n.muted {
                with_alpha(TEXT, 60)
            } else {
                ACCENT.gamma_multiply(0.45 + 0.55 * n.velocity as f32 / 127.0)
            };
            painter.rect_filled(rect_of(n, off), CornerRadius::same(1), fill);
            if n.selected {
                painter.rect_stroke(
                    rect_of(n, off),
                    CornerRadius::same(1),
                    Stroke::new(1.0, Color32::WHITE),
                    StrokeKind::Inside,
                );
            }
        }
    }

    let grid = Grid { qn: take.view.grid_qn, swing: take.view.swing.unwrap_or(0.0) };
    let resp = ui.interact(area, Id::unique(("inline-midi", id)), Sense::click_and_drag());
    let hit = |p: egui::Pos2| {
        passes.iter().find_map(|&off| take.notes.iter().rposition(|n| rect_of(n, off).contains(p)).map(|i| (i, off)))
    };
    let drag_id = Id::unique(("inline-midi-drag", id));
    if resp.drag_started()
        && let Some(p) = crate::ui::widgets::drag_origin(ui, &resp)
        && let Some((i, off)) = hit(p)
    {
        ui.data_mut(|d| d.insert_temp(drag_id, (i, x_tick(p.x) - off - take.notes[i].start as f64, off)));
    }
    if resp.dragged()
        && let (Some(p), Some((i, grab, off))) =
            (resp.interact_pointer_pos(), ui.data(|d| d.get_temp::<(usize, f64, f64)>(drag_id)))
    {
        let mut notes = take.notes.clone();
        let n = &mut notes[i];
        let len = n.length();
        n.start = grid.nearest(take.src.ticks_per_qn, x_tick(p.x) - off - grab).max(0.0).round() as u64;
        n.end = n.start + len;
        n.key = key_at(p.y);
        apply_notes(app, id, &notes, "Move note", true);
    }
    if resp.drag_stopped() {
        ui.data_mut(|d| d.remove::<(usize, f64, f64)>(drag_id));
        app.end_gesture();
    }
    if resp.clicked()
        && let Some(p) = resp.interact_pointer_pos()
        && hit(p).is_none()
    {
        let g = grid.qn * ppq;
        let start = ((in_source(x_tick(p.x)) / g).floor() * g).round() as u64;
        let mut notes = take.notes.clone();
        notes.push(EditNote::new(start, start + g.round().max(1.0) as u64, take.view.channel, key_at(p.y), 100));
        notes.sort_by_key(|n| (n.start, n.key, n.channel));
        apply_notes(app, id, &notes, "Insert note", false);
    }
    if resp.secondary_clicked()
        && let Some((i, _)) = resp.interact_pointer_pos().and_then(hit)
    {
        let mut notes = take.notes.clone();
        notes.remove(i);
        apply_notes(app, id, &notes, "Delete note", false);
    }
}

/// Where each pass of the take's source starts, in ticks from the source start: one pass, or
/// one per loop of a looped item up to its end.
fn loop_passes(app: &ScytheApp, id: ItemId, take: &super::Take) -> Vec<f64> {
    let Some((_, item)) = app.project.find_item(id) else { return vec![0.0] };
    let len = take.src.length_ticks as f64;
    if !item.looped || len <= 0.0 {
        return vec![0.0];
    }
    let tl = app.project.tempo.timeline();
    let end = (tl.qn_at(item.end()) - take.origin_qn) * take.src.ticks_per_qn as f64;
    pass_offsets(len, end)
}

fn pass_offsets(len: f64, end: f64) -> Vec<f64> {
    let n = ((end / len).ceil().max(1.0) as usize).min(4096);
    (0..n).map(|k| k as f64 * len).collect()
}

#[cfg(test)]
mod tests {
    use super::pass_offsets;

    #[test]
    fn a_looped_item_shows_every_pass() {
        assert_eq!(pass_offsets(960.0, 3840.0), [0.0, 960.0, 1920.0, 2880.0]);
        assert_eq!(pass_offsets(960.0, 1000.0), [0.0, 960.0], "a part of the next pass");
        assert_eq!(pass_offsets(960.0, 500.0), [0.0]);
    }
}
