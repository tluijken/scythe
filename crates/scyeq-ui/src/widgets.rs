//! Small drawing helpers shared by Scythe's editors and ScyEQ-Pro's.

use egui::{Align2, Color32, CornerRadius, Rect, Response, Sense, Ui, pos2, vec2};

use crate::theme::{self, RAISED, RAISED_HI, TEXT};

pub fn hover_tint(ui: &Ui, resp: &Response, bg: Color32) -> Color32 {
    if resp.is_pointer_button_down_on() {
        return lighten(bg, 0.12);
    }
    if resp.hovered() {
        let _ = ui;
        return if bg.a() == 0 { theme::with_alpha(RAISED_HI, 160) } else { lighten(bg, 0.07) };
    }
    bg
}

pub fn lighten(c: Color32, amount: f32) -> Color32 {
    let f = |v: u8| (v as f32 + (255.0 - v as f32) * amount) as u8;
    Color32::from_rgba_unmultiplied(f(c.r()), f(c.g()), f(c.b()), c.a().max(1))
}

/// What AccessKit (and so a screen reader or a UI test) knows of a painted button.
pub fn button_info(ui: &Ui, label: &str) -> egui::WidgetInfo {
    egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), label)
}

/// Small rounded text button (M / S, segmented tabs, "Add track").
pub fn text_pill(ui: &mut Ui, rect: Rect, text: &str, font: egui::FontId, bg: Color32, fg: Color32) -> Response {
    let resp = ui.allocate_rect(rect, Sense::click());
    resp.widget_info(|| button_info(ui, text));
    let radius = (rect.height() / 2.0).min(255.0) as u8;
    ui.painter().rect_filled(rect, CornerRadius::same(radius), hover_tint(ui, &resp, bg));
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, font, fg);
    resp
}

/// Width a pill needs for `text` in `font` with horizontal padding.
pub fn text_width(ui: &Ui, text: &str, font: egui::FontId) -> f32 {
    ui.painter().layout_no_wrap(text.to_owned(), font, TEXT).size().x
}

pub fn truncate(ui: &Ui, text: &str, font: &egui::FontId, max: f32) -> String {
    if text_width(ui, text, font.clone()) <= max {
        return text.to_owned();
    }
    let mut s: String = text.to_owned();
    while !s.is_empty() {
        s.pop();
        let candidate = format!("{}…", s.trim_end());
        if text_width(ui, &candidate, font.clone()) <= max {
            return candidate;
        }
    }
    String::new()
}

/// Where a drag that just started was pressed. egui reports a drag only once the pointer has
/// moved past a threshold, so the pointer's current position can already be off the thing it
/// grabbed.
pub fn drag_origin(ui: &Ui, resp: &egui::Response) -> Option<egui::Pos2> {
    ui.input(|i| i.pointer.press_origin()).or_else(|| resp.interact_pointer_pos())
}

/// A rounded card behind a section.
pub fn card(ui: &Ui, rect: Rect) {
    ui.painter().rect_filled(rect, CornerRadius::same(12), RAISED);
}

/// Splits `rect` into a left part `w` wide and the rest, with a gap.
pub fn split_left(rect: Rect, w: f32, gap: f32) -> (Rect, Rect) {
    let w = w.min(rect.width());
    (
        Rect::from_min_size(rect.min, vec2(w, rect.height())),
        Rect::from_min_max(pos2((rect.left() + w + gap).min(rect.right()), rect.top()), rect.max),
    )
}

/// Splits `rect` into a top part `h` tall and the rest, with a gap.
pub fn split_top(rect: Rect, h: f32, gap: f32) -> (Rect, Rect) {
    let h = h.min(rect.height());
    (
        Rect::from_min_size(rect.min, vec2(rect.width(), h)),
        Rect::from_min_max(pos2(rect.left(), (rect.top() + h + gap).min(rect.bottom())), rect.max),
    )
}

/// A track's colour as a small dot centred on `at`.
pub fn color_dot(painter: &egui::Painter, at: egui::Pos2, [r, g, b]: [u8; 3]) {
    painter.circle_filled(at, 4.0, Color32::from_rgb(r, g, b));
}
