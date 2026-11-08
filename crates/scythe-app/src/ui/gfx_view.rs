//! JSFX views: each open `@gfx` runs on the UI thread about 30 times a second in a window of
//! its own, with the mouse and keys over it. Its screen is drawn after the effect's lock is
//! let go and shown as a texture, uploaded again only when it changed.

use std::time::{Duration, Instant};

use egui::{
    Color32, ColorImage, Event, Id, Key, MouseWheelUnit, PointerButton, Rect, Sense, TextureOptions, Ui, pos2, vec2,
};
use scythe_engine::eel::gfx::{Input, Menu, MenuItem};

use crate::app::{GfxView, ScytheApp};

const PERIOD: Duration = Duration::from_millis(33);

pub fn show(app: &mut ScytheApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
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
            .show(&ctx, |ui| canvas(ui, view));
        open
    });
}

fn canvas(ui: &mut Ui, view: &mut GfxView) {
    let (rect, resp) = ui.allocate_exact_size(ui.available_size().max(vec2(64.0, 48.0)), Sense::click_and_drag());
    let hovered = resp.hovered() && view.menu.is_none();
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
    // While a menu is up the script waits for it, as REAPER's `gfx_showmenu` blocks.
    if let Some(pending) = view.menu.take() {
        match menu(ui, rect, &pending) {
            Some(choice) => run(ui, view, Input { menu_choice: Some(choice), ..pending.input }),
            None => view.menu = Some(GfxMenu { just_opened: false, ..pending }),
        }
    } else if view.ran.is_none_or(|t| t.elapsed() >= PERIOD) || view.pressed != 0 {
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
            menu_choice: None,
        };
        view.pressed = 0;
        run(ui, view, input);
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::BLACK);
    if let Some(t) = &view.texture {
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        painter.image(t.id(), Rect::from_min_size(rect.min, t.size_vec2()), uv, Color32::WHITE);
    }
}

/// A menu `@gfx` asked for and the input of the run to repeat once the user has chosen.
pub struct GfxMenu {
    pub menu: Menu,
    pub input: Input,
    pub just_opened: bool,
}

/// Runs `@gfx` under the effect's lock, then draws what it queued without the lock.
fn run(ui: &Ui, view: &mut GfxView, input: Input) {
    let Some(out) = view.instance.with(|e| e.gfx(&input)).flatten() else { return };
    view.ran = Some(Instant::now());
    if let Some(screen) = out.render().filter(|s| s.w > 0 && s.h > 0) {
        let image = ColorImage::from_rgba_unmultiplied([screen.w, screen.h], screen.px.as_flattened());
        match &mut view.texture {
            Some(t) => t.set(image, TextureOptions::NEAREST),
            None => view.texture = Some(ui.ctx().load_texture("jsfx-gfx", image, TextureOptions::NEAREST)),
        }
    }
    if let Some(menu) = out.menu {
        // The repeated run must not see the keys and wheel the first one used up.
        let input = Input { chars: Vec::new(), wheel: 0.0, hwheel: 0.0, ..input };
        view.menu = Some(GfxMenu { menu, input, just_opened: true });
    }
}

/// Shows the menu at its place in the view; the item chosen, or 0 once it is dismissed.
fn menu(ui: &Ui, rect: Rect, pending: &GfxMenu) -> Option<u32> {
    let at = rect.min + vec2(pending.menu.x as f32, pending.menu.y as f32);
    let mut chosen = None;
    let area = egui::Area::new(Id::unique(("jsfx-gfx-menu", rect.min.x as i32, rect.min.y as i32)))
        .order(egui::Order::Foreground)
        .fixed_pos(at)
        .show(ui.ctx(), |ui| {
            egui::Frame::menu(ui.style()).show(ui, |ui| {
                ui.set_min_width(140.0);
                menu_items(ui, &pending.menu.items, &mut chosen);
            });
        });
    let dismissed = ui.input(|i| i.key_pressed(Key::Escape))
        || (!pending.just_opened && area.response.clicked_elsewhere() && chosen.is_none());
    chosen.or(dismissed.then_some(0))
}

fn menu_items(ui: &mut Ui, items: &[MenuItem], chosen: &mut Option<u32>) {
    for item in items {
        match item {
            MenuItem::Item { label, id, checked, enabled } => {
                if ui.add_enabled(*enabled, egui::Button::selectable(*checked, label)).clicked() {
                    *chosen = Some(*id);
                }
            }
            MenuItem::Sub { label, items: sub, enabled } => {
                ui.add_enabled_ui(*enabled, |ui| {
                    ui.menu_button(label, |ui| menu_items(ui, sub, chosen));
                });
            }
            MenuItem::Separator => {
                ui.separator();
            }
        }
    }
}
