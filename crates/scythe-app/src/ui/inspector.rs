use egui::{Align2, CornerRadius, Id, Rect, Sense, Ui, pos2, vec2};
use scythe_core::units::format_db;
use scythe_core::{EnvelopeKind, Send, TrackId};
use scythe_engine::fx::FxStatus;
use scythe_engine::midi_io::{ALL_MIDI_INPUTS, midi_input_channel};

use crate::app::{InspectorTab, PresetSource, ScytheApp};
use crate::theme::{self, ACCENT, BG, PANEL, RAISED, RAISED_HI, TEXT, TEXT_DIM, track_color, with_alpha};
use crate::ui::widgets::{self, Icon, switch, text_pill};

const ROW_H: f32 = 50.0;

fn track_card(app: &mut ScytheApp, ui: &mut Ui, rect: Rect) {
    let painter = ui.painter().clone();
    let Some(track_id) = app.sel.track.filter(|t| app.project.track(*t).is_some()) else {
        let r = Rect::from_min_size(rect.min, vec2(rect.width(), 120.0));
        painter.rect_filled(r, CornerRadius::same(18), PANEL);
        painter.text(r.center(), Align2::CENTER_CENTER, "Select a track to inspect it", theme::regular(13.0), TEXT_DIM);
        return;
    };
    let track = app.project.track(track_id).unwrap().clone();
    let index = app.project.track_index(track_id).unwrap();
    let color = track_color(track.color);

    let content_h = content_height(app, &track);
    let panel = Rect::from_min_size(rect.min, vec2(rect.width(), content_h.min(rect.height())));
    painter.rect_filled(panel, CornerRadius::same(18), PANEL);
    let inner = panel.shrink(16.0);
    let clip = painter.with_clip_rect(panel);

    // Identity.
    let badge = Rect::from_min_size(inner.min, vec2(44.0, 44.0));
    clip.rect_filled(badge, CornerRadius::same(12), with_alpha(color, 0x2e));
    clip.circle_filled(badge.center(), 7.0, color);
    let mut name = track.name.clone();
    let name_rect =
        Rect::from_min_size(pos2(badge.right() + 12.0, badge.top() + 2.0), vec2(inner.width() - 56.0 - 76.0, 24.0));
    // Record input: cycles through mono inputs 1-4, stereo pairs 1/2, 3/4 and all MIDI inputs.
    const INPUTS: [i64; 7] = [0, 1, 2, 3, 1024, 1026, ALL_MIDI_INPUTS];
    let input_label = match (track.rec_input, midi_input_channel(track.rec_input)) {
        (v, _) if v < 0 => "No input".to_owned(),
        (_, Some(None)) => "MIDI".to_owned(),
        (_, Some(Some(ch))) => format!("MIDI {}", ch + 1),
        (v, None) if v & 1024 != 0 => format!("In {}/{}", (v & 511) + 1, (v & 511) + 2),
        (v, None) => format!("In {}", (v & 511) + 1),
    };
    let input_pill = Rect::from_min_size(pos2(inner.right() - 70.0, badge.top() + 2.0), vec2(70.0, 24.0));
    let mon_pill = Rect::from_min_size(pos2(inner.right() - 70.0, badge.top() + 28.0), vec2(70.0, 18.0));
    let mon_fg = if track.monitor { if track.armed { theme::PINK } else { TEXT } } else { TEXT_DIM };
    let auto_pill = Rect::from_min_size(pos2(inner.right() - 150.0, badge.top() + 28.0), vec2(74.0, 18.0));
    let mode = track.auto_mode;
    let auto_fg = if mode.writes() { theme::PINK } else { TEXT_DIM };
    if text_pill(ui, auto_pill, mode.label(), theme::mono(10.0), BG, auto_fg)
        .on_hover_text(
            "Automation mode (click to change). Touch, Latch and Write record fader and pan moves while playing.",
        )
        .clicked()
    {
        let next = scythe_core::AutoMode::ALL[(mode.rpp_code() as usize + 1) % scythe_core::AutoMode::ALL.len()];
        app.edit("Automation mode", |p| p.track_mut(track_id).unwrap().auto_mode = next);
    }
    if text_pill(ui, mon_pill, if track.monitor { "Mon on" } else { "Mon off" }, theme::mono(10.0), BG, mon_fg)
        .on_hover_text("Hear the input through this track while it is armed")
        .clicked()
    {
        app.edit("Toggle input monitoring", |p| {
            let t = p.track_mut(track_id).unwrap();
            t.monitor = !t.monitor;
        });
    }
    if text_pill(ui, input_pill, &input_label, theme::mono(11.0), BG, if track.armed { theme::PINK } else { TEXT_DIM })
        .on_hover_text(
            "Record input (click to change). MIDI plays the track's instrument while armed with monitoring on",
        )
        .clicked()
    {
        let next = INPUTS[(INPUTS.iter().position(|&v| v == track.rec_input).map_or(0, |i| i + 1)) % INPUTS.len()];
        app.edit("Set record input", |p| p.track_mut(track_id).unwrap().rec_input = next);
    }
    let edit = egui::TextEdit::singleline(&mut name)
        .frame(egui::Frame::NONE)
        .font(theme::semibold(17.0))
        .hint_text("Track name");
    let resp = ui.put(name_rect, edit);
    if resp.changed() {
        app.gesture("Rename track", |p| p.track_mut(track_id).unwrap().name = name.clone());
    }
    if resp.lost_focus() {
        app.end_gesture();
    }
    let parent = app.project.folder_parents()[index].map(|p| app.project.tracks[p].name.clone());
    let sub = match parent {
        Some(p) => format!("Track {} · in folder {p}", index + 1),
        None => format!("Track {} · {} items", index + 1, track.items.len()),
    };
    let sub = widgets::truncate(ui, &sub, &theme::regular(12.0), auto_pill.left() - badge.right() - 20.0);
    clip.text(
        pos2(badge.right() + 12.0, badge.bottom() - 9.0),
        Align2::LEFT_CENTER,
        sub,
        theme::regular(12.0),
        TEXT_DIM,
    );

    // Tabs.
    let tabs = Rect::from_min_size(pos2(inner.left(), badge.bottom() + 16.0), vec2(inner.width(), 40.0));
    clip.rect_filled(tabs, CornerRadius::same(255), BG);
    let tw = (tabs.width() - 8.0 - 8.0) / 3.0;
    for (k, (tab, label)) in
        [(InspectorTab::Fx, "FX"), (InspectorTab::Sends, "Sends"), (InspectorTab::Envelopes, "Envelopes")]
            .into_iter()
            .enumerate()
    {
        let r = Rect::from_min_size(pos2(tabs.left() + 4.0 + k as f32 * (tw + 4.0), tabs.top() + 4.0), vec2(tw, 32.0));
        let active = app.view.inspector_tab == tab;
        let (bg, fg) = if active { (RAISED_HI, TEXT) } else { (egui::Color32::TRANSPARENT, TEXT_DIM) };
        if text_pill(ui, r, label, theme::semibold(12.0), bg, fg).clicked() {
            app.view.inspector_tab = tab;
        }
    }

    let mut y = tabs.bottom() + 16.0;
    match app.view.inspector_tab {
        InspectorTab::Fx => fx_tab(app, ui, &clip, inner, &mut y, track_id),
        InspectorTab::Sends => sends_tab(app, ui, &clip, inner, &mut y, track_id),
        InspectorTab::Envelopes => envelopes_tab(app, ui, &clip, inner, &mut y, track_id),
    }
    if app.view.inspector_tab != InspectorTab::Sends && !track.sends.is_empty() {
        y += 8.0;
        clip.text(pos2(inner.left(), y + 8.0), Align2::LEFT_CENTER, "Sends", theme::semibold(12.0), TEXT_DIM);
        y += 22.0;
        for s in &track.sends {
            let r = Rect::from_min_size(pos2(inner.left(), y), vec2(inner.width(), 40.0));
            clip.rect_filled(r, CornerRadius::same(12), RAISED);
            let dest = app.project.track(s.dest).map_or("?".to_owned(), |t| t.name.clone());
            clip.text(r.left_center() + vec2(12.0, 0.0), Align2::LEFT_CENTER, dest, theme::regular(13.0), TEXT);
            clip.text(
                r.right_center() - vec2(12.0, 0.0),
                Align2::RIGHT_CENTER,
                format!("{} dB", format_db(s.volume)),
                theme::mono(12.0),
                TEXT_DIM,
            );
            y += 46.0;
        }
    }
}

/// A plugin binary the browser can add.
enum Installed {
    Clap(scythe_engine::clap_host::ClapDescriptor),
    Lv2(scythe_engine::lv2::Lv2Info),
    Vst3(scythe_engine::vst3_host::Vst3Info),
}

impl Installed {
    fn label(&self) -> String {
        match self {
            Self::Clap(d) => format!("{} · {}", d.name, d.vendor),
            Self::Lv2(i) => i.name.clone(),
            Self::Vst3(i) => format!("{} · {}", i.name, i.vendor),
        }
    }

    fn format(&self) -> &'static str {
        match self {
            Self::Clap(_) => "CLAP",
            Self::Lv2(_) => "LV2",
            Self::Vst3(_) => "VST3",
        }
    }

    fn slot(&self) -> scythe_core::FxSlot {
        match self {
            Self::Clap(d) => scythe_engine::fx::clap_slot(d),
            Self::Lv2(i) => scythe_engine::fx::lv2_slot(i),
            Self::Vst3(i) => scythe_engine::fx::vst3_slot(i),
        }
    }
}

fn installed_plugins(app: &ScytheApp) -> Vec<Installed> {
    let fx = app.engine.fx();
    let mut out: Vec<Installed> = fx.clap_plugins().iter().cloned().map(Installed::Clap).collect();
    out.extend(fx.lv2_plugins().iter().cloned().map(Installed::Lv2));
    out.extend(fx.vst3_plugins().iter().cloned().map(Installed::Vst3));
    out
}

fn content_height(app: &ScytheApp, track: &scythe_core::Track) -> f32 {
    let base = 16.0 + 44.0 + 16.0 + 40.0 + 16.0 + 16.0;
    let tab = match app.view.inspector_tab {
        InspectorTab::Fx => {
            let browser = if app.view.plugin_browser {
                (scythe_engine::builtin::Builtin::ALL.len() + installed_plugins(app).len().max(1)) as f32 * 36.0 + 8.0
            } else {
                0.0
            };
            let expanded = match app.view.expanded_fx {
                Some((t, i)) if t == track.id => track
                    .fx
                    .get(i)
                    .and_then(|slot| app.engine.fx().instance(Some(t), slot, i))
                    .and_then(|inst| inst.with(|e| e.params().len()))
                    .map_or(26.0, |n| if n == 0 { 26.0 } else { n as f32 * (PARAM_ROW + 4.0) + 4.0 }),
                _ => 0.0,
            };
            track.fx.len() as f32 * (ROW_H + 6.0) + 46.0 + 20.0 + browser + expanded
        }
        InspectorTab::Sends => {
            let receives = app.project.tracks.iter().filter(|t| t.sends.iter().any(|s| s.dest == track.id)).count();
            let picking = app.ctx.data(|d| d.get_temp::<bool>(Id::unique(("pick-send", track.id)))).unwrap_or(false);
            let picker = if picking { app.project.tracks.len() as f32 * 36.0 } else { 0.0 };
            30.0 + track.sends.len() as f32 * 56.0 + 46.0 + picker + 30.0 + receives as f32 * 46.0 + 60.0
        }
        InspectorTab::Envelopes => EnvelopeKind::ALL.len() as f32 * (ROW_H + 6.0) + 30.0,
    };
    let sends = if app.view.inspector_tab != InspectorTab::Sends && !track.sends.is_empty() {
        30.0 + track.sends.len() as f32 * 46.0
    } else {
        0.0
    };
    base + tab + sends
}

fn fx_tab(app: &mut ScytheApp, ui: &mut Ui, painter: &egui::Painter, inner: Rect, y: &mut f32, track_id: TrackId) {
    let track = app.project.track(track_id).unwrap().clone();
    for (i, fx) in track.fx.iter().enumerate() {
        let r = Rect::from_min_size(pos2(inner.left(), *y), vec2(inner.width(), ROW_H));
        painter.rect_filled(r, CornerRadius::same(12), RAISED);
        let num = Rect::from_center_size(r.left_center() + vec2(25.0, 0.0), vec2(26.0, 26.0));
        painter.rect_filled(num, CornerRadius::same(8), BG);
        painter.text(num.center(), Align2::CENTER_CENTER, (i + 1).to_string(), theme::mono(11.0), TEXT_DIM);
        let (short, kind) = split_plugin_name(&fx.name);
        let name = widgets::truncate(ui, &short, &theme::semibold(13.0), r.width() - 110.0);
        painter.text(
            pos2(num.right() + 12.0, r.center().y - 8.0),
            Align2::LEFT_CENTER,
            name,
            theme::semibold(13.0),
            if fx.enabled { TEXT } else { TEXT_DIM },
        );
        let status = match app.engine.fx().status(Some(track_id), fx, i) {
            Some(FxStatus::Native(what)) => format!("{kind} · {what}"),
            Some(FxStatus::Hosted(_)) => format!("{kind} · hosted"),
            Some(FxStatus::Unavailable(why)) => format!("{kind} · bypassed: {why}"),
            None => format!("{kind} · not loaded"),
        };
        let detail = widgets::truncate(ui, &status, &theme::regular(11.0), r.width() - 110.0);
        painter.text(
            pos2(num.right() + 12.0, r.center().y + 9.0),
            Align2::LEFT_CENTER,
            detail,
            theme::regular(11.0),
            TEXT_DIM,
        );
        let sw = Rect::from_min_size(pos2(r.right() - 12.0 - 36.0, r.center().y - 11.0), vec2(36.0, 22.0));
        let row = ui.interact(r, Id::unique(("fx-row", track_id, i)), Sense::click());
        let hosted = matches!(app.engine.fx().status(Some(track_id), fx, i), Some(FxStatus::Hosted(_)))
            || app.has_gfx(Some(track_id), i);
        if hosted {
            let open = app.editor_open(Some(track_id), i);
            let pill = Rect::from_min_size(pos2(sw.left() - 52.0, r.center().y - 11.0), vec2(44.0, 22.0));
            let (bg, fg) = if open { (ACCENT, BG) } else { (BG, TEXT_DIM) };
            if text_pill(ui, pill, "Edit", theme::regular(11.0), bg, fg)
                .on_hover_text(if open { "Close the plugin's editor" } else { "Open the plugin's own editor or view" })
                .clicked()
            {
                app.toggle_editor(Some(track_id), i);
            }
        }
        row.context_menu(|ui| {
            if hosted && ui.button("Open editor").clicked() {
                app.toggle_editor(Some(track_id), i);
                ui.close();
            }
            ui.menu_button("Save preset", |ui| {
                let id = Id::unique(("fx-preset-name", track_id, i));
                let mut name =
                    ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_else(|| app.new_preset_name(track_id, i));
                let edit = ui.text_edit_singleline(&mut name);
                let entered = edit.lost_focus() && ui.input(|inp| inp.key_pressed(egui::Key::Enter));
                if ui.button("Save").clicked() || entered {
                    app.save_fx_preset(track_id, i, &name);
                    ui.data_mut(|d| d.remove::<String>(id));
                    ui.close();
                } else {
                    ui.data_mut(|d| d.insert_temp(id, name));
                }
            });
            let presets = app.fx_presets(track_id, i);
            ui.add_enabled_ui(!presets.is_empty(), |ui| {
                ui.menu_button("Load preset", |ui| {
                    for (name, source) in &presets {
                        if ui.button(name).clicked() {
                            app.load_fx_preset(track_id, i, source);
                            ui.close();
                        }
                    }
                });
            });
            let own: Vec<_> = presets
                .iter()
                .filter(|(_, s)| matches!(s, PresetSource::Reaper(..) | PresetSource::FxChain(_)))
                .collect();
            ui.add_enabled_ui(!own.is_empty(), |ui| {
                ui.menu_button("Rename or delete preset", |ui| {
                    for (n, (name, source)) in own.iter().enumerate() {
                        ui.menu_button(name.as_str(), |ui| {
                            let id = Id::unique(("fx-preset-rename", track_id, i, n));
                            let mut to = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_else(|| name.clone());
                            let edit = ui.text_edit_singleline(&mut to);
                            let entered = edit.lost_focus() && ui.input(|inp| inp.key_pressed(egui::Key::Enter));
                            if ui.button("Rename").clicked() || entered {
                                app.rename_fx_preset(source, &to);
                                ui.data_mut(|d| d.remove::<String>(id));
                                ui.close();
                            } else {
                                ui.data_mut(|d| d.insert_temp(id, to));
                            }
                            if ui.button("Delete").clicked() {
                                app.delete_fx_preset(source);
                                ui.close();
                            }
                        });
                    }
                });
            });
            if ui.button("Import preset library…").clicked() {
                app.import_fx_presets(track_id, i);
                ui.close();
            }
            if ui.button("Export preset library…").clicked() {
                app.export_fx_presets(track_id, i);
                ui.close();
            }
            if ui.button("Remove plugin").clicked() {
                app.edit("Remove plugin", |p| {
                    p.track_mut(track_id).unwrap().fx.remove(i);
                });
                ui.close();
            }
        });
        if switch(ui, sw, fx.enabled, ACCENT).on_hover_text("Bypass").clicked() {
            app.edit("Toggle FX bypass", |p| {
                let slot = &mut p.track_mut(track_id).unwrap().fx[i];
                slot.enabled = !slot.enabled;
            });
        }
        if row.clicked() {
            let key = (track_id, i);
            app.view.expanded_fx = if app.view.expanded_fx == Some(key) { None } else { Some(key) };
        }
        *y += ROW_H + 6.0;
        if app.view.expanded_fx == Some((track_id, i)) {
            *y += fx_params(app, ui, painter, inner, *y, track_id, i);
        }
    }
    let add = Rect::from_min_size(pos2(inner.left(), *y), vec2(inner.width(), 40.0));
    let resp = ui.allocate_rect(add, Sense::click());
    if resp.hovered() {
        painter.rect_filled(add, CornerRadius::same(12), RAISED);
    }
    widgets::paint_icon(
        ui,
        Rect::from_center_size(add.center() - vec2(40.0, 0.0), vec2(14.0, 14.0)),
        Icon::Plus,
        TEXT_DIM,
        14.0,
    );
    painter.text(add.center() + vec2(8.0, 0.0), Align2::CENTER_CENTER, "Add plugin", theme::semibold(12.0), TEXT_DIM);
    if resp.clicked() {
        app.view.plugin_browser = !app.view.plugin_browser;
    }
    *y += 46.0;
    if app.view.plugin_browser {
        for builtin in scythe_engine::builtin::Builtin::ALL {
            let r = Rect::from_min_size(pos2(inner.left(), *y), vec2(inner.width(), 32.0));
            let resp = ui.allocate_rect(r, Sense::click());
            painter.rect_filled(r, CornerRadius::same(10), if resp.hovered() { RAISED_HI } else { RAISED });
            painter.text(
                r.left_center() + vec2(12.0, 0.0),
                Align2::LEFT_CENTER,
                builtin.label(),
                theme::regular(12.0),
                TEXT,
            );
            painter.text(
                r.right_center() - vec2(12.0, 0.0),
                Align2::RIGHT_CENTER,
                "built in",
                theme::regular(11.0),
                TEXT_DIM,
            );
            if resp.clicked() {
                let slot = builtin.slot();
                app.edit("Add plugin", |p| p.track_mut(track_id).unwrap().fx.push(slot));
                app.view.plugin_browser = false;
            }
            *y += 36.0;
        }
        let plugins = installed_plugins(app);
        if plugins.is_empty() {
            painter.text(
                pos2(inner.left() + 4.0, *y + 14.0),
                Align2::LEFT_CENTER,
                "No CLAP, LV2 or VST3 plugins found",
                theme::regular(11.0),
                TEXT_DIM,
            );
            *y += 36.0;
        }
        for plugin in plugins {
            let r = Rect::from_min_size(pos2(inner.left(), *y), vec2(inner.width(), 32.0));
            let resp = ui.allocate_rect(r, Sense::click());
            painter.rect_filled(r, CornerRadius::same(10), if resp.hovered() { RAISED_HI } else { RAISED });
            let label = widgets::truncate(ui, &plugin.label(), &theme::regular(12.0), r.width() - 70.0);
            painter.text(r.left_center() + vec2(12.0, 0.0), Align2::LEFT_CENTER, label, theme::regular(12.0), TEXT);
            painter.text(
                r.right_center() - vec2(12.0, 0.0),
                Align2::RIGHT_CENTER,
                plugin.format(),
                theme::regular(11.0),
                TEXT_DIM,
            );
            if resp.clicked() {
                let slot = plugin.slot();
                app.edit("Add plugin", |p| p.track_mut(track_id).unwrap().fx.push(slot));
                app.view.plugin_browser = false;
            }
            *y += 36.0;
        }
        *y += 8.0;
    }
}

fn envelopes_tab(
    app: &mut ScytheApp,
    ui: &mut Ui,
    painter: &egui::Painter,
    inner: Rect,
    y: &mut f32,
    track_id: TrackId,
) {
    let track = app.project.track(track_id).unwrap().clone();
    for kind in EnvelopeKind::ALL {
        let env = track.envelope(kind);
        let r = Rect::from_min_size(pos2(inner.left(), *y), vec2(inner.width(), ROW_H));
        painter.rect_filled(r, CornerRadius::same(12), RAISED);
        let on = env.is_some_and(|e| e.active);
        painter.text(
            pos2(r.left() + 14.0, r.center().y - 8.0),
            Align2::LEFT_CENTER,
            kind.label(),
            theme::semibold(13.0),
            if on { TEXT } else { TEXT_DIM },
        );
        let detail = match env {
            Some(e) if !e.points.is_empty() => format!("{} points", e.points.len()),
            Some(_) => "no points".to_owned(),
            None => "off".to_owned(),
        };
        painter.text(
            pos2(r.left() + 14.0, r.center().y + 9.0),
            Align2::LEFT_CENTER,
            detail,
            theme::regular(11.0),
            TEXT_DIM,
        );
        let sw = Rect::from_min_size(pos2(r.right() - 12.0 - 36.0, r.center().y - 11.0), vec2(36.0, 22.0));
        if switch(ui, sw, on, ACCENT).on_hover_text("Active").clicked() {
            let cursor = app.position();
            app.edit("Toggle envelope", |p| {
                let t = p.track_mut(track_id).unwrap();
                let fresh = t.envelope(kind).is_none_or(|e| e.points.is_empty());
                // The envelope multiplies with the fader, so a new volume envelope takes over the
                // fader level to keep the track sounding the same.
                let base = match kind {
                    EnvelopeKind::Volume if fresh => std::mem::replace(&mut t.volume, 1.0),
                    _ => kind.neutral(),
                };
                let e = t.envelope_mut(kind);
                e.active = !on;
                e.visible = !on;
                if fresh {
                    e.points.push(scythe_core::EnvPoint::new(cursor.max(0.0), base));
                }
            });
        }
        if env.is_some_and(|e| !e.points.is_empty()) {
            let clear = Rect::from_min_size(pos2(sw.left() - 64.0, r.center().y - 12.0), vec2(56.0, 24.0));
            if text_pill(ui, clear, "Clear", theme::semibold(11.0), BG, TEXT_DIM).clicked() {
                app.edit("Clear envelope", |p| p.track_mut(track_id).unwrap().envelopes.retain(|e| e.kind != kind));
            }
        }
        *y += ROW_H + 6.0;
    }
    painter.text(
        pos2(inner.left(), *y + 10.0),
        Align2::LEFT_CENTER,
        "Double-click a lane to add points, drag to move, right-click to delete",
        theme::regular(11.0),
        TEXT_DIM,
    );
    *y += 30.0;
}

const PARAM_ROW: f32 = 32.0;

/// Parameters of a hosted plugin as draggable rows; returns the height used.
fn fx_params(
    app: &mut ScytheApp,
    ui: &mut Ui,
    painter: &egui::Painter,
    inner: Rect,
    y: f32,
    track_id: TrackId,
    index: usize,
) -> f32 {
    let Some(slot) = app.project.track(track_id).and_then(|t| t.fx.get(index)).cloned() else { return 0.0 };
    let Some(inst) = app.engine.fx().instance(Some(track_id), &slot, index) else {
        painter.text(
            pos2(inner.left() + 12.0, y + 10.0),
            Align2::LEFT_CENTER,
            "No parameters: not running",
            theme::regular(11.0),
            TEXT_DIM,
        );
        return 26.0;
    };
    let params = inst
        .with(|e| {
            e.params()
                .into_iter()
                .map(|p| {
                    let v = e.param(p.id);
                    (p, v)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if params.is_empty() {
        painter.text(
            pos2(inner.left() + 12.0, y + 10.0),
            Align2::LEFT_CENTER,
            "This plugin exposes no parameters",
            theme::regular(11.0),
            TEXT_DIM,
        );
        return 26.0;
    }
    let mut at = y;
    for (p, value) in params {
        let r = Rect::from_min_size(pos2(inner.left() + 16.0, at), vec2(inner.width() - 16.0, PARAM_ROW));
        let (v, text) = value.unwrap_or((p.default, String::new()));
        let name = widgets::truncate(ui, &p.name, &theme::regular(12.0), r.width() - 130.0);
        let resp = ui.interact(r, Id::unique(("fx-param", track_id, index, p.id)), Sense::click_and_drag());
        painter.rect_filled(
            r,
            CornerRadius::same(10),
            if resp.hovered() || resp.dragged() { RAISED_HI } else { RAISED },
        );
        let span = (p.max - p.min).max(1e-9);
        let frac =
            if p.log && p.min > 0.0 { (v.max(p.min) / p.min).ln() / (p.max / p.min).ln() } else { (v - p.min) / span }
                .clamp(0.0, 1.0) as f32;
        let bar = Rect::from_min_size(r.left_bottom() - vec2(0.0, 3.0), vec2(r.width() * frac, 3.0));
        painter.rect_filled(bar, CornerRadius::same(2), ACCENT);
        painter.text(r.left_center() + vec2(10.0, 0.0), Align2::LEFT_CENTER, name, theme::regular(12.0), TEXT);
        painter.text(r.right_center() - vec2(10.0, 0.0), Align2::RIGHT_CENTER, text, theme::mono(11.0), TEXT_DIM);
        if resp.dragged() {
            let step = resp.drag_delta().x as f64 / r.width() as f64;
            let nv = if p.log && p.min > 0.0 {
                (v.max(p.min).ln() + step * (p.max / p.min).ln()).exp()
            } else {
                v + step * span
            }
            .clamp(p.min, p.max);
            inst.with(|e| e.set_param(p.id, nv));
            app.engine.wake_effects();
            app.dirty = true;
        }
        if resp.double_clicked() {
            inst.with(|e| e.set_param(p.id, p.default));
            app.engine.wake_effects();
            app.dirty = true;
        }
        at += PARAM_ROW + 4.0;
    }
    at - y + 4.0
}

/// REAPER stores plugin names as `VST3: Name (Vendor)`; show the name and keep the rest as detail.
fn split_plugin_name(full: &str) -> (String, String) {
    match full.split_once(": ") {
        Some((kind, rest)) => (rest.to_owned(), kind.to_owned()),
        None => (full.to_owned(), "plugin".to_owned()),
    }
}

fn sends_tab(app: &mut ScytheApp, ui: &mut Ui, painter: &egui::Painter, inner: Rect, y: &mut f32, track_id: TrackId) {
    let track = app.project.track(track_id).unwrap().clone();
    // Hardware MIDI output: click cycles through none and every output port.
    painter.text(pos2(inner.left(), *y + 12.0), Align2::LEFT_CENTER, "MIDI out", theme::semibold(12.0), TEXT_DIM);
    let names = app.midi_output_names().to_vec();
    let label = match track.midi_out {
        None => "None".to_owned(),
        Some((dev, _)) => names.get(dev as usize).cloned().unwrap_or_else(|| format!("Device {}", dev + 1)),
    };
    let pill = Rect::from_min_size(pos2(inner.right() - 170.0, *y), vec2(170.0, 24.0));
    let label = widgets::truncate(ui, &label, &theme::mono(10.0), pill.width() - 16.0);
    if text_pill(ui, pill, &label, theme::mono(10.0), BG, if track.midi_out.is_some() { TEXT } else { TEXT_DIM })
        .on_hover_text("Where this track's MIDI goes besides its instrument (click to change)")
        .clicked()
    {
        let next = match track.midi_out {
            None if !names.is_empty() => Some((0, 0)),
            Some((dev, ch)) if (dev as usize + 1) < names.len() => Some((dev + 1, ch)),
            _ => None,
        };
        app.edit("Set MIDI hardware output", |p| p.track_mut(track_id).unwrap().midi_out = next);
    }
    *y += 36.0;
    // Track channels: click for more, right-click for fewer.
    painter.text(pos2(inner.left(), *y + 12.0), Align2::LEFT_CENTER, "Track channels", theme::semibold(12.0), TEXT_DIM);
    let pill = Rect::from_min_size(pos2(inner.right() - 70.0, *y), vec2(70.0, 24.0));
    let resp = text_pill(ui, pill, &track.channels.to_string(), theme::mono(11.0), BG, TEXT)
        .on_hover_text("Audio channels of this track (click for more, right-click for fewer)");
    const CHANNELS: [u32; 7] = [2, 4, 6, 8, 12, 16, 32];
    let at = CHANNELS.iter().position(|&c| c >= track.channels).unwrap_or(0);
    let next = if resp.clicked() {
        Some(CHANNELS[(at + 1).min(CHANNELS.len() - 1)])
    } else if resp.secondary_clicked() {
        Some(CHANNELS[at.saturating_sub(1)])
    } else {
        None
    };
    if let Some(n) = next.filter(|&n| n != track.channels) {
        app.edit("Set track channels", |p| p.track_mut(track_id).unwrap().channels = n);
    }
    *y += 36.0;
    painter.text(pos2(inner.left(), *y + 8.0), Align2::LEFT_CENTER, "Sends", theme::semibold(12.0), TEXT_DIM);
    *y += 24.0;
    for (i, s) in track.sends.iter().enumerate() {
        let r = Rect::from_min_size(pos2(inner.left(), *y), vec2(inner.width(), ROW_H));
        painter.rect_filled(r, CornerRadius::same(12), RAISED);
        let dest = app.project.track(s.dest).map_or("?".to_owned(), |t| t.name.clone());
        painter.text(
            pos2(r.left() + 12.0, r.center().y - 8.0),
            Align2::LEFT_CENTER,
            widgets::truncate(ui, &dest, &theme::regular(13.0), r.width() - 210.0),
            theme::regular(13.0),
            TEXT,
        );
        painter.text(
            pos2(r.left() + 12.0, r.center().y + 9.0),
            Align2::LEFT_CENTER,
            "drag value to adjust",
            theme::regular(10.0),
            TEXT_DIM,
        );
        // Channels: source pair → destination pair.
        let dest_channels = app.project.track(s.dest).map_or(2, |t| t.channels);
        let (src, dst) = ((s.src_channels & 1023) as u32, (s.dst_channels & 1023) as u32);
        let routing = Rect::from_min_size(pos2(r.right() - 190.0, r.center().y - 12.0), vec2(76.0, 24.0));
        let label = format!("{}/{}→{}/{}", src + 1, src + 2, dst + 1, dst + 2);
        let resp = text_pill(ui, routing, &label, theme::mono(10.0), BG, TEXT_DIM)
            .on_hover_text("Channels sent (right-click: source pair) and where they arrive (click: destination pair)");
        if resp.clicked() {
            let next = (dst + 2) % dest_channels.max(2);
            app.edit("Set send destination channels", |p| {
                p.track_mut(track_id).unwrap().sends[i].dst_channels = next as i64
            });
        }
        if resp.secondary_clicked() {
            let next = (src + 2) % track.channels.max(2);
            app.edit("Set send source channels", |p| {
                p.track_mut(track_id).unwrap().sends[i].src_channels = next as i64
            });
        }
        let val = Rect::from_min_size(pos2(r.right() - 108.0, r.center().y - 12.0), vec2(70.0, 24.0));
        widgets::label_pill(ui, val, &format!("{} dB", format_db(s.volume)), BG, if s.muted { TEXT_DIM } else { TEXT });
        let drag = ui
            .interact(val, Id::unique(("send-vol", track_id, i)), Sense::click_and_drag())
            .on_hover_text("Drag to set send level, double-click for 0 dB");
        if drag.dragged() {
            let db = scythe_core::units::gain_to_db(s.volume).max(-60.0) - drag.drag_delta().y as f64 * 0.25;
            let gain = if db <= -60.0 { 0.0 } else { scythe_core::units::db_to_gain(db.min(12.0)) };
            app.gesture("Adjust send level", |p| p.track_mut(track_id).unwrap().sends[i].volume = gain);
        }
        if drag.drag_stopped() {
            app.end_gesture();
        }
        if drag.double_clicked() {
            app.edit("Reset send level", |p| p.track_mut(track_id).unwrap().sends[i].volume = 1.0);
        }
        let del = Rect::from_min_size(pos2(r.right() - 32.0, r.center().y - 12.0), vec2(24.0, 24.0));
        if text_pill(ui, del, "×", theme::semibold(14.0), RAISED_HI, TEXT_DIM).on_hover_text("Remove send").clicked() {
            app.edit("Remove send", |p| {
                p.track_mut(track_id).unwrap().sends.remove(i);
            });
        }
        *y += ROW_H + 6.0;
    }

    let pick_id = Id::unique(("pick-send", track_id));
    let picking: bool = ui.data(|d| d.get_temp(pick_id)).unwrap_or(false);
    let add = Rect::from_min_size(pos2(inner.left(), *y), vec2(inner.width(), 40.0));
    let label = if picking { "Choose a destination" } else { "Add send" };
    if text_pill(ui, add, label, theme::semibold(12.0), if picking { RAISED_HI } else { RAISED }, TEXT).clicked() {
        ui.data_mut(|d| d.insert_temp(pick_id, !picking));
    }
    *y += 46.0;
    if picking {
        let candidates: Vec<(TrackId, String)> = app
            .project
            .tracks
            .iter()
            .filter(|t| t.id != track_id && !track.sends.iter().any(|s| s.dest == t.id))
            .map(|t| (t.id, t.name.clone()))
            .collect();
        for (dest, name) in candidates {
            let r = Rect::from_min_size(pos2(inner.left() + 8.0, *y), vec2(inner.width() - 16.0, 32.0));
            let resp = ui.allocate_rect(r, Sense::click());
            painter.rect_filled(r, CornerRadius::same(10), if resp.hovered() { RAISED_HI } else { BG });
            painter.text(r.left_center() + vec2(12.0, 0.0), Align2::LEFT_CENTER, name, theme::regular(12.0), TEXT);
            if resp.clicked() {
                app.edit("Add send", |p| p.track_mut(track_id).unwrap().sends.push(Send::new(dest)));
                ui.data_mut(|d| d.insert_temp(pick_id, false));
            }
            *y += 36.0;
        }
    }

    *y += 4.0;
    painter.text(pos2(inner.left(), *y + 8.0), Align2::LEFT_CENTER, "Receives", theme::semibold(12.0), TEXT_DIM);
    *y += 26.0;
    let receives: Vec<(String, f64)> = app
        .project
        .tracks
        .iter()
        .flat_map(|t| t.sends.iter().filter(|s| s.dest == track_id).map(move |s| (t.name.clone(), s.volume)))
        .collect();
    if receives.is_empty() {
        painter.text(pos2(inner.left(), *y + 6.0), Align2::LEFT_CENTER, "None", theme::regular(12.0), TEXT_DIM);
        *y += 20.0;
    }
    for (src, vol) in receives {
        let r = Rect::from_min_size(pos2(inner.left(), *y), vec2(inner.width(), 40.0));
        painter.rect_filled(r, CornerRadius::same(12), RAISED);
        painter.text(r.left_center() + vec2(12.0, 0.0), Align2::LEFT_CENTER, src, theme::regular(13.0), TEXT);
        painter.text(
            r.right_center() - vec2(12.0, 0.0),
            Align2::RIGHT_CENTER,
            format!("{} dB", format_db(vol)),
            theme::mono(12.0),
            TEXT_DIM,
        );
        *y += 46.0;
    }

    *y += 6.0;
    let main = Rect::from_min_size(pos2(inner.left(), *y), vec2(inner.width(), 40.0));
    painter.rect_filled(main, CornerRadius::same(12), RAISED);
    painter.text(
        main.left_center() + vec2(12.0, 0.0),
        Align2::LEFT_CENTER,
        "Send to parent / master",
        theme::regular(13.0),
        TEXT,
    );
    let sw = Rect::from_min_size(pos2(main.right() - 48.0, main.center().y - 11.0), vec2(36.0, 22.0));
    if switch(ui, sw, track.main_send, ACCENT).clicked() {
        app.edit("Toggle master/parent send", |p| p.track_mut(track_id).unwrap().main_send ^= true);
    }
    *y += 46.0;
}

const ITEM_ROW: f32 = 40.0;
const ITEM_H: f32 = 16.0 + 30.0 + 8.0 * (ITEM_ROW + 6.0) + 10.0;

pub fn show(app: &mut ScytheApp, ui: &mut Ui, rect: Rect) {
    let item = app.sel.item.filter(|id| app.project.find_item(*id).is_some());
    let mut top = rect.top();
    if let Some(id) = item
        && rect.height() > ITEM_H + 200.0
    {
        item_card(app, ui, Rect::from_min_size(rect.min, vec2(rect.width(), ITEM_H)), id);
        top += ITEM_H + 10.0;
    }
    track_card(app, ui, Rect::from_min_max(pos2(rect.left(), top), rect.max));
}

/// A row with a label and a value pill that changes the value when dragged horizontally.
/// Returns the drag distance in points while dragging.
fn drag_row(
    ui: &mut Ui,
    painter: &egui::Painter,
    r: Rect,
    label: &str,
    value: &str,
    hint: &str,
) -> (f32, egui::Response) {
    painter.rect_filled(r, CornerRadius::same(12), RAISED);
    painter.text(r.left_center() + vec2(12.0, 0.0), Align2::LEFT_CENTER, label, theme::regular(13.0), TEXT);
    let pill = Rect::from_min_size(pos2(r.right() - 110.0, r.center().y - 12.0), vec2(100.0, 24.0));
    let resp = ui.interact(pill, Id::unique(("drag-row", label)), Sense::click_and_drag()).on_hover_text(hint);
    painter.rect_filled(pill, CornerRadius::same(255), if resp.hovered() || resp.dragged() { RAISED_HI } else { BG });
    painter.text(pill.center(), Align2::CENTER_CENTER, value, theme::mono(12.0), TEXT);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    (if resp.dragged() { resp.drag_delta().x } else { 0.0 }, resp)
}

const FADE_SHAPES: [&str; 7] = ["linear", "fast start", "curve", "steep start", "steep end", "S-curve", "steep S"];

fn item_card(app: &mut ScytheApp, ui: &mut Ui, rect: Rect, id: scythe_core::ItemId) {
    let painter = ui.painter().clone();
    painter.rect_filled(rect, CornerRadius::same(18), PANEL);
    let inner = rect.shrink(16.0);
    let Some((_, item)) = app.project.find_item(id) else { return };
    let item = item.clone();
    let title = widgets::truncate(ui, &format!("Item · {}", item.name()), &theme::semibold(15.0), inner.width() - 70.0);
    painter.text(inner.left_top() + vec2(0.0, 12.0), Align2::LEFT_CENTER, title, theme::semibold(15.0), TEXT);
    if item.takes.len() > 1 {
        painter.text(
            pos2(inner.right(), inner.top() + 12.0),
            Align2::RIGHT_CENTER,
            format!("take {}/{}", item.active_take + 1, item.takes.len()),
            theme::mono(11.0),
            TEXT_DIM,
        );
    }
    let mut y = inner.top() + 30.0;
    let mut row = || {
        let r = Rect::from_min_size(pos2(inner.left(), y), vec2(inner.width(), ITEM_ROW));
        y += ITEM_ROW + 6.0;
        r
    };
    let edit_item = |app: &mut ScytheApp, label: &str, f: &dyn Fn(&mut scythe_core::Item)| {
        app.gesture(label, |p| {
            if let Some(it) = p.tracks.iter_mut().find_map(|t| t.item_mut(id)) {
                f(it);
            }
        });
    };

    let (dx, resp) = drag_row(
        ui,
        &painter,
        row(),
        "Volume",
        &format!("{} dB", format_db(item.volume)),
        "Drag to change, double-click for 0 dB",
    );
    if dx != 0.0 {
        let db = (scythe_core::units::gain_to_db(item.volume).max(-60.0) + dx as f64 * 0.1).clamp(-60.0, 24.0);
        edit_item(app, "Item volume", &|it| it.volume = scythe_core::units::db_to_gain(db));
    }
    if resp.double_clicked() {
        edit_item(app, "Item volume", &|it| it.volume = 1.0);
    }
    if resp.drag_stopped() || resp.double_clicked() {
        app.end_gesture();
    }

    for (fade_out, label) in [(false, "Fade in"), (true, "Fade out")] {
        let fade = if fade_out { item.fade_out } else { item.fade_in };
        let r = row();
        let (dx, resp) =
            drag_row(ui, &painter, r, label, &format!("{:.3} s", fade.length), "Drag to change the length");
        if dx != 0.0 {
            let len = (fade.length + dx as f64 * 0.005).clamp(0.0, item.length);
            edit_item(app, label, &|it| if fade_out { it.fade_out.length = len } else { it.fade_in.length = len });
        }
        if resp.drag_stopped() {
            app.end_gesture();
        }
        let shape =
            Rect::from_min_size(pos2(r.left() + 84.0, r.center().y - 12.0), vec2(r.width() - 84.0 - 120.0, 24.0));
        let shape_name = FADE_SHAPES.get(fade.shape as usize).copied().unwrap_or("custom");
        if text_pill(ui, shape, shape_name, theme::regular(11.0), BG, TEXT_DIM)
            .on_hover_text("Click for the next shape")
            .clicked()
        {
            let next = (fade.shape + 1) % 7;
            app.edit("Fade shape", |p| {
                if let Some(it) = p.tracks.iter_mut().find_map(|t| t.item_mut(id)) {
                    if fade_out { it.fade_out.shape = next } else { it.fade_in.shape = next }
                }
            });
        }
    }

    let rate = item.take().map_or(1.0, |t| t.playrate);
    let (dx, resp) =
        drag_row(ui, &painter, row(), "Play rate", &format!("{rate:.3}×"), "Drag to change, double-click for 1×");
    if dx != 0.0 {
        let new = (rate * (1.0 + dx as f64 * 0.002)).clamp(0.1, 10.0);
        edit_item(app, "Play rate", &|it| {
            let active = it.active_take;
            if let Some(t) = it.takes.get_mut(active) {
                t.playrate = new;
            }
        });
    }
    if resp.double_clicked() {
        edit_item(app, "Play rate", &|it| {
            let active = it.active_take;
            if let Some(t) = it.takes.get_mut(active) {
                t.playrate = 1.0;
            }
        });
    }
    if resp.drag_stopped() || resp.double_clicked() {
        app.end_gesture();
    }

    let edit_take = |app: &mut ScytheApp, label: &str, f: &dyn Fn(&mut scythe_core::Take)| {
        edit_item(app, label, &|it| {
            let active = it.active_take;
            if let Some(t) = it.takes.get_mut(active) {
                f(t);
            }
        });
    };
    let pitch = item.take().map_or(0.0, |t| t.pitch);
    let (dx, resp) = drag_row(
        ui,
        &painter,
        row(),
        "Pitch",
        &format!("{pitch:+.2} st"),
        "Drag to shift in semitones, double-click for none",
    );
    if dx != 0.0 {
        let new = (pitch + dx as f64 * 0.02).clamp(-24.0, 24.0);
        edit_take(app, "Pitch", &|t| t.pitch = new);
    }
    if resp.double_clicked() {
        edit_take(app, "Pitch", &|t| t.pitch = 0.0);
    }
    if resp.drag_stopped() || resp.double_clicked() {
        app.end_gesture();
    }

    let preserve = item.take().is_none_or(|t| t.preserve_pitch);
    for (label, on, toggle) in [("Keep pitch", preserve, 2), ("Loop source", item.looped, 0), ("Mute", item.muted, 1)] {
        let r = row();
        painter.rect_filled(r, CornerRadius::same(12), RAISED);
        painter.text(r.left_center() + vec2(12.0, 0.0), Align2::LEFT_CENTER, label, theme::regular(13.0), TEXT);
        let sw = Rect::from_min_size(pos2(r.right() - 12.0 - 36.0, r.center().y - 11.0), vec2(36.0, 22.0));
        if switch(ui, sw, on, ACCENT).clicked() {
            app.edit(label, |p| {
                if let Some(it) = p.tracks.iter_mut().find_map(|t| t.item_mut(id)) {
                    match toggle {
                        0 => it.looped = !it.looped,
                        1 => it.muted = !it.muted,
                        _ => {
                            let active = it.active_take;
                            if let Some(t) = it.takes.get_mut(active) {
                                t.preserve_pitch = !t.preserve_pitch;
                            }
                        }
                    }
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_names_split_on_type_prefix() {
        assert_eq!(split_plugin_name("VST3: Curve 3 (AcmeAudio)"), ("Curve 3 (AcmeAudio)".into(), "VST3".into()));
        assert_eq!(split_plugin_name("EQ"), ("EQ".into(), "plugin".into()));
    }
}
