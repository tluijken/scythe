//! Comping on fixed-lane tracks: the comp areas drawn over the lanes they take from, and
//! swiping (Ctrl+Shift+drag across a lane) to make a new one, as dragging across a lane does in
//! REAPER once a comp lane exists.

use egui::{CornerRadius, Id, Rect, Stroke, StrokeKind, Ui};
use scythe_core::TrackId;

use crate::app::ScytheApp;
use crate::theme::{self, with_alpha};

const SWIPE: &str = "comp-swipe";

#[derive(Clone, Copy)]
struct Swipe {
    track: TrackId,
    lane: usize,
    from: f64,
}

/// A swipe is under way: item and marquee drags stand aside.
pub fn swiping(ui: &Ui) -> bool {
    ui.data(|d| d.get_temp::<Swipe>(Id::unique(SWIPE))).is_some()
}

/// Lane `k` of `n` within a track's row, as items are drawn there.
fn band(row: Rect, n: usize, k: usize) -> Rect {
    let y = |f: f32| row.top() + 2.0 + f * (row.height() - 4.0);
    Rect::from_x_y_ranges(row.x_range(), y(k as f32 / n as f32)..=y((k + 1) as f32 / n as f32))
}

/// Draws the comp areas of `track_id` and runs swiping on its row. Call before the track's
/// items take their input, so they leave a swipe alone.
pub fn track_lanes(
    app: &mut ScytheApp,
    ui: &Ui,
    painter: &egui::Painter,
    row: Rect,
    x: &dyn Fn(f64) -> f32,
    t: &dyn Fn(f32) -> f64,
    track_id: TrackId,
) {
    let Some(track) = app.project.track(track_id) else { return };
    let Some(comp) = track.comp_lane() else { return };
    let n = track.lane_count();
    let accent = theme::ACCENT;
    for a in &track.comp_areas {
        let b = band(row, n, a.lane);
        let r = Rect::from_x_y_ranges(x(a.start)..=x(a.end), b.y_range());
        painter.rect_filled(r, CornerRadius::same(4), with_alpha(accent, 0x28));
        painter.rect_stroke(r, CornerRadius::same(4), Stroke::new(1.5, accent), StrokeKind::Inside);
    }
    let c = band(row, n, comp);
    painter.rect_stroke(c, CornerRadius::same(6), Stroke::new(1.0, with_alpha(accent, 0x80)), StrokeKind::Inside);

    let id = Id::unique(SWIPE);
    let (pressed, down, modifiers, pos) =
        ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.modifiers, i.pointer.interact_pos()));
    if pressed
        && modifiers.command
        && modifiers.shift
        && let Some(p) = pos.filter(|p| row.contains(*p))
    {
        let lane = (((p.y - row.top() - 2.0) / (row.height() - 4.0) * n as f32).floor().max(0.0) as usize).min(n - 1);
        if lane != comp {
            ui.data_mut(|d| d.insert_temp(id, Swipe { track: track_id, lane, from: app.snap(t(p.x).max(0.0), false) }));
        }
    }
    let Some(swipe) = ui.data(|d| d.get_temp::<Swipe>(id)).filter(|s| s.track == track_id) else { return };
    let to = pos.map_or(swipe.from, |p| app.snap(t(p.x).max(0.0), false));
    let (a, b) = (swipe.from.min(to), swipe.from.max(to));
    let r = Rect::from_x_y_ranges(x(a)..=x(b), band(row, n, swipe.lane).y_range());
    painter.rect_filled(r, CornerRadius::same(4), with_alpha(theme::PINK, 0x40));
    painter.rect_stroke(r, CornerRadius::same(4), Stroke::new(1.5, theme::PINK), StrokeKind::Inside);
    if !down {
        ui.data_mut(|d| d.remove::<Swipe>(id));
        if b > a {
            app.edit("Swipe comp area", |p| {
                if let Some(t) = p.track_mut(track_id) {
                    t.swipe(swipe.lane, a, b);
                }
            });
        }
    }
}
