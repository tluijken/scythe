//! What every parameter control of an effect editor does, whoever runs the effect: drag
//! (Shift for fine), wheel, click to take the arrow keys, double-click to type a value,
//! right-click for a menu with reset and modulation.

use egui::{Align2, CornerRadius, Id, Rect, Sense, Stroke, Ui, pos2, vec2};
use scyeq_dsp::ParamInfo;

use crate::model;
use crate::theme::{self, ACCENT, BG, RAISED_HI, TEXT, TEXT_DIM};
use crate::widgets;

/// What a typed value goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Typing {
    Param(u32),
    Field(u64),
}

/// An editor's focus and keyboard state between frames.
#[derive(Debug, Clone, Default)]
pub struct Controls {
    /// The parameter arrow keys nudge.
    pub focus: Option<u32>,
    /// The selected band, tap or point.
    pub sel: Option<usize>,
    /// The value being typed, its text, and whether it shows for the first frame.
    pub typing: Option<(Typing, String, bool)>,
    /// Arrow keys held change the effect as one gesture.
    pub key_gesture: bool,
    /// Whether the pointer is over the editor; the host sets it.
    pub hovered: bool,
}

/// The parameters an editor's controls change.
pub trait Params {
    fn param_info(&self, id: u32) -> Option<ParamInfo>;
    fn param_value(&self, id: u32) -> f64;
    /// The value as the effect shows it.
    fn param_text(&self, id: u32) -> String;
    /// Sets a parameter as part of the current gesture (a drag), ended by [`Params::end_gesture`].
    fn set_param(&mut self, id: u32, v: f64);
    /// Sets a parameter as an undo step of its own.
    fn set_param_once(&mut self, id: u32, v: f64) {
        self.end_gesture();
        self.set_param(id, v);
        self.end_gesture();
    }
    fn end_gesture(&mut self);
    /// Whether the menu offers modulation and learn.
    fn can_modulate(&self) -> bool {
        false
    }
    fn modulate(&mut self, _id: u32) {}
    fn controls(&mut self) -> &mut Controls;
    /// Tells this editor's controls apart from another's.
    fn id_salt(&self) -> Id;
}

/// Which way a control is dragged, and over how many points the full range lies.
#[derive(Clone, Copy)]
pub enum Axis {
    Vertical(f32),
    Horizontal(f32),
}

pub fn interact<P: Params + ?Sized>(cx: &mut P, ui: &Ui, resp: &egui::Response, id: u32, axis: Axis) {
    let Some(info) = cx.param_info(id) else { return };
    let fine = ui.input(|i| i.modifiers.shift);
    if resp.clicked() || resp.drag_started() {
        let c = cx.controls();
        c.focus = Some(id);
        c.sel = None;
    }
    if resp.dragged() {
        let d = resp.drag_delta();
        let v = cx.param_value(id);
        let nv = match axis {
            Axis::Vertical(span) => model::drag(&info, v, d.y, span, fine),
            Axis::Horizontal(span) => model::drag(&info, v, -d.x, span, fine),
        };
        if nv != v {
            cx.set_param(id, nv);
        }
    }
    if resp.drag_stopped() {
        cx.end_gesture();
    }
    if resp.hovered() {
        let wheel = wheel(ui);
        if wheel != 0.0 {
            let v = model::nudge(
                &info,
                cx.param_value(id),
                wheel.signum() as f64,
                fine,
                model::is_switch(&cx.param_text(id)),
            );
            cx.set_param_once(id, v);
        }
    }
    if resp.double_clicked() {
        start_typing_param(cx, id);
    }
    resp.context_menu(|ui| param_menu(cx, ui, id));
}

pub fn start_typing_param<P: Params + ?Sized>(cx: &mut P, id: u32) {
    let shown = cx.param_text(id);
    let number: String = shown.split_whitespace().next().unwrap_or("").trim_start_matches('+').to_owned();
    cx.controls().typing = Some((Typing::Param(id), number, true));
}

pub fn param_menu<P: Params + ?Sized>(cx: &mut P, ui: &mut Ui, id: u32) {
    let name = cx.param_info(id).map(|p| p.name).unwrap_or_default();
    ui.label(egui::RichText::new(name).color(TEXT_DIM));
    if ui.button("Type a value…").clicked() {
        start_typing_param(cx, id);
        ui.close();
    }
    if ui.button("Reset to default").clicked() {
        if let Some(d) = cx.param_info(id).map(|p| p.default) {
            cx.set_param_once(id, d);
        }
        ui.close();
    }
    if cx.can_modulate() && ui.button("Modulate/Learn…").clicked() {
        cx.modulate(id);
        ui.close();
    }
}

/// The text field of a value being typed, when it is `id`'s; true while it shows.
pub fn typing_box<P: Params + ?Sized>(cx: &mut P, ui: &mut Ui, rect: Rect, id: u32) -> bool {
    let Some((Typing::Param(at), mut text, first)) = cx.controls().typing.clone() else { return false };
    if at != id {
        return false;
    }
    let edit = egui::TextEdit::singleline(&mut text).font(theme::mono(12.0)).desired_width(rect.width());
    let resp = ui.put(rect.shrink2(vec2(0.0, (rect.height() - 22.0).max(0.0) / 2.0)), edit);
    if first {
        resp.request_focus();
    }
    let (enter, escape) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
    if escape {
        cx.controls().typing = None;
    } else if enter || (resp.lost_focus() && !first) {
        if let Some(v) = model::parse_value(&text, &cx.param_text(id))
            && let Some(info) = cx.param_info(id)
        {
            let v = v.clamp(info.min.min(info.max), info.max.max(info.min));
            cx.set_param_once(id, v);
        }
        cx.controls().typing = None;
    } else {
        cx.controls().typing = Some((Typing::Param(id), text, false));
    }
    true
}

/// A compact value field: label, value, drag sideways, double-click to type.
pub fn value_box<P: Params + ?Sized>(cx: &mut P, ui: &mut Ui, rect: Rect, id: u32, label: &str) {
    if cx.param_info(id).is_none() {
        return;
    }
    let resp = ui.interact(rect, cx.id_salt().with(("fx-ed-box", id)), Sense::click_and_drag());
    let painter = ui.painter();
    let hot = resp.hovered() || resp.dragged();
    painter.rect_filled(rect, CornerRadius::same(8), if hot { RAISED_HI } else { BG });
    if cx.controls().focus == Some(id) {
        painter.rect_stroke(rect, CornerRadius::same(8), Stroke::new(1.0, ACCENT), egui::StrokeKind::Inside);
    }
    let text = cx.param_text(id);
    let needed =
        widgets::text_width(ui, label, theme::regular(11.0)) + widgets::text_width(ui, &text, theme::mono(11.0));
    if needed + 24.0 <= rect.width() {
        ui.painter().text(
            rect.left_center() + vec2(8.0, 0.0),
            Align2::LEFT_CENTER,
            label,
            theme::regular(11.0),
            TEXT_DIM,
        );
    }
    let value_rect = Rect::from_min_max(pos2(rect.left() + rect.width() * 0.4, rect.top()), rect.max - vec2(6.0, 0.0));
    if !typing_box(cx, ui, value_rect, id) {
        ui.painter().text(rect.right_center() - vec2(8.0, 0.0), Align2::RIGHT_CENTER, text, theme::mono(11.0), TEXT);
    }
    let resp = if label.is_empty() { resp } else { resp.on_hover_text(label) };
    interact(cx, ui, &resp, id, Axis::Horizontal(240.0));
}

/// Wheel notches turned this frame, up positive.
pub fn wheel(ui: &Ui) -> f32 {
    ui.input(|i| {
        i.events
            .iter()
            .filter_map(|e| match e {
                egui::Event::MouseWheel { delta, .. } => Some(delta.y.signum()),
                _ => None,
            })
            .sum()
    })
}

/// Arrow keys nudge the focused control; held keys make one gesture.
pub fn keys<P: Params + ?Sized>(cx: &mut P, ui: &Ui) {
    let c = cx.controls();
    if c.typing.is_some() || !c.hovered {
        return;
    }
    let (up, down, held, fine) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::ArrowUp) || i.key_pressed(egui::Key::ArrowRight),
            i.key_pressed(egui::Key::ArrowDown) || i.key_pressed(egui::Key::ArrowLeft),
            [egui::Key::ArrowUp, egui::Key::ArrowDown, egui::Key::ArrowLeft, egui::Key::ArrowRight]
                .iter()
                .any(|k| i.key_down(*k)),
            i.modifiers.shift,
        )
    });
    if let Some(id) = cx.controls().focus
        && (up || down)
        && let Some(info) = cx.param_info(id)
    {
        let v = model::nudge(
            &info,
            cx.param_value(id),
            if up { 1.0 } else { -1.0 },
            fine,
            model::is_switch(&cx.param_text(id)),
        );
        cx.set_param(id, v);
        cx.controls().key_gesture = true;
    }
    if cx.controls().key_gesture && !held {
        cx.controls().key_gesture = false;
        cx.end_gesture();
    }
}
