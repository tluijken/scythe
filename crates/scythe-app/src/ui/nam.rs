//! The Neural Amp Modeler panel: the slot's model with what is known about it, its levels and
//! snapshots, a bar while a TONE3000 model is previewed, and a two-pane browser over the model
//! library and TONE3000: a list on one side, the picked entry with its models on the other.

use std::path::{Path, PathBuf};

use egui::{Align2, Color32, CornerRadius, Id, Key, Modifiers, Rect, Sense, Ui, pos2, vec2};
use scythe_engine::fx::nam_settings;

use crate::app::ScytheApp;
use crate::nam::library::{Entry, Filter, Kind, Place};
use crate::nam::preview::Stage;
use crate::nam::snapshots::SNAPSHOTS;
use crate::nam::tone3000::{Model, Prompt, Tone, ToneList, open_browser};
use crate::nam::{BrowserTab, Gear, Remote, SIZES, Target, ToneSort, is_model_file};
use crate::theme::{self, ACCENT, BG, RAISED, RAISED_HI, TEXT, TEXT_DIM, TEXT_MID};
use crate::ui::widgets::{self, Icon, icon_button, knob, paint_star, text_pill};
use crate::ui::window::{WindowKind, scythe_window};

const ROW: f32 = 32.0;
const MODEL_ROW: f32 = 40.0;
const LEVEL_RANGE: (f64, f64) = (-20.0, 20.0);
const SNAPSHOT_NAMES: [&str; SNAPSHOTS] = ["A", "B", "C", "D"];
/// Below this width the list and the details stack instead of standing side by side.
const STACK_BELOW: f32 = 640.0;
/// Lines of a tone's description shown before "more".
const DESCRIPTION_LINES: usize = 3;

pub fn show(app: &mut ScytheApp, ui: &mut Ui) {
    let Some(target) = app.nam.panel.clone() else {
        app.nam.panel_rect = None;
        return;
    };
    if app.nam_path(&target).is_none() {
        app.nam.panel = None;
        app.nam.panel_rect = None;
        return;
    }
    if app.nam.preview.as_ref().is_some_and(|p| app.nam_path(&p.target).is_none()) {
        app.nam.preview = None;
    }
    let ctx = ui.ctx().clone();
    take_dropped_files(app, &ctx, &target);
    let track = app.project.track(target.track).map(|t| t.name.clone()).unwrap_or_default();
    let mut open = true;
    let title = if app.nam_rig(&target).is_some() { "Guitar rig" } else { "Neural Amp Modeler" };
    let shown = scythe_window("nam-panel", format!("{title} · {track}"), WindowKind::Panel)
        .default_size(vec2(880.0, 760.0))
        .min_size(vec2(420.0, 460.0))
        .show(&ctx, &mut open, |ui| body(app, ui, &target));
    app.nam.panel_rect = shown.map(|s| s.response.rect);
    if !open {
        app.nam.panel = None;
    }
}

/// While a model is previewed in the panel's slot: left and right step through the tone's
/// models, up and down through the listed tones, Enter keeps and Escape reverts. Runs before
/// the global shortcuts, which then do not see these keys.
pub fn preview_keys(app: &mut ScytheApp, ctx: &egui::Context) {
    let Some(p) = &app.nam.preview else { return };
    if app.nam.panel.as_ref() != Some(&p.target) {
        return;
    }
    let tones = app.nam.tab == BrowserTab::Tone3000 && !app.nam.remote.tones.is_empty();
    let (left, right, up, down, enter, escape) = ctx.input_mut(|i| {
        (
            i.consume_key(Modifiers::NONE, Key::ArrowLeft),
            i.consume_key(Modifiers::NONE, Key::ArrowRight),
            tones && i.consume_key(Modifiers::NONE, Key::ArrowUp),
            tones && i.consume_key(Modifiers::NONE, Key::ArrowDown),
            i.consume_key(Modifiers::NONE, Key::Enter),
            i.consume_key(Modifiers::NONE, Key::Escape),
        )
    });
    match () {
        _ if left => app.nam.step_preview(-1),
        _ if right => app.nam.step_preview(1),
        _ if up => app.nam.step_preview_tone(-1),
        _ if down => app.nam.step_preview_tone(1),
        _ if enter => app.nam_end_preview(true),
        _ if escape => app.nam_end_preview(false),
        _ => {}
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
    if let Some(at) = app.nam_rig(target) {
        crate::ui::rig::body(app, ui, target, at);
        return;
    }
    header(app, ui, target);
    ui.add_space(6.0);
    controls(app, ui, target, true);
    if app.nam.preview.as_ref().is_some_and(|p| p.target == *target) {
        ui.add_space(6.0);
        preview_bar(app, ui);
    }
    ui.add_space(8.0);
    browser(app, ui, target, None);
}

/// The tabs and the two-pane browser; `kind` keeps the library to models or impulse
/// responses (and TONE3000 to IR tones for these) in a rig's amp and cab sections.
pub(crate) fn browser(app: &mut ScytheApp, ui: &mut Ui, target: &Target, kind: Option<Kind>) {
    tabs(app, ui);
    ui.add_space(6.0);
    let tab = app.nam.tab;
    let list = |app: &mut ScytheApp, ui: &mut Ui| match tab {
        BrowserTab::Library => library_list(app, ui, target, false, kind),
        BrowserTab::Recent => library_list(app, ui, target, true, kind),
        BrowserTab::Tone3000 => tone3000_list(app, ui, kind),
    };
    let detail = |app: &mut ScytheApp, ui: &mut Ui| match tab {
        BrowserTab::Library | BrowserTab::Recent => entry_detail(app, ui, target, kind),
        BrowserTab::Tone3000 => tone_detail(app, ui, target),
    };
    if tab == BrowserTab::Tone3000 && !tone3000_ready(app, ui) {
        return;
    }
    let salt = match kind {
        Some(Kind::Ir) => "ir",
        Some(Kind::Model) => "model",
        None => "all",
    };
    split(ui, tab, salt, list, detail, app);
}

/// The list and the details side by side, with a handle between them, or stacked when the
/// panel is narrow. Each scrolls on its own.
fn split(
    ui: &mut Ui,
    tab: BrowserTab,
    kind: &str,
    list: impl FnOnce(&mut ScytheApp, &mut Ui),
    detail: impl FnOnce(&mut ScytheApp, &mut Ui),
    app: &mut ScytheApp,
) {
    let size = ui.available_size();
    let salt = (if tab == BrowserTab::Tone3000 { "t3k" } else { "lib" }, kind);
    if size.x < STACK_BELOW {
        egui::Panel::top(("nam-list-top", salt))
            .resizable(true)
            .frame(egui::Frame::NONE.inner_margin(egui::Margin { bottom: 6, ..Default::default() }))
            .default_size(size.y * 0.45)
            .size_range(120.0..=(size.y - 140.0).max(121.0))
            .show(ui, |ui| list(app, ui));
    } else {
        egui::Panel::left(("nam-list-left", salt))
            .resizable(true)
            .frame(egui::Frame::NONE.inner_margin(egui::Margin { right: 8, ..Default::default() }))
            .default_size(size.x * 0.46)
            .size_range(240.0..=(size.x - 260.0).max(241.0))
            .show(ui, |ui| list(app, ui));
    }
    egui::CentralPanel::no_frame().show(ui, |ui| {
        let r = ui.max_rect();
        let inner = if size.x < STACK_BELOW { r.shrink2(vec2(0.0, 4.0)) } else { r.shrink2(vec2(6.0, 0.0)) };
        ui.scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| detail(app, ui));
    });
}

/// What the slot plays, and what to do when the file is not in the library.
pub(crate) fn header(app: &mut ScytheApp, ui: &mut Ui, target: &Target) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 58.0), Sense::hover());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, CornerRadius::same(14), RAISED);
    let path = app.nam_model_path(target);
    let entry = path.as_ref().and_then(|p| app.nam.library.entry(p)).cloned();
    let previewing = app
        .nam
        .preview
        .as_ref()
        .filter(|p| p.target == *target && !p.comparing && !p.ir)
        .map(|p| p.model().name.clone());
    let loading = app.nam.loading.get(target).cloned();
    let file_name = |p: &Path| p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let name = match (&previewing, &entry, &path) {
        (Some(m), _, _) if !m.is_empty() => m.clone(),
        (_, Some(e), _) => e.name.clone(),
        (_, None, Some(p)) => p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        _ => "No model loaded".to_owned(),
    };
    let status = app.nam_status(target);
    let in_library = entry.is_some();
    let summary = match (&loading, status.as_ref(), &entry) {
        (Some(p), _, _) => format!("Loading {}…", file_name(p)),
        (_, Some(scythe_engine::fx::FxStatus::Unavailable(why)), _) => format!("Not running: {why}"),
        _ if previewing.is_some() => "Previewing from TONE3000, not in the library yet".to_owned(),
        (_, _, Some(e)) if !e.summary().is_empty() => e.summary(),
        (_, _, Some(_)) => "No gear details in the model file".to_owned(),
        (_, _, None) => match &path {
            Some(p) if !p.exists() => format!("{} is missing", p.display()),
            Some(p) => format!("{} · not in the library", file_name(p)),
            None => "Pick a model below, drop a .nam file here, or open one".to_owned(),
        },
    };
    let left = rect.left() + 14.0;
    let mut right = rect.right() - 10.0;
    if app.nam_rig(target).is_none() {
        let make = Rect::from_min_max(pos2(right - 104.0, rect.center().y - 12.0), pos2(right, rect.center().y + 12.0));
        if text_pill(ui, make, "Make rig", theme::semibold(12.0), ACCENT, BG)
            .on_hover_text(
                "Turn this slot (and the ReaVerb after it) into a guitar rig: gate, amp, cab, EQ and output in one \
                 slot, with snapshots and presets of the whole rig. REAPER opens it as an FX container.",
            )
            .clicked()
        {
            app.nam_make_rig(target);
        }
        right = make.left() - 8.0;
    }
    if let Some(e) = &entry {
        let star = Rect::from_center_size(pos2(right - 12.0, rect.center().y), vec2(24.0, 24.0));
        let resp = ui.interact(star, Id::unique(("nam-fav", &e.path)), Sense::click()).on_hover_text("Favourite");
        paint_star(ui, star, if e.favourite { theme::YELLOW } else { TEXT_DIM }, e.favourite);
        if resp.clicked() {
            app.nam.library.set_favourite(&e.path, !e.favourite);
            app.nam.library_dirty = true;
        }
        right = star.left() - 8.0;
        if let Some(url) = e.origin.as_ref().and_then(|o| o.url.clone()) {
            let link =
                Rect::from_min_max(pos2(right - 112.0, rect.center().y - 11.0), pos2(right, rect.center().y + 11.0));
            if text_pill(ui, link, "Open on TONE3000", theme::regular(11.0), BG, TEXT_MID).clicked() {
                let _ = open_browser(&url);
            }
            right = link.left() - 6.0;
        }
    } else if path.as_ref().is_some_and(|p| p.exists()) && previewing.is_none() && !in_library {
        let add = Rect::from_min_max(pos2(right - 128.0, rect.center().y - 12.0), pos2(right, rect.center().y + 12.0));
        if text_pill(ui, add, "Add to library", theme::semibold(12.0), ACCENT, BG)
            .on_hover_text("Adds the model's folder to the library, so its models are listed with their details")
            .clicked()
        {
            app.nam_add_model_folder(target);
        }
        right = add.left() - 8.0;
    }
    let width = right - left - 8.0;
    let title = widgets::truncate(ui, &name, &theme::semibold(16.0), width);
    painter.text(pos2(left, rect.top() + 19.0), Align2::LEFT_CENTER, title, theme::semibold(16.0), TEXT);
    let mut line = summary;
    if let Some(e) = &entry {
        let mut extra: Vec<String> = Vec::new();
        extra.extend(e.architecture.clone());
        extra.extend(e.sample_rate.map(|r| format!("{} kHz", trim(r / 1000.0))));
        if !extra.is_empty() {
            line = format!("{line} · {}", extra.join(" · "));
        }
    }
    let line = widgets::truncate(ui, &line, &theme::regular(12.0), width);
    painter.text(pos2(left, rect.top() + 40.0), Align2::LEFT_CENTER, line, theme::regular(12.0), TEXT_DIM);
    if let Some(p) = &path {
        let hover = Rect::from_min_max(rect.min, pos2(right, rect.bottom()));
        ui.interact(hover, Id::unique(("nam-head", target)), Sense::hover()).on_hover_text(p.display().to_string());
    }
    end_block(ui, rect);
}

/// Puts the layout cursor below `rect` again: the buttons inside it are placed by rect, and
/// allocating those moves the cursor back up to their bottom, under the next block.
pub(crate) fn end_block(ui: &mut Ui, rect: Rect) {
    ui.advance_cursor_after_rect(rect);
}

pub(crate) fn trim(v: f64) -> String {
    // Adding zero turns a rounded -0.0 into 0.0.
    let s = format!("{:.1}", (v * 10.0).round() / 10.0 + 0.0);
    s.trim_end_matches(".0").to_owned()
}

pub(crate) fn controls(app: &mut ScytheApp, ui: &mut Ui, target: &Target, snapshots: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 66.0), Sense::hover());
    let Some(settings) = app.nam_slot(target).as_ref().and_then(nam_settings) else { return };
    let mut x = rect.left();
    for (k, label) in ["Input", "Output"].into_iter().enumerate() {
        let value = if k == 0 { settings.input_db } else { settings.output_db };
        let r = Rect::from_min_size(pos2(x, rect.top()), vec2(60.0, 66.0));
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
        x += 64.0;
    }
    x += 10.0;
    let cy = rect.top() + 26.0;
    let previewing = app.nam.preview.as_ref().is_some_and(|p| p.target == *target && !p.ir);
    let (prev_tip, next_tip) = if previewing {
        ("Previous model of the previewed tone (Left)", "Next model of the previewed tone (Right)")
    } else {
        ("Previous library model (Alt+Left)", "Next library model (Alt+Right)")
    };
    let step = |ui: &mut Ui, x: f32, icon: Icon, tip: &str| {
        icon_button(ui, Rect::from_min_size(pos2(x, cy - 16.0), vec2(32.0, 32.0)), icon, RAISED, TEXT, 14.0, tip)
            .clicked()
    };
    if step(ui, x, Icon::Previous, prev_tip) {
        app.nam_step(-1);
    }
    x += 36.0;
    if step(ui, x, Icon::Next, next_tip) {
        app.nam_step(1);
    }
    x += 40.0;
    let open = Rect::from_min_size(pos2(x, cy - 14.0), vec2(64.0, 28.0));
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
    if !snapshots {
        end_block(ui, rect);
        return;
    }
    // Snapshots, right-aligned.
    let extras = app.nam_snapshots(target);
    let size = 34.0;
    let mut sx = rect.right() - SNAPSHOTS as f32 * (size + 6.0) + 6.0;
    ui.painter().text(pos2(sx, rect.top() + 4.0), Align2::LEFT_CENTER, "Snapshots", theme::regular(11.0), TEXT_DIM);
    for (n, name) in SNAPSHOT_NAMES.iter().enumerate() {
        let r = Rect::from_min_size(pos2(sx, cy - size / 2.0 + 6.0), vec2(size, size));
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

/// Shown while a TONE3000 model is previewed: what plays, stepping, A/B, Keep and Revert.
pub(crate) fn preview_bar(app: &mut ScytheApp, ui: &mut Ui) {
    let Some(p) = app.nam.preview.clone() else { return };
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::hover());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, CornerRadius::same(12), theme::with_alpha(ACCENT, 46));
    painter.rect_stroke(
        rect,
        CornerRadius::same(12),
        egui::Stroke::new(1.0, theme::with_alpha(ACCENT, 160)),
        egui::StrokeKind::Inside,
    );
    let cy = rect.center().y;
    let mut right = rect.right() - 8.0;
    let mut pill = |ui: &mut Ui, w: f32, text: &str, bg: Color32, fg: Color32, tip: &str| {
        let r = Rect::from_min_max(pos2(right - w, cy - 14.0), pos2(right, cy + 14.0));
        right = r.left() - 6.0;
        text_pill(ui, r, text, theme::semibold(12.0), bg, fg).on_hover_text(tip).clicked()
    };
    let keep_tip = if p.ir {
        "Keep this impulse response: it moves into the library and stays in the cab (Enter)"
    } else {
        "Keep this model: it moves into the library and stays loaded (Enter)"
    };
    if pill(ui, 64.0, "Keep", ACCENT, BG, keep_tip) {
        app.nam_end_preview(true);
    }
    if pill(ui, 64.0, "Revert", RAISED_HI, TEXT, "Back to what played before the preview (Escape)") {
        app.nam_end_preview(false);
    }
    if p.previous.is_some() {
        let (bg, fg, label) = if p.comparing { (TEXT, BG, "B: before") } else { (RAISED, TEXT, "A/B") };
        if pill(ui, 76.0, label, bg, fg, "Switch between the previewed model and the one from before the preview") {
            app.nam_toggle_compare();
        }
    }
    let next = Rect::from_min_size(pos2(right - 28.0, cy - 14.0), vec2(28.0, 28.0));
    let prev = Rect::from_min_size(pos2(next.left() - 32.0, cy - 14.0), vec2(28.0, 28.0));
    if icon_button(ui, prev, Icon::Previous, RAISED, TEXT, 12.0, "Previous model of this tone (Left)").clicked() {
        app.nam.step_preview(-1);
    }
    if icon_button(ui, next, Icon::Next, RAISED, TEXT, 12.0, "Next model of this tone (Right)").clicked() {
        app.nam.step_preview(1);
    }
    let text_right = prev.left() - 10.0;
    let left = rect.left() + 14.0;
    let (at, of) = p.position();
    let state = match &p.stage {
        _ if p.comparing => "playing what played before".to_owned(),
        Stage::Fetching => "fetching…".to_owned(),
        Stage::Loading => "loading…".to_owned(),
        Stage::Playing => format!("{at} of {of}"),
        Stage::Failed(e) => format!("failed: {e}"),
    };
    let model = if p.model().name.is_empty() { format!("model {}", p.model().id) } else { p.model().name.clone() };
    let what = if p.ir { "Previewing cab" } else { "Previewing" };
    let title = widgets::truncate(ui, &format!("{what} {model}"), &theme::semibold(13.0), text_right - left);
    painter.text(pos2(left, cy - 8.0), Align2::LEFT_CENTER, title, theme::semibold(13.0), TEXT);
    let sub = format!("{} · {state} · ←→ models, ↑↓ tones", p.tone.title);
    let color = if matches!(p.stage, Stage::Failed(_)) { theme::PINK } else { TEXT_MID };
    let sub = widgets::truncate(ui, &sub, &theme::regular(11.0), text_right - left);
    painter.text(pos2(left, cy + 9.0), Align2::LEFT_CENTER, sub, theme::regular(11.0), color);
    end_block(ui, rect);
}

fn tabs(app: &mut ScytheApp, ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(255), BG);
    let tw = ((rect.width() - 8.0) / 3.0 - 4.0).min(140.0);
    for (k, (tab, label)) in
        [(BrowserTab::Library, "Library"), (BrowserTab::Recent, "Recent"), (BrowserTab::Tone3000, "TONE3000")]
            .into_iter()
            .enumerate()
    {
        let r = Rect::from_min_size(pos2(rect.left() + 4.0 + k as f32 * (tw + 4.0), rect.top() + 3.0), vec2(tw, 26.0));
        let active = app.nam.tab == tab;
        let (bg, fg) = if active { (RAISED_HI, TEXT) } else { (Color32::TRANSPARENT, TEXT_DIM) };
        if text_pill(ui, r, label, theme::semibold(12.0), bg, fg).clicked() {
            app.nam.tab = tab;
        }
    }
    let note = if app.nam.scanning {
        Some("Reading library…".to_owned())
    } else if app.nam.tab == BrowserTab::Tone3000 && app.nam.client.signed_in() {
        app.nam.user.as_ref().map(|u| format!("Signed in as {}", u.shown_name()))
    } else {
        None
    };
    if let Some(note) = note {
        let x = rect.left() + 4.0 + 3.0 * (tw + 4.0) + 8.0;
        let note = widgets::truncate(ui, &note, &theme::regular(11.0), (rect.right() - 14.0 - x).max(0.0));
        ui.painter().text(
            rect.right_center() - vec2(14.0, 0.0),
            Align2::RIGHT_CENTER,
            note,
            theme::regular(11.0),
            TEXT_DIM,
        );
    }
    end_block(ui, rect);
}

fn dim(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text.into()).color(TEXT_DIM)
}

// ---- library ---------------------------------------------------------------------------

fn library_list(app: &mut ScytheApp, ui: &mut Ui, target: &Target, recent: bool, kind: Option<Kind>) {
    let entries: Vec<Entry> = if recent {
        let recent = app.nam.library.recent().iter().filter_map(|p| app.nam.library.entry(p).cloned());
        recent.filter(|e| kind.is_none_or(|k| e.kind == k)).collect()
    } else {
        library_filters(app, ui, kind.is_none());
        ui.add_space(4.0);
        let filter = Filter { kind: kind.or(app.nam.filter.kind), ..app.nam.filter.clone() };
        app.nam.library.search(&filter).into_iter().cloned().collect()
    };
    if entries.is_empty() {
        let text = if recent {
            "Models you load show up here."
        } else if app.nam.library.entries().is_empty() {
            "The library is empty. Add a folder of .nam models (Folders → Add folder…) or get models from TONE3000."
        } else {
            "Nothing matches."
        };
        ui.add(egui::Label::new(dim(text)).wrap());
        return;
    }
    let current = match kind {
        Some(Kind::Ir) => app
            .nam_rig(target)
            .and_then(|at| app.rig_settings(at))
            .and_then(|s| s.cab.ir)
            .map(|ir| app.nam_resolve(&ir)),
        _ => app.nam_model_path(target),
    };
    let loading = app.nam.loading.get(target).cloned();
    egui::ScrollArea::vertical()
        .id_salt(("nam-lib", recent, kind == Some(Kind::Ir)))
        .auto_shrink([false, false])
        .show_rows(ui, ROW, entries.len(), |ui, range| {
            for e in &entries[range] {
                let is_current = current.as_deref() == Some(e.path.as_path());
                entry_row(app, ui, target, e, is_current, loading.as_ref() == Some(&e.path));
            }
        });
}

fn library_filters(app: &mut ScytheApp, ui: &mut Ui, pick_kind: bool) {
    ui.add(
        egui::TextEdit::singleline(&mut app.nam.filter.text)
            .hint_text("Search name, gear, author, folder, tag")
            .desired_width(f32::INFINITY),
    );
    ui.horizontal_wrapped(|ui| {
        let kinds: &[(&str, Option<Kind>)] =
            if pick_kind { &[("All", None), ("Models", Some(Kind::Model)), ("IRs", Some(Kind::Ir))] } else { &[] };
        for &(label, kind) in kinds {
            if ui.selectable_label(app.nam.filter.kind == kind, label).clicked() {
                app.nam.filter.kind = kind;
            }
        }
        if ui.selectable_label(app.nam.filter.favourites, "Favourites").clicked() {
            app.nam.filter.favourites ^= true;
        }
        let place = match &app.nam.filter.place {
            None => "Everywhere".to_owned(),
            Some(Place::Downloads) => "TONE3000 downloads".to_owned(),
            Some(Place::Folder(f)) => {
                f.file_name().map_or_else(|| f.display().to_string(), |n| n.to_string_lossy().into())
            }
        };
        egui::ComboBox::from_id_salt("nam-place").width(110.0).selected_text(place).show_ui(ui, |ui| {
            ui.selectable_value(&mut app.nam.filter.place, None, "Everywhere");
            ui.selectable_value(&mut app.nam.filter.place, Some(Place::Downloads), "TONE3000 downloads");
            for folder in app.nam.library.folders.clone() {
                let label = folder.display().to_string();
                ui.selectable_value(&mut app.nam.filter.place, Some(Place::Folder(folder)), label);
            }
        });
        let tags = app.nam.library.tags();
        if !tags.is_empty() {
            egui::ComboBox::from_id_salt("nam-tag")
                .width(80.0)
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
}

/// One line per file: a click shows it beside the list and loads it, so stepping down the
/// list auditions model after model.
fn entry_row(app: &mut ScytheApp, ui: &mut Ui, target: &Target, e: &Entry, current: bool, loading: bool) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW - 2.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, current, format!("Library {}", e.name)));
    ui.add_space(2.0);
    let painter = ui.painter().clone();
    let selected = app.nam.selected.as_deref() == Some(e.path.as_path());
    let bg = if current {
        RAISED_HI
    } else if selected || resp.hovered() {
        RAISED
    } else {
        Color32::TRANSPARENT
    };
    painter.rect_filled(rect, CornerRadius::same(8), bg);
    if current {
        painter.rect_filled(
            Rect::from_min_size(rect.min + vec2(0.0, 7.0), vec2(3.0, rect.height() - 14.0)),
            CornerRadius::same(2),
            ACCENT,
        );
    }
    let star = Rect::from_center_size(pos2(rect.left() + 16.0, rect.center().y), vec2(16.0, 16.0));
    let star_resp = ui.interact(star, Id::unique(("nam-row-fav", &e.path)), Sense::click());
    paint_star(ui, star, if e.favourite { theme::YELLOW } else { theme::SWITCH_OFF }, e.favourite);
    if star_resp.clicked() {
        app.nam.library.set_favourite(&e.path, !e.favourite);
        app.nam.library_dirty = true;
    }
    let left = rect.left() + 32.0;
    let right_text = match e.kind {
        Kind::Ir => "IR".to_owned(),
        Kind::Model => e.location(),
    };
    let right_w = widgets::text_width(ui, &right_text, theme::regular(11.0)).min(rect.width() * 0.32);
    let name_w = rect.right() - 12.0 - right_w - 10.0 - left;
    let fg = if e.error.is_some() { TEXT_DIM } else { TEXT };
    let mut name = e.name.clone();
    if loading {
        name.push_str("  (loading)");
    }
    let name = widgets::truncate(ui, &name, &theme::semibold(13.0), name_w);
    painter.text(pos2(left, rect.center().y), Align2::LEFT_CENTER, name, theme::semibold(13.0), fg);
    let right_text = widgets::truncate(ui, &right_text, &theme::regular(11.0), right_w);
    painter.text(
        rect.right_center() - vec2(10.0, 0.0),
        Align2::RIGHT_CENTER,
        right_text,
        theme::regular(11.0),
        TEXT_DIM,
    );
    let tip = match &e.error {
        Some(err) => format!("{}\nunreadable: {err}", e.path.display()),
        None => format!("{}\n{}", e.summary(), e.path.display()),
    };
    let resp = resp.on_hover_text(tip);
    if resp.clicked() && !star_resp.clicked() {
        app.nam.selected = Some(e.path.clone());
        if e.error.is_none() {
            app.nam_open(target, e.path.clone());
        }
    }
    let path = e.path.clone();
    resp.context_menu(|ui| {
        let label = if e.kind == Kind::Ir { "Load as cabinet IR" } else { "Load" };
        if ui.button(label).clicked() {
            app.nam_open(target, path.clone());
            ui.close();
        }
        if e.kind == Kind::Ir
            && let Some(at) = app.nam_rig(target)
            && ui.button("Load as second IR").on_hover_text("Blend it with the first in the cab").clicked()
        {
            app.rig_set_ir(at, &path, true);
            ui.close();
        }
        if ui.button(if e.favourite { "Remove from favourites" } else { "Add to favourites" }).clicked() {
            app.nam.library.set_favourite(&path, !e.favourite);
            app.nam.library_dirty = true;
            ui.close();
        }
        if ui.button("Show in folder").clicked() {
            if let Some(dir) = path.parent() {
                let _ = open_browser(&dir.to_string_lossy());
            }
            ui.close();
        }
    });
}

/// The picked library file, or the slot's model: what is known about it, tags and actions.
fn entry_detail(app: &mut ScytheApp, ui: &mut Ui, target: &Target, kind: Option<Kind>) {
    let selected =
        app.nam.selected.clone().filter(|p| kind.is_none_or(|k| app.nam.library.entry(p).is_some_and(|e| e.kind == k)));
    let current = match kind {
        Some(Kind::Ir) => app
            .nam_rig(target)
            .and_then(|at| app.rig_settings(at))
            .and_then(|s| s.cab.ir)
            .map(|ir| app.nam_resolve(&ir)),
        _ => app.nam_model_path(target),
    };
    let path = selected.or(current);
    let Some(e) = path.as_ref().and_then(|p| app.nam.library.entry(p)).cloned() else {
        ui.add(
            egui::Label::new(dim(
                "Click a model to load it and see its details here. Alt+Left and Alt+Right step through the list \
                 while you play.",
            ))
            .wrap(),
        );
        return;
    };
    egui::ScrollArea::vertical().id_salt("nam-entry").auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal(|ui| {
            let star = if e.favourite { "★ Favourite" } else { "☆ Favourite" };
            if ui.selectable_label(e.favourite, star).clicked() {
                app.nam.library.set_favourite(&e.path, !e.favourite);
                app.nam.library_dirty = true;
            }
            ui.add(egui::Label::new(egui::RichText::new(&e.name).font(theme::semibold(15.0)).color(TEXT)).truncate());
        });
        let summary = e.summary();
        if !summary.is_empty() {
            ui.add(egui::Label::new(egui::RichText::new(summary).color(TEXT_MID)).wrap());
        }
        let mut facts: Vec<String> = Vec::new();
        facts.extend(e.architecture.clone());
        facts.extend(e.sample_rate.map(|r| format!("{} kHz", trim(r / 1000.0))));
        if let Some(o) = &e.origin {
            facts.push(format!("TONE3000 · {}", o.tone_title));
            facts.extend(o.license.as_ref().map(|l| format!("licence {l}")));
        } else if !e.location().is_empty() {
            facts.push(e.location());
        }
        if !facts.is_empty() {
            ui.add(egui::Label::new(dim(facts.join(" · "))).wrap());
        }
        if let Some(err) = &e.error {
            ui.colored_label(theme::PINK, format!("Unreadable: {err}"));
        }
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            let label = if e.kind == Kind::Ir { "Load as cabinet IR" } else { "Load" };
            if ui.add_enabled(e.error.is_none(), egui::Button::new(label)).clicked() {
                app.nam_open(target, e.path.clone());
            }
            if ui.button("Show in folder").clicked()
                && let Some(dir) = e.path.parent()
            {
                let _ = open_browser(&dir.to_string_lossy());
            }
            if let Some(url) = e.origin.as_ref().and_then(|o| o.url.clone())
                && ui.button("Open on TONE3000").clicked()
            {
                let _ = open_browser(&url);
            }
            if let Some(tone_id) = e.origin.as_ref().map(|o| o.tone_id)
                && ui
                    .button("Other captures")
                    .on_hover_text("This tone's other models, through TONE3000 (sign-in in the browser)")
                    .clicked()
                && let Err(err) = app.nam.sign_in(Prompt::LoadTone(tone_id))
            {
                app.notify(err.to_string());
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(dim("Tags"));
            let id = Id::unique(("nam-tags", &e.path));
            let mut text = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_else(|| e.tags.join(", "));
            let edit = ui.add(egui::TextEdit::singleline(&mut text).hint_text("comma, separated").desired_width(180.0));
            if edit.lost_focus() {
                app.nam.library.set_tags(&e.path, text.split(',').map(str::to_owned).collect());
                app.nam.library_dirty = true;
                ui.data_mut(|d| d.remove::<String>(id));
            } else if edit.has_focus() {
                ui.data_mut(|d| d.insert_temp(id, text));
            }
        });
        ui.add_space(4.0);
        ui.add(
            egui::Label::new(egui::RichText::new(e.path.display().to_string()).font(theme::mono(10.0)).color(TEXT_DIM))
                .wrap(),
        );
    });
}

// ---- TONE3000 --------------------------------------------------------------------------

/// The key and sign-in steps before TONE3000 can be browsed; false while one is needed.
fn tone3000_ready(app: &mut ScytheApp, ui: &mut Ui) -> bool {
    if !app.nam.has_client_id() {
        ui.add(
            egui::Label::new(
                "Scythe needs a TONE3000 publishable key to sign in. Create one at tone3000.com → Settings → \
                 API keys (see docs/tone3000.md), then paste it here.",
            )
            .wrap(),
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
        return false;
    }
    if app.nam.signing_in.is_some() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Waiting for TONE3000 in your browser…");
            if ui.button("Cancel").clicked() {
                app.nam.cancel_sign_in();
            }
        });
        return false;
    }
    if !app.nam.client.signed_in() {
        ui.label("Sign in to see your favourites, downloads and what is trending, and to preview models.");
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
        return false;
    }
    if app.nam.remote.source.is_none() {
        app.nam.browse(Remote::List(ToneList::Favorited), 1);
    }
    true
}

fn tone3000_list(app: &mut ScytheApp, ui: &mut Ui, kind: Option<Kind>) {
    ui.horizontal(|ui| {
        let edit = ui.add(
            egui::TextEdit::singleline(&mut app.nam.remote.search)
                .hint_text("Search TONE3000")
                .desired_width((ui.available_width() - 70.0).max(80.0)),
        );
        let entered = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        if (ui.button("Search").clicked() || entered) && !app.nam.remote.search.trim().is_empty() {
            let q = app.nam.remote.search.trim().to_owned();
            app.nam.browse(Remote::Search(q), 1);
        }
    });
    ui.horizontal(|ui| {
        if ui
            .link("Browse the full catalogue…")
            .on_hover_text("Pick any tone on tone3000.com; it comes back to this slot")
            .clicked()
            && let Err(e) = app.nam.sign_in(Prompt::SelectTone)
        {
            app.notify(e.to_string());
        }
        if ui.link("Sign out").clicked() {
            app.nam.sign_out();
        }
    });
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for list in ToneList::ALL {
            let active = app.nam.remote.source == Some(Remote::List(list));
            if ui.selectable_label(active, list.label()).clicked() {
                app.nam.browse(Remote::List(list), 1);
            }
        }
    });
    let models_only = kind != Some(Kind::Ir);
    ui.horizontal_wrapped(|ui| {
        let remote = &mut app.nam.remote;
        if models_only {
            egui::ComboBox::from_id_salt("t3k-gear")
                .width(78.0)
                .selected_text(remote.gear.map_or("Any gear", Gear::label))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut remote.gear, None, "Any gear");
                    for g in Gear::ALL {
                        ui.selectable_value(&mut remote.gear, Some(g), g.label());
                    }
                });
            egui::ComboBox::from_id_salt("t3k-size")
                .width(78.0)
                .selected_text(remote.size.unwrap_or("Any size"))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut remote.size, None, "Any size");
                    for s in SIZES {
                        ui.selectable_value(&mut remote.size, Some(s), s);
                    }
                })
                .response
                .on_hover_text("Model size: standard sounds closest, nano needs the least CPU");
        }
        egui::ComboBox::from_id_salt("t3k-sort").width(110.0).selected_text(remote.sort.label()).show_ui(ui, |ui| {
            for s in ToneSort::ALL {
                ui.selectable_value(&mut remote.sort, s, s.label());
            }
        });
    });
    if let Some(err) = app.nam.remote.error.clone() {
        ui.add(egui::Label::new(egui::RichText::new(err).color(theme::PINK)).wrap());
    }
    if app.nam.remote.loading {
        ui.spinner();
        return;
    }
    let (page, pages) = (app.nam.remote.page, app.nam.remote.total_pages);
    let tones: Vec<Tone> = app
        .nam
        .remote
        .shown()
        .into_iter()
        .filter(|t| match kind {
            Some(Kind::Ir) => t.is_ir(),
            Some(Kind::Model) => !t.is_ir(),
            None => true,
        })
        .cloned()
        .collect();
    let bottom = 26.0;
    if tones.is_empty() && app.nam.remote.error.is_none() {
        let text = if app.nam.remote.tones.is_empty() { "No tones here." } else { "No tones pass the filters." };
        ui.label(dim(text));
    }
    let selected = app.nam.remote.tone.as_ref().map(|t| t.id);
    let previewed = app.nam.preview.as_ref().map(|p| p.tone.id);
    let height = (ui.available_height() - if pages > 1 { bottom } else { 0.0 }).max(ROW);
    egui::ScrollArea::vertical().id_salt("nam-t3k").max_height(height).auto_shrink([false, false]).show_rows(
        ui,
        ROW,
        tones.len(),
        |ui, range| {
            for tone in &tones[range] {
                tone_row(app, ui, tone, selected == Some(tone.id), previewed == Some(tone.id));
            }
        },
    );
    if pages > 1 {
        ui.horizontal(|ui| {
            let source = app.nam.remote.source.clone();
            if ui.add_enabled(page > 1, egui::Button::new("‹ Previous")).clicked()
                && let Some(s) = source.clone()
            {
                app.nam.browse(s, page - 1);
            }
            ui.label(dim(format!("page {page} of {pages}")));
            if ui.add_enabled(page < pages, egui::Button::new("Next ›")).clicked()
                && let Some(s) = source
            {
                app.nam.browse(s, page + 1);
            }
        });
    }
}

fn short_count(n: i64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{}M", trim(n as f64 / 1e6)),
        n if n >= 1_000 => format!("{}k", trim(n as f64 / 1e3)),
        n => n.to_string(),
    }
}

fn tone_row(app: &mut ScytheApp, ui: &mut Ui, tone: &Tone, selected: bool, previewed: bool) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW - 2.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, format!("Tone {}", tone.title)));
    ui.add_space(2.0);
    let painter = ui.painter().clone();
    let bg = match (selected, resp.hovered()) {
        (true, _) => RAISED_HI,
        (false, true) => RAISED,
        _ => Color32::TRANSPARENT,
    };
    painter.rect_filled(rect, CornerRadius::same(8), bg);
    if previewed {
        painter.rect_filled(
            Rect::from_min_size(rect.min + vec2(0.0, 7.0), vec2(3.0, rect.height() - 14.0)),
            CornerRadius::same(2),
            ACCENT,
        );
    }
    let star = Rect::from_center_size(pos2(rect.left() + 16.0, rect.center().y), vec2(16.0, 16.0));
    let fav = tone.is_favorite.unwrap_or(false);
    let star_resp =
        ui.interact(star, Id::unique(("t3k-fav", tone.id)), Sense::click()).on_hover_text("Favourite on TONE3000");
    paint_star(ui, star, if fav { theme::YELLOW } else { theme::SWITCH_OFF }, fav);
    if star_resp.clicked() {
        app.nam.set_favourite(tone.id, !fav);
    }
    let mut right_parts: Vec<String> = Vec::new();
    if tone.is_ir() {
        right_parts.push("IR".into());
    } else if let Some(g) = &tone.gear {
        right_parts.push(g.clone());
    }
    right_parts.extend(tone.downloads_count.map(|n| format!("{} dl", short_count(n))));
    let right_text = right_parts.join(" · ");
    let right_w = widgets::text_width(ui, &right_text, theme::regular(11.0)).min(rect.width() * 0.4);
    let left = rect.left() + 32.0;
    let width = (rect.right() - 10.0 - right_w - 10.0 - left).max(20.0);
    let title_w = widgets::text_width(ui, &tone.title, theme::semibold(13.0));
    let title = widgets::truncate(ui, &tone.title, &theme::semibold(13.0), width);
    painter.text(pos2(left, rect.center().y), Align2::LEFT_CENTER, &title, theme::semibold(13.0), TEXT);
    if title_w + 30.0 < width
        && let Some(author) = tone.author().filter(|a| !a.is_empty())
    {
        let by = widgets::truncate(ui, &format!("  {author}"), &theme::regular(11.0), width - title_w);
        painter.text(pos2(left + title_w, rect.center().y), Align2::LEFT_CENTER, by, theme::regular(11.0), TEXT_DIM);
    }
    let right_text = widgets::truncate(ui, &right_text, &theme::regular(11.0), right_w);
    painter.text(
        rect.right_center() - vec2(10.0, 0.0),
        Align2::RIGHT_CENTER,
        right_text,
        theme::regular(11.0),
        TEXT_DIM,
    );
    let mut tip = vec![format!("by {}", tone.author().unwrap_or("unknown"))];
    tip.extend(tone.makes.first().map(|m| m.name.clone()));
    let count = if tone.is_ir() {
        tone.irs_count.map(|n| format!("{n} IRs"))
    } else {
        tone.models_count.map(|n| format!("{n} models"))
    };
    tip.extend(count);
    if !tone.sizes.is_empty() {
        tip.push(tone.sizes.join(", "));
    }
    let resp = resp.on_hover_text(tip.join(" · "));
    if resp.clicked() && !star_resp.clicked() && !selected {
        app.nam.open_tone(tone.clone());
    }
}

/// The picked tone: who made it and where to find it, its models (each to preview or load),
/// then its description, which starts folded so the models stay in view.
fn tone_detail(app: &mut ScytheApp, ui: &mut Ui, target: &Target) {
    let Some(tone) = app.nam.remote.tone.clone() else {
        ui.add(
            egui::Label::new(dim(
                "Pick a tone to see its models.\n\nPreview plays a model in this slot without adding it to the \
                 library; while it plays, Left and Right step through the tone's models and Up and Down through \
                 the list. Keep moves it into the library, Revert goes back to what played before.",
            ))
            .wrap(),
        );
        return;
    };
    ui.horizontal(|ui| {
        let fav = tone.is_favorite.unwrap_or(false);
        let w = ui.available_width();
        let title = widgets::truncate(ui, &tone.title, &theme::semibold(15.0), w - 34.0);
        ui.label(egui::RichText::new(title).font(theme::semibold(15.0)).color(TEXT)).on_hover_text(&tone.title);
        if ui
            .add(egui::Button::new(egui::RichText::new(if fav { "★" } else { "☆" }).color(if fav {
                theme::YELLOW
            } else {
                TEXT_DIM
            })))
            .on_hover_text("Favourite on TONE3000")
            .clicked()
        {
            app.nam.set_favourite(tone.id, !fav);
        }
    });
    let mut facts = vec![format!("by {}", tone.author().unwrap_or("unknown"))];
    facts.extend(tone.gear.clone());
    facts.extend(tone.makes.iter().map(|m| m.name.clone()));
    facts.extend(tone.license.as_ref().map(|l| format!("licence {l}")));
    ui.horizontal(|ui| {
        let w = ui.available_width() - 120.0;
        let line = widgets::truncate(ui, &facts.join(" · "), &theme::regular(12.0), w.max(40.0));
        ui.label(dim(line)).on_hover_text(facts.join(" · "));
        if let Some(url) = &tone.url
            && ui.small_button("Open on TONE3000").clicked()
        {
            let _ = open_browser(url);
        }
    });
    ui.add_space(4.0);
    let description = tone.description.clone().filter(|d| !d.trim().is_empty());
    let reserve = if description.is_some() { if app.nam.remote.description_open { 160.0 } else { 78.0 } } else { 0.0 };
    models_list(app, ui, target, &tone, reserve);
    if let Some(d) = description {
        ui.add_space(4.0);
        ui.separator();
        description_view(app, ui, &d);
    }
}

fn models_list(app: &mut ScytheApp, ui: &mut Ui, target: &Target, tone: &Tone, reserve: f32) {
    let models: Vec<Model> = app.nam.remote.models.clone();
    ui.horizontal(|ui| {
        let what = if tone.is_ir() { "Impulse responses" } else { "Models" };
        ui.label(egui::RichText::new(what).font(theme::semibold(13.0)).color(TEXT));
        if !models.is_empty() {
            ui.label(dim(models.len().to_string()));
        }
        if app.nam.remote.models_loading {
            ui.spinner();
        }
    });
    if let Some(err) = app.nam.remote.models_error.clone() {
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(format!("Could not list the models: {err}")).color(theme::PINK));
            if ui.small_button("Try again").clicked() {
                app.nam.open_tone(tone.clone());
            }
        });
        return;
    }
    if models.is_empty() {
        if !app.nam.remote.models_loading {
            ui.label(dim("TONE3000 lists no models for this tone."));
        }
        return;
    }
    let height = (ui.available_height() - reserve).max(MODEL_ROW * 1.5);
    egui::ScrollArea::vertical()
        .id_salt(("nam-t3k-models", tone.id))
        .max_height(height)
        .auto_shrink([false, true])
        .show_rows(ui, MODEL_ROW, models.len(), |ui, range| {
            for model in &models[range] {
                model_row(app, ui, target, tone, &models, model);
            }
        });
}

fn model_row(app: &mut ScytheApp, ui: &mut Ui, target: &Target, tone: &Tone, models: &[Model], model: &Model) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), MODEL_ROW - 4.0), Sense::hover());
    let shown_name = if model.name.is_empty() { format!("model {}", model.id) } else { model.name.clone() };
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ListItem, true, format!("Model {shown_name}")));
    ui.add_space(4.0);
    let painter = ui.painter().clone();
    let local: Option<PathBuf> = app.nam.library.downloaded(model.id).map(|e| e.path.clone());
    let preview: Option<Stage> = app
        .nam
        .preview
        .as_ref()
        .filter(|p| p.target == *target && p.model().id == model.id)
        .map(|p| if p.comparing { Stage::Loading } else { p.stage.clone() });
    let comparing = app.nam.preview.as_ref().is_some_and(|p| p.comparing);
    let playing_now = app.nam_model_path(target).is_some_and(|p| Some(&p) == local.as_ref());
    let bg = if preview.is_some() || playing_now {
        RAISED_HI
    } else if resp.hovered() {
        RAISED
    } else {
        theme::with_alpha(RAISED, 120)
    };
    painter.rect_filled(rect, CornerRadius::same(8), bg);
    if preview.is_some() || playing_now {
        painter.rect_filled(
            Rect::from_min_size(rect.min + vec2(0.0, 8.0), vec2(3.0, rect.height() - 16.0)),
            CornerRadius::same(2),
            ACCENT,
        );
    }
    let busy = app.nam.downloading.contains(&model.id);
    let cy = rect.center().y;
    let mut right = rect.right() - 6.0;
    let mut button = |ui: &mut Ui, text: &str, w: f32, bg: Color32, fg: Color32, tip: &str| {
        let r = Rect::from_min_max(pos2(right - w, cy - 12.0), pos2(right, cy + 12.0));
        right = r.left() - 6.0;
        text_pill(ui, r, text, theme::semibold(11.0), bg, fg).on_hover_text(tip).clicked()
    };
    let load_label = if busy {
        "Getting…"
    } else if playing_now {
        "Loaded"
    } else if local.is_some() {
        "Load"
    } else {
        "Get and load"
    };
    let load_tip =
        if local.is_some() { "Load it from the library" } else { "Download it into the library and load it" };
    if button(ui, load_label, if local.is_some() { 52.0 } else { 88.0 }, RAISED_HI, TEXT, load_tip) && !busy {
        match &local {
            Some(path) => app.nam_open(target, path.clone()),
            None => app.nam.download(tone, model, Some(target.clone())),
        }
    }
    if !tone.is_ir() || app.nam_rig(target).is_some() {
        let (label, bg, fg) = match &preview {
            Some(Stage::Playing) => ("Playing", ACCENT, BG),
            Some(Stage::Fetching) => ("Fetching…", theme::with_alpha(ACCENT, 90), TEXT),
            Some(Stage::Loading) if comparing => ("A (paused)", theme::with_alpha(ACCENT, 90), TEXT),
            Some(Stage::Loading) => ("Loading…", theme::with_alpha(ACCENT, 90), TEXT),
            Some(Stage::Failed(_)) => ("Failed", theme::PINK, BG),
            None => ("Preview", theme::with_alpha(ACCENT, 60), TEXT),
        };
        let tip = "Play it in this slot now, without adding it to the library: Keep or Revert afterwards";
        if button(ui, label, 72.0, bg, fg, tip) {
            app.nam_preview(target, tone, models, model);
        }
    }
    let left = rect.left() + 12.0;
    let width = (right - left - 6.0).max(20.0);
    let name = if model.name.is_empty() { format!("model {}", model.id) } else { model.name.clone() };
    let name = widgets::truncate(ui, &name, &theme::semibold(12.0), width);
    painter.text(pos2(left, cy - 7.0), Align2::LEFT_CENTER, name, theme::semibold(12.0), TEXT);
    let mut facts: Vec<String> = Vec::new();
    if !tone.is_ir() {
        facts.extend(model.size.clone());
    }
    facts.extend(
        model
            .architecture()
            .filter(|_| !tone.is_ir())
            .map(|a| if a == "custom" { a } else { format!("architecture {a}") }),
    );
    if local.is_some() {
        facts.push("in library".into());
    }
    if let Some(Stage::Failed(e)) = &preview {
        facts.push(e.clone());
    }
    let facts = widgets::truncate(ui, &facts.join(" · "), &theme::regular(11.0), width);
    painter.text(pos2(left, cy + 8.0), Align2::LEFT_CENTER, facts, theme::regular(11.0), TEXT_DIM);
}

fn description_view(app: &mut ScytheApp, ui: &mut Ui, text: &str) {
    let open = app.nam.remote.description_open;
    let width = ui.available_width();
    let mut job = egui::text::LayoutJob::simple(text.to_owned(), theme::regular(12.0), TEXT_MID, width);
    if !open {
        job.wrap.max_rows = DESCRIPTION_LINES;
    }
    let galley = ui.fonts_mut(|f| f.layout_job(job.clone()));
    let cut = galley.elided;
    if open {
        let height = (ui.available_height() - 24.0).max(40.0);
        egui::ScrollArea::vertical().id_salt("nam-t3k-description").max_height(height).show(ui, |ui| {
            ui.add(egui::Label::new(job).wrap());
        });
    } else {
        ui.add(egui::Label::new(galley));
    }
    if (cut || open) && ui.link(if open { "less" } else { "more" }).clicked() {
        app.nam.remote.description_open = !open;
    }
}
