//! Toolbars as REAPER keeps them in `reaper-menu.ini`: the main toolbar (a strip under the
//! header) and 32 floating toolbars, each a row of buttons running actions and lit while
//! their action's toggle state is on; the toolbar editor; and the mouse modifier editor for
//! the clicks Scythe's window has.

use egui::{Align2, CornerRadius, Id, Rect, RichText, Sense, Ui, pos2, vec2};
use scythe_core::keymap::SECTION_MAIN;
use scythe_core::menus::{self, Button, MAIN_TOOLBAR, Toolbar};
use scythe_core::mousemap::{self, Assignment, CLICK_CONTEXTS};
use scythe_session::api::customize::toolbar_or_default;

use crate::app::ScytheApp;
use crate::theme::{self, ACCENT, PANEL, RAISED, RAISED_HI, TEXT, TEXT_DIM};

pub const STRIP_H: f32 = 34.0;
const BUTTON_H: f32 = 28.0;

/// Draws a toolbar's buttons from `origin` along a row; answers the command clicked.
fn buttons(app: &mut ScytheApp, ui: &mut Ui, toolbar: &Toolbar, origin: egui::Pos2, id: &str) -> (Option<String>, f32) {
    let font = theme::regular(12.0);
    let mut x = origin.x;
    let mut clicked = None;
    for (i, b) in toolbar.buttons.iter().enumerate() {
        if b.is_separator() {
            let r = Rect::from_min_size(pos2(x + 4.0, origin.y + 6.0), vec2(1.0, BUTTON_H - 12.0));
            ui.painter().rect_filled(r, CornerRadius::ZERO, RAISED_HI);
            x += 9.0;
            continue;
        }
        let on = app.session.toggle_state(&b.command) == 1;
        let icon = b.icon.as_deref().filter(|i| !i.starts_with("text")).and_then(|i| app.toolbar_icon(i));
        let label = b.label();
        let w = match &icon {
            Some(_) => BUTTON_H,
            None => crate::ui::widgets::text_width(ui, &label, font.clone()) + 20.0,
        };
        let r = Rect::from_min_size(pos2(x, origin.y), vec2(w, BUTTON_H));
        let tip = app.session.action_name(&b.command).unwrap_or_else(|| b.command.clone());
        let tip = if b.text.is_empty() || b.text == tip { tip } else { format!("{}\n{tip}", b.text) };
        let resp = ui.interact(r, Id::unique((id, i)), Sense::click()).on_hover_text(tip);
        let bg = if on {
            RAISED_HI
        } else if resp.hovered() {
            RAISED
        } else {
            PANEL
        };
        ui.painter().rect_filled(r, CornerRadius::same(8), bg);
        match icon {
            Some(tex) => {
                // REAPER's icons hold three frames side by side: normal, hovered, on.
                let frame = if on {
                    2.0
                } else if resp.hovered() {
                    1.0
                } else {
                    0.0
                };
                let uv = Rect::from_min_max(pos2(frame / 3.0, 0.0), pos2((frame + 1.0) / 3.0, 1.0));
                ui.painter().image(tex.id(), r.shrink(2.0), uv, egui::Color32::WHITE);
            }
            None => {
                let fg = if on { ACCENT } else { TEXT };
                ui.painter().text(r.center(), Align2::CENTER_CENTER, &label, font.clone(), fg);
            }
        }
        if resp.clicked() {
            clicked = Some(b.command.clone());
        }
        x = r.right() + 4.0;
    }
    (clicked, x - origin.x)
}

/// The main toolbar, as a strip under the header.
pub fn main_strip(app: &mut ScytheApp, ui: &mut Ui, rect: Rect) {
    ui.painter().rect_filled(rect, CornerRadius::same(12), PANEL);
    let toolbar = toolbar_or_default(&app.session, MAIN_TOOLBAR);
    let origin = pos2(rect.left() + 6.0, rect.center().y - BUTTON_H / 2.0);
    let (clicked, _) = buttons(app, ui, &toolbar, origin, "main-toolbar");
    let resp = ui.interact(rect, Id::unique("main-toolbar-bg"), Sense::click());
    resp.context_menu(|ui| {
        if ui.button("Customize toolbar…").clicked() {
            app.view.toolbar_editor = Some(ToolbarEditor { section: MAIN_TOOLBAR.into(), ..Default::default() });
            ui.close();
        }
    });
    if let Some(c) = clicked {
        app.run_command(&c);
    }
}

/// The floating toolbars that are open.
pub fn floating(app: &mut ScytheApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    for n in app.view.toolbars.clone() {
        let section = menus::floating_toolbar(n as usize);
        let toolbar = toolbar_or_default(&app.session, &section);
        let title = toolbar.title.clone().unwrap_or_else(|| format!("Toolbar {n}"));
        let mut open = true;
        let mut clicked = None;
        egui::Window::new(title)
            .id(Id::unique(("floating-toolbar", n)))
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .frame(egui::Frame::window(&ctx.global_style()).fill(PANEL).corner_radius(CornerRadius::same(14)))
            .show(&ctx, |ui| {
                if toolbar.buttons.is_empty() {
                    if ui.button("Empty toolbar: customize…").clicked() {
                        app.view.toolbar_editor =
                            Some(ToolbarEditor { section: section.clone(), ..Default::default() });
                    }
                    return;
                }
                let origin = ui.cursor().min;
                let (c, w) = buttons(app, ui, &toolbar, origin, &section);
                ui.allocate_space(vec2(w, BUTTON_H));
                clicked = c;
            });
        if !open {
            app.view.toolbars.remove(&n);
        }
        if let Some(c) = clicked {
            app.run_command(&c);
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolbarEditor {
    pub section: String,
    pub query: String,
    pub selected: Option<usize>,
}

impl Default for ToolbarEditor {
    fn default() -> Self {
        Self { section: MAIN_TOOLBAR.into(), query: String::new(), selected: None }
    }
}

/// REAPER's "Customize menus/toolbars" for toolbars: add, remove and order buttons, set their
/// text and icon, reset, import a `.ReaperMenu`.
pub fn editor(app: &mut ScytheApp, ui: &mut Ui) {
    let Some(mut e) = app.view.toolbar_editor.take() else { return };
    let ctx = ui.ctx().clone();
    let mut open = true;
    let mut toolbar = toolbar_or_default(&app.session, &e.section);
    let before = toolbar.clone();
    let frame = egui::Frame::window(&ctx.global_style()).fill(PANEL).corner_radius(CornerRadius::same(18));
    egui::Window::new("Customize toolbars")
        .id(Id::unique("toolbar-editor"))
        .open(&mut open)
        .frame(frame)
        .default_size(vec2(680.0, 460.0))
        .show(&ctx, |ui| {
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("toolbar-pick").selected_text(&e.section).width(200.0).show_ui(ui, |ui| {
                    let names = std::iter::once(MAIN_TOOLBAR.to_owned())
                        .chain((1..=menus::FLOATING_TOOLBARS).map(menus::floating_toolbar));
                    for name in names {
                        if ui.selectable_label(e.section == name, &name).clicked() {
                            e.section = name;
                            e.selected = None;
                        }
                    }
                });
                if ui.button("Reset to default").clicked() {
                    app.session.reset_toolbar(&e.section);
                }
                if ui.button("Import…").on_hover_text("A .ReaperMenu or .ReaperMenuSet").clicked() {
                    app.import_menus_dialog();
                }
                if ui.button("Export…").clicked() {
                    app.export_menus_dialog(&e.section);
                }
            });
            if e.section != toolbar.section {
                return;
            }
            ui.add_space(6.0);
            ui.columns(2, |cols| {
                let ui = &mut cols[0];
                ui.label(RichText::new("Buttons").color(TEXT_DIM));
                let mut remove = None;
                let mut up = None;
                egui::ScrollArea::vertical().id_salt("tb-buttons").max_height(300.0).show(ui, |ui| {
                    for (i, b) in toolbar.buttons.iter().enumerate() {
                        ui.horizontal(|ui| {
                            if ui.small_button("x").on_hover_text("Remove").clicked() {
                                remove = Some(i);
                            }
                            if ui.add_enabled(i > 0, egui::Button::new("^").small()).on_hover_text("Move up").clicked()
                            {
                                up = Some(i);
                            }
                            let name = if b.is_separator() {
                                "— separator —".to_owned()
                            } else {
                                app.session.action_name(&b.command).unwrap_or_else(|| b.command.clone())
                            };
                            if ui.selectable_label(e.selected == Some(i), name).clicked() {
                                e.selected = Some(i);
                            }
                        });
                    }
                });
                if let Some(i) = remove {
                    toolbar.buttons.remove(i);
                    e.selected = None;
                }
                if let Some(i) = up {
                    toolbar.buttons.swap(i - 1, i);
                }
                if ui.button("Add separator").clicked() {
                    toolbar.buttons.push(Button::separator());
                }
                if let Some(b) = e.selected.and_then(|i| toolbar.buttons.get_mut(i)).filter(|b| !b.is_separator()) {
                    ui.horizontal(|ui| {
                        ui.label("Text");
                        ui.text_edit_singleline(&mut b.text);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Icon");
                        let mut icon = b.icon.clone().unwrap_or_default();
                        if ui
                            .text_edit_singleline(&mut icon)
                            .on_hover_text("A file in Data/toolbar_icons, or text")
                            .changed()
                        {
                            b.icon = (!icon.trim().is_empty()).then(|| icon.trim().to_owned());
                        }
                    });
                }
                let ui = &mut cols[1];
                ui.add(egui::TextEdit::singleline(&mut e.query).hint_text("Find an action to add"));
                egui::ScrollArea::vertical().id_salt("tb-pick").max_height(320.0).show(ui, |ui| {
                    let q = e.query.to_lowercase();
                    let words: Vec<&str> = q.split_whitespace().collect();
                    for a in app
                        .session
                        .action_list()
                        .into_iter()
                        .filter(|a| a.section == SECTION_MAIN)
                        .filter(|a| {
                            let n = a.name.to_lowercase();
                            words.iter().all(|w| n.contains(w))
                        })
                        .take(200)
                    {
                        if ui.selectable_label(false, &a.name).on_hover_text("Add to the toolbar").clicked() {
                            let text = scythe_session::api::customize::button_text(&app.session, &a.command);
                            toolbar.buttons.push(Button { command: a.command.clone(), text, icon: None, flags: None });
                        }
                    }
                });
            });
        });
    if toolbar != before {
        app.session.set_toolbar(&toolbar);
    }
    if open {
        app.view.toolbar_editor = Some(e);
    }
}

/// The mouse modifier editor: the action a click or double click runs per context and
/// modifier keys, as `reaper-mouse.ini` stores it.
pub fn mouse_editor(app: &mut ScytheApp, ui: &mut Ui) {
    if !app.view.mouse_editor {
        return;
    }
    let ctx = ui.ctx().clone();
    let mut open = true;
    let frame = egui::Frame::window(&ctx.global_style()).fill(PANEL).corner_radius(CornerRadius::same(18));
    let id = Id::unique("mouse-editor-state");
    let (mut context, mut mods, mut query): (usize, u8, String) = ctx.data(|d| d.get_temp(id)).unwrap_or_default();
    egui::Window::new("Mouse modifiers")
        .id(Id::unique("mouse-editor"))
        .open(&mut open)
        .frame(frame)
        .default_size(vec2(560.0, 420.0))
        .show(&ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Context");
                egui::ComboBox::from_id_salt("mm-context").selected_text(CLICK_CONTEXTS[context].1).show_ui(ui, |ui| {
                    for (i, (_, label)) in CLICK_CONTEXTS.iter().enumerate() {
                        ui.selectable_value(&mut context, i, *label);
                    }
                });
            });
            let key = CLICK_CONTEXTS[context].0;
            egui::Grid::new("mm-grid").striped(true).show(ui, |ui| {
                for m in 0..16u8 {
                    let name = modifier_name(m);
                    let assigned = app.session.mouse.get(key, m);
                    if ui.selectable_label(mods == m, name).clicked() {
                        mods = m;
                    }
                    let text = match &assigned {
                        Some(Assignment::Action(c)) => app.session.action_name(c).unwrap_or_else(|| c.clone()),
                        Some(Assignment::Behavior(n)) => format!("REAPER behaviour {n}"),
                        None => match mousemap::default_behaviour(key, m) {
                            Some(d) => format!("Default: {d}"),
                            None => "Default: no action".into(),
                        },
                    };
                    ui.label(RichText::new(text).color(if assigned.is_some() { TEXT } else { TEXT_DIM }));
                    if assigned.is_some() && ui.small_button("Reset").clicked() {
                        app.session.set_mouse_modifier(key, m, None);
                    }
                    ui.end_row();
                }
            });
            ui.separator();
            ui.label(format!("Assign to {} + {}:", CLICK_CONTEXTS[context].1, modifier_name(mods)));
            ui.add(egui::TextEdit::singleline(&mut query).hint_text("Find an action"));
            egui::ScrollArea::vertical().id_salt("mm-pick").max_height(140.0).show(ui, |ui| {
                let q = query.to_lowercase();
                for a in app
                    .session
                    .action_list()
                    .into_iter()
                    .filter(|a| a.section == SECTION_MAIN && !q.is_empty() && a.name.to_lowercase().contains(&q))
                    .take(100)
                {
                    if ui.selectable_label(false, &a.name).clicked() {
                        app.session.set_mouse_modifier(key, mods, Some(&Assignment::Action(a.command.clone())));
                    }
                }
            });
        });
    ctx.data_mut(|d| d.insert_temp(id, (context, mods, query)));
    app.view.mouse_editor = open;
}

fn modifier_name(m: u8) -> String {
    let mut parts = Vec::new();
    for (bit, name) in
        [(mousemap::CTRL, "Ctrl"), (mousemap::ALT, "Alt"), (mousemap::SHIFT, "Shift"), (mousemap::LOGO, "Logo")]
    {
        if m & bit != 0 {
            parts.push(name);
        }
    }
    if parts.is_empty() { "No modifier".into() } else { parts.join("+") }
}

/// Runs the action assigned to a click in `context` with the modifiers held, if any; answers
/// whether one ran (and the default click should not happen).
pub fn mouse_action(app: &mut ScytheApp, ui: &Ui, context: &str) -> bool {
    let m = ui.input(|i| i.modifiers);
    let flags = mousemap::modifiers(m.shift, m.ctrl || m.command, m.alt, false);
    match app.session.mouse.action(context, flags) {
        Some(c) => {
            app.run_command(&c);
            true
        }
        None => false,
    }
}
