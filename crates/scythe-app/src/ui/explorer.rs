//! The media explorer window (REAPER's Media explorer): shortcuts to folders and media
//! databases, the file list with its metadata columns, search, favourites, the preview player
//! with tempo match, and inserting into the project.

use std::path::PathBuf;

use egui::{CornerRadius, Id, RichText, Ui, vec2};
use scythe_core::mediadb::format_length;
use scythe_session::explorer::{Entry, InsertTarget, Location, TempoMatch};

use crate::app::ScytheApp;
use crate::theme::{self, TEXT_DIM};

/// What the window shows besides the session's explorer state.
#[derive(Debug, Default)]
pub struct ExplorerWindow {
    pub search: String,
    entries: Vec<Entry>,
    /// The listing is out of date (another place, search or scan).
    stale: bool,
    error: Option<String>,
    new_database: Option<String>,
}

impl ExplorerWindow {
    pub fn open() -> Self {
        Self { stale: true, ..Default::default() }
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

fn refresh(app: &mut ScytheApp, w: &mut ExplorerWindow) {
    w.stale = false;
    let loc = app.session.explorer.location.clone().unwrap_or_else(|| Location::Folder(home()));
    app.session.explorer.location = Some(loc.clone());
    let result =
        if w.search.trim().is_empty() { app.session.browse(&loc) } else { app.session.search_media(&loc, &w.search) };
    match result {
        Ok(e) => (w.entries, w.error) = (e, None),
        Err(e) => (w.entries, w.error) = (Vec::new(), Some(e)),
    }
}

pub fn show(app: &mut ScytheApp, ui: &mut Ui) {
    let Some(mut w) = app.view.explorer.take() else { return };
    if w.stale {
        refresh(app, &mut w);
    }
    let ctx = ui.ctx().clone();
    let mut open = true;
    let frame = egui::Frame::window(ui.style()).fill(theme::PANEL).corner_radius(CornerRadius::same(18));
    egui::Window::new("Media explorer")
        .id(Id::unique("media-explorer"))
        .open(&mut open)
        .frame(frame)
        .default_size(vec2(980.0, 460.0))
        .resizable(true)
        .show(&ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Up").on_hover_text("Go to parent folder").clicked() {
                    let _ = app.session.run_explorer_action(1004);
                    w.stale = true;
                }
                let place = match &app.session.explorer.location {
                    Some(Location::Folder(p)) => p.display().to_string(),
                    Some(Location::Database(d)) => {
                        let name = app.session.explorer_shortcuts().into_iter().find(|s| &s.target == d);
                        format!("Database: {}", name.map_or_else(|| d.clone(), |s| s.name()))
                    }
                    None => String::new(),
                };
                ui.label(RichText::new(place).color(TEXT_DIM));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let search =
                        ui.add(egui::TextEdit::singleline(&mut w.search).hint_text("Search").desired_width(220.0));
                    if search.changed() {
                        w.stale = true;
                    }
                });
            });
            ui.add_space(4.0);
            preview_bar(app, &mut w, ui);
            ui.add_space(6.0);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(180.0);
                    shortcuts(app, &mut w, ui);
                });
                ui.separator();
                ui.vertical(|ui| {
                    if let Some(e) = &w.error {
                        ui.label(RichText::new(e).color(theme::PINK));
                    }
                    list(app, &mut w, ui);
                });
            });
        });
    if open {
        app.view.explorer = Some(w);
    } else {
        app.session.preview_stop();
    }
}

fn shortcuts(app: &mut ScytheApp, w: &mut ExplorerWindow, ui: &mut Ui) {
    let current = app.session.explorer.location.clone();
    let mut target: Option<Location> = None;
    if ui.selectable_label(false, "Home").clicked() {
        target = Some(Location::Folder(home()));
    }
    if let Some(dir) = app.project.path.as_deref().and_then(std::path::Path::parent).map(std::path::Path::to_owned)
        && ui.selectable_label(current == Some(Location::Folder(dir.clone())), "Project directory").clicked()
    {
        target = Some(Location::Folder(dir));
    }
    let list = app.session.explorer_shortcuts();
    let mut remove = None;
    for (i, s) in list.iter().enumerate() {
        let loc = if s.is_database() {
            Location::Database(s.target.clone())
        } else {
            Location::Folder(s.target.clone().into())
        };
        let label = if s.is_database() { format!("DB: {}", s.name()) } else { s.name() };
        let r = ui.selectable_label(current.as_ref() == Some(&loc), label);
        if r.clicked() {
            target = Some(loc.clone());
        }
        r.context_menu(|ui| {
            if s.is_database() && ui.button("Scan database for new files").clicked() {
                if let Err(e) = app.session.scan_database(&s.target, false) {
                    app.notify(e);
                }
                w.stale = true;
                ui.close();
            }
            if s.is_database() && ui.button("Remove missing files from database").clicked() {
                let _ = app.session.remove_missing_from_database(&s.target);
                w.stale = true;
                ui.close();
            }
            if ui.button("Remove shortcut").clicked() {
                remove = Some(i);
                ui.close();
            }
        });
    }
    if let Some(loc) = target {
        app.session.explorer.location = Some(loc);
        w.stale = true;
    }
    if let Some(i) = remove {
        let mut list = list;
        list.remove(i);
        if let Err(e) = app.session.set_explorer_shortcuts(&list) {
            app.notify(e);
        }
    }
    ui.add_space(6.0);
    if let Some(Location::Folder(dir)) = &current
        && ui.small_button("Add folder to shortcuts").clicked()
    {
        let mut list = app.session.explorer_shortcuts();
        list.push(scythe_core::mediadb::Shortcut { target: dir.display().to_string(), title: None });
        if let Err(e) = app.session.set_explorer_shortcuts(&list) {
            app.notify(e);
        }
    }
    match w.new_database.take() {
        None => {
            if let Some(Location::Folder(_)) = &current
                && ui.small_button("Create database from folder…").clicked()
            {
                w.new_database = Some(String::new());
            }
        }
        Some(mut name) => {
            let mut keep = true;
            ui.add(egui::TextEdit::singleline(&mut name).hint_text("Database name"));
            ui.horizontal(|ui| {
                if ui.small_button("Create").clicked()
                    && let Some(Location::Folder(dir)) = &current
                {
                    let title = if name.trim().is_empty() { "New database".to_owned() } else { name.trim().to_owned() };
                    match app.session.create_database(&title, std::slice::from_ref(dir)) {
                        Ok(db) => app.session.explorer.location = Some(Location::Database(db)),
                        Err(e) => app.notify(e),
                    }
                    keep = false;
                    w.stale = true;
                }
                if ui.small_button("Cancel").clicked() {
                    keep = false;
                }
            });
            if keep {
                w.new_database = Some(name);
            }
        }
    }
}

fn preview_bar(app: &mut ScytheApp, w: &mut ExplorerWindow, ui: &mut Ui) {
    let preview = app.session.engine.preview().clone();
    ui.horizontal(|ui| {
        let playing = preview.is_playing();
        if ui.button(if playing { "Stop" } else { "Play" }).clicked()
            && let Err(e) = app.session.run_explorer_action(40024)
        {
            app.notify(e);
        }
        if ui.button("Pause").clicked() {
            preview.pause();
        }
        let mut repeat = preview.repeat();
        if ui.toggle_value(&mut repeat, "Repeat").changed() {
            preview.set_repeat(repeat);
        }
        ui.separator();
        let e = &mut app.session.explorer;
        let before = e.tempo_match;
        egui::ComboBox::from_id_salt("tempo-match")
            .selected_text(match e.tempo_match {
                TempoMatch::Off => "Tempo match off",
                TempoMatch::On => "Tempo match",
                TempoMatch::Half => "Tempo match /2",
                TempoMatch::Double => "Tempo match x2",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut e.tempo_match, TempoMatch::Off, "Off");
                ui.selectable_value(&mut e.tempo_match, TempoMatch::On, "On");
                ui.selectable_value(&mut e.tempo_match, TempoMatch::Half, "/2");
                ui.selectable_value(&mut e.tempo_match, TempoMatch::Double, "x2");
            });
        ui.checkbox(&mut e.preserve_pitch, "Preserve pitch");
        ui.label("Pitch");
        ui.add(egui::DragValue::new(&mut e.pitch).range(-24.0..=24.0).speed(0.1).suffix(" st"));
        ui.label("Volume");
        if ui.add(egui::DragValue::new(&mut e.volume_db).range(-60.0..=12.0).speed(0.1).suffix(" dB")).changed() {
            preview.set_gain(scythe_core::units::db_to_gain(e.volume_db) as f32);
        }
        if e.tempo_match != before {
            w.stale = true;
        }
        if let Some((pos, len)) = preview.position() {
            ui.label(RichText::new(format!("{} / {}", format_length(pos), format_length(len))).color(TEXT_DIM));
        }
    });
}

fn list(app: &mut ScytheApp, w: &mut ExplorerWindow, ui: &mut Ui) {
    let database = match &app.session.explorer.location {
        Some(Location::Database(d)) => Some(d.clone()),
        _ => None,
    };
    let selected = app.session.explorer.selected.clone();
    let mut action: Option<(PathBuf, u32)> = None;
    let mut open_folder = None;
    let mut favorite = None;
    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
        egui::Grid::new("explorer-list").striped(true).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
            for head in
                ["Fav", "File", "Length", "Rate", "Ch", "Bits", "BPM", "Key", "Title", "Artist", "Genre", "Comment"]
            {
                ui.label(RichText::new(head).color(TEXT_DIM).size(11.0));
            }
            ui.end_row();
            for e in &w.entries {
                let mut fav = e.favorite;
                if e.folder {
                    ui.label("");
                } else if ui.checkbox(&mut fav, "").on_hover_text("Favourite").changed() {
                    favorite = Some((e.path.clone(), fav));
                }
                let name = if e.folder { format!("{}/", e.name()) } else { e.name() };
                let r = ui.selectable_label(selected.as_ref() == Some(&e.path), name);
                if r.clicked() {
                    app.session.explorer.selected = Some(e.path.clone());
                }
                if r.double_clicked() {
                    if e.folder {
                        open_folder = Some(e.path.clone());
                    } else {
                        action = Some((e.path.clone(), 41000));
                    }
                }
                if !e.folder {
                    r.context_menu(|ui| {
                        for (label, id) in [
                            ("Preview", 1008),
                            ("Insert into project", 41000),
                            ("Insert on a new track", 41001),
                            ("Set as favorite", 42084),
                        ] {
                            if ui.button(label).clicked() {
                                action = Some((e.path.clone(), id));
                                ui.close();
                            }
                        }
                    });
                }
                let i = e.info.clone().unwrap_or_default();
                let cell = |ui: &mut Ui, v: Option<String>| {
                    ui.label(v.unwrap_or_default());
                };
                cell(ui, i.length.map(format_length));
                cell(ui, i.sample_rate.map(|r| format!("{r}")));
                cell(ui, i.channels.map(|c| c.to_string()));
                cell(ui, i.bits.map(|b| b.to_string()).or(i.bitrate.map(|k| format!("{k}k"))));
                cell(ui, i.bpm.map(|b| format!("{b}")));
                cell(ui, i.key);
                cell(ui, i.title);
                cell(ui, i.artist);
                cell(ui, i.genre);
                cell(ui, i.comment);
                ui.end_row();
            }
        });
        if w.entries.is_empty() && w.error.is_none() {
            ui.label(RichText::new("No media here.").color(TEXT_DIM));
        }
    });
    if let Some(dir) = open_folder {
        app.session.explorer.location = Some(Location::Folder(dir));
        w.search.clear();
        w.stale = true;
    }
    if let Some((path, on)) = favorite {
        if let Err(e) = app.session.set_favorite(&path, on, database.as_deref()) {
            app.notify(e);
        }
        w.stale = true;
    }
    if let Some((path, id)) = action {
        app.session.explorer.selected = Some(path.clone());
        let result = match id {
            41000 => app.session.insert_media(&path, InsertTarget::Project).map(|(t, i)| {
                app.sel.track = Some(t);
                app.sel.item = Some(i);
            }),
            _ => app.session.run_explorer_action(id),
        };
        if let Err(e) = result {
            app.notify(e);
        }
        if id == 42084 {
            w.stale = true;
        }
    }
}
