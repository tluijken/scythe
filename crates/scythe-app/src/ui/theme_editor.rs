//! The theme window: a colour per main UI element, Scythe's colours back, theme files, and
//! the colours of a REAPER theme where they map.

use egui::{RichText, Ui, vec2};
use scythe_core::theme::{Rgb, Slot, Theme};

use crate::app::ScytheApp;
use crate::theme::{self, TEXT_DIM};
use crate::ui::window::{WindowKind, scythe_window};

pub fn show(app: &mut ScytheApp, ui: &mut Ui) {
    if !app.view.theme_editor {
        return;
    }
    let ctx = ui.ctx().clone();
    let mut open = true;
    scythe_window("theme-editor", "Theme colors", WindowKind::Panel).default_size(vec2(440.0, 520.0)).show(
        &ctx,
        &mut open,
        |ui| {
            ui.horizontal(|ui| {
                if ui.button("Import REAPER theme…").clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("REAPER theme", &["ReaperTheme", "ReaperThemeZip"])
                        .pick_file()
                {
                    match app.session.load_theme_file(&path) {
                        Ok(slots) => app.notify(format!("Imported {} colours from {}", slots.len(), path.display())),
                        Err(e) => app.notify(e),
                    }
                }
                if ui.button("Load…").clicked()
                    && let Some(path) = rfd::FileDialog::new().add_filter("Scythe theme", &["ScytheTheme"]).pick_file()
                    && let Err(e) = app.session.load_theme_file(&path)
                {
                    app.notify(e);
                }
                if ui.button("Save as…").clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("Scythe theme", &["ScytheTheme"])
                        .set_file_name("My theme.ScytheTheme")
                        .save_file()
                    && let Err(e) = app.session.save_theme_file(&path)
                {
                    app.notify(e);
                }
                if ui.add_enabled(!app.session.theme.is_default(), egui::Button::new("Reset")).clicked() {
                    app.session.set_theme(Theme::default());
                }
            });
            ui.add_space(6.0);
            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("theme-colors").striped(true).spacing(vec2(12.0, 6.0)).show(ui, |ui| {
                    for slot in Slot::ALL {
                        let c = app.session.theme.get(slot);
                        let mut rgb = [c.0, c.1, c.2];
                        if ui.color_edit_button_srgb(&mut rgb).changed() {
                            app.session.set_theme_color(slot, Rgb(rgb[0], rgb[1], rgb[2]));
                        }
                        ui.label(slot.label());
                        ui.label(RichText::new(c.hex()).color(TEXT_DIM).font(theme::mono(11.0)));
                        ui.end_row();
                    }
                });
            });
        },
    );
    app.view.theme_editor = open;
}
