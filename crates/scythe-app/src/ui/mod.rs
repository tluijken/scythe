mod arrange;
pub mod gfx_view;
mod inspector;
pub mod midi_editor;
mod mixer;
pub mod monitor;
mod nam;
mod setlist;
pub mod widgets;

use egui::{Align2, CornerRadius, Rect, Sense, Ui, pos2, vec2};
use scythe_core::tempo::format_time;

use crate::app::ScytheApp;
use crate::palette::{self, Action};
use crate::theme::{self, ACCENT, BG, PANEL, RAISED, RAISED_HI, TEXT, TEXT_DIM, TEXT_MID};
use widgets::{Icon, icon_button, label_pill, paint_icon, rail_button};

pub const GAP: f32 = 10.0;
const HEADER_H: f32 = 56.0;
const RAIL_W: f32 = 56.0;
const INSPECTOR_W: f32 = 300.0;
const MIXER_H: f32 = 236.0;
const MIDI_H: f32 = 340.0;
const SEARCH_W: f32 = 216.0;

pub fn root(app: &mut ScytheApp, ui: &mut Ui) {
    let full = ui.max_rect();
    ui.painter().rect_filled(full, CornerRadius::ZERO, BG);
    let area = full.shrink(GAP);

    let header = Rect::from_min_size(area.min, vec2(area.width(), HEADER_H));
    top_bar(app, ui, header);

    let body = Rect::from_min_max(pos2(area.left(), header.bottom() + GAP), area.max);
    let show_inspector = body.width() > 980.0;
    let rail = Rect::from_min_size(body.min, vec2(RAIL_W, 8.0 + 8.0 * 48.0 + 1.0 + 8.0));
    nav_rail(app, ui, rail);

    if app.view.setlist {
        let page = Rect::from_min_max(pos2(rail.right() + GAP, body.top()), body.max);
        setlist::show(app, ui, page);
        finish(app, ui, full);
        return;
    }

    let main_right = if show_inspector { body.right() - INSPECTOR_W - GAP } else { body.right() };
    let main = Rect::from_min_max(pos2(rail.right() + GAP, body.top()), pos2(main_right, body.bottom()));
    let editor = app.view.midi_editor;
    let dock_h = if editor.is_some() { MIDI_H } else { MIXER_H };
    let (arrange_rect, mixer_rect) = if (app.view.show_mixer || editor.is_some()) && main.height() > dock_h + 260.0 {
        let split = main.bottom() - dock_h;
        (
            Rect::from_min_max(main.min, pos2(main.right(), split - GAP)),
            Some(Rect::from_min_max(pos2(main.left(), split), main.max)),
        )
    } else {
        (main, None)
    };
    if app.view.monitor_page {
        monitor::show(app, ui, main);
    } else {
        arrange::show(app, ui, arrange_rect);
        match (mixer_rect, editor) {
            (Some(r), Some(item)) => midi_editor::show(app, ui, r, item),
            (Some(r), None) => mixer::show(app, ui, r),
            _ => {}
        }
    }
    if show_inspector {
        let r = Rect::from_min_max(pos2(body.right() - INSPECTOR_W, body.top()), body.max);
        inspector::show(app, ui, r);
    }

    finish(app, ui, full);
}

/// Windows and overlays drawn over every page.
fn finish(app: &mut ScytheApp, ui: &mut Ui, full: Rect) {
    gfx_view::show(app, ui);
    nam::show(app, ui);
    arrange::color_picker(app, ui);
    toast(app, ui, full);
    script_console(app, ui);
    monitor::wizards(app, ui);
    palette::show(app, ui);
}

fn top_bar(app: &mut ScytheApp, ui: &mut Ui, rect: Rect) {
    let painter = ui.painter().clone();
    let cy = rect.center().y;

    let title = app.project.name();
    let mut sub = app.saved_label();
    if app.media_loading() {
        sub.push_str(" · loading media…");
    }
    if let Some(busy) = &app.busy {
        sub = busy.clone();
    }
    let title_w = 52.0
        + widgets::text_width(ui, &title, theme::semibold(15.0)).max(widgets::text_width(
            ui,
            &sub,
            theme::regular(12.0),
        ));

    // Transport group: centred, but it gives up the clock details before it covers the title.
    let pos = app.position();
    let tempo = app.project.tempo.clone();
    let bb = tempo.format_bar_beat(pos);
    let time = format_time(pos);
    let bpm = format!("{} bpm", trim_float((tempo.bpm_at(pos) * 100.0).round() / 100.0));
    let (num, denom) = tempo.time_sig_at(pos);
    let sig = format!("{num}/{denom}");
    let bb_w = widgets::text_width(ui, &bb, theme::mono(20.0)).max(92.0);
    let time_w = widgets::text_width(ui, &time, theme::mono(12.0));
    let bpm_w = widgets::text_width(ui, &bpm, theme::mono(12.0)) + 20.0;
    let sig_w = widgets::text_width(ui, &sig, theme::mono(12.0)) + 20.0;
    let buttons_w = 6.0 + 40.0 * 2.0 + 52.0 + 40.0 * 2.0 + 6.0 * 4.0 + 13.0 + 16.0 + bb_w + 10.0;
    let full_w = buttons_w + time_w + 8.0 + bpm_w + sig_w + 32.0 + 4.0 * 2.0 + 10.0;
    let compact_w = buttons_w + bpm_w + 10.0;
    let room = rect.width() - title_w - GAP;
    let compact = full_w > room;
    let show_title = compact_w <= room;
    let width = if compact { compact_w } else { full_w };
    let left_limit = if show_title { rect.left() + title_w + GAP } else { rect.left() };
    // Keep the action search visible: slide the group left of centre when it would cover it.
    let right_limit = rect.right() - SEARCH_W - GAP;
    let left = (rect.center().x - width / 2.0).min(right_limit - width).max(left_limit);
    let group = Rect::from_min_size(pos2(left, cy - 28.0), vec2(width, 56.0));

    if show_title {
        let logo = Rect::from_center_size(pos2(rect.left() + 20.0, cy), vec2(40.0, 40.0));
        painter.rect_filled(logo, CornerRadius::same(12), ACCENT);
        paint_icon(ui, logo, Icon::Logo, BG, 20.0);
        painter.text(pos2(logo.right() + 12.0, cy - 9.0), Align2::LEFT_CENTER, &title, theme::semibold(15.0), TEXT);
        painter.text(pos2(logo.right() + 12.0, cy + 10.0), Align2::LEFT_CENTER, &sub, theme::regular(12.0), TEXT_DIM);
    }
    painter.rect_filled(group, CornerRadius::same(255), PANEL);
    let mut x = group.left() + 6.0;
    let mut slot = |w: f32, h: f32| {
        let r = Rect::from_min_size(pos2(x, cy - h / 2.0), vec2(w, h));
        x += w + 6.0;
        r
    };
    let transparent = egui::Color32::TRANSPARENT;
    if icon_button(ui, slot(40.0, 40.0), Icon::GoStart, transparent, TEXT_MID, 18.0, "Go to start (Home)").clicked() {
        app.go_to_start();
    }
    if icon_button(ui, slot(40.0, 40.0), Icon::Stop, transparent, TEXT_MID, 15.0, "Stop (Esc)").clicked() {
        app.stop();
    }
    let playing = app.is_playing();
    let (icon, tip) = if playing { (Icon::Pause, "Pause (Space)") } else { (Icon::Play, "Play (Space)") };
    if icon_button(ui, slot(52.0, 44.0), icon, ACCENT, BG, 18.0, tip).clicked() {
        app.toggle_play();
    }
    let rec_bg = if app.is_recording() { theme::ARM_BG } else { transparent };
    if icon_button(ui, slot(40.0, 40.0), Icon::Record, rec_bg, theme::PINK, 14.0, "Record armed tracks (Ctrl+R)")
        .clicked()
    {
        app.toggle_record();
    }
    let looping = app.project.loop_enabled;
    let (bg, fg) = if looping { (RAISED_HI, ACCENT) } else { (transparent, TEXT_MID) };
    if icon_button(ui, slot(40.0, 40.0), Icon::Loop, bg, fg, 18.0, "Toggle repeat (R)").clicked() {
        app.toggle_loop();
    }
    let sep = slot(1.0, 24.0);
    painter.rect_filled(sep.expand2(vec2(0.0, 0.0)), CornerRadius::ZERO, RAISED_HI);
    x += 8.0;
    painter.text(pos2(x, cy), Align2::LEFT_CENTER, &bb, theme::mono(20.0), TEXT);
    x += bb_w + 10.0;
    if !compact {
        painter.text(pos2(x, cy + 2.0), Align2::LEFT_CENTER, &time, theme::mono(12.0), TEXT_DIM);
        x += time_w + 8.0;
    }
    let bpm_rect = Rect::from_min_size(pos2(x, cy - 14.0), vec2(bpm_w, 28.0));
    label_pill(ui, bpm_rect, &bpm, RAISED, TEXT);
    tempo_drag(app, ui, bpm_rect);
    x += bpm_w + 4.0;
    if !compact {
        label_pill(ui, Rect::from_min_size(pos2(x, cy - 14.0), vec2(sig_w, 28.0)), &sig, RAISED, TEXT);
        x += sig_w + 4.0;
        let metro = Rect::from_min_size(pos2(x, cy - 16.0), vec2(32.0, 32.0));
        let fg = if app.metronome() { ACCENT } else { TEXT_DIM };
        let tip = "Metronome (right-click for count-in, level and output)";
        let resp = icon_button(ui, metro, Icon::Metronome, RAISED, fg, 16.0, tip);
        if resp.clicked() {
            app.toggle_metronome();
        }
        resp.context_menu(|ui| metronome_menu(app, ui));
    }

    // Status + action search, right aligned.
    let info = app.engine.info();
    let load = app.engine.transport().load();
    let rate = if info.sample_rate.is_multiple_of(1000) {
        format!("{}k", info.sample_rate / 1000)
    } else {
        format!("{:.1}k", info.sample_rate as f32 / 1000.0)
    };
    let block = info.block_size.map_or("auto".to_owned(), |b| b.to_string());
    let xruns = app.engine.health().xruns;
    let status = match xruns {
        0 => format!("{rate} · {block} · CPU {:.0}%", load * 100.0),
        n => format!("{rate} · {block} · CPU {:.0}% · {n} xruns", load * 100.0),
    };
    let ok = info.fallback_reason.is_none();
    let search_w = SEARCH_W;
    let search = Rect::from_min_size(pos2(rect.right() - search_w, cy - 20.0), vec2(search_w, 40.0));
    let status_w = widgets::text_width(ui, &status, theme::mono(11.0)) + 38.0;
    let status_rect = Rect::from_min_size(pos2(search.left() - 8.0 - status_w, cy - 16.0), vec2(status_w, 32.0));
    if status_rect.left() > group.right() + GAP {
        painter.rect_filled(status_rect, CornerRadius::same(255), PANEL);
        painter.circle_filled(
            status_rect.left_center() + vec2(15.0, 0.0),
            3.0,
            if ok { theme::GREEN } else { theme::YELLOW },
        );
        painter.text(
            status_rect.left_center() + vec2(25.0, 0.0),
            Align2::LEFT_CENTER,
            &status,
            theme::mono(11.0),
            TEXT_DIM,
        );
        let tip = match &info.fallback_reason {
            Some(r) => format!("{}\n{r}", info.name),
            None => info.name.clone(),
        };
        ui.allocate_rect(status_rect, Sense::hover()).on_hover_text(tip);
    }
    if search.left() >= group.right() + GAP - 0.5 {
        let resp = ui.allocate_rect(search, Sense::click());
        painter.rect_filled(search, CornerRadius::same(255), if resp.hovered() { RAISED } else { PANEL });
        paint_icon(
            ui,
            Rect::from_center_size(search.left_center() + vec2(22.0, 0.0), vec2(16.0, 16.0)),
            Icon::Search,
            TEXT_DIM,
            16.0,
        );
        painter.text(
            search.left_center() + vec2(40.0, 0.0),
            Align2::LEFT_CENTER,
            "Search actions",
            theme::regular(13.0),
            TEXT_DIM,
        );
        let kbd = palette::shortcut_text(ui.ctx(), Action::ShowPalette);
        let kw = widgets::text_width(ui, &kbd, theme::mono(11.0)) + 16.0;
        label_pill(
            ui,
            Rect::from_min_size(search.right_center() - vec2(kw + 6.0, 12.0), vec2(kw, 24.0)),
            &kbd,
            RAISED,
            TEXT_DIM,
        );
        if resp.clicked() {
            app.palette = Some(Default::default());
        }
    }
}

/// Dragging the tempo pill changes the project tempo, like REAPER's tempo field.
fn tempo_drag(app: &mut ScytheApp, ui: &mut Ui, rect: Rect) {
    let resp = ui
        .interact(rect, ui.scope_id().with("tempo"), Sense::click_and_drag())
        .on_hover_text("Drag to change the tempo at the cursor, double-click to reset to 120");
    let at = app.position();
    let start_id = ui.scope_id().with("tempo-start");
    if resp.drag_started() {
        let bpm = app.project.tempo.bpm_at(at);
        ui.data_mut(|d| d.insert_temp(start_id, bpm));
    }
    if resp.dragged()
        && let (Some(origin), Some(now)) = (ui.input(|i| i.pointer.press_origin()), resp.interact_pointer_pos())
    {
        let start: f64 = ui.data(|d| d.get_temp(start_id)).unwrap_or(120.0);
        let bpm = (start + (origin.y - now.y) as f64 * 0.25).round().clamp(20.0, 400.0);
        app.gesture("Change tempo", |p| p.tempo.set_bpm_at(at, bpm));
    }
    if resp.drag_stopped() {
        app.end_gesture();
    }
    if resp.double_clicked() {
        app.edit("Change tempo", |p| p.tempo.set_bpm_at(at, 120.0));
    }
}

fn trim_float(v: f64) -> String {
    if v.fract() == 0.0 { format!("{v:.0}") } else { format!("{v:.2}") }
}

fn nav_rail(app: &mut ScytheApp, ui: &mut Ui, rect: Rect) {
    ui.painter().rect_filled(rect, CornerRadius::same(18), PANEL);
    let mut y = rect.top() + 8.0;
    let mut next = |h: f32| {
        let r = Rect::from_center_size(pos2(rect.center().x, y + h / 2.0), vec2(44.0, h));
        y += h + 4.0;
        r
    };
    let monitors = app.view.monitor_page;
    let setlist = app.view.setlist;
    let arrange = !monitors && !setlist;
    if rail_button(ui, next(44.0), Icon::Arrange, arrange, ACCENT, "Arrange").clicked() {
        app.view.monitor_page = false;
        app.view.setlist = false;
    }
    let tip = format!("Setlist ({})", palette::shortcut_text(ui.ctx(), Action::ToggleSetlist));
    if rail_button(ui, next(44.0), Icon::Setlist, setlist, ACCENT, &tip).clicked() {
        app.view.setlist = !setlist;
        app.view.monitor_page = false;
    }
    let mixer_shown = app.view.show_mixer && arrange;
    if rail_button(ui, next(44.0), Icon::Mixer, mixer_shown, ACCENT, "Mixer (Ctrl+M)").clicked() {
        app.view.show_mixer = !mixer_shown;
        app.view.monitor_page = false;
        app.view.setlist = false;
    }
    if rail_button(ui, next(44.0), Icon::Headphones, monitors, ACCENT, "Monitor mixes (in-ear)").clicked() {
        app.view.monitor_page = !monitors;
        app.view.setlist = false;
    }
    if rail_button(ui, next(44.0), Icon::Folder, false, ACCENT, "Insert media files (Insert)").clicked() {
        app.import_dialog();
    }
    if rail_button(ui, next(44.0), Icon::Fx, false, ACCENT, "FX browser").clicked() {
        app.view.inspector_tab = crate::app::InspectorTab::Fx;
        app.view.plugin_browser = true;
        app.view.monitor_page = false;
        app.view.setlist = false;
    }
    if rail_button(ui, next(44.0), Icon::Routing, false, ACCENT, "Routing").clicked() {
        app.view.inspector_tab = crate::app::InspectorTab::Sends;
    }
    let sep = next(1.0);
    ui.painter().rect_filled(Rect::from_center_size(sep.center(), vec2(24.0, 1.0)), CornerRadius::ZERO, RAISED_HI);
    if rail_button(ui, next(44.0), Icon::Settings, false, ACCENT, "Settings").clicked() {
        let info = app.engine.info().clone();
        app.notify(format!("Audio: {} · {} Hz", info.name, info.sample_rate));
    }
}

/// What the last script printed, in a window until closed.
fn script_console(app: &mut ScytheApp, ui: &mut Ui) {
    let Some(text) = app.script_console.clone() else { return };
    let mut open = true;
    egui::Window::new("ReaScript console").open(&mut open).default_width(480.0).show(ui.ctx(), |ui| {
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            ui.add(egui::Label::new(egui::RichText::new(text).font(theme::mono(12.0))).wrap());
        });
    });
    if !open {
        app.script_console = None;
    }
}

fn toast(app: &mut ScytheApp, ui: &mut Ui, screen: Rect) {
    let Some(msg) = app.toast().map(str::to_owned) else { return };
    let w = widgets::text_width(ui, &msg, theme::regular(13.0)) + 32.0;
    let r = Rect::from_center_size(
        pos2(screen.center().x, screen.bottom() - 36.0),
        vec2(w.min(screen.width() - 32.0), 36.0),
    );
    ui.painter().rect_filled(r, CornerRadius::same(255), RAISED_HI);
    ui.painter().text(r.center(), Align2::CENTER_CENTER, msg, theme::regular(13.0), TEXT);
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
}

/// The metronome's settings, as in REAPER's metronome and pre-roll dialog: on or off, count-in,
/// level, and the device output it plays to (a drummer's in-ears instead of the PA).
fn metronome_menu(app: &mut ScytheApp, ui: &mut Ui) {
    use scythe_core::metronome::{COUNT_IN_PLAY, COUNT_IN_RECORD};
    use scythe_core::units::{db_to_gain, gain_to_db};
    let m = app.project.metronome.clone();
    let mut on = m.enabled();
    if ui.checkbox(&mut on, "Metronome").changed() {
        app.set_metronome(on);
    }
    let mut play = m.has(COUNT_IN_PLAY);
    if ui.checkbox(&mut play, "Count-in before playback").changed() {
        app.change_metronome(|m| m.set(COUNT_IN_PLAY, play));
    }
    let mut record = m.has(COUNT_IN_RECORD);
    if ui.checkbox(&mut record, "Count-in before recording").changed() {
        app.change_metronome(|m| m.set(COUNT_IN_RECORD, record));
    }
    ui.horizontal(|ui| {
        ui.label("Count-in length");
        let mut measures = m.count_in_measures;
        let field = egui::DragValue::new(&mut measures).range(0.25..=16.0).speed(0.05).suffix(" bars");
        if ui.add(field).changed() {
            app.change_metronome(|m| m.count_in_measures = measures);
        }
    });
    ui.horizontal(|ui| {
        ui.label("Level");
        let mut db = gain_to_db(m.volume).max(-60.0);
        if ui.add(egui::Slider::new(&mut db, -60.0..=12.0).suffix(" dB")).changed() {
            app.change_metronome(|m| m.volume = if db <= -60.0 { 0.0 } else { db_to_gain(db) });
        }
    });
    ui.separator();
    let current = m.device_outputs().first().copied();
    if ui.radio(current.is_none(), "Play with the master").clicked() {
        app.change_metronome(|m| m.outputs.clear());
        ui.close();
    }
    ui.menu_button("Play to device output", |ui| {
        for first in (0..16).step_by(2) {
            let label = format!("Out {}/{}", first + 1, first + 2);
            if ui.radio(current == Some((first, false)), label).clicked() {
                app.change_metronome(|m| m.outputs = vec![first as i64]);
                ui.close();
            }
        }
    });
}
