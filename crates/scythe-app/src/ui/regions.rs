//! The region/marker manager (REAPER's View: Region/Marker Manager) and the region render
//! matrix (View: Region Render Matrix): edit names, times and colours, and tick which tracks
//! (and the master) each region renders.

use egui::{CornerRadius, Id, RichText, Ui, vec2};
use scythe_core::model::RegionRenderTrack;
use scythe_core::tempo::format_time;

use crate::app::ScytheApp;
use crate::theme::{self, TEXT_DIM};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionsTab {
    List,
    Matrix,
}

pub fn show(app: &mut ScytheApp, ui: &mut Ui) {
    let Some(mut tab) = app.view.regions else { return };
    let ctx = ui.ctx().clone();
    let mut open = true;
    let frame = egui::Frame::window(ui.style()).fill(theme::PANEL).corner_radius(CornerRadius::same(18));
    egui::Window::new("Regions and markers")
        .id(Id::unique("region-manager"))
        .open(&mut open)
        .frame(frame)
        .default_size(vec2(760.0, 420.0))
        .resizable(true)
        .show(&ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut tab, RegionsTab::List, "Regions and markers");
                ui.selectable_value(&mut tab, RegionsTab::Matrix, "Render matrix");
                ui.separator();
                if ui.button("Add marker").clicked() {
                    let at = app.project.cursor;
                    app.edit("Insert marker", |p| p.add_marker(at, ""));
                }
                if ui.button("Add region").clicked() {
                    let range = app.project.time_selection.unwrap_or(scythe_core::TimeRange::new(
                        app.project.cursor,
                        app.project.cursor + app.project.tempo.bar_seconds(),
                    ));
                    app.edit("Insert region", |p| {
                        p.add_marker(range.start, "");
                        let index = p.markers.iter().map(|m| m.index).max().unwrap_or(0);
                        if let Some(m) = p.markers.iter_mut().find(|m| m.index == index) {
                            m.region_end = Some(range.end);
                        }
                    });
                }
            });
            ui.add_space(6.0);
            egui::ScrollArea::both().show(ui, |ui| match tab {
                RegionsTab::List => list(app, ui),
                RegionsTab::Matrix => matrix(app, ui),
            });
        });
    app.view.regions = open.then_some(tab);
}

fn list(app: &mut ScytheApp, ui: &mut Ui) {
    if app.project.markers.is_empty() {
        ui.label(RichText::new("No markers or regions yet.").color(TEXT_DIM));
        return;
    }
    let mut remove = None;
    let mut goto = None;
    egui::Grid::new("region-list").striped(true).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
        for head in ["", "#", "Name", "Start", "End", "Length", ""] {
            ui.label(RichText::new(head).color(TEXT_DIM).size(11.0));
        }
        ui.end_row();
        for at in 0..app.project.markers.len() {
            let m = app.project.markers[at].clone();
            let mut rgb = m.color.map_or([0x75, 0x71, 0x5e], |c| [c.r, c.g, c.b]);
            if ui.color_edit_button_srgb(&mut rgb).changed() {
                app.gesture("Marker colour", |p| {
                    p.markers[at].color = Some(scythe_core::Color { r: rgb[0], g: rgb[1], b: rgb[2] });
                });
            }
            let kind = if m.region_end.is_some() { "R" } else { "M" };
            ui.label(format!("{kind}{}", m.index));
            let mut name = m.name.clone();
            let edit = ui.add_sized([200.0, 22.0], egui::TextEdit::singleline(&mut name));
            if edit.changed() {
                app.gesture("Rename marker", |p| p.markers[at].name = name.clone());
            }
            let mut start = m.position;
            let start_edit = ui.add(
                egui::DragValue::new(&mut start)
                    .speed(0.01)
                    .range(0.0..=f64::MAX)
                    .custom_formatter(|v, _| format_time(v)),
            );
            if start_edit.changed() {
                app.gesture("Move marker", |p| {
                    let mk = &mut p.markers[at];
                    let len = mk.region_end.map(|e| e - mk.position);
                    mk.position = start;
                    mk.region_end = len.map(|l| start + l);
                });
            }
            let mut end_edit = None;
            match m.region_end {
                Some(end) => {
                    let mut end = end;
                    let r = ui.add(
                        egui::DragValue::new(&mut end)
                            .speed(0.01)
                            .range(m.position + 0.001..=f64::MAX)
                            .custom_formatter(|v, _| format_time(v)),
                    );
                    if r.changed() {
                        app.gesture("Resize region", |p| p.markers[at].region_end = Some(end));
                    }
                    ui.label(format_time(end - m.position));
                    end_edit = Some(r);
                }
                None => {
                    ui.label("");
                    ui.label("");
                }
            }
            ui.horizontal(|ui| {
                if ui.small_button("Go").on_hover_text("Move the edit cursor here").clicked() {
                    goto = Some(m.position);
                }
                if ui.small_button("Delete").clicked() {
                    remove = Some(at);
                }
            });
            ui.end_row();
            let done = [Some(&edit), Some(&start_edit), end_edit.as_ref()]
                .into_iter()
                .flatten()
                .any(|r| r.lost_focus() || r.drag_stopped());
            if done {
                app.end_gesture();
            }
        }
    });
    if let Some(at) = remove {
        app.edit("Delete marker", |p| {
            p.markers.remove(at);
        });
    }
    if let Some(t) = goto {
        app.set_cursor(t);
    }
}

fn matrix(app: &mut ScytheApp, ui: &mut Ui) {
    let regions: Vec<usize> =
        (0..app.project.markers.len()).filter(|&i| app.project.markers[i].region_end.is_some()).collect();
    if regions.is_empty() {
        ui.label(RichText::new("No regions yet. Add one to choose what it renders.").color(TEXT_DIM));
        return;
    }
    ui.label(
        RichText::new("Tick what each region renders when the render source is the region render matrix.")
            .color(TEXT_DIM)
            .size(11.0),
    );
    let tracks: Vec<(String, String)> = app
        .project
        .tracks
        .iter()
        .enumerate()
        .map(|(i, t)| (t.guid.clone(), if t.name.is_empty() { format!("{}", i + 1) } else { t.name.clone() }))
        .collect();
    egui::Grid::new("region-matrix").striped(true).spacing(vec2(12.0, 6.0)).show(ui, |ui| {
        ui.label("");
        ui.label(RichText::new("Master").color(TEXT_DIM).size(11.0));
        for (_, name) in &tracks {
            ui.label(RichText::new(name).color(TEXT_DIM).size(11.0));
        }
        ui.end_row();
        for &at in &regions {
            let m = app.project.markers[at].clone();
            let name =
                if m.name.is_empty() { format!("Region {}", m.index) } else { format!("{} {}", m.index, m.name) };
            ui.label(name);
            let mut master = m.renders_master();
            if ui.checkbox(&mut master, "").changed() {
                app.edit("Region render matrix", |p| p.markers[at].set_renders_master(master));
            }
            for (guid, _) in &tracks {
                let mut on = m.render_matrix.iter().any(|r| r.guid.eq_ignore_ascii_case(guid));
                if ui.checkbox(&mut on, "").changed() {
                    app.edit("Region render matrix", |p| {
                        let row = &mut p.markers[at].render_matrix;
                        row.retain(|r| !r.guid.eq_ignore_ascii_case(guid));
                        if on {
                            row.push(RegionRenderTrack { guid: guid.clone(), channels: 0 });
                        }
                    });
                }
            }
            ui.end_row();
        }
    });
}
