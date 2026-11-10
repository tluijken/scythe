//! ReaDelay: its taps on a time line, each echo of the feedback fading after it, and a row of
//! values per tap.

use egui::{Align2, CornerRadius, Id, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use scythe_engine::delay::{ReaDelayCompat, Tap};

use super::model::{Plot, Scale};
use super::{Ctx, Field, band_color, card, field_box, handle, knob_row, split_left, split_top};
use crate::theme::{self, ACCENT, BG, RAISED, TEXT, TEXT_DIM, TEXT_MID};
use crate::ui::widgets;

const DB_LO: f64 = -48.0;

fn db(gain: f64) -> f64 {
    if gain <= 1e-6 { -150.0 } else { 20.0 * gain.log10() }
}

fn gain(db: f64) -> f64 {
    if db <= -59.9 { 0.0 } else { 10f64.powf(db / 20.0) }
}

/// A tap's delay in milliseconds at `bpm`: its time plus its musical length in eighths.
pub(crate) fn tap_ms(t: &Tap, bpm: f64) -> f64 {
    t.time_ms + t.musical * 30_000.0 / bpm.max(1.0)
}

/// The tap's time for a delay of `ms` when dragged on the time line: the musical part stays.
pub(crate) fn time_for(t: &Tap, ms: f64, bpm: f64) -> f64 {
    (ms - t.musical * 30_000.0 / bpm.max(1.0)).clamp(0.0, 10_000.0)
}

/// How long the time line runs: past the longest tap's second echo, at least half a second.
pub(crate) fn span_ms(taps: &[Tap], bpm: f64) -> f64 {
    let longest = taps.iter().map(|t| tap_ms(t, bpm)).fold(0.0, f64::max);
    (longest * 2.5).clamp(500.0, 12_000.0)
}

pub fn show(cx: &mut Ctx, ui: &mut Ui, rect: Rect) {
    let Some(taps) = cx.read(|d: &mut ReaDelayCompat| d.taps()) else { return };
    let bpm = cx.app.project.tempo.bpm_at(cx.app.engine.transport().position_seconds());
    if cx.ed.sel.is_some_and(|k| k >= taps.len()) {
        cx.ed.sel = None;
    }
    let list_h = (taps.len() as f32 * 30.0 + 40.0).min(rect.height() * 0.4).max(70.0);
    let (top, list) = split_top(rect, rect.height() - list_h - 8.0, 8.0);
    let (line_card, knobs) = split_left(top, top.width() - 150.0, 8.0);
    card(ui, line_card);
    let graph = line_card.shrink(10.0);
    let span = span_ms(&taps, bpm);
    let plot = Plot::new(graph, Scale::lin(0.0, span), Scale::lin(DB_LO, 6.0));
    timeline(cx, ui, &plot, &taps, bpm);
    card(ui, knobs);
    let inner = knobs.shrink(6.0);
    let (a, b) = split_top(inner, inner.height() / 2.0, 0.0);
    knob_row(cx, ui, a, &[(0, "Wet")]);
    knob_row(cx, ui, b, &[(1, "Dry")]);
    tap_list(cx, ui, list, &taps, bpm);
}

fn timeline(cx: &mut Ctx, ui: &mut Ui, plot: &Plot, taps: &[Tap], bpm: f64) {
    let graph = plot.rect;
    let painter = ui.painter_at(graph);
    painter.rect_filled(graph, CornerRadius::same(8), BG);
    let beat = 60_000.0 / bpm.max(1.0);
    let mut t = 0.0;
    let mut k = 0;
    while t <= plot.x.hi && k < 400 {
        let x = plot.sx(t);
        let bar = k % 4 == 0;
        painter.line_segment(
            [pos2(x, graph.top()), pos2(x, graph.bottom())],
            Stroke::new(1.0, theme::with_alpha(theme::RAISED_HI, if bar { 255 } else { 140 })),
        );
        if bar || plot.x.hi / beat < 12.0 {
            painter.text(
                pos2(x + 3.0, graph.bottom() - 3.0),
                Align2::LEFT_BOTTOM,
                format!("{:.0} ms", t),
                theme::regular(10.0),
                TEXT_DIM,
            );
        }
        t += beat;
        k += 1;
    }
    for d in [-12.0, -24.0, -36.0, 0.0] {
        let y = plot.sy(d);
        painter.line_segment(
            [pos2(graph.left(), y), pos2(graph.right(), y)],
            Stroke::new(1.0, theme::with_alpha(theme::RAISED_HI, 120)),
        );
        painter.text(
            pos2(graph.right() - 4.0, y - 1.0),
            Align2::RIGHT_BOTTOM,
            format!("{d:.0} dB"),
            theme::regular(10.0),
            TEXT_DIM,
        );
    }
    let base = graph.bottom();
    let mut at = Vec::new();
    for (k, tap) in taps.iter().enumerate() {
        let color = if tap.enabled { band_color(k) } else { theme::SWITCH_OFF };
        let ms = tap_ms(tap, bpm);
        let mut level = db(tap.volume);
        let mut n = 1.0;
        while ms > 0.0 && n < 24.0 && ms * n <= plot.x.hi {
            let x = plot.sx(ms * n);
            let alpha = if n == 1.0 { 255 } else { (160.0 / n) as u8 + 30 };
            let y = plot.sy(level.max(DB_LO));
            painter.line_segment(
                [pos2(x, base), pos2(x, y)],
                Stroke::new(if n == 1.0 { 2.5 } else { 1.5 }, theme::with_alpha(color, alpha)),
            );
            let fb = db(tap.feedback);
            if fb < -59.0 {
                break;
            }
            level += fb;
            n += 1.0;
            if level < DB_LO {
                break;
            }
        }
        let p = plot.point(ms, db(tap.volume).clamp(DB_LO, 6.0));
        let pan = Rect::from_center_size(pos2(p.x, base - 6.0), vec2(24.0, 4.0));
        painter.rect_filled(pan, CornerRadius::same(2), theme::with_alpha(TEXT_DIM, 60));
        painter.circle_filled(pos2(pan.center().x + tap.pan as f32 * 12.0, pan.center().y), 3.0, color);
        at.push(p);
    }
    for (k, p) in at.iter().enumerate() {
        handle(
            ui,
            graph,
            *p,
            if taps[k].enabled { band_color(k) } else { theme::SWITCH_OFF },
            cx.ed.sel == Some(k),
            &(k + 1).to_string(),
        );
    }
    let resp = ui.interact(graph, Id::unique(("fx-delay-line", &cx.ed.owner, &cx.ed.path)), Sense::click_and_drag());
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
        cx.ed.sel = origin.and_then(near);
        cx.ed.dragging = if resp.drag_started() { cx.ed.sel } else { None };
        cx.ed.focus = None;
    }
    if resp.dragged()
        && let (Some(k), Some(p)) = (cx.ed.dragging, resp.interact_pointer_pos())
        && let Some(tap) = taps.get(k)
    {
        let (ms, level) = plot.values(p);
        let mut t = *tap;
        t.time_ms = time_for(tap, ms, bpm);
        t.volume = gain(level.clamp(DB_LO, 6.0));
        cx.edit("Change delay tap", |d: &mut ReaDelayCompat| d.set_tap(k, t));
    }
    if resp.drag_stopped() {
        cx.ed.dragging = None;
        cx.end();
    }
    if resp.double_clicked() {
        let pointer = resp.interact_pointer_pos();
        match pointer.and_then(near) {
            Some(k) => {
                let mut t = taps[k];
                t.enabled = !t.enabled;
                cx.edit_once("Change delay tap", |d: &mut ReaDelayCompat| d.set_tap(k, t));
            }
            None => {
                if let (Some(p), Some(last)) = (pointer, taps.last()) {
                    let (ms, level) = plot.values(p);
                    let mut t = *last;
                    (t.musical, t.time_ms, t.volume) = (0.0, ms.clamp(0.0, 10_000.0), gain(level.clamp(DB_LO, 6.0)));
                    add_tap(cx, t);
                }
            }
        }
    }
    if resp.hovered()
        && let Some(k) = cx.ed.sel
        && let Some(tap) = taps.get(k)
        && cx.ed.typing.is_none()
    {
        let (l, r, u, d, fine) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::ArrowRight),
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.modifiers.shift,
            )
        });
        let (side, vert) = (r as i32 - l as i32, u as i32 - d as i32);
        if side != 0 || vert != 0 {
            let mut t = *tap;
            t.time_ms = (t.time_ms + side as f64 * if fine { 1.0 } else { 10.0 }).clamp(0.0, 10_000.0);
            t.volume = gain((db(t.volume) + vert as f64 * if fine { 0.1 } else { 1.0 }).clamp(-60.0, 6.0));
            cx.edit("Change delay tap", |d: &mut ReaDelayCompat| d.set_tap(k, t));
            cx.ed.key_gesture = true;
        }
    }
    resp.context_menu(|ui| {
        if let Some(k) = cx.ed.sel.filter(|k| *k < taps.len()) {
            if ui.button(if taps[k].enabled { "Disable tap" } else { "Enable tap" }).clicked() {
                let mut t = taps[k];
                t.enabled = !t.enabled;
                cx.edit_once("Change delay tap", |d: &mut ReaDelayCompat| d.set_tap(k, t));
                ui.close();
            }
            if taps.len() > 1 && ui.button("Remove tap").clicked() {
                cx.edit_once("Remove delay tap", |d: &mut ReaDelayCompat| d.remove_tap(k));
                cx.ed.sel = None;
                ui.close();
            }
            if k == 0 && cx.ed.track_fx().is_some() {
                ui.menu_button("Modulate/Learn…", |ui| {
                    for (id, label) in [(2, "Time"), (3, "Length"), (4, "Feedback")] {
                        if ui.button(label).clicked() {
                            cx.modulate(id);
                            ui.close();
                        }
                    }
                });
            }
        } else if ui.button("Add tap").clicked() {
            if let Some(last) = taps.last() {
                add_tap(cx, *last);
            }
            ui.close();
        }
    });
}

fn add_tap(cx: &mut Ctx, t: Tap) {
    if let Some(k) = cx.edit_once("Add delay tap", |d: &mut ReaDelayCompat| d.add_tap(t)) {
        cx.ed.sel = Some(k);
    }
}

/// The values of a tap the list shows, in its columns.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Column {
    Time,
    Length,
    Volume,
    Pan,
    Feedback,
    Lowpass,
    Highpass,
    Width,
}

const COLUMNS: [(Column, &str); 8] = [
    (Column::Time, "Time"),
    (Column::Length, "1/8s"),
    (Column::Volume, "Level"),
    (Column::Pan, "Pan"),
    (Column::Feedback, "Feedback"),
    (Column::Lowpass, "LP"),
    (Column::Highpass, "HP"),
    (Column::Width, "Width"),
];

fn field(column: Column, tap: &Tap, key: u64) -> Field {
    let (value, min, max, log, text) = match column {
        Column::Time => (tap.time_ms, 0.0, 10_000.0, false, format!("{:.0} ms", tap.time_ms)),
        Column::Length => (tap.musical, 0.0, 64.0, false, format!("{:.2}", tap.musical)),
        Column::Volume => {
            (db(tap.volume).max(-60.0), -60.0, 6.0, false, format!("{:+.1} dB", db(tap.volume).max(-60.0)))
        }
        Column::Pan => (tap.pan, -1.0, 1.0, false, format!("{:+.0}%", tap.pan * 100.0)),
        Column::Feedback => (
            db(tap.feedback).max(-60.0),
            -60.0,
            0.0,
            false,
            if tap.feedback <= 1e-3 { "off".into() } else { format!("{:.1} dB", db(tap.feedback)) },
        ),
        Column::Lowpass => (
            tap.lowpass.max(20.0),
            20.0,
            20_000.0,
            true,
            if tap.lowpass >= 20_000.0 { "off".into() } else { format!("{:.0} Hz", tap.lowpass) },
        ),
        Column::Highpass => (
            tap.hipass.max(1.0),
            1.0,
            20_000.0,
            true,
            if tap.hipass < 1.0 { "off".into() } else { format!("{:.0} Hz", tap.hipass) },
        ),
        Column::Width => (tap.width, -1.0, 1.0, false, format!("{:+.2}", tap.width)),
    };
    Field { key, value, min, max, log, text }
}

fn apply(column: Column, tap: &mut Tap, v: f64) {
    match column {
        Column::Time => tap.time_ms = v,
        Column::Length => tap.musical = v.max(0.0),
        Column::Volume => tap.volume = gain(v),
        Column::Pan => tap.pan = v.clamp(-1.0, 1.0),
        Column::Feedback => tap.feedback = gain(v).min(1.0),
        Column::Lowpass => tap.lowpass = if v >= 19_900.0 { 20_000.0 } else { v },
        Column::Highpass => tap.hipass = if v <= 1.0 { 0.0 } else { v },
        Column::Width => tap.width = v.clamp(-1.0, 1.0),
    }
}

fn tap_list(cx: &mut Ctx, ui: &mut Ui, rect: Rect, taps: &[Tap], bpm: f64) {
    card(ui, rect);
    let inner = rect.shrink2(vec2(8.0, 6.0));
    let head = Rect::from_min_size(inner.min, vec2(inner.width(), 20.0));
    let lead = 64.0;
    let tail = 30.0;
    let shown: Vec<(Column, &str)> = if inner.width() < 560.0 { COLUMNS[..5].to_vec() } else { COLUMNS.to_vec() };
    let w = (inner.width() - lead - tail) / shown.len() as f32;
    for (i, (_, label)) in shown.iter().enumerate() {
        ui.painter().text(
            pos2(inner.left() + lead + i as f32 * w + 6.0, head.center().y),
            Align2::LEFT_CENTER,
            *label,
            theme::regular(10.0),
            TEXT_DIM,
        );
    }
    let add = Rect::from_min_size(pos2(inner.left(), head.top()), vec2(56.0, 18.0));
    if widgets::text_pill(ui, add, "+ Tap", theme::regular(10.0), RAISED, TEXT_MID).clicked()
        && let Some(last) = taps.last()
    {
        let mut t = *last;
        t.time_ms = (tap_ms(last, bpm) * 1.5).clamp(1.0, 10_000.0);
        t.musical = 0.0;
        add_tap(cx, t);
    }
    let mut child = ui.new_child(
        egui::UiBuilder::new().max_rect(Rect::from_min_max(pos2(inner.left(), head.bottom() + 4.0), inner.max)),
    );
    egui::ScrollArea::vertical().id_salt(("fx-delay-taps", &cx.ed.owner, &cx.ed.path)).show(&mut child, |ui| {
        for (k, tap) in taps.iter().enumerate() {
            let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
            let selected = cx.ed.sel == Some(k);
            if selected {
                ui.painter().rect_stroke(
                    row,
                    CornerRadius::same(8),
                    Stroke::new(1.0, band_color(k)),
                    egui::StrokeKind::Inside,
                );
            }
            let chip = Rect::from_min_size(row.min + vec2(2.0, 2.0), vec2(22.0, 22.0));
            let resp = ui.interact(chip, Id::unique(("fx-delay-chip", &cx.ed.owner, &cx.ed.path, k)), Sense::click());
            ui.painter().circle_filled(
                chip.center(),
                11.0,
                if tap.enabled { band_color(k) } else { theme::SWITCH_OFF },
            );
            ui.painter().text(chip.center(), Align2::CENTER_CENTER, (k + 1).to_string(), theme::semibold(10.0), BG);
            if resp.clicked() {
                cx.ed.sel = Some(k);
            }
            let sw = Rect::from_min_size(pos2(chip.right() + 6.0, row.top() + 4.0), vec2(30.0, 18.0));
            if widgets::switch(ui, sw, tap.enabled, band_color(k)).on_hover_text("Tap on or off").clicked() {
                let mut t = *tap;
                t.enabled = !t.enabled;
                cx.edit_once("Change delay tap", |d: &mut ReaDelayCompat| d.set_tap(k, t));
            }
            for (i, (column, _)) in shown.iter().enumerate() {
                let r = Rect::from_min_size(
                    pos2(row.left() + lead + i as f32 * w + 2.0, row.top() + 1.0),
                    vec2(w - 4.0, 24.0),
                );
                let key = (k as u64) << 8 | i as u64;
                let change = field_box(cx, ui, r, &field(*column, tap, key), "");
                if let Some(v) = change.value {
                    let mut t = *tap;
                    apply(*column, &mut t, v);
                    cx.edit("Change delay tap", |d: &mut ReaDelayCompat| d.set_tap(k, t));
                }
                if change.done {
                    cx.end();
                }
            }
            let remove = Rect::from_min_size(pos2(row.right() - 24.0, row.top() + 2.0), vec2(22.0, 22.0));
            if taps.len() > 1
                && widgets::icon_button(ui, remove, widgets::Icon::Close, RAISED, TEXT_DIM, 10.0, "Remove tap")
                    .clicked()
            {
                cx.edit_once("Remove delay tap", |d: &mut ReaDelayCompat| d.remove_tap(k));
                cx.ed.sel = None;
                return;
            }
            ui.add_space(2.0);
        }
    });
    let _ = (ACCENT, TEXT);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tap(time_ms: f64, musical: f64) -> Tap {
        Tap {
            enabled: true,
            time_ms,
            musical,
            feedback: 0.5,
            lowpass: 20000.0,
            hipass: 0.0,
            width: 1.0,
            volume: 0.5,
            pan: 0.0,
        }
    }

    #[test]
    fn taps_sit_at_time_plus_musical_length() {
        assert_eq!(tap_ms(&tap(100.0, 2.0), 120.0), 600.0);
        let t = tap(100.0, 2.0);
        assert_eq!(time_for(&t, 700.0, 120.0), 200.0);
        assert_eq!(time_for(&t, 100.0, 120.0), 0.0);
        assert_eq!(span_ms(&[tap(100.0, 0.0)], 120.0), 500.0);
        assert_eq!(span_ms(&[tap(1000.0, 0.0)], 120.0), 2500.0);
    }

    #[test]
    fn list_fields_round_trip_through_the_tap() {
        let original = tap(250.0, 0.0);
        for (column, _) in COLUMNS {
            let f = field(column, &original, 0);
            let mut t = original;
            apply(column, &mut t, f.value);
            let back = field(column, &t, 0);
            assert!((back.value - f.value).abs() < 1e-9, "{}", f.text);
        }
    }
}
