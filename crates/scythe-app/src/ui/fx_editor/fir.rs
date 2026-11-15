//! ReaFIR in its EQ mode: the curve it builds its filter from, drawn with points over the
//! spectrum before and after it.

use egui::{Align2, CornerRadius, Id, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use scythe_engine::stock::reafir::Fir;

use super::model::{Plot, Scale, hz_text};
use super::{Ctx, area, grid, handle, knob_row, polyline, spectrum, split_left, split_top};
use crate::theme::{self, ACCENT, TEXT, TEXT_DIM};
use crate::ui::widgets;

const DB_LO: f64 = -36.0;
const DB_HI: f64 = 18.0;

/// `points` with point `k` moved to (`hz`, `db`), kept within the graph.
pub(crate) fn moved(points: &[(f32, f32)], k: usize, hz: f64, db: f64, top: f64) -> Vec<(f32, f32)> {
    let mut out = points.to_vec();
    if let Some(p) = out.get_mut(k) {
        *p = (hz.clamp(10.0, top) as f32, db.clamp(DB_LO, DB_HI) as f32);
    }
    out
}

pub fn show(cx: &mut Ctx, ui: &mut Ui, rect: Rect) {
    let Some((points, sr, gain_db)) = cx.read(|f: &mut Fir| (f.points().to_vec(), f.sample_rate(), f.gain_db())) else {
        return;
    };
    if cx.ed.controls.sel.is_some_and(|k| k >= points.len()) {
        cx.ed.controls.sel = None;
    }
    let (graph_rect, bottom) = split_top(rect, rect.height() - 104.0, 8.0);
    let top = (sr / 2.0).min(22000.0);
    let plot = Plot::new(graph_rect, Scale::log(20.0, top), Scale::lin(DB_LO, DB_HI));
    grid(ui, &plot, true, 6.0, "");
    let levels = Plot::new(graph_rect, plot.x, Scale::lin(-120.0, 12.0));
    spectrum(ui, &levels, &cx.ed.pre, 0.0, theme::with_alpha(TEXT_DIM, 18), theme::with_alpha(TEXT_DIM, 60));
    spectrum(ui, &levels, &cx.ed.post, 0.0, theme::with_alpha(ACCENT, 22), theme::with_alpha(ACCENT, 80));
    let xs = plot.xs(2.0);
    let curve = cx.read(|f: &mut Fir| f.curve_db(&xs)).unwrap_or_default();
    let line: Vec<Pos2> =
        xs.iter().zip(&curve).map(|(&f, &db)| plot.point(f, (db + gain_db).clamp(DB_LO - 6.0, DB_HI + 6.0))).collect();
    area(ui, graph_rect, &line, plot.sy(0.0), theme::with_alpha(ACCENT, 30));
    polyline(ui, graph_rect, line, Stroke::new(2.5, TEXT));
    let at: Vec<Pos2> = points.iter().map(|&(hz, db)| plot.point(hz as f64, db as f64)).collect();
    for (k, p) in at.iter().enumerate() {
        handle(ui, graph_rect, *p, ACCENT, cx.ed.controls.sel == Some(k), "");
    }
    input(cx, ui, &plot, &points, &at, top);
    if let Some(k) = cx.ed.controls.sel
        && let (Some(&(hz, db)), Some(&p)) = (points.get(k), at.get(k))
    {
        let tip = format!("{} · {db:+.1} dB", hz_text(hz as f64));
        let w = widgets::text_width(ui, &tip, theme::regular(11.0)) + 16.0;
        let r = Rect::from_min_size(
            pos2(
                (p.x - w / 2.0).clamp(graph_rect.left(), graph_rect.right() - w),
                (p.y - 32.0).max(graph_rect.top() + 2.0),
            ),
            vec2(w, 20.0),
        );
        ui.painter().rect_filled(r, CornerRadius::same(6), theme::with_alpha(theme::PANEL, 230));
        ui.painter().text(r.center(), Align2::CENTER_CENTER, tip, theme::regular(11.0), TEXT);
    }
    let (knob_area, help) = split_left(bottom, 140.0, 8.0);
    super::card(ui, knob_area);
    knob_row(cx, ui, knob_area.shrink(4.0), &[(4, "Output")]);
    super::card(ui, help);
    let lines = [
        format!("{} points · drag to move, arrows to nudge", points.len()),
        "Double-click the graph to add a point, a point to remove it".to_owned(),
        "Right-click for the point menu".to_owned(),
    ];
    for (i, l) in lines.iter().enumerate() {
        ui.painter().text(
            help.left_top() + vec2(12.0, 14.0 + i as f32 * 20.0),
            Align2::LEFT_CENTER,
            l,
            theme::regular(11.0),
            TEXT_DIM,
        );
    }
}

fn set_points(cx: &mut Ctx, points: Vec<(f32, f32)>, once: bool) {
    if once {
        cx.edit_once("Draw FFT EQ curve", |f: &mut Fir| f.set_points(points));
    } else {
        cx.edit("Draw FFT EQ curve", |f: &mut Fir| f.set_points(points));
    }
}

fn input(cx: &mut Ctx, ui: &mut Ui, plot: &Plot, points: &[(f32, f32)], at: &[Pos2], top: f64) {
    let resp = ui.interact(plot.rect, Id::unique(("fx-fir-graph", &cx.ed.owner, &cx.ed.path)), Sense::click_and_drag());
    let near = |p: Pos2| {
        at.iter()
            .enumerate()
            .map(|(k, a)| (k, a.distance(p)))
            .filter(|(_, d)| *d <= 12.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(k, _)| k)
    };
    if resp.drag_started() || resp.clicked() || resp.secondary_clicked() {
        let origin = widgets::drag_origin(ui, &resp);
        cx.ed.controls.sel = origin.and_then(near);
        cx.ed.dragging = if resp.drag_started() { cx.ed.controls.sel } else { None };
        cx.ed.controls.focus = None;
    }
    if resp.dragged()
        && let (Some(k), Some(p)) = (cx.ed.dragging, resp.interact_pointer_pos())
    {
        let (hz, db) = plot.values(p);
        set_points(cx, moved(points, k, hz, db, top), false);
    }
    if resp.drag_stopped() {
        cx.ed.dragging = None;
        cx.end();
    }
    if resp.double_clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        match near(p) {
            Some(k) => {
                let mut next = points.to_vec();
                next.remove(k);
                set_points(cx, next, true);
                cx.ed.controls.sel = None;
            }
            None => {
                let (hz, db) = plot.values(p);
                let mut next = points.to_vec();
                next.push((hz.clamp(10.0, top) as f32, db.clamp(DB_LO, DB_HI) as f32));
                set_points(cx, next, true);
                cx.ed.controls.sel = Some(points.len());
            }
        }
    }
    if resp.hovered()
        && let Some(k) = cx.ed.controls.sel
        && let Some(&(hz, db)) = points.get(k)
        && cx.ed.controls.typing.is_none()
    {
        let (l, r, u, d, del, fine) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::ArrowRight),
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace),
                i.modifiers.shift,
            )
        });
        let (side, vert) = (r as i32 - l as i32, u as i32 - d as i32);
        if side != 0 || vert != 0 {
            let hz = hz as f64 * 2f64.powf(side as f64 * if fine { 0.01 } else { 1.0 / 12.0 });
            let db = db as f64 + vert as f64 * if fine { 0.1 } else { 0.5 };
            set_points(cx, moved(points, k, hz, db, top), false);
            cx.ed.controls.key_gesture = true;
        }
        if del {
            let mut next = points.to_vec();
            next.remove(k);
            set_points(cx, next, true);
            cx.ed.controls.sel = None;
        }
    }
    let menu_at = resp.interact_pointer_pos().or(resp.hover_pos());
    resp.context_menu(|ui| match cx.ed.controls.sel.filter(|k| *k < points.len()) {
        Some(k) => {
            if ui.button("Remove point").clicked() {
                let mut next = points.to_vec();
                next.remove(k);
                set_points(cx, next, true);
                cx.ed.controls.sel = None;
                ui.close();
            }
            if ui.button("Set to 0 dB").clicked() {
                set_points(cx, moved(points, k, points[k].0 as f64, 0.0, top), true);
                ui.close();
            }
        }
        None => {
            if ui.button("Add point here").clicked() {
                if let Some(p) = menu_at {
                    let (hz, db) = plot.values(p);
                    let mut next = points.to_vec();
                    next.push((hz.clamp(10.0, top) as f32, db.clamp(DB_LO, DB_HI) as f32));
                    set_points(cx, next, true);
                }
                ui.close();
            }
            if ui.button("Flatten the curve").clicked() {
                let flat = points.iter().map(|&(hz, _)| (hz, 0.0)).collect();
                set_points(cx, flat, true);
                ui.close();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_move_within_the_graph() {
        let pts = [(1000.0, 0.0), (100.0, -3.0)];
        assert_eq!(moved(&pts, 1, 50.0, -100.0, 20000.0), vec![(1000.0, 0.0), (50.0, DB_LO as f32)]);
        assert_eq!(moved(&pts, 0, 30000.0, 3.0, 20000.0)[0], (20000.0, 3.0));
        let plot = Plot::new(
            Rect::from_min_size(pos2(0.0, 0.0), vec2(500.0, 200.0)),
            Scale::log(20.0, 20000.0),
            Scale::lin(DB_LO, DB_HI),
        );
        let (hz, db) = plot.values(plot.point(440.0, -6.0));
        assert_eq!(moved(&pts, 0, hz, db, 20000.0)[0].0.round(), 440.0);
    }
}
