//! The Neural Amp Modeler panel: the slot's model with what is known about it, its levels,
//! snapshots to switch between, and a browser over the model library and TONE3000.

use std::path::PathBuf;

use egui::{Align2, Color32, CornerRadius, Id, Rect, Sense, Ui, pos2, vec2};
use scythe_engine::fx::nam_settings;

use crate::app::ScytheApp;
use crate::nam::library::{Entry, Kind};
use crate::nam::snapshots::SNAPSHOTS;
use crate::nam::tone3000::{Model, Prompt, Tone, ToneList, open_browser};
use crate::nam::{BrowserTab, Remote, Target, is_model_file};
use crate::theme::{self, ACCENT, BG, RAISED, RAISED_HI, TEXT, TEXT_DIM, TEXT_MID};
use crate::ui::widgets::{self, Icon, icon_button, knob, paint_star, text_pill};

const ROW: f32 = 44.0;
const LEVEL_RANGE: (f64, f64) = (-20.0, 20.0);
const SNAPSHOT_NAMES: [&str; SNAPSHOTS] = ["A", "B", "C", "D"];

pub fn show(app: &mut ScytheApp, ui: &mut Ui) {
    let Some(target) = app.nam.panel.clone() else {
        app.nam.panel_rect = None;
        return;
    };
    if app.nam_index(&target).is_none() {
        app.nam.panel = None;
        app.nam.panel_rect = None;
        return;
    }
    let ctx = ui.ctx().clone();
    take_dropped_files(app, &ctx, &target);
    let track = app.project.track(target.track).map(|t| t.name.clone()).unwrap_or_default();
    let mut open = true;
    let frame = egui::Frame::window(ui.style()).fill(theme::PANEL).corner_radius(CornerRadius::same(18));
    let shown = egui::Window::new(format!("Neural Amp Modeler · {track}"))
        .id(Id::unique("nam-panel"))
        .default_pos(ui.max_rect().center() - vec2(380.0, 310.0))
        .open(&mut open)
        .frame(frame)
        .default_size(vec2(760.0, 620.0))
        .min_width(560.0)
        .min_height(420.0)
        .resizable(true)
        .show(&ctx, |ui| body(app, ui, &target));
    app.nam.panel_rect = shown.map(|s| s.response.rect);
    if !open {
        app.nam.panel = None;
    }
}

/// Model files dropped on the panel load into its slot.
fn take_dropped_files(app: &mut ScytheApp, ctx: &egui::Context, target: &Target) {
    let Some(rect) = app.nam.panel_rect else { return };
    let (files, pos) = ctx.input(|i| {
        (
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_owned())
                .filter(|p| !p.as_os_str().is_empty())
                .collect::<Vec<_>>(),
            i.pointer.hover_pos(),
        )
    });
    if files.is_empty() || !pos.is_some_and(|p| rect.contains(p)) {
        return;
    }
    if let Some(path) =
        files.into_iter().find(|p| is_model_file(p) || p.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav")))
    {
        app.nam_open(target, path);
    }
}

fn body(app: &mut ScytheApp, ui: &mut Ui, target: &Target) {
    header(app, ui, target);
    ui.add_space(8.0);
    controls(app, ui, target);
    ui.add_space(10.0);
    tabs(app, ui);
    ui.add_space(8.0);
    match app.nam.tab {
        BrowserTab::Library => library_tab(app, ui, target),
        BrowserTab::Recent => recent_tab(app, ui, target),
        BrowserTab::Tone3000 => tone3000_tab(app, ui, target),
    }
}

fn header(app: &mut ScytheApp, ui: &mut Ui, target: &Target) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 78.0), Sense::hover());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, CornerRadius::same(14), RAISED);
    let path = app.nam_model_path(target);
    let entry = path.as_ref().and_then(|p| app.nam.library.entry(p)).cloned();
    let loading = app.nam.loading.get(target).cloned();
    let name = match (&entry, &path) {
        (Some(e), _) => e.name.clone(),
        (None, Some(p)) => p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        (None, None) => "No model loaded".to_owned(),
    };
    let status =
        app.engine.fx().status(Some(target.track), app.nam_slot(target).unwrap(), app.nam_index(target).unwrap());
    let summary = match (&loading, status, &entry) {
        (Some(p), _, _) => format!("Loading {}…", p.file_name().map(|f| f.to_string_lossy()).unwrap_or_default()),
        (_, Some(scythe_engine::fx::FxStatus::Unavailable(why)), _) => format!("Not running: {why}"),
        (_, _, Some(e)) if !e.summary().is_empty() => e.summary(),
        (_, _, Some(_)) => "No gear details in the model file".to_owned(),
        (_, _, _) if path.is_some() => "Not in the library (add its folder to see what it is)".to_owned(),
        _ => "Pick a model below, drop a .nam file here, or open one".to_owned(),
    };
    let left = rect.left() + 16.0;
    let star = Rect::from_center_size(pos2(rect.right() - 26.0, rect.top() + 24.0), vec2(26.0, 26.0));
    let title = widgets::truncate(ui, &name, &theme::semibold(18.0), star.left() - left - 10.0);
    painter.text(pos2(left, rect.top() + 22.0), Align2::LEFT_CENTER, title, theme::semibold(18.0), TEXT);
    let summary = widgets::truncate(ui, &summary, &theme::regular(12.0), rect.width() - 40.0);
    painter.text(pos2(left, rect.top() + 44.0), Align2::LEFT_CENTER, summary, theme::regular(12.0), TEXT_DIM);
    let mut x = left;
    let chips: Vec<String> = entry
        .as_ref()
        .map(|e| {
            let mut c = Vec::new();
            if let Some(a) = &e.architecture {
                c.push(a.clone());
            }
            if let Some(r) = e.sample_rate {
                c.push(format!("{} kHz", trim(r / 1000.0)));
            }
            if let Some(o) = &e.origin {
                c.push(format!("TONE3000 · {}", o.tone_title));
            } else if !e.location().is_empty() {
                c.push(e.location());
            }
            c
        })
        .unwrap_or_default();
    for chip in chips {
        let chip = widgets::truncate(ui, &chip, &theme::mono(10.0), 260.0);
        let w = widgets::text_width(ui, &chip, theme::mono(10.0)) + 16.0;
        if x + w > rect.right() - 140.0 {
            break;
        }
        let r = Rect::from_min_size(pos2(x, rect.bottom() - 22.0), vec2(w, 16.0));
        painter.rect_filled(r, CornerRadius::same(8), BG);
        painter.text(r.center(), Align2::CENTER_CENTER, chip, theme::mono(10.0), TEXT_MID);
        x += w + 6.0;
    }
    if let Some(e) = &entry {
        let resp = ui.interact(star, Id::unique(("nam-fav", &e.path)), Sense::click()).on_hover_text("Favourite");
        paint_star(ui, star, if e.favourite { theme::YELLOW } else { TEXT_DIM }, e.favourite);
        if resp.clicked() {
            app.nam.library.set_favourite(&e.path, !e.favourite);
            app.nam.library_dirty = true;
        }
        if let Some(url) = e.origin.as_ref().and_then(|o| o.url.clone()) {
            let link = Rect::from_min_size(pos2(rect.right() - 128.0, rect.top() + 36.0), vec2(116.0, 18.0));
            if text_pill(ui, link, "Open on TONE3000", theme::regular(10.0), BG, TEXT_DIM).clicked() {
                let _ = open_browser(&url);
            }
        }
        if let Some(tone_id) = e.origin.as_ref().map(|o| o.tone_id) {
            let more = Rect::from_min_size(pos2(rect.right() - 128.0, rect.top() + 56.0), vec2(116.0, 18.0));
            if text_pill(ui, more, "Other captures", theme::regular(10.0), BG, TEXT_DIM)
                .on_hover_text("This tone's other models, through TONE3000 (sign-in in the browser)")
                .clicked()
                && let Err(err) = app.nam.sign_in(Prompt::LoadTone(tone_id))
            {
                app.notify(err.to_string());
            }
        }
    }
    end_block(ui, rect);
}

/// Puts the layout cursor below `rect` again: the buttons inside it are placed by rect, and
/// allocating those moves the cursor back up to their bottom, under the next block.
fn end_block(ui: &mut Ui, rect: Rect) {
    ui.advance_cursor_after_rect(rect);
}

fn trim(v: f64) -> String {
    // Adding zero turns a rounded -0.0 into 0.0.
    let s = format!("{:.1}", (v * 10.0).round() / 10.0 + 0.0);
    s.trim_end_matches(".0").to_owned()
}

fn controls(app: &mut ScytheApp, ui: &mut Ui, target: &Target) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 92.0), Sense::hover());
    let Some(settings) = app.nam_slot(target).and_then(nam_settings) else { return };
    let mut x = rect.left();
    for (k, label) in ["Input", "Output"].into_iter().enumerate() {
        let value = if k == 0 { settings.input_db } else { settings.output_db };
        let r = Rect::from_min_size(pos2(x, rect.top()), vec2(72.0, 92.0));
        let text = format!("{}{} dB", if value > 0.0 { "+" } else { "" }, trim(value));
        let (resp, changed) = knob(ui, r, Id::unique(("nam-knob", target, k)), value, LEVEL_RANGE, 0.0, label, &text);
        let resp = resp.on_hover_text("Drag to change, Shift for fine steps, double-click for 0 dB");
        if let Some(v) = changed {
            let (i, o) = if k == 0 { (v, settings.output_db) } else { (settings.input_db, v) };
            app.nam_set_levels(target, i, o, resp.dragged());
        }
        if resp.drag_stopped() {
            app.end_gesture();
        }
        x += 80.0;
    }
    x += 12.0;
    let cy = rect.top() + 34.0;
    if icon_button(
        ui,
        Rect::from_min_size(pos2(x, cy - 18.0), vec2(36.0, 36.0)),
        Icon::Previous,
        RAISED,
        TEXT,
        16.0,
        "Previous model (Alt+Left)",
    )
    .clicked()
    {
        app.nam_step(-1);
    }
    x += 42.0;
    if icon_button(
        ui,
        Rect::from_min_size(pos2(x, cy - 18.0), vec2(36.0, 36.0)),
        Icon::Next,
        RAISED,
        TEXT,
        16.0,
        "Next model (Alt+Right)",
    )
    .clicked()
    {
        app.nam_step(1);
    }
    x += 46.0;
    let open = Rect::from_min_size(pos2(x, cy - 14.0), vec2(72.0, 28.0));
    if text_pill(ui, open, "Open…", theme::semibold(12.0), RAISED, TEXT)
        .on_hover_text("Load a .nam model or an impulse response")
        .clicked()
    {
        let dir = app.nam.library.folders.first().cloned();
        let mut dialog = rfd::FileDialog::new().add_filter("NAM model or impulse response", &["nam", "wav"]);
        if let Some(d) = dir {
            dialog = dialog.set_directory(d);
        }
        if let Some(path) = dialog.pick_file() {
            app.nam_open(target, path);
        }
    }
    // Snapshots, right-aligned.
    let extras = app.nam_snapshots(target);
    let size = 40.0;
    let mut sx = rect.right() - SNAPSHOTS as f32 * (size + 6.0);
    ui.painter().text(pos2(sx, rect.top() + 4.0), Align2::LEFT_CENTER, "Snapshots", theme::regular(11.0), TEXT_DIM);
    for (n, name) in SNAPSHOT_NAMES.iter().enumerate() {
        let r = Rect::from_min_size(pos2(sx, cy - size / 2.0 + 4.0), vec2(size, size));
        let snap = extras.get(n);
        let active = snap.is_some_and(|s| {
            s.model == settings.model
                && (s.input_db - settings.input_db).abs() < 1e-6
                && (s.output_db - settings.output_db).abs() < 1e-6
        });
        let (bg, fg) = match (snap, active) {
            (_, true) => (ACCENT, BG),
            (Some(_), false) => (RAISED_HI, TEXT),
            (None, false) => (BG, TEXT_DIM),
        };
        let tip = match snap {
            Some(s) => format!(
                "{}: {} ({} / {} dB)\nClick to switch (Alt+{}), Shift+click to store the current model, right-click for more",
                name,
                s.model
                    .as_deref()
                    .and_then(|m| std::path::Path::new(m).file_stem())
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "no model".into()),
                trim(s.input_db),
                trim(s.output_db),
                n + 1
            ),
            None => format!("{name}: empty. Click to store the current model and levels"),
        };
        let resp = text_pill(ui, r, name, theme::semibold(14.0), bg, fg).on_hover_text(tip);
        let shift = ui.input(|i| i.modifiers.shift);
        if resp.clicked() {
            if snap.is_none() || shift {
                app.nam_store_snapshot(target, n);
            } else {
                app.nam_recall_snapshot(target, n);
            }
        }
        resp.context_menu(|ui| {
            if ui.button("Store current model and levels").clicked() {
                app.nam_store_snapshot(target, n);
                ui.close();
            }
            if ui.add_enabled(snap.is_some(), egui::Button::new("Clear")).clicked() {
                app.nam_clear_snapshot(target, n);
                ui.close();
            }
        });
        sx += size + 6.0;
    }
    end_block(ui, rect);
}

fn tabs(app: &mut ScytheApp, ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(255), BG);
    let tw = ((rect.width() - 8.0) / 3.0 - 4.0).min(160.0);
    for (k, (tab, label)) in
        [(BrowserTab::Library, "Library"), (BrowserTab::Recent, "Recent"), (BrowserTab::Tone3000, "TONE3000")]
            .into_iter()
            .enumerate()
    {
        let r = Rect::from_min_size(pos2(rect.left() + 4.0 + k as f32 * (tw + 4.0), rect.top() + 4.0), vec2(tw, 28.0));
        let active = app.nam.tab == tab;
        let (bg, fg) = if active { (RAISED_HI, TEXT) } else { (Color32::TRANSPARENT, TEXT_DIM) };
        if text_pill(ui, r, label, theme::semibold(12.0), bg, fg).clicked() {
            app.nam.tab = tab;
        }
    }
    if app.nam.scanning {
        ui.painter().text(
            rect.right_center() - vec2(14.0, 0.0),
            Align2::RIGHT_CENTER,
            "Reading library…",
            theme::regular(11.0),
            TEXT_DIM,
        );
    }
    end_block(ui, rect);
}

fn library_tab(app: &mut ScytheApp, ui: &mut Ui, target: &Target) {
    ui.horizontal(|ui| {
        let search = egui::TextEdit::singleline(&mut app.nam.filter.text)
            .hint_text("Search name, gear, author, folder, tag")
            .desired_width((ui.available_width() - 330.0).max(140.0));
        ui.add(search);
        for (label, kind) in [("All", None), ("Models", Some(Kind::Model)), ("IRs", Some(Kind::Ir))] {
            if ui.selectable_label(app.nam.filter.kind == kind, label).clicked() {
                app.nam.filter.kind = kind;
            }
        }
        if ui.selectable_label(app.nam.filter.favourites, "Favourites").clicked() {
            app.nam.filter.favourites ^= true;
        }
        let tags = app.nam.library.tags();
        if !tags.is_empty() {
            egui::ComboBox::from_id_salt("nam-tag")
                .width(90.0)
                .selected_text(app.nam.filter.tag.clone().unwrap_or_else(|| "Any tag".into()))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut app.nam.filter.tag, None, "Any tag");
                    for t in tags {
                        ui.selectable_value(&mut app.nam.filter.tag, Some(t.clone()), t);
                    }
                });
        }
        ui.menu_button("Folders", |ui| {
            for folder in app.nam.library.folders.clone() {
                ui.horizontal(|ui| {
                    ui.label(folder.display().to_string());
                    if ui.small_button("Remove").clicked() {
                        app.nam.library.remove_folder(&folder);
                        app.nam.library_dirty = true;
                    }
                });
            }
            if ui.button("Add folder…").clicked() {
                if let Some(dir) = rfd::FileDialog::new().pick_folder()
                    && app.nam.library.add_folder(dir)
                {
                    app.nam.rescan();
                }
                ui.close();
            }
            if ui.button("Read folders again").clicked() {
                app.nam.rescan();
                ui.close();
            }
        });
    });
    ui.add_space(6.0);
    let entries: Vec<Entry> = app.nam.library.search(&app.nam.filter).into_iter().cloned().collect();
    if entries.is_empty() {
        let text = if app.nam.library.entries().is_empty() {
            "The library is empty. Add a folder of .nam models (Folders → Add folder…) or download from TONE3000."
        } else {
            "Nothing matches."
        };
        ui.label(egui::RichText::new(text).color(TEXT_DIM));
        return;
    }
    entry_list(app, ui, target, &entries, "nam-lib");
}

fn recent_tab(app: &mut ScytheApp, ui: &mut Ui, target: &Target) {
    let entries: Vec<Entry> =
        app.nam.library.recent().iter().filter_map(|p| app.nam.library.entry(p).cloned()).collect();
    if entries.is_empty() {
        ui.label(egui::RichText::new("Models you load show up here.").color(TEXT_DIM));
        return;
    }
    entry_list(app, ui, target, &entries, "nam-recent");
}

fn entry_list(app: &mut ScytheApp, ui: &mut Ui, target: &Target, entries: &[Entry], salt: &str) {
    let current = app.nam_model_path(target);
    let loading = app.nam.loading.get(target).cloned();
    egui::ScrollArea::vertical().id_salt(salt).auto_shrink([false, false]).show_rows(
        ui,
        ROW,
        entries.len(),
        |ui, range| {
            for e in &entries[range] {
                entry_row(
                    app,
                    ui,
                    target,
                    e,
                    current.as_deref() == Some(e.path.as_path()),
                    loading.as_ref() == Some(&e.path),
                );
            }
        },
    );
}

fn entry_row(app: &mut ScytheApp, ui: &mut Ui, target: &Target, e: &Entry, current: bool, loading: bool) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW - 4.0), Sense::click());
    ui.add_space(4.0);
    let painter = ui.painter().clone();
    let bg = if current {
        RAISED_HI
    } else if resp.hovered() {
        RAISED
    } else {
        Color32::TRANSPARENT
    };
    painter.rect_filled(rect, CornerRadius::same(10), bg);
    if current {
        painter.rect_filled(
            Rect::from_min_size(rect.min + vec2(0.0, 8.0), vec2(3.0, rect.height() - 16.0)),
            CornerRadius::same(2),
            ACCENT,
        );
    }
    let star = Rect::from_center_size(pos2(rect.left() + 20.0, rect.center().y), vec2(18.0, 18.0));
    let star_resp = ui.interact(star, Id::unique(("nam-row-fav", &e.path)), Sense::click());
    paint_star(ui, star, if e.favourite { theme::YELLOW } else { theme::SWITCH_OFF }, e.favourite);
    if star_resp.clicked() {
        app.nam.library.set_favourite(&e.path, !e.favourite);
        app.nam.library_dirty = true;
    }
    let left = rect.left() + 38.0;
    let right_text = match e.kind {
        Kind::Ir => "IR".to_owned(),
        Kind::Model => e.location(),
    };
    let right_w = widgets::text_width(ui, &right_text, theme::regular(11.0)).min(rect.width() * 0.3);
    let name_w = rect.width() - 50.0 - right_w - 16.0;
    let fg = if e.error.is_some() { TEXT_DIM } else { TEXT };
    let mut name = e.name.clone();
    if loading {
        name.push_str("  (loading)");
    }
    painter.text(
        pos2(left, rect.center().y - 8.0),
        Align2::LEFT_CENTER,
        widgets::truncate(ui, &name, &theme::semibold(13.0), name_w),
        theme::semibold(13.0),
        fg,
    );
    let mut detail = e.summary();
    if !e.tags.is_empty() {
        detail = format!("{detail}  #{}", e.tags.join(" #"));
    }
    if let Some(err) = &e.error {
        detail = format!("unreadable: {err}");
    }
    painter.text(
        pos2(left, rect.center().y + 9.0),
        Align2::LEFT_CENTER,
        widgets::truncate(ui, &detail, &theme::regular(11.0), name_w),
        theme::regular(11.0),
        TEXT_DIM,
    );
    let right_text = widgets::truncate(ui, &right_text, &theme::regular(11.0), right_w);
    painter.text(
        rect.right_center() - vec2(12.0, 0.0),
        Align2::RIGHT_CENTER,
        right_text,
        theme::regular(11.0),
        TEXT_DIM,
    );
    let resp = resp.on_hover_text(e.path.display().to_string());
    if resp.clicked() && !star_resp.clicked() && e.error.is_none() {
        app.nam_open(target, e.path.clone());
    }
    let path = e.path.clone();
    let tags = e.tags.clone();
    resp.context_menu(|ui| {
        let label = if e.kind == Kind::Ir { "Load as cabinet IR" } else { "Load" };
        if ui.button(label).clicked() {
            app.nam_open(target, path.clone());
            ui.close();
        }
        if ui.button(if e.favourite { "Remove from favourites" } else { "Add to favourites" }).clicked() {
            app.nam.library.set_favourite(&path, !e.favourite);
            app.nam.library_dirty = true;
            ui.close();
        }
        ui.menu_button("Tags", |ui| {
            let id = Id::unique(("nam-tags", &path));
            let mut text = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_else(|| tags.join(", "));
            let edit = ui.add(egui::TextEdit::singleline(&mut text).hint_text("comma, separated"));
            let entered = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button("Set tags").clicked() || entered {
                app.nam.library.set_tags(&path, text.split(',').map(str::to_owned).collect());
                app.nam.library_dirty = true;
                ui.data_mut(|d| d.remove::<String>(id));
                ui.close();
            } else {
                ui.data_mut(|d| d.insert_temp(id, text));
            }
        });
        if ui.button("Show in folder").clicked() {
            if let Some(dir) = path.parent() {
                let _ = open_browser(&dir.to_string_lossy());
            }
            ui.close();
        }
    });
}

fn tone3000_tab(app: &mut ScytheApp, ui: &mut Ui, target: &Target) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("TONE3000").font(theme::semibold(16.0)).color(TEXT));
        ui.label(egui::RichText::new("amp and pedal captures shared by the community").color(TEXT_DIM));
    });
    ui.add_space(6.0);
    if !app.nam.has_client_id() {
        ui.label(
            "Scythe needs a TONE3000 publishable key to sign in. Create one at tone3000.com → Settings → \
             API keys (see docs/tone3000.md), then paste it here.",
        );
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut app.nam.key_input).hint_text("t3k_pub_…").desired_width(280.0));
            if ui.button("Save").clicked() {
                let key = std::mem::take(&mut app.nam.key_input);
                if let Err(e) = app.nam.save_client_id(&key) {
                    app.notify(format!("Could not save the key: {e}"));
                }
            }
            if ui.button("Open TONE3000 settings").clicked() {
                let _ = open_browser("https://www.tone3000.com/settings");
            }
        });
        return;
    }
    if app.nam.signing_in.is_some() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Waiting for TONE3000 in your browser…");
            if ui.button("Cancel").clicked() {
                app.nam.cancel_sign_in();
            }
        });
        return;
    }
    if !app.nam.client.signed_in() {
        ui.label("Sign in to load your favourites and downloads, or pick any tone on TONE3000.");
        ui.horizontal(|ui| {
            if ui.button("Sign in with TONE3000").clicked()
                && let Err(e) = app.nam.sign_in(Prompt::SignIn)
            {
                app.notify(e.to_string());
            }
            if ui
                .button("Browse TONE3000…")
                .on_hover_text("Pick a tone on tone3000.com; it comes back to this slot")
                .clicked()
                && let Err(e) = app.nam.sign_in(Prompt::SelectTone)
            {
                app.notify(e.to_string());
            }
        });
        return;
    }
    if app.nam.remote.source.is_none() {
        app.nam.browse(Remote::List(ToneList::Favorited), 1);
    }
    ui.horizontal(|ui| {
        let who = app.nam.user.as_ref().map_or("…".to_owned(), |u| u.shown_name().to_owned());
        ui.label(egui::RichText::new(format!("Signed in as {who}")).color(TEXT_DIM));
        if ui.small_button("Sign out").clicked() {
            app.nam.sign_out();
        }
        if ui.small_button("Browse the full catalogue…").clicked()
            && let Err(e) = app.nam.sign_in(Prompt::SelectTone)
        {
            app.notify(e.to_string());
        }
    });
    if let Some(tone) = app.nam.remote.tone.clone() {
        tone_view(app, ui, target, &tone);
        return;
    }
    ui.horizontal(|ui| {
        for list in ToneList::ALL {
            let active = app.nam.remote.source == Some(Remote::List(list));
            if ui.selectable_label(active, list.label()).clicked() {
                app.nam.browse(Remote::List(list), 1);
            }
        }
        let edit = ui.add(
            egui::TextEdit::singleline(&mut app.nam.remote.search).hint_text("Search TONE3000").desired_width(160.0),
        );
        let entered = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui.button("Search").clicked() || entered) && !app.nam.remote.search.trim().is_empty() {
            let q = app.nam.remote.search.trim().to_owned();
            app.nam.browse(Remote::Search(q), 1);
        }
    });
    if let Some(err) = &app.nam.remote.error {
        ui.colored_label(theme::PINK, err);
    }
    if app.nam.remote.loading {
        ui.spinner();
        return;
    }
    let (page, pages) = (app.nam.remote.page, app.nam.remote.total_pages);
    if pages > 1 {
        ui.horizontal(|ui| {
            let source = app.nam.remote.source.clone();
            if ui.add_enabled(page > 1, egui::Button::new("Previous page")).clicked()
                && let Some(s) = source.clone()
            {
                app.nam.browse(s, page - 1);
            }
            ui.label(format!("{page} / {pages}"));
            if ui.add_enabled(page < pages, egui::Button::new("Next page")).clicked()
                && let Some(s) = source
            {
                app.nam.browse(s, page + 1);
            }
        });
    }
    let tones = app.nam.remote.tones.clone();
    if tones.is_empty() && app.nam.remote.error.is_none() {
        ui.label(egui::RichText::new("No tones here.").color(TEXT_DIM));
    }
    egui::ScrollArea::vertical().id_salt("nam-t3k").auto_shrink([false, false]).show_rows(
        ui,
        ROW,
        tones.len(),
        |ui, range| {
            for tone in &tones[range] {
                tone_row(app, ui, tone);
            }
        },
    );
}

fn tone_row(app: &mut ScytheApp, ui: &mut Ui, tone: &Tone) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW - 4.0), Sense::click());
    ui.add_space(4.0);
    let painter = ui.painter().clone();
    painter.rect_filled(rect, CornerRadius::same(10), if resp.hovered() { RAISED } else { Color32::TRANSPARENT });
    let star = Rect::from_center_size(pos2(rect.left() + 20.0, rect.center().y), vec2(18.0, 18.0));
    let fav = tone.is_favorite.unwrap_or(false);
    let star_resp =
        ui.interact(star, Id::unique(("t3k-fav", tone.id)), Sense::click()).on_hover_text("Favourite on TONE3000");
    paint_star(ui, star, if fav { theme::YELLOW } else { theme::SWITCH_OFF }, fav);
    if star_resp.clicked() {
        app.nam.set_favourite(tone.id, !fav);
    }
    let left = rect.left() + 38.0;
    let width = rect.width() - 50.0;
    painter.text(
        pos2(left, rect.center().y - 8.0),
        Align2::LEFT_CENTER,
        widgets::truncate(ui, &tone.title, &theme::semibold(13.0), width),
        theme::semibold(13.0),
        TEXT,
    );
    let mut detail = vec![format!("by {}", tone.author().unwrap_or("unknown"))];
    detail.extend(tone.gear.clone());
    detail.extend(tone.makes.first().map(|m| m.name.clone()));
    let count = if tone.is_ir() {
        tone.irs_count.map(|n| format!("{n} IRs"))
    } else {
        tone.models_count.map(|n| format!("{n} models"))
    };
    detail.extend(count);
    detail.extend(tone.downloads_count.map(|n| format!("{n} downloads")));
    painter.text(
        pos2(left, rect.center().y + 9.0),
        Align2::LEFT_CENTER,
        widgets::truncate(ui, &detail.join(" · "), &theme::regular(11.0), width),
        theme::regular(11.0),
        TEXT_DIM,
    );
    if resp.clicked() && !star_resp.clicked() {
        app.nam.open_tone(tone.clone());
    }
}

fn tone_view(app: &mut ScytheApp, ui: &mut Ui, target: &Target, tone: &Tone) {
    ui.horizontal(|ui| {
        if ui.button("Back").clicked() {
            app.nam.remote.tone = None;
            app.nam.remote.models.clear();
        }
        ui.label(egui::RichText::new(&tone.title).font(theme::semibold(15.0)).color(TEXT));
        ui.label(egui::RichText::new(format!("by {}", tone.author().unwrap_or("unknown"))).color(TEXT_DIM));
        if let Some(url) = &tone.url
            && ui.small_button("Open on TONE3000").clicked()
        {
            let _ = open_browser(url);
        }
    });
    let mut facts: Vec<String> = Vec::new();
    facts.extend(tone.gear.clone());
    facts.extend(tone.makes.iter().map(|m| m.name.clone()));
    facts.extend(tone.license.as_ref().map(|l| format!("licence {l}")));
    if !facts.is_empty() {
        ui.label(egui::RichText::new(facts.join(" · ")).color(TEXT_DIM));
    }
    if let Some(d) = tone.description.as_ref().filter(|d| !d.is_empty()) {
        ui.label(egui::RichText::new(d).color(TEXT_MID));
    }
    if let Some(err) = &app.nam.remote.error {
        ui.colored_label(theme::PINK, err);
    }
    if app.nam.remote.models_loading {
        ui.spinner();
        return;
    }
    let models: Vec<Model> = app.nam.remote.models.clone();
    egui::ScrollArea::vertical().id_salt("nam-t3k-models").auto_shrink([false, false]).show(ui, |ui| {
        for model in &models {
            let local: Option<PathBuf> = app.nam.library.downloaded(model.id).map(|e| e.path.clone());
            ui.horizontal(|ui| {
                let busy = app.nam.downloading.contains(&model.id);
                let label = if local.is_some() { "Load" } else { "Get and load" };
                if ui.add_enabled(!busy, egui::Button::new(label)).clicked() {
                    match &local {
                        Some(path) => app.nam_open(target, path.clone()),
                        None => app.nam.download(tone, model, Some(target.clone())),
                    }
                }
                if busy {
                    ui.spinner();
                }
                ui.label(egui::RichText::new(&model.name).color(TEXT));
                let mut facts = Vec::new();
                facts.extend(model.size.clone());
                facts.extend(model.architecture().map(|a| format!("A{a}")));
                if local.is_some() {
                    facts.push("in library".into());
                }
                ui.label(egui::RichText::new(facts.join(" · ")).color(TEXT_DIM));
            });
        }
    });
}
