//! The project bay window (REAPER's View: Project bay): the project's media files, items and
//! effects, retargeting a file, and cleaning the project folder of unused files (File: Clean
//! current project directory).

use std::path::PathBuf;

use egui::{RichText, Ui, vec2};
use scythe_core::bay;
use scythe_core::tempo::format_time;

use crate::app::ScytheApp;
use crate::file_dialog::Ask;
use crate::theme::{self, TEXT_DIM};
use crate::ui::window::{WindowKind, scythe_window};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BayTab {
    Media,
    Items,
    Fx,
    Clean,
}

#[derive(Debug)]
pub struct BayWindow {
    pub tab: BayTab,
    /// The unused files with whether each is ticked for removal, once looked for.
    unused: Option<Result<(bay::Unused, Vec<bool>), String>>,
    trash: bool,
}

impl BayWindow {
    pub fn new(tab: BayTab) -> Self {
        Self { tab, unused: None, trash: true }
    }
}

pub fn show(app: &mut ScytheApp, ui: &mut Ui) {
    let Some(mut w) = app.view.bay.take() else { return };
    let ctx = ui.ctx().clone();
    let mut open = true;
    let title = if w.tab == BayTab::Clean { "Project directory cleanup" } else { "Project bay" };
    scythe_window("project-bay", title, WindowKind::Panel).default_size(vec2(760.0, 420.0)).show(
        &ctx,
        &mut open,
        |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut w.tab, BayTab::Media, "Source media");
                ui.selectable_value(&mut w.tab, BayTab::Items, "Media items");
                ui.selectable_value(&mut w.tab, BayTab::Fx, "FX");
                if ui.selectable_value(&mut w.tab, BayTab::Clean, "Unused files").clicked() {
                    w.unused = None;
                }
            });
            ui.add_space(6.0);
            egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| match w.tab {
                BayTab::Media => media(app, ui),
                BayTab::Items => items(app, ui),
                BayTab::Fx => effects(app, ui),
                BayTab::Clean => clean(app, &mut w, ui),
            });
        },
    );
    if open {
        app.view.bay = Some(w);
    }
}

fn media(app: &mut ScytheApp, ui: &mut Ui) {
    let sources = bay::sources(&app.project);
    if sources.is_empty() {
        ui.label(RichText::new("The project uses no media files.").color(TEXT_DIM));
        return;
    }
    let mut retarget: Option<PathBuf> = None;
    egui::Grid::new("bay-media").striped(true).spacing(vec2(12.0, 6.0)).show(ui, |ui| {
        for head in ["File", "Takes", "Items", "Tracks", ""] {
            ui.label(RichText::new(head).color(TEXT_DIM).size(11.0));
        }
        ui.end_row();
        for s in &sources {
            let name = s.file.display().to_string();
            if s.exists {
                ui.label(name)
            } else {
                ui.label(RichText::new(format!("{name} (missing)")).color(theme::PINK))
            }
            .on_hover_text(s.path.display().to_string());
            ui.label(s.takes.to_string());
            ui.label(s.items.len().to_string());
            let names: Vec<String> =
                s.tracks.iter().filter_map(|t| app.project.track(*t)).map(|t| t.name.clone()).collect();
            ui.label(names.join(", "));
            if ui.small_button("Retarget…").on_hover_text("Replace this file in every take").clicked() {
                retarget = Some(s.path.clone());
            }
            ui.end_row();
        }
    });
    if let Some(from) = retarget
        && let Some(to) = rfd::FileDialog::new().set_title("Replace media with").ask_file()
    {
        match app.session.retarget_media(&from, &to) {
            Ok(n) => app.notify(format!("Retargeted {n} takes")),
            Err(e) => app.notify(e),
        }
    }
}

fn items(app: &mut ScytheApp, ui: &mut Ui) {
    let mut select = None;
    egui::Grid::new("bay-items").striped(true).spacing(vec2(12.0, 6.0)).show(ui, |ui| {
        for head in ["Item", "Track", "Position", "Length", "File"] {
            ui.label(RichText::new(head).color(TEXT_DIM).size(11.0));
        }
        ui.end_row();
        for t in &app.project.tracks {
            for it in &t.items {
                let take = it.takes.get(it.active_take);
                let name = take.map(|t| t.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| "(unnamed)".into());
                if ui.selectable_label(app.sel.item == Some(it.id), name).clicked() {
                    select = Some((t.id, it.id));
                }
                ui.label(&t.name);
                ui.label(format_time(it.position));
                ui.label(format_time(it.length));
                ui.label(take.and_then(bay::take_file).map(|f| f.display().to_string()).unwrap_or_default());
                ui.end_row();
            }
        }
    });
    if let Some((t, i)) = select {
        app.sel.track = Some(t);
        app.sel.item = Some(i);
    }
}

fn effects(app: &mut ScytheApp, ui: &mut Ui) {
    let fx = bay::effects(&app.project);
    if fx.is_empty() {
        ui.label(RichText::new("The project uses no effects.").color(TEXT_DIM));
        return;
    }
    egui::Grid::new("bay-fx").striped(true).spacing(vec2(12.0, 6.0)).show(ui, |ui| {
        for head in ["Effect", "Instances", "Tracks"] {
            ui.label(RichText::new(head).color(TEXT_DIM).size(11.0));
        }
        ui.end_row();
        for f in fx {
            ui.label(scythe_engine::stock::shown_name(&f.name));
            ui.label(f.instances.to_string());
            ui.label(f.tracks.join(", "));
            ui.end_row();
        }
    });
}

fn clean(app: &mut ScytheApp, w: &mut BayWindow, ui: &mut Ui) {
    if w.unused.is_none() {
        w.unused = Some(app.session.unused_project_files().map(|u| {
            let ticks = vec![true; u.files.len()];
            (u, ticks)
        }));
    }
    let mut remove = None;
    match &mut w.unused {
        Some(Err(e)) => {
            ui.label(RichText::new(e.as_str()).color(TEXT_DIM));
        }
        Some(Ok((u, ticks))) => {
            ui.label(format!("{} of {} files are not used in {}", u.files.len(), u.total, u.dir.display()));
            ui.add_space(4.0);
            for (f, tick) in u.files.iter().zip(ticks.iter_mut()) {
                ui.checkbox(tick, f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
            }
            ui.add_space(6.0);
            ui.label(
                RichText::new("Files listed here are not used by this project, but may be used by another one.")
                    .color(TEXT_DIM),
            );
            ui.horizontal(|ui| {
                ui.checkbox(&mut w.trash, "Send files to the trash (safer)");
                let chosen: Vec<PathBuf> =
                    u.files.iter().zip(ticks.iter()).filter(|(_, t)| **t).map(|(f, _)| f.clone()).collect();
                if ui.add_enabled(!chosen.is_empty(), egui::Button::new("Remove selected files")).clicked() {
                    remove = Some(chosen);
                }
            });
        }
        None => {}
    }
    if let Some(files) = remove {
        match app.session.clean_project_files(&files, w.trash) {
            Ok(done) => app.notify(format!("Removed {} files", done.len())),
            Err(e) => app.notify(e),
        }
        w.unused = None;
    }
}
