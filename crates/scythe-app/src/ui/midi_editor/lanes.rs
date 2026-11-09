//! The controller lanes under the roll, as many as the take's `VELLANE` lines list. Each lane's
//! label picks what it shows (velocity, any controller, 14-bit pairs, pitch bend, program,
//! aftertouch, sysex, text); `+` adds a lane and `×` removes one.
//!
//! Draw tool: click or drag sets values on the grid. Line tool: a drag draws a line or curve,
//! two events joined by a CC shape. Right-click a value for its shape or to delete it.

use egui::{Align2, CornerRadius, Id, Rect, Sense, Stroke, Ui, pos2, vec2};
use scythe_core::midi::lanes::LanePoint;
use scythe_core::midi::view::LaneKind;
use scythe_core::midi::{CcShape, ShapeKind};

use super::roll::{Axis, shown};
use super::{EditorState, Take, Tool, apply_notes, apply_source, apply_view, lane_menu, shape_name};
use crate::app::ScytheApp;
use crate::theme::{self, ACCENT, LANE, RAISED, RAISED_HI, TEXT, TEXT_DIM};
use crate::ui::widgets::text_pill;

const LABEL_W: f32 = 44.0;

pub fn show(app: &mut ScytheApp, ui: &mut Ui, rect: Rect, take: &Take, state: &mut EditorState, axis: &Axis) {
    let count = take.view.lanes.len().max(1);
    let h = ((rect.height() - 4.0 * (count as f32 - 1.0)) / count as f32).max(24.0);
    let mut view = take.view.clone();
    let mut remove = None;
    for (i, (kind, _)) in take.view.lanes.iter().enumerate() {
        let top = rect.top() + i as f32 * (h + 4.0);
        let lane = Rect::from_min_max(pos2(axis.left, top), pos2(rect.right(), top + h));
        let label = Rect::from_min_size(pos2(rect.left(), top), vec2(LABEL_W - 4.0, 22.0));
        let short = match kind {
            LaneKind::Velocity => "Vel".to_owned(),
            LaneKind::OffVelocity => "Off".to_owned(),
            LaneKind::Cc(n) => format!("{n}"),
            LaneKind::Cc14(n) => format!("{n}+"),
            LaneKind::PitchBend => "Pitch".to_owned(),
            LaneKind::Program => "Prog".to_owned(),
            LaneKind::ChannelPressure => "Aft".to_owned(),
            LaneKind::PolyAftertouch => "Poly".to_owned(),
            LaneKind::Sysex => "Syx".to_owned(),
            LaneKind::Text => "Text".to_owned(),
            _ => "?".to_owned(),
        };
        let resp = text_pill(ui, label, &short, theme::mono(10.0), RAISED, TEXT)
            .on_hover_text(format!("{} (click to change)", kind.label()));
        egui::Popup::menu(&resp).show(|ui| {
            if let Some(k) = lane_menu(ui, *kind) {
                view.lanes[i].0 = k;
            }
        });
        let small = |col: f32| {
            Rect::from_min_size(
                pos2(rect.left() + (LABEL_W - 4.0) / 2.0 * col, top + 24.0),
                vec2((LABEL_W - 8.0) / 2.0, 18.0),
            )
        };
        if h >= 44.0 {
            if text_pill(ui, small(0.0), "+", theme::mono(10.0), RAISED, TEXT_DIM).on_hover_text("Add a lane").clicked()
            {
                view.lanes.insert(i + 1, (LaneKind::Cc(1), 100));
            }
            if count > 1
                && text_pill(ui, small(1.0), "×", theme::mono(10.0), RAISED, TEXT_DIM)
                    .on_hover_text("Remove the lane")
                    .clicked()
            {
                remove = Some(i);
            }
        }
        ui.painter().rect_filled(lane, CornerRadius::same(8), LANE);
        let resp = ui.interact(lane, Id::unique(("midi-lane", take.id, i)), Sense::click_and_drag());
        match kind {
            LaneKind::Velocity | LaneKind::OffVelocity => {
                velocity_lane(app, ui, lane, &resp, take, *kind == LaneKind::OffVelocity, axis)
            }
            k if k.has_values() => value_lane(app, ui, lane, &resp, take, state, *k, axis),
            LaneKind::Sysex | LaneKind::Text => long_lane(app, ui, lane, &resp, take, *kind == LaneKind::Sysex, axis),
            _ => {}
        }
    }
    if let Some(i) = remove {
        view.lanes.remove(i);
    }
    if view != take.view {
        apply_view(app, take.id, &view);
    }
}

fn velocity_lane(
    app: &mut ScytheApp,
    ui: &mut Ui,
    lane: Rect,
    resp: &egui::Response,
    take: &Take,
    off: bool,
    axis: &Axis,
) {
    let painter = ui.painter().with_clip_rect(lane);
    let inner = lane.shrink2(vec2(0.0, 3.0));
    let value = |n: &scythe_core::midi::edit::EditNote| if off { n.off_velocity } else { n.velocity };
    for n in take.notes.iter().filter(|n| shown(take, n.channel)) {
        let x = axis.x(n.start as f64);
        let hgt = inner.height() * value(n) as f32 / 127.0;
        let bar = Rect::from_min_max(pos2(x, inner.bottom() - hgt), pos2(x + 3.0, inner.bottom()));
        let color = if n.selected { theme::TEXT } else { ACCENT.gamma_multiply(0.8) };
        painter.rect_filled(bar, CornerRadius::same(1), color);
    }
    if (resp.dragged() || resp.clicked())
        && let Some(p) = resp.interact_pointer_pos()
    {
        let v = (((inner.bottom() - p.y) / inner.height()).clamp(0.0, 1.0) * 127.0).round() as u8;
        let mut notes = take.notes.clone();
        let near: Vec<usize> = notes
            .iter()
            .enumerate()
            .filter(|(_, n)| shown(take, n.channel) && (axis.x(n.start as f64) - p.x).abs() < 6.0)
            .map(|(i, _)| i)
            .collect();
        let mut changed = false;
        for i in near {
            let n = &mut notes[i];
            let new = if off { v } else { v.max(1) };
            let slot = if off { &mut n.off_velocity } else { &mut n.velocity };
            changed |= std::mem::replace(slot, new) != new;
        }
        if changed {
            apply_notes(app, take.id, &notes, "Edit velocity", true);
        }
    }
    if resp.drag_stopped() || resp.clicked() {
        app.end_gesture();
    }
}

#[allow(clippy::too_many_arguments)]
fn value_lane(
    app: &mut ScytheApp,
    ui: &mut Ui,
    lane: Rect,
    resp: &egui::Response,
    take: &Take,
    state: &mut EditorState,
    kind: LaneKind,
    axis: &Axis,
) {
    let painter = ui.painter().with_clip_rect(lane);
    let inner = lane.shrink2(vec2(0.0, 3.0));
    let max = kind.max() as f32;
    let y = |v: u16| inner.bottom() - inner.height() * v as f32 / max;
    let value_at = |py: f32| (((inner.bottom() - py) / inner.height()).clamp(0.0, 1.0) * max).round() as u16;
    let ppq = take.src.ticks_per_qn;
    let channel = take.view.channel;
    let all = take.src.lane(kind);
    let points: Vec<&LanePoint> = all.iter().filter(|p| shown(take, p.channel)).collect();
    if kind == LaneKind::PitchBend {
        painter.hline(inner.x_range(), y(8192), Stroke::new(1.0, RAISED_HI));
    }
    for (i, p) in points.iter().enumerate() {
        let x = axis.x(p.tick as f64);
        let next = points[i + 1..].iter().find(|q| q.channel == p.channel && q.key == p.key);
        let stroke = Stroke::new(1.5, ACCENT.gamma_multiply(0.7));
        match (p.shape.map(|s| s.kind), next) {
            (Some(k), Some(q)) if k != ShapeKind::Square => {
                painter.line_segment([pos2(x, y(p.value)), pos2(axis.x(q.tick as f64), y(q.value))], stroke);
            }
            (_, Some(q)) => {
                painter.hline(x..=axis.x(q.tick as f64), y(p.value), stroke);
            }
            (_, None) => {
                painter.hline(x..=lane.right(), y(p.value), stroke);
            }
        }
        let dot = Rect::from_center_size(pos2(x, y(p.value)), vec2(5.0, 5.0));
        painter.rect_filled(dot, CornerRadius::same(2), if p.selected { TEXT } else { ACCENT });
    }

    let grid = scythe_core::midi::edit::Grid { qn: take.view.grid_qn, swing: take.view.swing.unwrap_or(0.0) };
    let near = |px: f32| {
        all.iter()
            .enumerate()
            .filter(|(_, q)| shown(take, q.channel))
            .min_by(|a, b| (axis.x(a.1.tick as f64) - px).abs().total_cmp(&(axis.x(b.1.tick as f64) - px).abs()))
            .filter(|(_, q)| (axis.x(q.tick as f64) - px).abs() < 8.0)
            .map(|(i, _)| i)
    };
    let line_id = Id::unique(("midi-line", take.id, kind.code()));
    match state.tool {
        Tool::Draw => {
            if (resp.dragged() || resp.clicked())
                && let Some(p) = resp.interact_pointer_pos()
            {
                let tick = axis.floor(&grid, axis.tick(p.x));
                let value = value_at(p.y);
                let mut pts = all.clone();
                let changed = match pts.iter_mut().find(|q| q.tick == tick && q.channel == channel) {
                    Some(q) => std::mem::replace(&mut q.value, value) != value,
                    None => {
                        pts.push(LanePoint::new(tick, value, channel));
                        pts.sort_by_key(|q| q.tick);
                        true
                    }
                };
                if changed {
                    apply_source(app, take.id, take.src.with_lane(kind, &pts), "Draw MIDI controllers", true);
                }
            }
        }
        Tool::Line => {
            if resp.drag_started()
                && let Some(p) = resp.interact_pointer_pos()
            {
                ui.data_mut(|d| d.insert_temp(line_id, (axis.tick(p.x), value_at(p.y))));
            }
            let start: Option<(f64, u16)> = ui.data(|d| d.get_temp(line_id));
            if let (Some((t0, v0)), Some(p)) = (start, resp.interact_pointer_pos()) {
                if resp.dragged() {
                    painter.line_segment([pos2(axis.x(t0), y(v0)), p], Stroke::new(1.5, TEXT));
                }
                if resp.drag_stopped() {
                    let (a, b) = (t0.min(axis.tick(p.x)).round() as u64, t0.max(axis.tick(p.x)).round() as u64);
                    let (va, vb) = if axis.tick(p.x) >= t0 { (v0, value_at(p.y)) } else { (value_at(p.y), v0) };
                    if b > a {
                        let mut pts: Vec<LanePoint> =
                            all.iter().filter(|q| q.channel != channel || q.tick < a || q.tick > b).copied().collect();
                        let shape = (state.shape != ShapeKind::Square).then(|| CcShape::new(state.shape, 0.0));
                        pts.push(LanePoint { shape, ..LanePoint::new(a, va, channel) });
                        pts.push(LanePoint::new(b, vb, channel));
                        pts.sort_by_key(|q| q.tick);
                        apply_source(app, take.id, take.src.with_lane(kind, &pts), "Draw MIDI controller line", false);
                    }
                    ui.data_mut(|d| d.remove::<(f64, u16)>(line_id));
                }
            }
        }
    }
    if resp.drag_stopped() || resp.clicked() {
        app.end_gesture();
    }
    let menu_id = Id::unique(("midi-lane-point", take.id, kind.code()));
    if resp.secondary_clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        ui.data_mut(|d| d.insert_temp(menu_id, near(p.x)));
    }
    let target: Option<usize> = ui.data(|d| d.get_temp::<Option<usize>>(menu_id)).flatten();
    egui::Popup::context_menu(resp).show(|ui| {
        let Some(i) = target else {
            ui.label(format!("{} events: {}", kind.label(), all.len()));
            if ui.button("Delete all in this lane").clicked() {
                apply_source(app, take.id, take.src.with_lane(kind, &[]), "Delete MIDI controllers", false);
            }
            return;
        };
        let mut pts = all.clone();
        let current = pts[i].shape.map_or(ShapeKind::Square, |s| s.kind);
        ui.label(format!("Value {} at {:.3}", pts[i].value, pts[i].tick as f64 / ppq as f64));
        for k in ShapeKind::ALL {
            if ui.selectable_label(current == k, shape_name(k)).clicked() {
                let tension = pts[i].shape.map_or(0.0, |s| s.tension);
                pts[i].shape = (k != ShapeKind::Square).then(|| CcShape::new(k, tension));
                apply_source(app, take.id, take.src.with_lane(kind, &pts), "Set CC shape", false);
            }
        }
        if current == ShapeKind::Bezier {
            let mut t = pts[i].shape.map_or(0.0, |s| s.tension);
            if ui.add(egui::Slider::new(&mut t, -1.0..=1.0).text("tension")).changed() {
                pts[i].shape = Some(CcShape::new(ShapeKind::Bezier, t));
                apply_source(app, take.id, take.src.with_lane(kind, &pts), "Set CC shape", true);
            }
        }
        if ui.button("Delete").clicked() {
            pts.remove(i);
            apply_source(app, take.id, take.src.with_lane(kind, &pts), "Delete MIDI controller", false);
        }
    });
}

/// System exclusive or text events as flags along the lane; right-click deletes one.
fn long_lane(
    app: &mut ScytheApp,
    ui: &mut Ui,
    lane: Rect,
    resp: &egui::Response,
    take: &Take,
    sysex: bool,
    axis: &Axis,
) {
    let painter = ui.painter().with_clip_rect(lane);
    let is = |d: &[u8]| if sysex { d.first() == Some(&0xf0) } else { d.first() == Some(&0xff) };
    for x in take.src.long.iter().filter(|x| is(&x.data)) {
        let px = axis.x(x.tick as f64);
        painter.vline(px, lane.y_range(), Stroke::new(1.5, ACCENT));
        let text = if sysex {
            format!("{} bytes", x.data.len())
        } else {
            String::from_utf8_lossy(x.data.get(2..).unwrap_or_default()).into_owned()
        };
        painter.text(pos2(px + 3.0, lane.top() + 4.0), Align2::LEFT_TOP, text, theme::mono(9.0), TEXT_DIM);
    }
    if resp.secondary_clicked()
        && let Some(p) = resp.interact_pointer_pos()
        && let Some(i) = take.src.long.iter().position(|x| is(&x.data) && (axis.x(x.tick as f64) - p.x).abs() < 6.0)
    {
        let mut src = take.src.clone();
        src.long.remove(i);
        apply_source(app, take.id, src, "Delete MIDI event", false);
    }
}
