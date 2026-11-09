//! A track's recording menu, as REAPER's record-arm context menu: record mode, where output
//! recording taps, input monitoring, the project record mode and lane/comp options.

use egui::Ui;
use scythe_core::TrackId;
use scythe_core::prefs::{AUTOXFADE_LANES_EXCLUSIVE, AUTOXFADE_LOOP_TAKES, AUTOXFADE_RECORD_LANES};
use scythe_core::recording::{MonitorMode, ProjectRecordMode, RecordMode, RecordTap};

use crate::app::ScytheApp;

pub fn track_menu(app: &mut ScytheApp, ui: &mut Ui, track_id: TrackId) {
    let Some(track) = app.project.track(track_id).cloned() else { return };
    let set = |app: &mut ScytheApp, label: &str, f: &dyn Fn(&mut scythe_core::Track)| {
        app.edit(label, |p| {
            if let Some(t) = p.track_mut(track_id) {
                f(t);
            }
        });
    };
    ui.menu_button("Record mode", |ui| {
        for (i, mode) in RecordMode::ALL.into_iter().enumerate() {
            // REAPER's menu groups: input, output, MIDI, disable.
            if matches!(i, 5 | 12 | 16) {
                ui.separator();
            }
            if ui.radio(track.rec_mode == mode, mode.label()).clicked() {
                set(app, "Set record mode", &|t| t.rec_mode = mode);
                ui.close();
            }
        }
    });
    ui.menu_button("Output recording taps", |ui| {
        for tap in RecordTap::ALL {
            if ui.radio(track.rec_tap == tap, tap.label()).clicked() {
                set(app, "Set record output mode", &|t| t.rec_tap = tap);
                ui.close();
            }
        }
    });
    ui.separator();
    for mode in MonitorMode::ALL {
        if ui.radio(track.monitor_mode() == mode, mode.label()).clicked() {
            set(app, "Set input monitoring", &|t| t.set_monitor_mode(mode));
            ui.close();
        }
    }
    let mut items = track.monitor_items;
    if ui.checkbox(&mut items, "Monitor track media when recording").changed() {
        set(app, "Monitor track media when recording", &|t| t.monitor_items = items);
    }
    let mut auto_arm = track.auto_arm;
    if ui.checkbox(&mut auto_arm, "Automatic record-arm when track selected").changed() {
        set(app, "Automatic record-arm", &|t| t.auto_arm = auto_arm);
    }
    ui.separator();
    project_menu(app, ui);
    ui.separator();
    lanes_menu(app, ui, &track);
}

/// The project record mode and the preferences for recording into lanes.
pub fn project_menu(app: &mut ScytheApp, ui: &mut Ui) {
    let current = ProjectRecordMode::of(&app.project);
    ui.menu_button("Project record mode", |ui| {
        for mode in ProjectRecordMode::ALL {
            if ui.radio(current == mode, mode.label()).clicked() {
                app.set_record_mode(mode);
                ui.close();
            }
        }
    });
    let prefs = app.prefs.clone();
    for (bit, label) in [
        (AUTOXFADE_RECORD_LANES, "New recording adds lanes (fixed lane tracks)"),
        (AUTOXFADE_LANES_EXCLUSIVE, "New lanes play exclusively"),
        (AUTOXFADE_LOOP_TAKES, "Loop recording always adds takes"),
    ] {
        let mut on = prefs.autoxfade_bit(bit);
        if ui.checkbox(&mut on, label).changed() {
            app.change_preferences(|p| p.set_autoxfade_bit(bit, on));
        }
    }
}

fn lanes_menu(app: &mut ScytheApp, ui: &mut Ui, track: &scythe_core::Track) {
    let id = track.id;
    let mut fixed = track.fixed_lanes;
    if ui.checkbox(&mut fixed, "Fixed item lanes").changed() {
        app.edit(if fixed { "Fixed item lanes" } else { "Free item lanes" }, |p| {
            if let Some(t) = p.track_mut(id) {
                t.set_fixed_lanes(fixed);
            }
        });
    }
    if !track.fixed_lanes {
        return;
    }
    if ui.button("Add lane").clicked() {
        app.edit("Add lane", |p| {
            if let Some(t) = p.track_mut(id) {
                t.add_lane();
            }
        });
        ui.close();
    }
    match track.comp_lane() {
        None => {
            if ui
                .button("Comp into new empty lane")
                .on_hover_text("Then Ctrl+Shift+drag across a lane to swipe it into the comp")
                .clicked()
            {
                app.edit("Comp into new lane", |p| {
                    if let Some(t) = p.track_mut(id) {
                        t.create_comp_lane();
                    }
                });
                ui.close();
            }
        }
        Some(_) => {
            if ui.button("Turn off comping").clicked() {
                app.edit("Turn off comping", |p| {
                    if let Some(t) = p.track_mut(id) {
                        t.stop_comping();
                    }
                });
                ui.close();
            }
        }
    }
    ui.menu_button("Play lanes", |ui| {
        if ui.button("All lanes").clicked() {
            app.edit("Play all lanes", |p| {
                if let Some(t) = p.track_mut(id) {
                    t.lane_solo = None;
                }
            });
            ui.close();
        }
        for lane in 0..track.lane_count() {
            let name = track.lane_names.get(lane).cloned().unwrap_or_else(|| (lane + 1).to_string());
            let only = track.playing_lanes() == [lane] && track.lane_solo.is_some();
            if ui.radio(only, format!("Only lane {name}")).clicked() {
                app.edit("Play only lane", |p| {
                    if let Some(t) = p.track_mut(id) {
                        t.play_lanes([lane]);
                    }
                });
                ui.close();
            }
        }
    });
}
