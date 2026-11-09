//! The Actions window (REAPER's "Show action list"): search every action, run it, give it
//! keyboard shortcuts, make custom actions (macros) and register scripts as actions, and
//! import or export the shortcut file as a `.ReaperKeyMap`.

use egui::{CornerRadius, Id, RichText, Ui, vec2};
use scythe_core::keymap::{Chord, SECTION_MAIN};
use scythe_session::actions::{ActionInfo, Kind};

use crate::app::ScytheApp;
use crate::theme::{self, ACCENT, TEXT_DIM};

#[derive(Debug, Default)]
pub struct ActionsWindow {
    pub query: String,
    /// The selected action's command.
    pub selected: Option<String>,
    /// Waiting for a key to bind to the selected action.
    pub capturing: bool,
    pub editor: Option<CustomEditor>,
    /// The filter has had the keyboard since the window opened, so typing searches rather than
    /// running shortcuts.
    pub focused_once: bool,
}

/// A custom action being made or changed.
#[derive(Debug, Default, Clone)]
pub struct CustomEditor {
    /// The id of the custom action being changed; a new one gets an id when saved.
    pub id: Option<String>,
    pub name: String,
    pub commands: Vec<String>,
    pub consolidate_undo: bool,
    pub query: String,
}

fn matches(a: &ActionInfo, query: &str) -> bool {
    let name = a.name.to_lowercase();
    let command = a.command.to_lowercase();
    query.to_lowercase().split_whitespace().all(|w| name.contains(w) || command == w)
}

pub fn show(app: &mut ScytheApp, ui: &mut Ui) {
    let Some(mut state) = app.view.actions.take() else { return };
    let ctx = ui.ctx().clone();
    let mut open = true;
    if state.capturing {
        capture_key(app, &ctx, &mut state);
    }
    let frame = egui::Frame::window(ui.style()).fill(theme::PANEL).corner_radius(CornerRadius::same(18));
    egui::Window::new("Actions")
        .id(Id::unique("actions-window"))
        .open(&mut open)
        .frame(frame)
        .default_size(vec2(780.0, 520.0))
        .min_size(vec2(560.0, 320.0))
        .resizable(true)
        .show(&ctx, |ui| body(app, ui, &mut state));
    if let Some(mut editor) = state.editor.take() {
        let keep = custom_editor(app, &ctx, &mut editor);
        if keep {
            state.editor = Some(editor);
        }
    }
    if open {
        app.view.actions = Some(state);
    }
}

fn body(app: &mut ScytheApp, ui: &mut Ui, state: &mut ActionsWindow) {
    ui.horizontal(|ui| {
        ui.label("Filter");
        let filter =
            ui.add(egui::TextEdit::singleline(&mut state.query).hint_text("name or command id").desired_width(260.0));
        if !std::mem::replace(&mut state.focused_once, true) {
            filter.request_focus();
        }
        ui.separator();
        if ui.button("New custom action…").clicked() {
            state.editor = Some(CustomEditor { consolidate_undo: true, ..Default::default() });
        }
        if ui.button("Add script…").clicked()
            && let Some(path) = rfd::FileDialog::new().add_filter("ReaScript", &["lua", "eel", "py"]).pick_file()
        {
            let command = app.session.add_script_action(&path.to_string_lossy(), SECTION_MAIN);
            state.selected = Some(command);
        }
        if ui.button("Import…").on_hover_text("Merge a .ReaperKeyMap or reaper-kb.ini").clicked() {
            app.import_keymap_dialog();
        }
        if ui.button("Export…").on_hover_text("Save the shortcuts and custom actions as a .ReaperKeyMap").clicked() {
            app.export_keymap_dialog();
        }
    });
    ui.add_space(6.0);
    let list: Vec<ActionInfo> = app
        .session
        .action_list()
        .into_iter()
        .filter(|a| a.section == SECTION_MAIN && matches(a, &state.query))
        .collect();
    let mut run = None;
    let avail = ui.available_height() - 44.0;
    egui::ScrollArea::vertical().max_height(avail.max(120.0)).show(ui, |ui| {
        egui::Grid::new("action-list").striped(true).spacing(vec2(12.0, 4.0)).min_col_width(40.0).show(ui, |ui| {
            for head in ["Shortcut", "Description", "Command ID", "State"] {
                ui.label(RichText::new(head).color(TEXT_DIM).size(11.0));
            }
            ui.end_row();
            for a in &list {
                let keys = crate::keys::keys_text(&app.session, &a.command);
                let selected = state.selected.as_deref() == Some(a.command.as_str());
                ui.label(RichText::new(keys).monospace().size(11.0));
                let name = match a.kind {
                    Kind::Builtin => RichText::new(&a.name),
                    Kind::Custom | Kind::Script => RichText::new(&a.name).color(ACCENT),
                };
                let resp = ui.selectable_label(selected, name);
                if resp.clicked() {
                    state.selected = Some(a.command.clone());
                    state.capturing = false;
                }
                if resp.double_clicked() {
                    run = Some(a.command.clone());
                }
                ui.label(RichText::new(&a.command).monospace().size(11.0).color(TEXT_DIM));
                let toggle = match app.session.toggle_state(&a.command) {
                    1 => "on",
                    0 => "off",
                    _ => "",
                };
                ui.label(RichText::new(toggle).size(11.0).color(TEXT_DIM));
                ui.end_row();
            }
        });
    });
    ui.separator();
    let selected = state.selected.clone().and_then(|c| list.iter().find(|a| a.command == c).cloned());
    ui.horizontal(|ui| {
        let Some(a) = selected else {
            ui.label(RichText::new("Select an action; double-click runs it.").color(TEXT_DIM));
            return;
        };
        if ui.button("Run").clicked() {
            run = Some(a.command.clone());
        }
        let label = if state.capturing { "Press a key… (Esc cancels)" } else { "Add shortcut…" };
        if ui.selectable_label(state.capturing, label).clicked() {
            state.capturing = !state.capturing;
        }
        let keys = crate::keys::keys_for(&app.session, &a.command);
        if !keys.is_empty() {
            ui.menu_button("Remove shortcut", |ui| {
                for k in keys {
                    if ui.button(k.describe()).clicked() {
                        if app.session.key_command(k, SECTION_MAIN).is_some() {
                            app.session.unbind_key(k, SECTION_MAIN);
                        } else {
                            app.session.disable_default_key(k, SECTION_MAIN);
                        }
                        ui.close();
                    }
                }
            });
        }
        if a.kind == Kind::Custom && ui.button("Edit…").clicked() {
            state.editor = edit_existing(app, &a.command);
        }
        if a.kind != Kind::Builtin && ui.button("Delete").clicked() {
            app.session.remove_named_action(&a.command);
            state.selected = None;
        }
        if ui.button("Copy command ID").clicked() {
            ui.ctx().copy_text(a.command.clone());
        }
    });
    if let Some(c) = run {
        app.run_command(&c);
    }
}

/// Binds the next key pressed to the selected action.
fn capture_key(app: &mut ScytheApp, ctx: &egui::Context, state: &mut ActionsWindow) {
    let pressed = ctx.input_mut(|i| {
        let found = i.events.iter().find_map(|e| match e {
            egui::Event::Key { key, pressed: true, modifiers, .. } if !crate::keys::is_modifier(*key) => {
                Some((*key, *modifiers))
            }
            _ => None,
        });
        if found.is_some() {
            i.events.retain(|e| !matches!(e, egui::Event::Key { .. } | egui::Event::Text(_)));
        }
        found
    });
    let Some((key, mods)) = pressed else { return };
    state.capturing = false;
    if key == egui::Key::Escape && mods.is_none() {
        return;
    }
    let (Some(chord), Some(command)) = (crate::keys::chord(key, mods), state.selected.clone()) else {
        app.notify("That key cannot be a shortcut");
        return;
    };
    bind(app, chord, &command);
}

fn bind(app: &mut ScytheApp, chord: Chord, command: &str) {
    if let Some(old) = crate::keys::command_for(app, chord).filter(|c| c != command) {
        let name = app.session.action_name(&old).unwrap_or(old);
        app.notify(format!("{} now runs this action instead of \"{name}\"", chord.describe()));
    }
    app.session.bind_key(chord, SECTION_MAIN, command);
}

fn edit_existing(app: &ScytheApp, command: &str) -> Option<CustomEditor> {
    let scythe_core::keymap::Line::Act(a) = app.session.keymap.named(command, SECTION_MAIN)? else { return None };
    Some(CustomEditor {
        id: Some(a.id.clone()),
        name: a.name.trim_start_matches("Custom: ").to_owned(),
        commands: a.commands.clone(),
        consolidate_undo: a.consolidate_undo(),
        query: String::new(),
    })
}

/// The custom action editor: its name, the actions it runs in order, and whether it makes one
/// undo step. Answers whether it stays open.
fn custom_editor(app: &mut ScytheApp, ctx: &egui::Context, e: &mut CustomEditor) -> bool {
    let mut open = true;
    let mut done = false;
    let frame = egui::Frame::window(&ctx.global_style()).fill(theme::PANEL).corner_radius(CornerRadius::same(18));
    egui::Window::new("Custom action")
        .id(Id::unique("custom-action-editor"))
        .open(&mut open)
        .frame(frame)
        .default_size(vec2(640.0, 420.0))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.add(egui::TextEdit::singleline(&mut e.name).desired_width(320.0));
                ui.checkbox(&mut e.consolidate_undo, "Consolidate undo points");
            });
            ui.add_space(6.0);
            ui.columns(2, |cols| {
                cols[0].label(RichText::new("Runs, in order").color(TEXT_DIM));
                let mut remove = None;
                let mut up = None;
                egui::ScrollArea::vertical().id_salt("custom-members").max_height(260.0).show(&mut cols[0], |ui| {
                    for (i, c) in e.commands.iter().enumerate() {
                        ui.horizontal(|ui| {
                            if ui.small_button("x").on_hover_text("Remove").clicked() {
                                remove = Some(i);
                            }
                            if i > 0 && ui.small_button("^").on_hover_text("Move up").clicked() {
                                up = Some(i);
                            }
                            ui.label(app.session.action_name(c).unwrap_or_else(|| c.clone()));
                        });
                    }
                });
                if let Some(i) = remove {
                    e.commands.remove(i);
                }
                if let Some(i) = up {
                    e.commands.swap(i - 1, i);
                }
                cols[1].add(egui::TextEdit::singleline(&mut e.query).hint_text("Find an action to add"));
                egui::ScrollArea::vertical().id_salt("custom-pick").max_height(240.0).show(&mut cols[1], |ui| {
                    let own = e.id.as_ref().map(|i| format!("_{i}"));
                    for a in app
                        .session
                        .action_list()
                        .into_iter()
                        .filter(|a| a.section == SECTION_MAIN && matches(a, &e.query))
                        .take(200)
                    {
                        if own.as_deref() == Some(a.command.as_str()) {
                            continue;
                        }
                        if ui.selectable_label(false, &a.name).on_hover_text("Add").clicked() {
                            e.commands.push(a.command.clone());
                        }
                    }
                });
            });
            ui.separator();
            ui.horizontal(|ui| {
                let ok = !e.name.trim().is_empty() && !e.commands.is_empty();
                if ui.add_enabled(ok, egui::Button::new("Save")).clicked() {
                    app.session.define_custom_action(
                        e.id.clone(),
                        e.name.trim(),
                        e.commands.clone(),
                        e.consolidate_undo,
                    );
                    done = true;
                }
                if ui.button("Cancel").clicked() {
                    done = true;
                }
            });
        });
    open && !done
}
