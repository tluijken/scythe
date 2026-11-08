//! JSFX views: each open `@gfx` runs on the UI thread about 30 times a second in a window of
//! its own, with the mouse and keys over it, and its display list is painted there.

use std::time::{Duration, Instant};

use egui::epaint::{Mesh, PathStroke};
use egui::{
    Align2, Color32, CornerRadius, Event, Id, Key, MouseWheelUnit, Painter, PointerButton, Pos2, Rect, Sense, Shape,
    Stroke, StrokeKind, Ui, pos2, vec2,
};
use scythe_engine::eel::gfx::{self, Draw, Input};

use crate::app::{GfxView, ScytheApp};
use crate::theme;

const PERIOD: Duration = Duration::from_millis(33);

pub fn show(app: &mut ScytheApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    // Monospace advance per point of font size, to fit text to the grid `@gfx` lays it out on.
    let advance = super::widgets::text_width(ui, "MMMMMMMMMM", theme::mono(100.0)) / 1000.0;
    app.gfx_views.retain_mut(|view| {
        let mut open = true;
        let (w, h) = view.size;
        let size =
            vec2(if w == 0 { 600.0 } else { w as f32 }.max(420.0), if h == 0 { 400.0 } else { h as f32 }.max(300.0));
        egui::Window::new(view.title.as_str())
            .id(Id::unique(("jsfx-gfx", view.track, view.index)))
            .open(&mut open)
            .default_size(size)
            .resizable(true)
            .show(&ctx, |ui| canvas(ui, view, advance));
        open
    });
}

fn canvas(ui: &mut Ui, view: &mut GfxView, advance: f32) {
    let (rect, resp) = ui.allocate_exact_size(ui.available_size().max(vec2(64.0, 48.0)), Sense::click_and_drag());
    let hovered = resp.hovered();
    let held = resp.is_pointer_button_down_on();
    let (buttons, mods, pointer) = ui.input(|i| {
        let mut buttons = 0;
        for (b, bit) in [(PointerButton::Primary, 1), (PointerButton::Secondary, 2), (PointerButton::Middle, 64)] {
            if held && i.pointer.button_down(b) {
                buttons |= bit;
            }
            if hovered && i.pointer.button_pressed(b) {
                view.pressed |= bit;
            }
        }
        for e in &i.events {
            match e {
                Event::MouseWheel { unit, delta, .. } if hovered => {
                    let notch = match unit {
                        MouseWheelUnit::Point => 120.0 / 50.0,
                        MouseWheelUnit::Line => 120.0,
                        MouseWheelUnit::Page => 360.0,
                    };
                    view.wheel.0 += (delta.y * notch) as f64;
                    view.wheel.1 += (delta.x * notch) as f64;
                }
                Event::Text(t) if hovered => view.chars.extend(t.chars().map(u32::from)),
                Event::Key { key, pressed: true, .. } if hovered => {
                    let code = match key {
                        Key::Enter => 13,
                        Key::Backspace => 8,
                        Key::Escape => 27,
                        Key::Tab => 9,
                        Key::Delete => 0x64656c,
                        _ => 0,
                    };
                    if code != 0 {
                        view.chars.push(code);
                    }
                }
                _ => {}
            }
        }
        let m = i.modifiers;
        let mods = [(m.ctrl, 4), (m.shift, 8), (m.alt, 16)].iter().filter(|(on, _)| *on).map(|(_, b)| b).sum::<u32>();
        (buttons, mods, i.pointer.latest_pos())
    });
    if let Some(p) = pointer.filter(|_| hovered || held) {
        view.mouse = ((p.x - rect.left()) as f64, (p.y - rect.top()) as f64);
    }
    let due = view.ran.is_none_or(|t| t.elapsed() >= PERIOD) || view.pressed != 0;
    if due {
        let input = Input {
            w: rect.width().floor() as f64,
            h: rect.height().floor() as f64,
            mouse_x: view.mouse.0,
            mouse_y: view.mouse.1,
            mouse_cap: buttons | view.pressed | mods,
            wheel: std::mem::take(&mut view.wheel.0),
            hwheel: std::mem::take(&mut view.wheel.1),
            mouse_in: hovered,
            chars: std::mem::take(&mut view.chars),
        };
        if let Some(draws) = view.instance.with(|e| e.gfx(&input)).flatten() {
            view.draws = draws;
        }
        view.pressed = 0;
        view.ran = Some(Instant::now());
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, Color32::BLACK);
    for d in &view.draws {
        paint(&painter, rect, d, advance);
    }
}

fn color(c: gfx::Color) -> Color32 {
    let [r, g, b, a] = c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

fn paint(base: &Painter, rect: Rect, d: &Draw, advance: f32) {
    let o = rect.min.to_vec2();
    let clipped;
    let painter = match d.clip {
        Some([l, t, r, b]) => {
            clipped = base.with_clip_rect(Rect::from_min_max(pos2(l, t) + o, pos2(r, b) + o).intersect(rect));
            &clipped
        }
        None => base,
    };
    let c = color(d.color);
    let stroke = Stroke::new(1.0, c);
    // Lines and outlines run through pixel centres, as REAPER draws them.
    let at = |x: f32, y: f32| pos2(x + 0.5, y + 0.5) + o;
    let area = |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(pos2(x, y) + o, vec2(w, h));
    match &d.shape {
        gfx::Shape::Rect { x, y, w, h, filled: true } => {
            painter.rect_filled(area(*x, *y, *w, *h), CornerRadius::ZERO, c);
        }
        gfx::Shape::Rect { x, y, w, h, .. } => {
            painter.rect_stroke(area(*x, *y, *w, *h), CornerRadius::ZERO, stroke, StrokeKind::Inside);
        }
        gfx::Shape::RoundRect { x, y, w, h, r } => {
            let radius = CornerRadius::same(r.clamp(0.0, 255.0) as u8);
            painter.rect_stroke(area(*x, *y, *w, *h), radius, stroke, StrokeKind::Inside);
        }
        gfx::Shape::Line { a, b } => {
            painter.line_segment([at(a[0], a[1]), at(b[0], b[1])], stroke);
        }
        gfx::Shape::Circle { x, y, r, filled } => {
            if *filled {
                painter.circle_filled(at(*x, *y), *r, c);
            } else {
                painter.circle_stroke(at(*x, *y), *r, stroke);
            }
        }
        gfx::Shape::Arc { x, y, r, from, to } => {
            let steps = ((to - from).abs() * r).ceil().clamp(4.0, 256.0) as usize;
            let points: Vec<Pos2> = (0..=steps)
                .map(|k| {
                    let t = from + (to - from) * k as f32 / steps as f32;
                    at(x + r * t.sin(), y - r * t.cos())
                })
                .collect();
            painter.add(Shape::line(points, PathStroke::new(1.0, c)));
        }
        gfx::Shape::Polygon(points) => {
            painter.add(Shape::convex_polygon(points.iter().map(|p| pos2(p[0], p[1]) + o).collect(), c, Stroke::NONE));
        }
        gfx::Shape::Gradient { x, y, w, h, corners } => {
            let r = area(*x, *y, *w, *h);
            let mut mesh = Mesh::default();
            for (p, k) in [r.left_top(), r.right_top(), r.left_bottom(), r.right_bottom()].into_iter().zip(corners) {
                mesh.colored_vertex(p, color(*k));
            }
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(1, 3, 2);
            painter.add(Shape::mesh(mesh));
        }
        gfx::Shape::Text { x, y, size, text } => {
            let font = (gfx::char_width(*size) / advance.max(0.1)).min(*size).max(1.0);
            let top = pos2(*x, *y + (size - font) / 2.0) + o;
            painter.text(top, Align2::LEFT_TOP, text, theme::mono(font), c);
        }
    }
}
