//! ReaComp, ReaXcomp, ReaGate and ReaLimit: transfer curves, gain reduction meters and the
//! last seconds of what the detector heard.

use egui::{Align2, CornerRadius, Id, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use scythe_engine::stock::reacomp::Compressor;
use scythe_engine::stock::reaxcomp::{self, BandView, MultibandCompressor};

use super::model::{Plot, Scale, amp_db, hz_text};
use super::{
    Ctx, area, band_color, card, grid, handle, history, knob_row, labelled_meter, polyline, section_title, spectrum,
    split_left, split_top,
};
use crate::theme::{self, ACCENT, BG, RAISED, TEXT, TEXT_DIM, TEXT_MID};
use crate::ui::widgets;

const LO_DB: f64 = -60.0;

/// The input range a transfer curve shows.
pub(crate) fn transfer_plot(rect: Rect) -> Plot {
    Plot::new(rect, Scale::lin(LO_DB, 6.0), Scale::lin(LO_DB, 6.0))
}

/// The ratio that puts the curve's output at `out_db` for a 0 dB input, given the threshold.
pub(crate) fn ratio_for(threshold_db: f64, out_db: f64) -> f64 {
    let over = -threshold_db;
    let rise = out_db - threshold_db;
    if over <= 0.0 || rise <= 0.0 { 100.0 } else { (over / rise).clamp(1.0, 100.0) }
}

/// Draws a transfer curve from `f` (input dB to output dB) with the unity line, the threshold
/// and, when given, the detector's level as a dot on the curve.
fn transfer(ui: &Ui, plot: &Plot, f: &dyn Fn(f64) -> f64, threshold: f64, knee: f64, dot: Option<f64>) {
    grid(ui, plot, false, 12.0, "");
    let painter = ui.painter_at(plot.rect);
    for db in super::model::db_ticks(LO_DB, 0.0, 12.0) {
        let x = plot.sx(db);
        painter.line_segment(
            [pos2(x, plot.rect.top()), pos2(x, plot.rect.bottom())],
            Stroke::new(1.0, theme::with_alpha(theme::RAISED_HI, 200)),
        );
    }
    if knee > 0.0 {
        let k = Rect::from_x_y_ranges(plot.sx(threshold - knee)..=plot.sx(threshold + knee), plot.rect.y_range());
        painter.rect_filled(k, CornerRadius::ZERO, theme::with_alpha(ACCENT, 18));
    }
    painter.line_segment(
        [plot.point(LO_DB, LO_DB), plot.point(6.0, 6.0)],
        Stroke::new(1.0, theme::with_alpha(TEXT_DIM, 90)),
    );
    let x = plot.sx(threshold);
    painter.line_segment(
        [pos2(x, plot.rect.top()), pos2(x, plot.rect.bottom())],
        Stroke::new(1.0, theme::with_alpha(ACCENT, 140)),
    );
    let xs = plot.xs(2.0);
    let points: Vec<Pos2> = xs.iter().map(|&v| plot.point(v, f(v).max(LO_DB - 6.0))).collect();
    area(ui, plot.rect, &points, plot.rect.bottom(), theme::with_alpha(ACCENT, 26));
    polyline(ui, plot.rect, points, Stroke::new(2.5, TEXT));
    if let Some(level) = dot.filter(|l| *l > LO_DB) {
        let p = plot.point(level.min(6.0), f(level).max(LO_DB));
        painter.circle_filled(p, 9.0, theme::with_alpha(theme::YELLOW, 60));
        painter.circle_filled(p, 5.0, theme::YELLOW);
    }
    painter.text(plot.rect.left_top() + vec2(6.0, 4.0), Align2::LEFT_TOP, "out", theme::regular(10.0), TEXT_DIM);
    painter.text(
        plot.rect.right_bottom() - vec2(28.0, 4.0),
        Align2::RIGHT_BOTTOM,
        "in",
        theme::regular(10.0),
        TEXT_DIM,
    );
}

/// The threshold handle (drag sideways) and ratio handle (drag up and down) of a curve.
fn curve_handles(cx: &mut Ctx, ui: &mut Ui, plot: &Plot, threshold: f64, out_at_zero: f64, ids: (u32, u32, u32)) {
    let (thr_id, ratio_id, knee_id) = ids;
    let thr = plot.point(threshold, threshold);
    let ratio = plot.point(0.0, out_at_zero.max(LO_DB));
    let resp =
        ui.interact(plot.rect, Id::unique(("fx-dyn-curve", &cx.ed.owner, &cx.ed.path, thr_id)), Sense::click_and_drag());
    let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
    if resp.drag_started() {
        let origin = widgets::drag_origin(ui, &resp);
        cx.ed.dragging = origin.and_then(|p| {
            let (dt, dr) = (p.distance(thr), p.distance(ratio));
            if dt <= 14.0 && dt <= dr {
                Some(0)
            } else if dr <= 14.0 {
                Some(1)
            } else {
                None
            }
        });
        cx.ed.focus = cx.ed.dragging.map(|h| if h == 0 { thr_id } else { ratio_id });
    }
    if resp.dragged()
        && let Some(p) = pointer
    {
        let (x, y) = plot.values(p);
        match cx.ed.dragging {
            Some(0) => cx.set(thr_id, x.clamp(LO_DB, 6.0)),
            Some(1) => cx.set(ratio_id, ratio_for(threshold, y)),
            _ => {}
        }
    }
    if resp.drag_stopped() {
        cx.ed.dragging = None;
        cx.end();
    }
    if resp.hovered() && cx.info(knee_id).is_some() {
        let wheel = super::wheel(ui);
        if wheel != 0.0 && pointer.is_some_and(|p| p.distance(thr) < 20.0) {
            let v = (cx.value(knee_id) + wheel.signum() as f64).clamp(0.0, 24.0);
            cx.set_once(knee_id, v);
        }
    }
    if resp.double_clicked() {
        let near = pointer.map(|p| (p.distance(thr), p.distance(ratio)));
        match near {
            Some((dt, dr)) if dt <= 14.0 && dt <= dr => super::start_typing_param(cx, thr_id),
            Some((_, dr)) if dr <= 14.0 => super::start_typing_param(cx, ratio_id),
            _ => {}
        }
    }
    resp.context_menu(|ui| {
        ui.menu_button("Threshold", |ui| super::param_menu(cx, ui, thr_id));
        ui.menu_button("Ratio", |ui| super::param_menu(cx, ui, ratio_id));
        if cx.info(knee_id).is_some() {
            ui.menu_button("Knee", |ui| super::param_menu(cx, ui, knee_id));
        }
    });
    let hot = |h: usize| cx.ed.dragging == Some(h) || cx.ed.focus == Some(if h == 0 { thr_id } else { ratio_id });
    handle(ui, plot.rect, thr, ACCENT, hot(0), "");
    handle(ui, plot.rect, ratio, theme::YELLOW, hot(1), "");
    let tip = format!(
        "threshold {} · ratio {} · knee {}",
        cx.text(thr_id),
        cx.text(ratio_id),
        if cx.info(knee_id).is_some() { cx.text(knee_id) } else { "soft".into() }
    );
    if plot.rect.width() >= 230.0 {
        ui.painter().text(
            plot.rect.left_top() + vec2(6.0, 18.0),
            Align2::LEFT_TOP,
            tip,
            theme::regular(10.0),
            TEXT_MID,
        );
    }
}

/// Input, output and gain reduction meters side by side.
fn meters(cx: &Ctx, ui: &Ui, rect: Rect, reduction: bool) {
    card(ui, rect);
    let inner = rect.shrink(8.0);
    let n = if reduction { 3.0 } else { 2.0 };
    let w = (inner.width() / n).min(36.0);
    let left = inner.center().x - w * n / 2.0;
    let col = |i: f32| Rect::from_min_size(pos2(left + i * w + 3.0, inner.top()), vec2(w - 6.0, inner.height()));
    labelled_meter(ui, col(0.0), "in", &cx.ed.input, false);
    if reduction {
        labelled_meter(ui, col(1.0), "GR", &cx.ed.reduction, true);
        labelled_meter(ui, col(2.0), "out", &cx.ed.output, false);
    } else {
        labelled_meter(ui, col(1.0), "out", &cx.ed.output, false);
    }
}

fn reduction_history(cx: &Ctx, ui: &Ui, rect: Rect) {
    card(ui, rect);
    section_title(ui, rect, "Gain reduction");
    let graph = Rect::from_min_max(rect.min + vec2(10.0, 28.0), rect.max - vec2(10.0, 10.0));
    ui.painter().rect_filled(graph, CornerRadius::same(8), BG);
    for db in [6.0, 12.0, 18.0] {
        let y = graph.top() + graph.height() * db / 24.0;
        ui.painter().line_segment(
            [pos2(graph.left(), y), pos2(graph.right(), y)],
            Stroke::new(1.0, theme::with_alpha(theme::RAISED_HI, 200)),
        );
        ui.painter().text(
            pos2(graph.right() - 4.0, y - 1.0),
            Align2::RIGHT_BOTTOM,
            format!("-{db:.0}"),
            theme::regular(10.0),
            TEXT_DIM,
        );
    }
    history(ui, graph, &cx.ed.history, 0.0, 24.0, theme::PINK, true);
}

/// The bottom part: knobs, and switches when there are any.
fn controls(cx: &mut Ctx, ui: &mut Ui, rect: Rect, knobs: &[(u32, &str)], switches: &[(u32, &str)]) {
    card(ui, rect);
    let inner = rect.shrink2(vec2(8.0, 6.0));
    let (knob_area, switch_area) =
        if switches.is_empty() { (inner, Rect::NOTHING) } else { split_top(inner, inner.height() - 26.0, 0.0) };
    let narrow = knob_area.width() / (knobs.len().max(1) as f32) < 62.0;
    let two_rows = (knob_area.height() >= 150.0 || (narrow && knob_area.height() >= 90.0)) && knobs.len() > 5;
    if two_rows {
        let half = knobs.len().div_ceil(2);
        let (a, b) = split_top(knob_area, knob_area.height() / 2.0, 0.0);
        knob_row(cx, ui, a, &knobs[..half]);
        knob_row(cx, ui, b, &knobs[half..]);
    } else {
        knob_row(cx, ui, knob_area, knobs);
    }
    if !switches.is_empty() {
        super::switch_row(cx, ui, switch_area.shrink2(vec2(6.0, 0.0)), switches);
    }
}

/// Splits an editor into its graphs and its controls, giving the controls what they need.
fn layout(rect: Rect, controls_h: f32) -> (Rect, Rect) {
    let h = if rect.height() > 430.0 { controls_h } else { controls_h.min(136.0).min(rect.height() * 0.5) };
    let (top, bottom) = split_top(rect, rect.height() - h - 8.0, 8.0);
    (top, bottom)
}

pub fn comp(cx: &mut Ctx, ui: &mut Ui, rect: Rect) {
    let Some((thr, knee)) = cx.read(|c: &mut Compressor| c.threshold_db()) else { return };
    let curve: Vec<(f64, f64)> = cx
        .read(|c: &mut Compressor| {
            let mut pts: Vec<f64> = (0..=264).map(|i| LO_DB + i as f64 * 0.25).collect();
            pts.push(0.0);
            pts.into_iter().map(|v| (v, c.transfer_db(v))).collect()
        })
        .unwrap_or_default();
    let f = |v: f64| lookup(&curve, v);
    let (graphs, bottom) = layout(rect, 186.0);
    let side = graphs.height().min(graphs.width() * 0.45);
    let (curve_card, rest) = split_left(graphs, side, 8.0);
    let (meter_rect, hist) = split_left(rest, 112.0, 8.0);
    card(ui, curve_card);
    let plot = transfer_plot(curve_card.shrink(10.0));
    let dot = (cx.ed.detector.db > -89.0).then_some(cx.ed.detector.db as f64);
    transfer(ui, &plot, &f, thr, knee, dot);
    curve_handles(cx, ui, &plot, thr, f(0.0), (0, 1, 14));
    meters(cx, ui, meter_rect, true);
    if hist.width() > 60.0 {
        reduction_history(cx, ui, hist);
    }
    controls(
        cx,
        ui,
        bottom,
        &[
            (0, "Threshold"),
            (1, "Ratio"),
            (14, "Knee"),
            (2, "Attack"),
            (3, "Release"),
            (13, "RMS"),
            (4, "Pre-comp"),
            (7, "Det. HP"),
            (6, "Det. LP"),
            (10, "Dry"),
            (11, "Wet"),
        ],
        &[(15, "Auto make-up"), (16, "Auto release"), (12, "Preview filter")],
    );
}

/// `curve` (sorted (x, y) points) at `v`, interpolated.
fn lookup(curve: &[(f64, f64)], v: f64) -> f64 {
    let i = curve.partition_point(|p| p.0 < v);
    match (curve.get(i.wrapping_sub(1)), curve.get(i)) {
        (Some(a), Some(b)) if b.0 > a.0 => a.1 + (b.1 - a.1) * (v - a.0) / (b.0 - a.0),
        (_, Some(b)) => b.1,
        (Some(a), None) => a.1 + (v - a.0),
        _ => v,
    }
}

pub fn gate(cx: &mut Ctx, ui: &mut Ui, rect: Rect) {
    let (graphs, bottom) = layout(rect, 186.0);
    let (graph_card, meter_rect) = split_left(graphs, graphs.width() - 120.0, 8.0);
    card(ui, graph_card);
    section_title(ui, graph_card, "Detector level");
    let open_now = cx.ed.gate.last().unwrap_or(0.0) > 0.5;
    let state = if open_now { "open" } else { "closed" };
    let pill = Rect::from_min_size(pos2(graph_card.right() - 76.0, graph_card.top() + 6.0), vec2(66.0, 20.0));
    ui.painter().rect_filled(pill, CornerRadius::same(10), if open_now { theme::GREEN } else { theme::SWITCH_OFF });
    ui.painter().text(
        pill.center(),
        Align2::CENTER_CENTER,
        state,
        theme::semibold(11.0),
        if open_now { BG } else { TEXT },
    );
    let graph = Rect::from_min_max(graph_card.min + vec2(10.0, 30.0), graph_card.max - vec2(10.0, 10.0));
    let plot = Plot::new(graph, Scale::lin(0.0, 1.0), Scale::lin(LO_DB, 6.0));
    grid(ui, &plot, false, 12.0, " dB");
    let n = cx.ed.gate.capacity().max(2);
    let start = n - cx.ed.gate.len();
    let x = |i: usize| graph.left() + (start + i) as f32 / (n - 1) as f32 * graph.width();
    let gates: Vec<f32> = cx.ed.gate.iter().collect();
    let mut i = 0;
    while i < gates.len() {
        if gates[i] < 0.5 {
            i += 1;
            continue;
        }
        let from = i;
        while i < gates.len() && gates[i] >= 0.5 {
            i += 1;
        }
        let r = Rect::from_min_max(pos2(x(from), graph.top()), pos2(x(i - 1).max(x(from) + 1.0), graph.bottom()));
        ui.painter_at(graph).rect_filled(r, CornerRadius::ZERO, theme::with_alpha(theme::GREEN, 30));
    }
    let lo = LO_DB as f32;
    history(ui, graph, &cx.ed.history, lo, 6.0, theme::YELLOW, false);
    let threshold = cx.value(0);
    let hysteresis = cx.value(12);
    let close = threshold + hysteresis;
    let line = |db: f64| plot.sy(db.clamp(LO_DB, 6.0));
    let (ty, cy) = (line(threshold), line(close));
    let painter = ui.painter_at(graph);
    painter.line_segment([pos2(graph.left(), ty), pos2(graph.right(), ty)], Stroke::new(2.0, ACCENT));
    painter.line_segment(
        [pos2(graph.left(), cy), pos2(graph.right(), cy)],
        Stroke::new(1.0, theme::with_alpha(ACCENT, 150)),
    );
    painter.text(
        pos2(graph.left() + 6.0, ty - 2.0),
        Align2::LEFT_BOTTOM,
        format!("open {}", cx.text(0)),
        theme::regular(10.0),
        ACCENT,
    );
    painter.text(
        pos2(graph.left() + 6.0, cy + 2.0),
        Align2::LEFT_TOP,
        format!("close {close:+.1} dB"),
        theme::regular(10.0),
        TEXT_MID,
    );
    let resp = ui.interact(graph, Id::unique(("fx-gate-graph", &cx.ed.owner, &cx.ed.path)), Sense::click_and_drag());
    if resp.drag_started() {
        let origin = widgets::drag_origin(ui, &resp);
        cx.ed.dragging = origin.and_then(|p| {
            let (dt, dc) = ((p.y - ty).abs(), (p.y - cy).abs());
            if dt <= 8.0 && dt <= dc {
                Some(0)
            } else if dc <= 8.0 {
                Some(1)
            } else {
                None
            }
        });
        cx.ed.focus = cx.ed.dragging.map(|h| if h == 0 { 0 } else { 12 });
    }
    if resp.dragged()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let (_, db) = plot.values(p);
        match cx.ed.dragging {
            Some(0) => cx.set(0, db.clamp(LO_DB, 6.0)),
            Some(1) => cx.set(12, (db - threshold).clamp(-60.0, 0.0)),
            _ => {}
        }
    }
    if resp.drag_stopped() {
        cx.ed.dragging = None;
        cx.end();
    }
    resp.context_menu(|ui| {
        ui.menu_button("Threshold", |ui| super::param_menu(cx, ui, 0));
        ui.menu_button("Hysteresis", |ui| super::param_menu(cx, ui, 12));
    });
    let resp = resp.on_hover_text("Drag the lines: opening threshold, and the closing one (hysteresis)");
    let _ = resp;
    meters(cx, ui, meter_rect, false);
    controls(
        cx,
        ui,
        bottom,
        &[
            (0, "Threshold"),
            (12, "Hysteresis"),
            (1, "Attack"),
            (4, "Hold"),
            (2, "Release"),
            (3, "Pre-open"),
            (14, "RMS"),
            (6, "Det. HP"),
            (5, "Det. LP"),
            (9, "Dry"),
            (10, "Wet"),
            (11, "Noise"),
        ],
        &[(13, "Preview filter"), (15, "Send MIDI")],
    );
}

pub fn limit(cx: &mut Ctx, ui: &mut Ui, rect: Rect) {
    let (graphs, bottom) = layout(rect, 120.0);
    let (meter_rect, hist) = split_left(graphs, 150.0, 8.0);
    card(ui, meter_rect);
    let inner = meter_rect.shrink(8.0);
    let w = inner.width() / 3.0;
    let col =
        |i: f32| Rect::from_min_size(pos2(inner.left() + i * w + 4.0, inner.top()), vec2(w - 8.0, inner.height()));
    labelled_meter(ui, col(0.0), "in", &cx.ed.input, false);
    labelled_meter(ui, col(1.0), "GR", &cx.ed.reduction, true);
    labelled_meter(ui, col(2.0), "out", &cx.ed.output, false);
    let marker = |col: Rect, db: f64, color| {
        let bar = Rect::from_min_max(col.min + vec2(0.0, 16.0), col.max - vec2(0.0, 18.0));
        let y = bar.bottom() - bar.height() * ((db as f32 + 60.0) / 66.0).clamp(0.0, 1.0);
        ui.painter().line_segment([pos2(bar.left() - 3.0, y), pos2(bar.right() + 3.0, y)], Stroke::new(2.0, color));
        bar
    };
    let in_bar = marker(col(0.0), cx.value(0), ACCENT);
    let out_bar = marker(col(2.0), cx.value(1), theme::YELLOW);
    for (bar, id) in [(in_bar, 0u32), (out_bar, 1u32)] {
        let resp = ui.interact(
            bar.expand2(vec2(4.0, 0.0)),
            Id::unique(("fx-limit-line", &cx.ed.owner, &cx.ed.path, id)),
            Sense::click_and_drag(),
        );
        if resp.dragged()
            && let Some(p) = resp.interact_pointer_pos()
        {
            let db = ((bar.bottom() - p.y) / bar.height()) as f64 * 66.0 - 60.0;
            let info = cx.info(id).cloned();
            if let Some(info) = info {
                cx.set(id, db.clamp(info.min, info.max));
            }
            cx.ed.focus = Some(id);
        }
        if resp.drag_stopped() {
            cx.end();
        }
        if resp.double_clicked() {
            super::start_typing_param(cx, id);
        }
        resp.on_hover_text(if id == 0 { "Drag: threshold" } else { "Drag: ceiling" })
            .context_menu(|ui| super::param_menu(cx, ui, id));
    }
    if hist.width() > 60.0 {
        reduction_history(cx, ui, hist);
    }
    controls(cx, ui, bottom, &[(0, "Threshold"), (1, "Ceiling"), (2, "Release")], &[]);
}

pub fn xcomp(cx: &mut Ctx, ui: &mut Ui, rect: Rect) {
    let Some(bands) = cx.read(|x: &mut MultibandCompressor| x.band_views()) else { return };
    if bands.is_empty() {
        return;
    }
    cx.ed.band = cx.ed.band.min(bands.len() - 1);
    let k = cx.ed.band;
    let sr = cx.inst.probe.sample_rate().max(44100) as f64;
    let top = (sr / 2.0).min(22000.0);
    let (graph_card, bottom) = split_top(rect, (rect.height() * 0.48).max(120.0), 8.0);
    card(ui, graph_card);
    let graph = graph_card.shrink(8.0);
    let plot = Plot::new(graph, Scale::log(20.0, top), Scale::lin(-36.0, 6.0));
    grid(ui, &plot, true, 12.0, "");
    let levels = Plot::new(graph, plot.x, Scale::lin(-120.0, 12.0));
    spectrum(ui, &levels, &cx.ed.post, 0.0, theme::with_alpha(TEXT_DIM, 30), theme::with_alpha(TEXT_DIM, 90));
    let xs = plot.xs(3.0);
    let curves: Vec<Vec<f64>> = cx
        .read(|x: &mut MultibandCompressor| {
            (0..bands.len()).map(|b| xs.iter().map(|&f| x.band_response_db(b, f)).collect()).collect()
        })
        .unwrap_or_default();
    for (b, curve) in curves.iter().enumerate() {
        let gr = cx.ed.bands.get(b).map_or(0.0, |m| m.db as f64);
        let points: Vec<Pos2> = xs.iter().zip(curve).map(|(&f, &db)| plot.point(f, (db - gr).max(-60.0))).collect();
        let color = band_color(b);
        if b == k {
            area(ui, graph, &points, graph.bottom(), theme::with_alpha(color, 36));
        }
        polyline(
            ui,
            graph,
            points,
            Stroke::new(
                if b == k { 2.0 } else { 1.2 },
                theme::with_alpha(color, if bands[b].active { 220 } else { 80 }),
            ),
        );
    }
    crossovers(cx, ui, &plot, &bands);
    let (curve_rect, rest) = split_left(bottom, bottom.height().min(bottom.width() * 0.34), 8.0);
    card(ui, curve_rect);
    let tplot = transfer_plot(curve_rect.shrink(10.0));
    let band_curve: Vec<(f64, f64)> = cx
        .read(|x: &mut MultibandCompressor| {
            (0..=264).map(|i| LO_DB + i as f64 * 0.25).map(|v| (v, x.transfer_db(k, v))).collect()
        })
        .unwrap_or_default();
    let f = |v: f64| lookup(&band_curve, v);
    let view = bands[k];
    transfer(ui, &tplot, &f, view.threshold_db, view.knee_db, None);
    let base = k as u32 * reaxcomp::BAND_PARAMS;
    curve_handles(
        cx,
        ui,
        &tplot,
        view.threshold_db,
        f(0.0),
        (base + reaxcomp::THRESHOLD, base + reaxcomp::RATIO, base + reaxcomp::KNEE),
    );
    let (gr_rect, knobs) = split_left(rest, (bands.len() as f32 * 26.0 + 20.0).min(rest.width() * 0.3), 8.0);
    card(ui, gr_rect);
    let inner = gr_rect.shrink(8.0);
    let w = inner.width() / bands.len() as f32;
    for (b, view) in bands.iter().enumerate() {
        let col =
            Rect::from_min_size(pos2(inner.left() + b as f32 * w + 2.0, inner.top()), vec2(w - 4.0, inner.height()));
        let resp = ui.interact(col, Id::unique(("fx-xcomp-gr", &cx.ed.owner, &cx.ed.path, b)), Sense::click());
        if resp.clicked() {
            cx.ed.band = b;
        }
        let bar = Rect::from_min_max(col.min + vec2(0.0, 18.0), col.max - vec2(0.0, 16.0));
        let reading = cx.ed.bands.get(b).map_or(0.0, |m| m.db);
        super::meter(
            ui,
            bar,
            reading,
            None,
            0.0,
            24.0,
            theme::with_alpha(band_color(b), if view.active { 255 } else { 90 }),
            true,
        );
        let label_color = if b == k { band_color(b) } else { TEXT_DIM };
        ui.painter().text(
            pos2(col.center().x, col.top() + 7.0),
            Align2::CENTER_CENTER,
            (b + 1).to_string(),
            theme::semibold(11.0),
            label_color,
        );
        ui.painter().text(
            pos2(col.center().x, col.bottom() - 6.0),
            Align2::CENTER_CENTER,
            if reading < 0.5 { "0".to_owned() } else { format!("-{reading:.0}") },
            theme::mono(9.0),
            TEXT,
        );
    }
    card(ui, knobs);
    let inner = knobs.shrink2(vec2(8.0, 6.0));
    let (title, rest) = split_top(inner, 22.0, 2.0);
    let lo = if k == 0 { 0.0 } else { bands[k - 1].top };
    let range = if k == 0 {
        format!("below {}", hz_text(view.top))
    } else if k + 1 == bands.len() {
        format!("above {}", hz_text(lo))
    } else {
        format!("{} – {}", hz_text(lo), hz_text(view.top))
    };
    ui.painter().text(
        title.left_center(),
        Align2::LEFT_CENTER,
        format!("Band {} · {range}", k + 1),
        theme::semibold(12.0),
        band_color(k),
    );
    let (knob_area, switch_area) = split_top(rest, rest.height() - 26.0, 0.0);
    knob_row(
        cx,
        ui,
        knob_area,
        &[
            (base + reaxcomp::THRESHOLD, "Threshold"),
            (base + reaxcomp::RATIO, "Ratio"),
            (base + reaxcomp::KNEE, "Knee"),
            (base + reaxcomp::ATTACK, "Attack"),
            (base + reaxcomp::RELEASE, "Release"),
            (base + reaxcomp::RMS, "RMS"),
            (base + 1, "Gain"),
        ],
    );
    let switches = [
        (base + reaxcomp::ACTIVE, "Active"),
        (base + 8, "Make-up"),
        (base + 9, "Auto release"),
        (base + 10, "Feedback"),
    ];
    super::switch_row(cx, ui, switch_area, &switches);
}

/// The crossover lines between bands, dragged sideways.
fn crossovers(cx: &mut Ctx, ui: &mut Ui, plot: &Plot, bands: &[BandView]) {
    let n = bands.len().saturating_sub(1);
    let xs: Vec<f32> = bands[..n].iter().map(|b| plot.sx(b.top)).collect();
    let painter = ui.painter_at(plot.rect);
    for (b, &x) in xs.iter().enumerate() {
        let color = band_color(b);
        painter.line_segment(
            [pos2(x, plot.rect.top()), pos2(x, plot.rect.bottom())],
            Stroke::new(1.5, theme::with_alpha(color, 200)),
        );
        let knob = pos2(x, plot.rect.top() + 12.0);
        handle(ui, plot.rect, knob, color, cx.ed.sel == Some(b), "");
        painter.text(
            pos2(x + 9.0, plot.rect.top() + 4.0),
            Align2::LEFT_TOP,
            hz_text(bands[b].top),
            theme::regular(10.0),
            TEXT_MID,
        );
    }
    let resp = ui.interact(plot.rect, Id::unique(("fx-xcomp-x", &cx.ed.owner, &cx.ed.path)), Sense::click_and_drag());
    let near = |p: Pos2| {
        xs.iter()
            .enumerate()
            .map(|(b, &x)| (b, (p.x - x).abs()))
            .filter(|(_, d)| *d <= 8.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(b, _)| b)
    };
    if resp.drag_started() || resp.clicked() {
        let origin = widgets::drag_origin(ui, &resp);
        cx.ed.sel = origin.and_then(near);
        if let Some(b) = cx.ed.sel {
            cx.ed.band = b;
        } else if let Some(p) = origin {
            let (hz, _) = plot.values(p);
            cx.ed.band = bands.iter().position(|b| hz <= b.top).unwrap_or(bands.len() - 1);
        }
    }
    if resp.dragged()
        && let (Some(b), Some(p)) = (cx.ed.sel, resp.interact_pointer_pos())
    {
        let (hz, _) = plot.values(p);
        let lo = if b == 0 { 20.0 } else { bands[b - 1].top * 1.05 };
        let hi = bands.get(b + 1).map_or(plot.x.hi, |n| if b + 2 == bands.len() { plot.x.hi } else { n.top / 1.05 });
        cx.set(b as u32 * reaxcomp::BAND_PARAMS + reaxcomp::TOP, hz.clamp(lo, hi.max(lo)));
    }
    if resp.drag_stopped() {
        cx.end();
    }
    if resp.hovered()
        && let Some(b) = cx.ed.sel
        && b < n
    {
        let (l, r) = ui.input(|i| (i.key_pressed(egui::Key::ArrowLeft), i.key_pressed(egui::Key::ArrowRight)));
        if l || r {
            let id = b as u32 * reaxcomp::BAND_PARAMS + reaxcomp::TOP;
            cx.set(id, bands[b].top * 2f64.powf(if r { 1.0 / 12.0 } else { -1.0 / 12.0 }));
            cx.ed.key_gesture = true;
        }
    }
    resp.context_menu(|ui| {
        if let Some(b) = cx.ed.sel.filter(|b| *b < n) {
            super::param_menu(cx, ui, b as u32 * reaxcomp::BAND_PARAMS + reaxcomp::TOP);
        } else {
            ui.label(egui::RichText::new("Drag a crossover line").color(TEXT_DIM));
        }
    });
    let _ = (RAISED, amp_db(1.0));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ratio_handle_maps_to_a_ratio() {
        assert!((ratio_for(-20.0, -15.0) - 4.0).abs() < 1e-12);
        assert_eq!(ratio_for(-20.0, 0.0), 1.0);
        assert_eq!(ratio_for(-20.0, -25.0), 100.0);
        let plot = transfer_plot(Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 300.0)));
        let p = plot.point(0.0, -15.0);
        let (_, y) = plot.values(p);
        assert!((ratio_for(-20.0, y) - 4.0).abs() < 1e-6);
    }

    #[test]
    fn curves_are_read_between_their_points() {
        let curve = [(-10.0, -10.0), (0.0, -5.0)];
        assert_eq!(lookup(&curve, -5.0), -7.5);
        assert_eq!(lookup(&curve, -20.0), -10.0);
        assert_eq!(lookup(&curve, 2.0), -3.0);
    }
}
