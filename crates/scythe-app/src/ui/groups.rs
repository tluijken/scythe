//! Track groups in the UI: the grouping matrix window, the group badge on track headers and
//! mixer strips, and the mute/solo/arm/polarity switches that carry over to group followers.

use egui::{Align2, CornerRadius, Id, Rect, Sense, Ui, vec2};
use scythe_core::groups::{GROUP_COUNT, GroupParam, GroupedControl};
use scythe_core::{SoloMode, Track, TrackId};

use crate::app::ScytheApp;
use crate::theme::{self, BG, GREEN, RAISED_HI, TEXT, TEXT_DIM, TEXT_MID};
use crate::ui::widgets::text_pill;

/// Flips a switch on a track and the followers of its groups for it.
pub fn toggle(app: &mut ScytheApp, id: TrackId, control: GroupedControl) {
    let label = match control {
        GroupedControl::Mute => "Toggle mute",
        GroupedControl::Solo => "Toggle solo",
        GroupedControl::RecArm => "Toggle record arm",
        _ => "Toggle polarity",
    };
    app.edit(label, |p| {
        let Some(i) = p.track_index(id) else { return };
        let t = &p.tracks[i];
        match control {
            GroupedControl::Mute => p.set_mute_grouped(i, !t.muted),
            GroupedControl::Solo => p.set_solo_grouped(i, !t.solo),
            GroupedControl::RecArm => p.set_arm_grouped(i, !t.armed),
            _ => p.set_phase_grouped(i, !t.phase),
        }
    });
}

/// The solo switch: click solos (in place unless the track says otherwise), right-click picks
/// the solo mode and solo defeat. A defeated track shows a ring.
pub fn solo_button(app: &mut ScytheApp, ui: &mut Ui, rect: Rect, track: &Track) {
    let (bg, fg) = if track.solo { (GREEN, BG) } else { (RAISED_HI, TEXT_MID) };
    let tip = match (track.solo_mode, track.solo_defeat) {
        (_, true) => "Solo (solo defeat: stays heard while others are soloed). Right-click for modes",
        (SoloMode::IgnoreRouting, _) => "Solo, ignoring routing. Right-click for modes",
        (SoloMode::InPlace, _) => "Solo in place. Right-click for modes",
    };
    let label = if track.solo && track.solo_mode == SoloMode::IgnoreRouting { "S!" } else { "S" };
    let resp = text_pill(ui, rect, label, theme::semibold(11.0), bg, fg).on_hover_text(tip);
    if track.solo_defeat {
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            CornerRadius::same(255),
            egui::Stroke::new(1.5, GREEN),
            egui::StrokeKind::Inside,
        );
    }
    if resp.clicked() {
        toggle(app, track.id, GroupedControl::Solo);
    }
    let id = track.id;
    resp.context_menu(|ui| {
        for (mode, name) in [(SoloMode::InPlace, "Solo in place"), (SoloMode::IgnoreRouting, "Solo ignoring routing")] {
            let on = app.project.track(id).is_some_and(|t| t.solo && t.solo_mode == mode);
            if ui.radio(on, name).clicked() {
                app.edit("Solo", |p| {
                    if let Some(i) = p.track_index(id) {
                        p.tracks[i].solo_mode = mode;
                        p.set_solo_grouped(i, true);
                    }
                });
                ui.close();
            }
        }
        ui.separator();
        let defeat = app.project.track(id).is_some_and(|t| t.solo_defeat);
        if ui
            .checkbox(&mut defeat.clone(), "Solo defeat")
            .on_hover_text("Stays heard while others are soloed")
            .clicked()
        {
            app.edit("Toggle solo defeat", |p| {
                if let Some(t) = p.track_mut(id) {
                    t.solo_defeat = !defeat;
                }
            });
            ui.close();
        }
    });
}

/// What a track does in a group, short: "VCA", "Lead", "Follow" or a mix.
fn role(t: &Track, g: usize) -> Option<&'static str> {
    let has = |p: GroupParam| t.groups.has(p, g);
    let leads = GroupParam::ALL.iter().any(|p| p.name().ends_with("_LEAD") && has(*p));
    match () {
        _ if has(GroupParam::VcaLead) => Some("VCA"),
        _ if has(GroupParam::VcaFollow) => Some("vca"),
        _ if leads => Some("Lead"),
        _ if t.groups.groups() & (1 << g) != 0 => Some("Follow"),
        _ => None,
    }
}

/// The colour of a group's badge.
fn group_color(g: usize) -> egui::Color32 {
    const COLORS: [egui::Color32; 6] =
        [theme::ORANGE, theme::GREEN, theme::PINK, theme::YELLOW, egui::Color32::from_rgb(0x66, 0xd9, 0xef), TEXT_MID];
    COLORS[g % COLORS.len()]
}

/// A badge naming the track's first group (a VCA lead marked so); click opens the matrix on
/// it. Nothing for an ungrouped track.
pub fn badge(app: &mut ScytheApp, ui: &mut Ui, rect: Rect, track: &Track) {
    let groups = track.groups.groups();
    if groups == 0 {
        return;
    }
    let first = groups.trailing_zeros() as usize;
    let more = groups.count_ones() > 1;
    let label = match role(track, first) {
        Some("VCA") => format!("V{}", first + 1),
        _ => format!("G{}{}", first + 1, if more { "+" } else { "" }),
    };
    let tip: Vec<String> = (0..GROUP_COUNT)
        .filter(|g| groups & (1 << g) != 0)
        .map(|g| {
            let name = app.project.group_name(g).map(|n| format!(" {n}")).unwrap_or_default();
            format!("Group {}{name}: {}", g + 1, role(track, g).unwrap_or(""))
        })
        .collect();
    let color = group_color(first);
    let resp = text_pill(ui, rect, &label, theme::mono(10.0), theme::with_alpha(color, 60), color)
        .on_hover_text(format!("{}\nClick for the grouping matrix", tip.join("\n")));
    if resp.clicked() {
        app.view.groups = Some(first);
    }
}

/// The parameters as matrix columns: lead and follow side by side, then the flags.
const COLUMNS: [(&str, GroupParam, Option<GroupParam>); 11] = [
    ("Volume", GroupParam::VolumeLead, Some(GroupParam::VolumeFollow)),
    ("VCA", GroupParam::VcaLead, Some(GroupParam::VcaFollow)),
    ("Pan", GroupParam::PanLead, Some(GroupParam::PanFollow)),
    ("Width", GroupParam::WidthLead, Some(GroupParam::WidthFollow)),
    ("Mute", GroupParam::MuteLead, Some(GroupParam::MuteFollow)),
    ("Solo", GroupParam::SoloLead, Some(GroupParam::SoloFollow)),
    ("Arm", GroupParam::RecArmLead, Some(GroupParam::RecArmFollow)),
    ("Polarity", GroupParam::PolarityLead, Some(GroupParam::PolarityFollow)),
    ("Auto", GroupParam::AutomodeLead, Some(GroupParam::AutomodeFollow)),
    ("Media", GroupParam::MediaEditLead, Some(GroupParam::MediaEditFollow)),
    ("Reverse", GroupParam::VolumeReverse, Some(GroupParam::PanReverse)),
];

/// Single flags after the pairs: header, parameter, cell label.
const EXTRA: [(&str, GroupParam, &str); 3] = [
    ("VCA pre-FX", GroupParam::VcaFollowPreFx, "P"),
    ("Width rev", GroupParam::WidthReverse, "W"),
    ("No lead", GroupParam::NoLeadWhenFollow, "N"),
];

/// The grouping matrix: pick a group, name it, and tick per track what it leads and follows.
pub fn show(app: &mut ScytheApp, ui: &mut Ui) {
    let Some(mut group) = app.view.groups else { return };
    let ctx = ui.ctx().clone();
    let mut open = true;
    let frame = egui::Frame::window(ui.style()).fill(theme::PANEL).corner_radius(CornerRadius::same(18));
    egui::Window::new("Track grouping")
        .id(Id::unique("group-matrix"))
        .open(&mut open)
        .frame(frame)
        .default_size(vec2(860.0, 420.0))
        .resizable(true)
        .show(&ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Group");
                egui::ComboBox::from_id_salt("group-pick")
                    .selected_text(group_title(app, group))
                    .height(320.0)
                    .show_ui(ui, |ui| {
                        for g in 0..GROUP_COUNT {
                            ui.selectable_value(&mut group, g, group_title(app, g));
                        }
                    });
                let mut name = app.project.group_name(group).unwrap_or_default().to_owned();
                let edit = ui.add(egui::TextEdit::singleline(&mut name).hint_text("Name").desired_width(160.0));
                if edit.changed() {
                    app.gesture("Rename group", |p| {
                        if name.is_empty() {
                            p.group_names.remove(&group);
                        } else {
                            p.group_names.insert(group, name.clone());
                        }
                    });
                }
                if edit.lost_focus() {
                    app.end_gesture();
                }
                if ui.button("Clear group").clicked() {
                    app.edit("Clear group", |p| {
                        for t in &mut p.tracks {
                            for q in GroupParam::ALL {
                                t.groups.set(q, group, false);
                            }
                        }
                        p.group_names.remove(&group);
                    });
                }
            });
            ui.label(
                egui::RichText::new(
                    "L leads, F follows. A lead's edits carry over to its followers; a VCA lead scales them instead. \
                     Reverse: volume (L column) and pan (F column) move the other way.",
                )
                .color(TEXT_DIM)
                .size(11.0),
            );
            ui.add_space(6.0);
            egui::ScrollArea::both().show(ui, |ui| matrix(app, ui, group));
        });
    app.view.groups = open.then_some(group);
}

fn group_title(app: &ScytheApp, g: usize) -> String {
    match app.project.group_name(g) {
        Some(n) => format!("{} · {n}", g + 1),
        None => format!("{}", g + 1),
    }
}

fn matrix(app: &mut ScytheApp, ui: &mut Ui, group: usize) {
    const NAME_W: f32 = 150.0;
    const CELL: f32 = 22.0;
    const COL_W: f32 = 2.0 * CELL + 10.0;
    let ids: Vec<TrackId> = app.project.tracks.iter().map(|t| t.id).collect();
    const EXTRA_W: f32 = 74.0;
    let width = NAME_W + COLUMNS.len() as f32 * COL_W + EXTRA.len() as f32 * EXTRA_W;
    let (rect, _) = ui.allocate_exact_size(vec2(width, (ids.len() + 1) as f32 * (CELL + 4.0) + 8.0), Sense::hover());
    let painter = ui.painter().clone();
    let mut y = rect.top() + CELL / 2.0;
    for (k, (name, _, _)) in COLUMNS.iter().enumerate() {
        let x = rect.left() + NAME_W + k as f32 * COL_W + CELL;
        painter.text(egui::pos2(x, y), Align2::CENTER_CENTER, *name, theme::semibold(11.0), TEXT_MID);
    }
    let extra_x = |k: usize| rect.left() + NAME_W + COLUMNS.len() as f32 * COL_W + (k as f32 + 0.5) * EXTRA_W;
    for (k, (name, _, _)) in EXTRA.iter().enumerate() {
        painter.text(egui::pos2(extra_x(k), y), Align2::CENTER_CENTER, *name, theme::semibold(11.0), TEXT_MID);
    }
    for id in ids {
        y += CELL + 4.0;
        let Some(t) = app.project.track(id) else { continue };
        let name = if t.name.is_empty() { "(unnamed)".to_owned() } else { t.name.clone() };
        let flags = t.groups;
        painter.circle_filled(egui::pos2(rect.left() + 6.0, y), 4.0, theme::track_color(t.color));
        painter.text(egui::pos2(rect.left() + 16.0, y), Align2::LEFT_CENTER, name, theme::regular(12.0), TEXT);
        let mut cells: Vec<(GroupParam, f32, &str)> = Vec::new();
        for (k, (_, lead, follow)) in COLUMNS.iter().enumerate() {
            let x = rect.left() + NAME_W + k as f32 * COL_W;
            let (l, f) = if *lead == GroupParam::VolumeReverse { ("V", "P") } else { ("L", "F") };
            cells.push((*lead, x, l));
            if let Some(f_param) = follow {
                cells.push((*f_param, x + CELL + 2.0, f));
            }
        }
        for (k, (_, param, label)) in EXTRA.iter().enumerate() {
            cells.push((*param, extra_x(k) - CELL / 2.0, label));
        }
        for (param, x, label) in cells {
            let on = flags.has(param, group);
            let r = Rect::from_min_size(egui::pos2(x, y - CELL / 2.0), vec2(CELL, CELL));
            let (bg, fg) = if on { (group_color(group), BG) } else { (RAISED_HI, TEXT_DIM) };
            if text_pill(ui, r, label, theme::semibold(10.0), bg, fg).on_hover_text(param.label()).clicked() {
                app.edit("Track grouping", |p| {
                    if let Some(t) = p.track_mut(id) {
                        t.groups.set(param, group, !on);
                    }
                });
            }
        }
    }
}
