//! Stretch markers on items: a line per marker with a grip at the bottom. Dragging the grip
//! moves the marker along the item, stretching the audio on either side; Shift-dragging keeps
//! the marker in place and slides the source under it (REAPER's Shift-drag); right-click
//! removes it.

use egui::{Id, Pos2, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};
use scythe_core::{Item, TrackId};

use crate::app::{ScytheApp, Selection};
use crate::theme::{ACCENT, TEXT, with_alpha};

/// Where a dragged marker started: its take time and source position, and the pointer's time.
#[derive(Clone, Copy)]
struct Grab {
    pos: f64,
    src: f64,
    at: f64,
}

/// Draws the active take's stretch markers over `outer` and handles their gestures. `x` and `t`
/// convert between project time and screen x.
pub fn show(
    app: &mut ScytheApp,
    ui: &mut Ui,
    painter: &egui::Painter,
    outer: Rect,
    (x, t): (&dyn Fn(f64) -> f32, &dyn Fn(f32) -> f64),
    track: TrackId,
    item: &Item,
) {
    let Some(take) = item.take() else { return };
    if take.stretch_markers.is_empty() {
        return;
    }
    let id = item.id;
    let rate = if take.playrate > 0.0 { take.playrate } else { 1.0 };
    let color = with_alpha(TEXT, 150);
    for (i, m) in take.stretch_markers.iter().enumerate() {
        let mx = x(item.position + m.pos / rate);
        if mx < outer.left() || mx > outer.right() {
            continue;
        }
        let grip = Rect::from_center_size(pos2(mx, outer.bottom() - 5.0), vec2(10.0, 10.0));
        let hit = grip.intersect(painter.clip_rect());
        if hit.width() <= 0.0 || hit.height() <= 0.0 {
            continue;
        }
        let resp = ui.interact(hit, Id::unique(("stretch-marker", id, i)), Sense::click_and_drag());
        let hot = resp.hovered() || resp.dragged();
        let stroke = Stroke::new(1.0, if hot { ACCENT } else { color });
        painter.vline(mx, outer.top()..=outer.bottom(), stroke);
        let tri = [pos2(mx - 5.0, outer.bottom()), pos2(mx + 5.0, outer.bottom()), pos2(mx, outer.bottom() - 8.0)];
        painter.add(Shape::convex_polygon(tri.to_vec(), if hot { ACCENT } else { color }, Stroke::NONE));
        if hot {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            let label = if m.slope != 0.0 {
                format!("{:.3} s · slope {:+.2}", m.src, m.slope)
            } else {
                format!("{:.3} s", m.src)
            };
            painter.text(
                Pos2::new(mx + 6.0, outer.top() + 22.0),
                egui::Align2::LEFT_CENTER,
                label,
                crate::theme::mono(9.0),
                ACCENT,
            );
        }
        if resp.drag_started() {
            app.sel = Selection { track: Some(track), item: Some(id) };
            let at = resp.interact_pointer_pos().map_or(0.0, |p| t(p.x));
            ui.data_mut(|d| d.insert_temp(Id::unique("stretch-grab"), Grab { pos: m.pos, src: m.src, at }));
        }
        if resp.dragged()
            && let Some(p) = resp.interact_pointer_pos()
            && let Some(grab) = ui.data(|d| d.get_temp::<Grab>(Id::unique("stretch-grab")))
        {
            let moved = t(p.x) - grab.at;
            let lo = i.checked_sub(1).map_or(0.0, |k| take.stretch_markers[k].pos) + 1e-4;
            let hi = take.stretch_markers.get(i + 1).map_or(item.length * rate, |n| n.pos) - 1e-4;
            let slide = ui.ctx().input(|inp| inp.modifiers.shift);
            app.gesture("Move stretch marker", |proj| {
                let Some(it) = proj.tracks.iter_mut().find_map(|tr| tr.item_mut(id)) else { return };
                let k = it.active_take;
                let Some(m) = it.takes.get_mut(k).and_then(|tk| tk.stretch_markers.get_mut(i)) else { return };
                if slide {
                    m.src = (grab.src - moved * rate).max(0.0);
                } else if lo < hi {
                    m.pos = (grab.pos + moved * rate).clamp(lo, hi);
                }
            });
        }
        if resp.drag_stopped() {
            app.end_gesture();
        }
        if resp.secondary_clicked() {
            app.edit("Delete stretch marker", |proj| {
                let Some(it) = proj.tracks.iter_mut().find_map(|tr| tr.item_mut(id)) else { return };
                let k = it.active_take;
                if let Some(tk) = it.takes.get_mut(k)
                    && i < tk.stretch_markers.len()
                {
                    tk.stretch_markers.remove(i);
                }
            });
        }
    }
}
