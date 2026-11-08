use egui::{Align2, Color32, CornerRadius, Id, Rect, Sense, Ui, pos2, vec2};
use scythe_core::TrackId;
use scythe_core::fader::{db_to_slider, slider_to_db};
use scythe_core::units::{db_to_gain, format_db, format_pan, gain_to_db};

use crate::app::{AutoTarget, ScytheApp, Selection};
use crate::theme::{self, ACCENT, BG, GREEN, PANEL, RAISED, RAISED_HI, TEXT, TEXT_DIM, YELLOW, track_color};
use crate::ui::arrange::meter_fraction;
use crate::ui::widgets;

const STRIP_W: f32 = 108.0;
const STRIP_GAP: f32 = 8.0;
const FADER_H: f32 = 116.0;
/// Fader travel follows REAPER's fader law, from silence to +12 dB.
pub fn fader_position(gain: f64) -> f32 {
    if gain <= 0.0 {
        return 0.0;
    }
    (db_to_slider(gain_to_db(gain)) / 1000.0).clamp(0.0, 1.0) as f32
}

pub fn fader_gain(pos: f32) -> f64 {
    if pos <= 0.0 { 0.0 } else { db_to_gain(slider_to_db(pos.min(1.0) as f64 * 1000.0)) }
}

#[derive(Clone, Copy)]
enum StripTarget {
    Track(TrackId),
    Master,
}

pub fn show(app: &mut ScytheApp, ui: &mut Ui, rect: Rect) {
    let painter = ui.painter().clone();
    painter.rect_filled(rect, CornerRadius::same(18), PANEL);
    let inner = rect.shrink2(vec2(14.0, 14.0));
    painter.text(inner.left_top() + vec2(0.0, 9.0), Align2::LEFT_CENTER, "Mixer", theme::semibold(15.0), TEXT);
    // Master loudness after BS.1770: momentary (400 ms) and short-term (3 s).
    let header = match app.engine.meters().loudness.as_ref() {
        Some(l) if l.short_term().is_finite() => {
            let fmt = |v: f32| if v.is_finite() { format!("{v:.1}") } else { "–".to_owned() };
            format!("Master  M {} · S {} LUFS", fmt(l.momentary()), fmt(l.short_term()))
        }
        _ => "Post-fader · peak".to_owned(),
    };
    painter.text(inner.right_top() + vec2(0.0, 9.0), Align2::RIGHT_CENTER, header, theme::mono(11.0), TEXT_DIM);

    let strips_area = Rect::from_min_max(pos2(inner.left(), inner.top() + 30.0), inner.max);
    let count = app.project.tracks.len() + 1;
    let content_w = count as f32 * (STRIP_W + STRIP_GAP) - STRIP_GAP;
    let max_scroll = (content_w - strips_area.width()).max(0.0);
    let scroll_id = Id::unique("mixer-scroll");
    let mut scroll: f32 = ui.data(|d| d.get_temp(scroll_id)).unwrap_or(0.0);
    if ui.ctx().input(|i| i.pointer.hover_pos()).is_some_and(|p| strips_area.contains(p)) {
        let d = ui.ctx().input(|i| i.smooth_scroll_delta);
        scroll -= if d.x != 0.0 { d.x } else { d.y };
    }
    scroll = scroll.clamp(0.0, max_scroll);
    ui.data_mut(|d| d.insert_temp(scroll_id, scroll));

    let clip = painter.with_clip_rect(strips_area.expand(2.0));
    let ids: Vec<TrackId> = app.project.tracks.iter().map(|t| t.id).collect();
    for (i, target) in ids.into_iter().map(StripTarget::Track).chain([StripTarget::Master]).enumerate() {
        let x = strips_area.left() + i as f32 * (STRIP_W + STRIP_GAP) - scroll;
        let r = Rect::from_min_size(pos2(x, strips_area.top()), vec2(STRIP_W, strips_area.height().min(190.0)));
        if r.right() < strips_area.left() || r.left() > strips_area.right() {
            continue;
        }
        strip(app, ui, &clip, r, strips_area, target);
    }
}

fn strip(app: &mut ScytheApp, ui: &mut Ui, painter: &egui::Painter, r: Rect, area: Rect, target: StripTarget) {
    let (name, color, volume, pan, selected, levels, audible) = match target {
        StripTarget::Track(id) => {
            let Some(t) = app.project.track(id) else { return };
            (
                t.name.clone(),
                track_color(t.color),
                app.automation_value(id, AutoTarget::Volume).unwrap_or(t.volume),
                app.automation_value(id, AutoTarget::Pan).unwrap_or(t.pan),
                app.sel.track == Some(id),
                app.levels.get(&id).copied().unwrap_or_default(),
                app.project.is_audible(t),
            )
        }
        StripTarget::Master => {
            let m = &app.project.master;
            ("Master".to_owned(), TEXT, m.volume, m.pan, false, app.master_level, !m.muted)
        }
    };
    let id_salt = match target {
        StripTarget::Track(id) => id.0,
        StripTarget::Master => u64::MAX,
    };
    let visible = r.intersect(area);
    if visible.width() <= 0.0 {
        return;
    }

    painter.rect_filled(r, CornerRadius::same(14), if selected { RAISED_HI } else { RAISED });
    if selected {
        painter.rect_stroke(r, CornerRadius::same(14), egui::Stroke::new(2.0, ACCENT), egui::StrokeKind::Inside);
    }
    let pad = 10.0;
    let mut y = r.top() + pad + 6.0;
    painter.circle_filled(pos2(r.left() + pad + 4.0, y), 4.0, color);
    let label = widgets::truncate(ui, &name, &theme::semibold(12.0), STRIP_W - 2.0 * pad - 14.0);
    painter.text(pos2(r.left() + pad + 14.0, y), Align2::LEFT_CENTER, label, theme::semibold(12.0), TEXT);
    let select = ui.interact(
        Rect::from_min_size(r.min, vec2(STRIP_W, 26.0)).intersect(visible),
        Id::unique(("strip-head", id_salt)),
        Sense::click(),
    );
    if select.clicked()
        && let StripTarget::Track(id) = target
    {
        app.sel = Selection { track: Some(id), item: None };
    }

    // Pan.
    y += 16.0;
    let pan_rect = Rect::from_min_size(pos2(r.left() + pad, y), vec2(STRIP_W - 2.0 * pad, 4.0));
    painter.rect_filled(pan_rect, CornerRadius::same(255), BG);
    painter.vline(
        pan_rect.center().x,
        (pan_rect.top() - 2.0)..=(pan_rect.bottom() + 2.0),
        egui::Stroke::new(1.0, theme::SWITCH_OFF),
    );
    let knob_x = pan_rect.left() + (pan as f32 + 1.0) / 2.0 * pan_rect.width();
    painter.circle_filled(pos2(knob_x, pan_rect.center().y), 5.0, TEXT);
    let pan_hit = ui
        .interact(
            pan_rect.expand2(vec2(4.0, 8.0)).intersect(visible),
            Id::unique(("pan", id_salt)),
            Sense::click_and_drag(),
        )
        .on_hover_text("Drag to pan, double-click to center");
    if pan_hit.dragged() {
        let d = pan_hit.drag_delta().x as f64 / pan_rect.width() as f64 * 2.0;
        let written = match target {
            StripTarget::Track(id) => app.automate(id, AutoTarget::Pan, (pan + d).clamp(-1.0, 1.0)),
            StripTarget::Master => false,
        };
        if !written {
            app.gesture("Adjust pan", |p| {
                let t = match target {
                    StripTarget::Track(id) => p.track_mut(id).unwrap(),
                    StripTarget::Master => &mut p.master,
                };
                t.pan = (t.pan + d).clamp(-1.0, 1.0);
            });
        }
    }
    if pan_hit.drag_stopped() {
        match target {
            StripTarget::Track(id) if app.automation_value(id, AutoTarget::Pan).is_some() => {
                app.release_automation(id, AutoTarget::Pan)
            }
            _ => app.end_gesture(),
        }
    }
    if pan_hit.double_clicked() {
        app.edit("Center pan", |p| match target {
            StripTarget::Track(id) => p.track_mut(id).unwrap().pan = 0.0,
            StripTarget::Master => p.master.pan = 0.0,
        });
    }
    y += 14.0;
    painter.text(pos2(r.center().x, y), Align2::CENTER_CENTER, format_pan(pan), theme::mono(10.0), TEXT_DIM);

    // Fader and meters.
    y += 10.0;
    let fader_h = (r.bottom() - 26.0 - y).clamp(40.0, FADER_H);
    let fader = Rect::from_min_size(pos2(r.center().x - 22.0, y), vec2(32.0, fader_h));
    painter.rect_filled(fader, CornerRadius::same(10), BG);
    let pos = fader_position(volume);
    let fill_top = fader.bottom() - pos * fader.height();
    let fill = Rect::from_min_max(pos2(fader.left(), fill_top), fader.max);
    if pos > 0.0 {
        painter.rect_filled(fill, CornerRadius::same(10), color.gamma_multiply(0.85));
    }
    let unity = fader.bottom() - fader_position(1.0) * fader.height();
    painter.hline(fader.left()..=fader.left() + 5.0, unity, egui::Stroke::new(1.0, TEXT_DIM));
    painter.rect_filled(
        Rect::from_min_max(pos2(fader.left() + 6.0, fill_top - 1.5), pos2(fader.right() - 6.0, fill_top + 1.5)),
        CornerRadius::same(2),
        BG.gamma_multiply(0.6),
    );

    let fader_hit = ui
        .interact(fader.intersect(visible), Id::unique(("fader", id_salt)), Sense::click_and_drag())
        .on_hover_text("Drag to set volume, double-click for 0 dB, Shift for fine");
    if fader_hit.dragged() {
        let fine = ui.ctx().input(|i| i.modifiers.shift);
        let d = -fader_hit.drag_delta().y / fader.height() * if fine { 0.1 } else { 1.0 };
        let new_gain = fader_gain(pos + d);
        let written = match target {
            StripTarget::Track(id) => app.automate(id, AutoTarget::Volume, new_gain),
            StripTarget::Master => false,
        };
        if !written {
            app.gesture("Adjust volume", |p| match target {
                StripTarget::Track(id) => p.track_mut(id).unwrap().volume = new_gain,
                StripTarget::Master => p.master.volume = new_gain,
            });
        }
    }
    if fader_hit.drag_stopped() {
        match target {
            StripTarget::Track(id) if app.automation_value(id, AutoTarget::Volume).is_some() => {
                app.release_automation(id, AutoTarget::Volume)
            }
            _ => app.end_gesture(),
        }
    }
    if fader_hit.double_clicked() {
        app.edit("Reset volume", |p| match target {
            StripTarget::Track(id) => p.track_mut(id).unwrap().volume = 1.0,
            StripTarget::Master => p.master.volume = 1.0,
        });
    }

    for (k, level) in [levels.0, levels.1].into_iter().enumerate() {
        let m = Rect::from_min_size(pos2(fader.right() + 6.0 + k as f32 * 5.0, fader.top()), vec2(3.0, fader.height()));
        painter.rect_filled(m, CornerRadius::same(255), BG);
        let f = if audible { meter_fraction(level) } else { 0.0 };
        if f > 0.0 {
            let lvl = Rect::from_min_max(pos2(m.left(), m.bottom() - f * m.height()), m.max);
            let c: Color32 = if level >= 0.89 { YELLOW } else { GREEN };
            painter.rect_filled(lvl, CornerRadius::same(255), c);
        }
    }

    let db = format!("{} dB", format_db(volume));
    painter.text(pos2(r.center().x, r.bottom() - 14.0), Align2::CENTER_CENTER, db, theme::mono(12.0), TEXT);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fader_mapping_round_trips() {
        for g in [0.01, 0.25, 0.5, 1.0, 1.9] {
            let back = fader_gain(fader_position(g));
            assert!((back - g).abs() / g < 1e-4, "{g} -> {back}");
        }
        assert!((fader_position(1.0) - 0.716).abs() < 1e-6);
        assert_eq!(fader_gain(0.0), 0.0);
        assert_eq!(fader_position(0.0), 0.0);
        assert_eq!(fader_position(100.0), 1.0);
    }
}
