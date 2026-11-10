//! ReaEQ: the response of every band and of the whole EQ over the spectrum before and after
//! it, with a handle per band.

use egui::{Align2, CornerRadius, Id, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use scythe_engine::reaeq::{Band, BandKind, ReaEqCompat, response_db};

use super::model::{Plot, Scale, hz_text};
use super::{Ctx, area, band_color, grid, handle, polyline, spectrum, split_top, value_box};
use crate::theme::{self, ACCENT, BG, RAISED, TEXT, TEXT_DIM, TEXT_MID};
use crate::ui::widgets;

/// The types the menu offers, in REAPER's order of its own menu.
pub(crate) const KINDS: [BandKind; 8] = [
    BandKind::LowShelf,
    BandKind::HighShelf,
    BandKind::Peak,
    BandKind::Notch,
    BandKind::LowPass,
    BandKind::HighPass,
    BandKind::BandPass,
    BandKind::AllPass,
];

const DB_RANGE: f64 = 24.0;

/// Parameter ids of band `k`: on, frequency, gain, width.
pub(crate) fn ids(k: usize) -> [u32; 4] {
    let b = k as u32 * 4;
    [b, b + 1, b + 2, b + 3]
}

/// Where band `band`'s handle sits: its frequency, and its gain (or, for types without one,
/// its own response there).
pub(crate) fn handle_values(band: &Band, sample_rate: f64) -> (f64, f64) {
    let db = if band.kind.has_gain() {
        20.0 * band.gain.max(1e-9).log10()
    } else {
        response_db(band, band.freq, sample_rate).max(-DB_RANGE)
    };
    (band.freq, db)
}

/// The parameter values a handle dragged to (`hz`, `db`) asks for: frequency, and the gain in
/// dB for types that have one.
pub(crate) fn handle_params(kind: BandKind, hz: f64, db: f64, top: f64) -> (f64, Option<f64>) {
    let hz = hz.clamp(20.0, top);
    (hz, kind.has_gain().then(|| db.clamp(-DB_RANGE, DB_RANGE)))
}

/// A new width after `steps` wheel notches: a twelfth of an octave each way.
pub(crate) fn wheel_width(width: f64, steps: f64) -> f64 {
    (width * 2f64.powf(-steps / 6.0)).clamp(0.05, 4.0)
}

pub fn show(cx: &mut Ctx, ui: &mut Ui, rect: Rect) {
    let Some((bands, sr)) = cx.read(|eq: &mut ReaEqCompat| (eq.bands.clone(), eq.sample_rate())) else { return };
    let output_db = cx.read(|eq: &mut ReaEqCompat| 20.0 * eq.output.max(1e-9).log10()).unwrap_or(0.0);
    if cx.ed.sel.is_some_and(|k| k >= bands.len()) {
        cx.ed.sel = None;
    }
    let strip_h = if rect.width() < 640.0 { 68.0 } else { 34.0 };
    let (graph, strip) = split_top(rect, rect.height() - strip_h - 8.0, 8.0);
    let top = (sr / 2.0).min(22000.0);
    let plot = Plot::new(graph, Scale::log(20.0, top), Scale::lin(-DB_RANGE, DB_RANGE));
    grid(ui, &plot, true, 6.0, "");
    let levels = Plot::new(graph, plot.x, Scale::lin(-120.0, 12.0));
    spectrum(ui, &levels, &cx.ed.pre, 0.0, theme::with_alpha(TEXT_DIM, 18), theme::with_alpha(TEXT_DIM, 60));
    spectrum(ui, &levels, &cx.ed.post, 0.0, theme::with_alpha(ACCENT, 22), theme::with_alpha(ACCENT, 80));
    let xs = plot.xs(2.0);
    let mut total = vec![output_db; xs.len()];
    for (k, b) in bands.iter().enumerate().filter(|(_, b)| b.enabled) {
        let curve: Vec<f64> = xs.iter().map(|&f| response_db(b, f, sr)).collect();
        for (t, c) in total.iter_mut().zip(&curve) {
            *t += c;
        }
        let selected = cx.ed.sel == Some(k);
        let points: Vec<Pos2> = xs.iter().zip(&curve).map(|(&f, &db)| plot.point(f, db.clamp(-60.0, 60.0))).collect();
        let color = band_color(k);
        if selected {
            area(ui, graph, &points, plot.sy(0.0), theme::with_alpha(color, 40));
        }
        polyline(ui, graph, points, Stroke::new(if selected { 1.5 } else { 1.0 }, theme::with_alpha(color, 150)));
    }
    let points: Vec<Pos2> = xs.iter().zip(&total).map(|(&f, &db)| plot.point(f, db.clamp(-60.0, 60.0))).collect();
    polyline(ui, graph, points, Stroke::new(2.5, TEXT));
    let at: Vec<Pos2> = bands
        .iter()
        .map(|b| {
            let (hz, db) = handle_values(b, sr);
            plot.point(hz, db.clamp(-DB_RANGE, DB_RANGE))
        })
        .collect();
    for (k, b) in bands.iter().enumerate() {
        let color = if b.enabled { band_color(k) } else { theme::SWITCH_OFF };
        handle(ui, graph, at[k], color, cx.ed.sel == Some(k), &(k + 1).to_string());
    }
    graph_input(cx, ui, &plot, &bands, &at, sr);
    // The handles are painted, not widgets: hover-only nodes let AccessKit find them without
    // taking clicks or drags from the graph under them.
    for (k, p) in at.iter().enumerate().filter(|(_, p)| graph.contains(**p)) {
        let id = Id::unique(("fx-eq-handle", cx.ed.track, cx.ed.index, k));
        let r = ui.interact(Rect::from_center_size(*p, vec2(16.0, 16.0)), id, Sense::hover());
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Band {}", k + 1)));
    }
    if let Some(k) = cx.ed.sel
        && let Some(b) = bands.get(k)
    {
        let (hz, db) = handle_values(b, sr);
        let tip = if b.kind.has_gain() {
            format!("{} · {} · {db:+.1} dB · {:.2} oct", b.kind.name(), hz_text(hz), b.bandwidth)
        } else {
            format!("{} · {} · {:.2} oct", b.kind.name(), hz_text(hz), b.bandwidth)
        };
        let w = widgets::text_width(ui, &tip, theme::regular(11.0)) + 16.0;
        let p = at[k];
        let x = (p.x - w / 2.0).clamp(graph.left() + 4.0, graph.right() - w - 4.0);
        let y = if p.y - 34.0 > graph.top() { p.y - 34.0 } else { p.y + 14.0 };
        let r = Rect::from_min_size(pos2(x, y), vec2(w, 20.0));
        ui.painter().rect_filled(r, CornerRadius::same(6), theme::with_alpha(theme::PANEL, 230));
        ui.painter().text(r.center(), Align2::CENTER_CENTER, tip, theme::regular(11.0), TEXT);
    }
    strip_row(cx, ui, strip, &bands);
}

fn hit(at: &[Pos2], p: Pos2) -> Option<usize> {
    at.iter()
        .enumerate()
        .map(|(k, a)| (k, a.distance(p)))
        .filter(|(_, d)| *d <= 12.0)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(k, _)| k)
}

fn graph_input(cx: &mut Ctx, ui: &mut Ui, plot: &Plot, bands: &[Band], at: &[Pos2], sr: f64) {
    let resp = ui.interact(plot.rect, Id::unique(("fx-eq-graph", &cx.ed.owner, &cx.ed.path)), Sense::click_and_drag());
    let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
    let top = plot.x.hi;
    if resp.drag_started() || resp.clicked() || resp.secondary_clicked() {
        let origin = widgets::drag_origin(ui, &resp).or(pointer);
        cx.ed.sel = origin.and_then(|p| hit(at, p));
        cx.ed.dragging = if resp.drag_started() { cx.ed.sel } else { None };
        cx.ed.focus = None;
    }
    if let (Some(k), Some(p)) = (cx.ed.dragging, pointer)
        && resp.dragged()
        && let Some(b) = bands.get(k)
    {
        let [_, freq, gain, width] = ids(k);
        if ui.input(|i| i.modifiers.shift) {
            let w = wheel_width(b.bandwidth, -resp.drag_delta().y as f64 / 8.0);
            cx.set(width, w);
        } else {
            let (hz, db) = plot.values(p);
            let (hz, db) = handle_params(b.kind, hz, db, top);
            cx.set(freq, hz);
            if let Some(db) = db {
                cx.set(gain, db);
            }
        }
    }
    if resp.drag_stopped() {
        cx.ed.dragging = None;
        cx.end();
    }
    if resp.double_clicked() {
        match pointer.and_then(|p| hit(at, p)) {
            Some(k) => {
                let on = ids(k)[0];
                let enabled = bands[k].enabled;
                cx.set_once(on, if enabled { 0.0 } else { 1.0 });
            }
            None => {
                if let Some(p) = pointer {
                    let (hz, db) = plot.values(p);
                    add_band(cx, BandKind::Peak, hz.clamp(20.0, top), db);
                }
            }
        }
    }
    if resp.hovered() {
        let wheel = super::wheel(ui);
        let target = pointer.and_then(|p| hit(at, p)).or(cx.ed.sel);
        if wheel != 0.0
            && let Some(k) = target
            && let Some(b) = bands.get(k)
        {
            cx.set_once(ids(k)[3], wheel_width(b.bandwidth, wheel.signum() as f64));
        }
    }
    let menu_at = pointer;
    resp.context_menu(|ui| match cx.ed.sel {
        Some(k) if k < bands.len() => band_menu(cx, ui, k, &bands[k]),
        _ => {
            if ui.button("Add band here").clicked() {
                if let Some(p) = menu_at {
                    let (hz, db) = plot.values(p);
                    add_band(cx, BandKind::Peak, hz.clamp(20.0, top), db);
                }
                ui.close();
            }
        }
    });
    if let Some(k) = cx.ed.sel
        && cx.ed.hovered
        && cx.ed.typing.is_none()
        && let Some(b) = bands.get(k)
    {
        arrow_keys(cx, ui, k, b, sr, top);
    }
}

/// Left and right move the selected band by a twelfth of an octave (Shift: a hundredth), up
/// and down its gain by 0.5 dB (Shift: 0.1), or its width for types without a gain.
fn arrow_keys(cx: &mut Ctx, ui: &Ui, k: usize, b: &Band, sr: f64, top: f64) {
    let (l, r, u, d, fine) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::ArrowLeft),
            i.key_pressed(egui::Key::ArrowRight),
            i.key_pressed(egui::Key::ArrowUp),
            i.key_pressed(egui::Key::ArrowDown),
            i.modifiers.shift,
        )
    });
    let [_, freq, gain, width] = ids(k);
    let side = r as i32 - l as i32;
    let vert = u as i32 - d as i32;
    if side != 0 {
        let step = if fine { 1.0 / 100.0 } else { 1.0 / 12.0 };
        cx.set(freq, (b.freq * 2f64.powf(side as f64 * step)).clamp(20.0, top));
        cx.ed.key_gesture = true;
    }
    if vert != 0 {
        if b.kind.has_gain() {
            let (_, db) = handle_values(b, sr);
            let step = if fine { 0.1 } else { 0.5 };
            cx.set(gain, (db + vert as f64 * step).clamp(-DB_RANGE, DB_RANGE));
        } else {
            cx.set(width, wheel_width(b.bandwidth, vert as f64));
        }
        cx.ed.key_gesture = true;
    }
    if ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)) {
        remove_band(cx, k);
    }
}

fn add_band(cx: &mut Ctx, kind: BandKind, hz: f64, db: f64) {
    let band =
        Band { kind, enabled: true, freq: hz, gain: 10f64.powf(db.clamp(-DB_RANGE, DB_RANGE) / 20.0), bandwidth: 1.0 };
    if let Some(k) = cx.edit_once("Add EQ band", |eq: &mut ReaEqCompat| eq.add_band(band)) {
        cx.ed.sel = Some(k);
    }
}

fn remove_band(cx: &mut Ctx, k: usize) {
    cx.edit_once("Remove EQ band", |eq: &mut ReaEqCompat| eq.remove_band(k));
    cx.ed.sel = None;
}

fn band_menu(cx: &mut Ctx, ui: &mut Ui, k: usize, b: &Band) {
    ui.label(egui::RichText::new(format!("Band {}", k + 1)).color(TEXT_DIM));
    for kind in KINDS {
        let current = b.kind == kind || (kind == BandKind::Peak && b.kind == BandKind::PeakWide);
        if ui.radio(current, kind.name()).clicked() {
            cx.edit_once("Change EQ band type", |eq: &mut ReaEqCompat| eq.set_kind(k, kind));
            ui.close();
        }
    }
    ui.separator();
    if ui.button(if b.enabled { "Disable band" } else { "Enable band" }).clicked() {
        cx.set_once(ids(k)[0], if b.enabled { 0.0 } else { 1.0 });
        ui.close();
    }
    if ui.button("Remove band").clicked() {
        remove_band(cx, k);
        ui.close();
    }
    if cx.ed.track_fx().is_some() {
        ui.menu_button("Modulate/Learn…", |ui| {
            let [_, freq, gain, width] = ids(k);
            for (id, label) in [(freq, "Frequency"), (gain, "Gain"), (width, "Width")] {
                if cx.info(id).is_some() && ui.button(label).clicked() {
                    cx.modulate(id);
                    ui.close();
                }
            }
        });
    }
}

/// Band chips, add, and the selected band's type and values.
fn strip_row(cx: &mut Ctx, ui: &mut Ui, strip: Rect, bands: &[Band]) {
    let two_rows = strip.height() > 40.0;
    let (chips, fields) = if two_rows {
        split_top(strip, 30.0, 6.0)
    } else {
        let w = (bands.len() as f32 * 32.0 + 70.0).min(strip.width() * 0.45);
        super::split_left(strip, w, 8.0)
    };
    let mut x = chips.left();
    for (k, b) in bands.iter().enumerate() {
        let r = Rect::from_min_size(pos2(x, chips.top() + 2.0), vec2(26.0, 26.0));
        if r.right() > chips.right() - 60.0 {
            break;
        }
        let selected = cx.ed.sel == Some(k);
        let color = if b.enabled { band_color(k) } else { theme::SWITCH_OFF };
        let resp = ui.interact(r, Id::unique(("fx-eq-chip", &cx.ed.owner, &cx.ed.path, k)), Sense::click());
        ui.painter().rect_filled(r, CornerRadius::same(13), if selected { color } else { RAISED });
        ui.painter().rect_stroke(r, CornerRadius::same(13), Stroke::new(1.5, color), egui::StrokeKind::Inside);
        ui.painter().text(
            r.center(),
            Align2::CENTER_CENTER,
            (k + 1).to_string(),
            theme::semibold(11.0),
            if selected { BG } else { TEXT },
        );
        if resp.clicked() {
            cx.ed.sel = Some(k);
            cx.ed.focus = None;
        }
        x += 30.0;
    }
    let add = Rect::from_min_size(pos2(x + 2.0, chips.top() + 2.0), vec2(54.0, 26.0));
    if widgets::text_pill(ui, add, "+ Band", theme::regular(11.0), RAISED, TEXT_MID)
        .on_hover_text("Add a band (or double-click the graph)")
        .clicked()
    {
        let hz = bands.last().map_or(1000.0, |b| (b.freq * 2.0).min(16000.0));
        add_band(cx, BandKind::Peak, hz, 0.0);
    }
    let Some(k) = cx.ed.sel.filter(|k| *k < bands.len()) else {
        ui.painter().text(
            fields.left_center() + vec2(4.0, 2.0),
            Align2::LEFT_CENTER,
            "Drag a band; scroll or Shift-drag for its width; right-click for its type",
            theme::regular(11.0),
            TEXT_DIM,
        );
        return;
    };
    let b = bands[k];
    let [on, freq, gain, width] = ids(k);
    let row = Rect::from_min_size(pos2(fields.left(), fields.top() + 2.0), vec2(fields.width(), 26.0));
    let kind_w = 92.0;
    let kind = Rect::from_min_size(row.min, vec2(kind_w, 26.0));
    let kind_resp = widgets::text_pill(ui, kind, b.kind.name(), theme::regular(11.0), band_color(k), BG);
    egui::Popup::menu(&kind_resp).show(|ui| band_menu(cx, ui, k, &b));
    let remove = Rect::from_min_size(pos2(row.right() - 26.0, row.top()), vec2(26.0, 26.0));
    if widgets::icon_button(ui, remove, widgets::Icon::Close, RAISED, TEXT_DIM, 12.0, "Remove band").clicked() {
        remove_band(cx, k);
        return;
    }
    let sw = Rect::from_min_size(pos2(remove.left() - 42.0, row.top() + 3.0), vec2(36.0, 20.0));
    if widgets::switch(ui, sw, b.enabled, band_color(k)).on_hover_text("Band on or off").clicked() {
        cx.set_once(on, if b.enabled { 0.0 } else { 1.0 });
    }
    let boxes: Vec<(u32, &str)> = if b.kind.has_gain() {
        vec![(freq, "Freq"), (gain, "Gain"), (width, "Width")]
    } else {
        vec![(freq, "Freq"), (width, "Width")]
    };
    let left = kind.right() + 6.0;
    let w = ((sw.left() - 6.0 - left) / boxes.len() as f32 - 6.0).max(40.0);
    for (i, (id, label)) in boxes.into_iter().enumerate() {
        let r = Rect::from_min_size(pos2(left + i as f32 * (w + 6.0), row.top()), vec2(w, 26.0));
        value_box(cx, ui, r, id, label);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_map_to_parameters_and_back() {
        let plot = Plot::new(
            Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 400.0)),
            Scale::log(20.0, 22000.0),
            Scale::lin(-DB_RANGE, DB_RANGE),
        );
        for (kind, freq, gain_db) in [(BandKind::Peak, 1234.0, 6.5), (BandKind::LowShelf, 80.0, -9.0)] {
            let band = Band { kind, enabled: true, freq, gain: 10f64.powf(gain_db / 20.0), bandwidth: 1.0 };
            let (hz, db) = handle_values(&band, 48000.0);
            let (back_hz, back_db) = plot.values(plot.point(hz, db));
            let (f, g) = handle_params(kind, back_hz, back_db, 22000.0);
            assert!((f / freq - 1.0).abs() < 1e-4, "{f} vs {freq}");
            assert!((g.unwrap() - gain_db).abs() < 1e-3);
        }
        let hp = Band { kind: BandKind::HighPass, enabled: true, freq: 100.0, gain: 1.0, bandwidth: 1.0 };
        let (hz, db) = handle_values(&hp, 48000.0);
        assert_eq!(hz, 100.0);
        assert!((db - response_db(&hp, 100.0, 48000.0)).abs() < 1e-9);
        assert_eq!(handle_params(BandKind::HighPass, 50.0, 12.0, 22000.0), (50.0, None));
        assert_eq!(handle_params(BandKind::Peak, 5.0, 40.0, 22000.0), (20.0, Some(DB_RANGE)));
    }

    #[test]
    fn the_wheel_steps_width_in_twelfths_of_an_octave() {
        let w = wheel_width(1.0, 6.0);
        assert!((w - 0.5).abs() < 1e-12);
        assert!((wheel_width(w, -6.0) - 1.0).abs() < 1e-12);
        assert_eq!(wheel_width(0.06, 100.0), 0.05);
    }

    #[test]
    fn band_ids_follow_the_parameter_layout() {
        assert_eq!(ids(0), [0, 1, 2, 3]);
        assert_eq!(ids(3), [12, 13, 14, 15]);
    }
}
