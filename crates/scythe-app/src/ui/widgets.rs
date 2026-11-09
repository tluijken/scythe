use egui::{Align2, Color32, CornerRadius, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, pos2, vec2};

use crate::theme::{self, RAISED_HI, TEXT};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Logo,
    GoStart,
    Stop,
    Play,
    Pause,
    Record,
    Loop,
    Metronome,
    Search,
    Arrange,
    Mixer,
    Folder,
    Fx,
    Routing,
    Settings,
    Plus,
    Star,
    Previous,
    Next,
    Headphones,
    SkipBack,
    SkipForward,
    Close,
    Setlist,
}

/// Maps the 24×24 SVG coordinate space of the mockup's icons into `rect`.
struct Canvas {
    origin: Pos2,
    scale: f32,
}

impl Canvas {
    fn new(rect: Rect, size: f32) -> Self {
        let s = rect.width().min(rect.height()).min(size);
        let origin = rect.center() - vec2(s, s) / 2.0;
        Self { origin, scale: s / 24.0 }
    }

    fn p(&self, x: f32, y: f32) -> Pos2 {
        self.origin + vec2(x, y) * self.scale
    }

    fn pts(&self, xy: &[(f32, f32)]) -> Vec<Pos2> {
        xy.iter().map(|&(x, y)| self.p(x, y)).collect()
    }
}

pub fn paint_icon(ui: &Ui, rect: Rect, icon: Icon, color: Color32, size: f32) {
    let painter = ui.painter();
    let c = Canvas::new(rect, size);
    let w = (2.0 * c.scale).max(1.2);
    let stroke = Stroke::new(w, color);
    let line = |xy: &[(f32, f32)]| {
        painter.add(Shape::line(c.pts(xy), stroke));
    };
    match icon {
        Icon::Logo => {
            let s = Stroke::new(2.4 * c.scale, color);
            for (x, a, b) in
                [(4.0, 10.0, 14.0), (8.0, 6.0, 18.0), (12.0, 3.0, 21.0), (16.0, 7.0, 17.0), (20.0, 10.0, 14.0)]
            {
                painter.line_segment([c.p(x, a), c.p(x, b)], s);
            }
        }
        Icon::GoStart => {
            line(&[(6.0, 5.0), (6.0, 19.0)]);
            painter.add(Shape::closed_line(c.pts(&[(19.0, 5.0), (10.0, 12.0), (19.0, 19.0)]), stroke));
        }
        Icon::Stop => {
            painter.rect_filled(
                Rect::from_min_max(c.p(4.0, 4.0), c.p(20.0, 20.0)),
                CornerRadius::same((3.0 * c.scale) as u8),
                color,
            );
        }
        Icon::Play => {
            painter.add(Shape::convex_polygon(c.pts(&[(7.0, 4.5), (20.0, 12.0), (7.0, 19.5)]), color, Stroke::NONE));
        }
        Icon::Pause => {
            let r = CornerRadius::same((1.5 * c.scale) as u8);
            painter.rect_filled(Rect::from_min_max(c.p(6.0, 5.0), c.p(10.0, 19.0)), r, color);
            painter.rect_filled(Rect::from_min_max(c.p(14.0, 5.0), c.p(18.0, 19.0)), r, color);
        }
        Icon::Record => {
            painter.circle_filled(c.p(12.0, 12.0), 9.0 * c.scale, color);
        }
        Icon::Loop => {
            line(&[(17.0, 2.0), (21.0, 6.0), (17.0, 10.0)]);
            line(&[(3.0, 11.0), (3.0, 10.0), (4.2, 7.2), (7.0, 6.0), (21.0, 6.0)]);
            line(&[(7.0, 22.0), (3.0, 18.0), (7.0, 14.0)]);
            line(&[(21.0, 13.0), (21.0, 14.0), (19.8, 16.8), (17.0, 18.0), (3.0, 18.0)]);
        }
        Icon::Metronome => {
            painter.add(Shape::closed_line(c.pts(&[(9.0, 3.0), (15.0, 3.0), (19.0, 21.0), (5.0, 21.0)]), stroke));
            line(&[(12.0, 15.0), (18.0, 6.0)]);
        }
        Icon::Search => {
            painter.circle_stroke(c.p(11.0, 11.0), 7.0 * c.scale, stroke);
            line(&[(20.0, 20.0), (16.5, 16.5)]);
        }
        Icon::Arrange => {
            line(&[(3.0, 6.0), (13.0, 6.0)]);
            line(&[(3.0, 12.0), (19.0, 12.0)]);
            line(&[(3.0, 18.0), (10.0, 18.0)]);
        }
        Icon::Mixer => {
            for x in [6.0, 12.0, 18.0] {
                line(&[(x, 3.0), (x, 21.0)]);
            }
            let r = CornerRadius::same(c.scale as u8);
            for (x, y) in [(4.0, 13.0), (10.0, 7.0), (16.0, 15.0)] {
                painter.rect_filled(Rect::from_min_max(c.p(x, y), c.p(x + 4.0, y + 3.0)), r, color);
            }
        }
        Icon::Folder => {
            painter.add(Shape::closed_line(
                c.pts(&[
                    (3.0, 6.0),
                    (5.0, 4.0),
                    (9.0, 4.0),
                    (11.0, 6.0),
                    (19.0, 6.0),
                    (21.0, 8.0),
                    (21.0, 18.0),
                    (19.0, 20.0),
                    (5.0, 20.0),
                    (3.0, 18.0),
                ]),
                stroke,
            ));
        }
        Icon::Fx => {
            line(&[(9.0, 2.0), (9.0, 8.0)]);
            line(&[(15.0, 2.0), (15.0, 8.0)]);
            line(&[
                (6.0, 8.0),
                (18.0, 8.0),
                (18.0, 12.0),
                (16.2, 16.2),
                (12.0, 18.0),
                (7.8, 16.2),
                (6.0, 12.0),
                (6.0, 8.0),
            ]);
            line(&[(12.0, 18.0), (12.0, 22.0)]);
        }
        Icon::Routing => {
            for (x, y) in [(6.0, 6.0), (6.0, 18.0), (18.0, 12.0)] {
                painter.circle_stroke(c.p(x, y), 2.5 * c.scale, stroke);
            }
            line(&[(8.5, 6.0), (10.5, 6.0), (13.5, 9.0), (15.5, 11.8)]);
            line(&[(8.5, 18.0), (10.5, 18.0), (13.5, 15.0), (15.5, 12.2)]);
        }
        Icon::Settings => {
            painter.circle_stroke(c.p(12.0, 12.0), 3.0 * c.scale, stroke);
            for (a, b) in [
                ((12.0, 2.0), (12.0, 5.0)),
                ((12.0, 19.0), (12.0, 22.0)),
                ((2.0, 12.0), (5.0, 12.0)),
                ((19.0, 12.0), (22.0, 12.0)),
                ((4.9, 4.9), (7.0, 7.0)),
                ((17.0, 17.0), (19.1, 19.1)),
                ((4.9, 19.1), (7.0, 17.0)),
                ((17.0, 7.0), (19.1, 4.9)),
            ] {
                line(&[a, b]);
            }
        }
        Icon::Plus => {
            line(&[(12.0, 5.0), (12.0, 19.0)]);
            line(&[(5.0, 12.0), (19.0, 12.0)]);
        }
        Icon::Star => {
            let pts: Vec<(f32, f32)> = (0..10)
                .map(|k| {
                    let a = std::f32::consts::PI * (k as f32 / 5.0 - 0.5);
                    let r = if k % 2 == 0 { 9.5 } else { 4.0 };
                    (12.0 + r * a.cos(), 12.5 + r * a.sin())
                })
                .collect();
            painter.add(Shape::closed_line(c.pts(&pts), stroke));
        }
        Icon::Previous => line(&[(15.0, 5.0), (8.0, 12.0), (15.0, 19.0)]),
        Icon::Next => line(&[(9.0, 5.0), (16.0, 12.0), (9.0, 19.0)]),
        Icon::Headphones => {
            line(&[
                (4.0, 15.0),
                (4.0, 12.0),
                (5.2, 7.8),
                (8.2, 4.8),
                (12.0, 4.0),
                (15.8, 4.8),
                (18.8, 7.8),
                (20.0, 12.0),
                (20.0, 15.0),
            ]);
            let r = CornerRadius::same((2.0 * c.scale) as u8);
            painter.rect_filled(Rect::from_min_max(c.p(3.0, 13.0), c.p(8.0, 20.0)), r, color);
            painter.rect_filled(Rect::from_min_max(c.p(16.0, 13.0), c.p(21.0, 20.0)), r, color);
        }
        Icon::SkipBack | Icon::SkipForward => {
            let flip = |x: f32| if icon == Icon::SkipForward { 24.0 - x } else { x };
            let bar = |x0: f32, x1: f32| {
                Rect::from_min_max(c.p(flip(x0).min(flip(x1)), 5.0), c.p(flip(x0).max(flip(x1)), 19.0))
            };
            painter.rect_filled(bar(5.0, 7.5), CornerRadius::same(c.scale as u8), color);
            let tri = c.pts(&[(flip(19.0), 5.0), (flip(9.0), 12.0), (flip(19.0), 19.0)]);
            painter.add(Shape::convex_polygon(tri, color, Stroke::NONE));
        }
        Icon::Close => {
            line(&[(6.0, 6.0), (18.0, 18.0)]);
            line(&[(18.0, 6.0), (6.0, 18.0)]);
        }
        Icon::Setlist => {
            for y in [6.0, 12.0, 18.0] {
                painter.circle_filled(c.p(4.5, y), 1.4 * c.scale, color);
                line(&[(9.0, y), (20.0, y)]);
            }
        }
    }
}

/// A filled star, for favourites that are set.
pub fn paint_star(ui: &Ui, rect: Rect, color: Color32, filled: bool) {
    if !filled {
        paint_icon(ui, rect, Icon::Star, color, 16.0);
        return;
    }
    let c = Canvas::new(rect, 16.0);
    let pts: Vec<Pos2> = (0..10)
        .map(|k| {
            let a = std::f32::consts::PI * (k as f32 / 5.0 - 0.5);
            let r = if k % 2 == 0 { 9.5 } else { 4.0 };
            c.p(12.0 + r * a.cos(), 12.5 + r * a.sin())
        })
        .collect();
    // A star is not convex, but all of it is visible from its centre: fill it as a fan.
    let centre = c.p(12.0, 12.5);
    let painter = ui.painter();
    for k in 0..10 {
        let tri = vec![centre, pts[k], pts[(k + 1) % 10]];
        painter.add(Shape::convex_polygon(tri, color, Stroke::NONE));
    }
}

/// A rotary knob: drag up or down to turn (Shift for fine steps), double-click for `default`.
/// Returns the new value while it changes.
#[allow(clippy::too_many_arguments)]
pub fn knob(
    ui: &mut Ui,
    rect: Rect,
    id: egui::Id,
    value: f64,
    range: (f64, f64),
    default: f64,
    label: &str,
    text: &str,
) -> (Response, Option<f64>) {
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    let painter = ui.painter();
    let dial = Rect::from_center_size(
        rect.center() - vec2(0.0, 8.0),
        vec2(rect.width().min(rect.height() - 22.0), rect.width().min(rect.height() - 22.0)),
    );
    let r = dial.width() / 2.0 - 3.0;
    let centre = dial.center();
    let (start, sweep) = (std::f32::consts::PI * 0.75, std::f32::consts::PI * 1.5);
    let frac = ((value - range.0) / (range.1 - range.0)).clamp(0.0, 1.0) as f32;
    let arc = |from: f32, to: f32| -> Vec<Pos2> {
        let n = 32;
        (0..=n)
            .map(|k| {
                let a = start + sweep * (from + (to - from) * k as f32 / n as f32);
                centre + vec2(a.cos(), a.sin()) * r
            })
            .collect()
    };
    painter.circle_filled(centre, r - 4.0, if resp.hovered() || resp.dragged() { RAISED_HI } else { theme::RAISED });
    painter.add(Shape::line(arc(0.0, 1.0), Stroke::new(3.0, theme::BG)));
    let zero = ((0.0 - range.0) / (range.1 - range.0)).clamp(0.0, 1.0) as f32;
    painter.add(Shape::line(arc(zero.min(frac), zero.max(frac)), Stroke::new(3.0, theme::ACCENT)));
    let a = start + sweep * frac;
    painter.line_segment(
        [centre + vec2(a.cos(), a.sin()) * (r - 14.0), centre + vec2(a.cos(), a.sin()) * (r - 5.0)],
        Stroke::new(2.0, TEXT),
    );
    painter.text(
        pos2(rect.center().x, rect.bottom() - 18.0),
        Align2::CENTER_CENTER,
        label,
        theme::regular(11.0),
        theme::TEXT_DIM,
    );
    painter.text(pos2(rect.center().x, rect.bottom() - 5.0), Align2::CENTER_CENTER, text, theme::mono(11.0), TEXT);
    let mut changed = None;
    if resp.dragged() {
        let fine = ui.input(|i| i.modifiers.shift);
        let per_point = (range.1 - range.0) / if fine { 1000.0 } else { 200.0 };
        let v = (value - resp.drag_delta().y as f64 * per_point).clamp(range.0, range.1);
        if v != value {
            changed = Some(v);
        }
    }
    if resp.double_clicked() {
        changed = Some(default);
    }
    (resp, changed)
}

fn hover_tint(ui: &Ui, resp: &Response, bg: Color32) -> Color32 {
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

/// A rounded button with an icon, laid out by the caller.
pub fn icon_button(
    ui: &mut Ui,
    rect: Rect,
    icon: Icon,
    bg: Color32,
    fg: Color32,
    icon_size: f32,
    tip: &str,
) -> Response {
    let resp = ui.allocate_rect(rect, Sense::click());
    paint_icon_button(ui, resp, rect, icon, bg, fg, icon_size, tip)
}

/// An [`icon_button`] with its own id, for buttons that come and go (on hover, say): an
/// allocated id would shift the ids of every widget after it and send their clicks astray.
#[allow(clippy::too_many_arguments)]
pub fn icon_button_with_id(
    ui: &mut Ui,
    id: egui::Id,
    rect: Rect,
    icon: Icon,
    bg: Color32,
    fg: Color32,
    icon_size: f32,
    tip: &str,
) -> Response {
    let resp = ui.interact(rect, id, Sense::click());
    paint_icon_button(ui, resp, rect, icon, bg, fg, icon_size, tip)
}

#[allow(clippy::too_many_arguments)]
fn paint_icon_button(
    ui: &Ui,
    resp: Response,
    rect: Rect,
    icon: Icon,
    bg: Color32,
    fg: Color32,
    icon_size: f32,
    tip: &str,
) -> Response {
    let resp = resp.on_hover_text(tip);
    let radius = (rect.height() / 2.0).min(255.0) as u8;
    ui.painter().rect_filled(rect, CornerRadius::same(radius), hover_tint(ui, &resp, bg));
    paint_icon(ui, rect, icon, fg, icon_size);
    resp
}

pub fn rail_button(ui: &mut Ui, rect: Rect, icon: Icon, active: bool, accent: Color32, tip: &str) -> Response {
    let resp = ui.allocate_rect(rect, Sense::click()).on_hover_text(tip);
    let bg = if active { RAISED_HI } else { Color32::TRANSPARENT };
    ui.painter().rect_filled(rect, CornerRadius::same(12), hover_tint(ui, &resp, bg));
    paint_icon(ui, rect, icon, if active { accent } else { theme::TEXT_DIM }, 20.0);
    resp
}

/// Small rounded text button (M / S, segmented tabs, "Add track").
pub fn text_pill(ui: &mut Ui, rect: Rect, text: &str, font: egui::FontId, bg: Color32, fg: Color32) -> Response {
    let resp = ui.allocate_rect(rect, Sense::click());
    let radius = (rect.height() / 2.0).min(255.0) as u8;
    ui.painter().rect_filled(rect, CornerRadius::same(radius), hover_tint(ui, &resp, bg));
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, font, fg);
    resp
}

pub fn label_pill(ui: &Ui, rect: Rect, text: &str, bg: Color32, fg: Color32) {
    let radius = (rect.height() / 2.0).min(255.0) as u8;
    ui.painter().rect_filled(rect, CornerRadius::same(radius), bg);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, theme::mono(12.0), fg);
}

/// iOS-style switch as in the FX chain rows.
pub fn switch(ui: &mut Ui, rect: Rect, on: bool, accent: Color32) -> Response {
    let resp = ui.allocate_rect(rect, Sense::click());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(255), if on { accent } else { theme::SWITCH_OFF });
    let knob_x = if on { rect.right() - 11.0 } else { rect.left() + 11.0 };
    painter.circle_filled(pos2(knob_x, rect.center().y), 8.0, TEXT);
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
