//! The guitar rig window: the rig's signal chain drawn left to right (gate, amp, cab, EQ,
//! output) with a switch per stage, the picked stage's controls under it, and for the amp and
//! the cab the model or impulse response browser with its TONE3000 previews.

use egui::{Align2, Color32, CornerRadius, Id, Rect, Sense, Stroke, Ui, pos2, vec2};
use scythe_engine::rig::{RigSettings, Stage, cab_wanted};
use scythe_session::rig::{RigAt, SNAPSHOT_NAMES, SNAPSHOTS, list_presets, presets_dir, save_preset};

use crate::app::ScytheApp;
use crate::nam::Target;
use crate::nam::library::Kind;
use crate::theme::{self, ACCENT, BG, RAISED, RAISED_HI, TEXT, TEXT_DIM, TEXT_MID};
use crate::ui::nam::{browser, controls, end_block, header, preview_bar, trim};
use crate::ui::widgets::{self, knob, switch, text_pill};

const CHAIN_H: f32 = 92.0;
const KNOB_W: f32 = 64.0;
const KNOBS_H: f32 = 70.0;
const LOW_CUT: (f64, f64) = (20.0, 400.0);
const HIGH_CUT: (f64, f64) = (1500.0, 20000.0);

pub fn body(app: &mut ScytheApp, ui: &mut Ui, target: &Target, at: RigAt) {
    let Some(s) = app.rig_settings(at) else { return };
    top_bar(app, ui, target, at, &s);
    ui.add_space(8.0);
    chain(app, ui, target, at, &s);
    ui.add_space(10.0);
    let preview_ir = app.nam.preview.as_ref().filter(|p| p.target == *target).map(|p| p.ir);
    match app.nam.section {
        Stage::Amp => {
            header(app, ui, target);
            ui.add_space(6.0);
            controls(app, ui, target, false);
            if preview_ir == Some(false) {
                ui.add_space(6.0);
                preview_bar(app, ui);
            }
            ui.add_space(8.0);
            browser(app, ui, target, Some(Kind::Model));
        }
        Stage::Cab => {
            cab(app, ui, target, at, &s);
            if preview_ir == Some(true) {
                ui.add_space(6.0);
                preview_bar(app, ui);
            }
            ui.add_space(8.0);
            browser(app, ui, target, Some(Kind::Ir));
        }
        Stage::Gate => gate(app, ui, at, &s),
        Stage::Eq => eq(app, ui, at, &s),
        Stage::Output => output(app, ui, at, &s),
    }
}

fn db_text(v: f64) -> String {
    format!("{}{} dB", if v > 0.0 { "+" } else { "" }, trim(v))
}

fn hz_text(v: f64) -> String {
    if v >= 1000.0 { format!("{} kHz", trim(v / 1000.0)) } else { format!("{v:.0} Hz") }
}

fn file_name(path: &str) -> String {
    std::path::Path::new(path).file_stem().map_or_else(|| path.to_owned(), |s| s.to_string_lossy().into_owned())
}

/// The library's name for a file, else its file name.
fn shown_name(app: &ScytheApp, path: &str) -> String {
    let resolved = app.nam_resolve(path);
    app.nam.library.entry(&resolved).map_or_else(|| file_name(path), |e| e.name.clone())
}

/// The rig's name, its presets and its four snapshots.
fn top_bar(app: &mut ScytheApp, ui: &mut Ui, target: &Target, at: RigAt, s: &RigSettings) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 50.0), Sense::hover());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, CornerRadius::same(14), RAISED);
    let name = app
        .project
        .track(at.track)
        .and_then(|t| t.fx.get(at.index))
        .and_then(scythe_engine::rig::container_name)
        .unwrap_or_else(|| "Rig".into());
    let extras = app.rig_extras(at);
    let size = 32.0;
    let cy = rect.center().y;
    let mut sx = rect.right() - 10.0 - SNAPSHOTS as f32 * (size + 6.0) + 6.0;
    for (n, label) in SNAPSHOT_NAMES.iter().enumerate() {
        let r = Rect::from_min_size(pos2(sx, cy - size / 2.0), vec2(size, size));
        let snap = extras.snapshot(n);
        let (bg, fg) = match (snap, snap == Some(s)) {
            (_, true) => (ACCENT, BG),
            (Some(_), false) => (RAISED_HI, TEXT),
            (None, false) => (BG, TEXT_DIM),
        };
        let tip = match snap {
            Some(snap) => format!(
                "{label}: {}\nClick to switch the whole rig (Alt+{}), Shift+click to store, right-click for more",
                scythe_engine::rig::default_name(snap),
                n + 1
            ),
            None => format!("{label}: empty. Click to store the whole rig: amp, cab, gate, EQ and levels"),
        };
        let resp = text_pill(ui, r, label, theme::semibold(14.0), bg, fg).on_hover_text(tip);
        if resp.clicked() {
            if snap.is_none() || ui.input(|i| i.modifiers.shift) {
                app.nam_store_snapshot(target, n);
            } else {
                app.nam_recall_snapshot(target, n);
            }
        }
        resp.context_menu(|ui| {
            if ui.button("Store the rig").clicked() {
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
    let first = rect.right() - 10.0 - SNAPSHOTS as f32 * (size + 6.0) + 6.0;
    let presets = Rect::from_min_max(pos2(first - 112.0, cy - 14.0), pos2(first - 12.0, cy + 14.0));
    let label = extras.preset.clone().unwrap_or_else(|| "Presets".into());
    let label = widgets::truncate(ui, &label, &theme::semibold(12.0), presets.width() - 16.0);
    let resp = text_pill(ui, presets, &label, theme::semibold(12.0), BG, TEXT)
        .on_hover_text("Rig presets: the whole rig, loadable on any track");
    egui::Popup::menu(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        ui.set_min_width(220.0);
        presets_menu(app, ui, target, at, s);
    });
    let text_right = presets.left() - 12.0;
    let left = rect.left() + 14.0;
    let title = widgets::truncate(ui, &name, &theme::semibold(16.0), text_right - left);
    painter.text(pos2(left, cy - 8.0), Align2::LEFT_CENTER, title, theme::semibold(16.0), TEXT);
    let line = "One slot in the effects list; REAPER opens it as an FX container";
    let line = widgets::truncate(ui, line, &theme::regular(11.0), text_right - left);
    painter.text(pos2(left, cy + 11.0), Align2::LEFT_CENTER, line, theme::regular(11.0), TEXT_DIM);
    end_block(ui, rect);
}

fn presets_menu(app: &mut ScytheApp, ui: &mut Ui, target: &Target, at: RigAt, s: &RigSettings) {
    let dir = presets_dir();
    let presets = list_presets(&dir);
    if presets.is_empty() {
        ui.label(egui::RichText::new("No presets yet").color(TEXT_DIM));
    }
    for p in &presets {
        ui.horizontal(|ui| {
            if ui.button(&p.name).on_hover_text(scythe_engine::rig::default_name(&p.settings)).clicked() {
                app.rig_load_preset(target, p);
                ui.close();
            }
            if ui.small_button("×").on_hover_text("Delete the preset").clicked() {
                if let Err(e) = scythe_session::rig::delete_preset(&dir, &p.name) {
                    app.notify(format!("Could not delete {}: {e}", p.name));
                }
                ui.close();
            }
        });
    }
    ui.separator();
    let id = Id::unique(("rig-preset-name", at.track, at.index));
    let mut name = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_else(|| {
        app.rig_extras(at).preset.unwrap_or_else(|| scythe_engine::rig::default_name(s).replace("Rig: ", ""))
    });
    ui.horizontal(|ui| {
        let edit = ui.add(egui::TextEdit::singleline(&mut name).hint_text("Preset name").desired_width(150.0));
        let entered = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if ui.button("Save").clicked() || entered {
            match save_preset(&dir, &name, s) {
                Ok(_) => {
                    let saved = name.trim().to_owned();
                    app.rig_mark_preset(at, &saved);
                    app.notify(format!("Saved rig preset {saved}"));
                    ui.data_mut(|d| d.remove::<String>(id));
                    ui.close();
                    return;
                }
                Err(e) => app.notify(format!("Could not save the preset: {e}")),
            }
        }
        ui.data_mut(|d| d.insert_temp(id, name.clone()));
    });
}

/// What the cab does about the amp model's gear type, and why.
fn cab_note(app: &ScytheApp, target: &Target, s: &RigSettings) -> (String, Color32) {
    if !s.cab.auto {
        return ("set by hand".into(), TEXT_DIM);
    }
    let gear =
        app.nam_model_path(target).and_then(|p| app.nam.library.entry(&p).and_then(|e| e.gear_type.clone())).or_else(
            || app.nam.preview.as_ref().filter(|p| p.target == *target && !p.ir).and_then(|p| p.tone.gear.clone()),
        );
    match gear.as_deref().and_then(cab_wanted) {
        Some(false) => ("auto off: capture has a cab".into(), theme::YELLOW),
        Some(true) => ("auto on: amp-only capture".into(), theme::GREEN),
        None => ("auto: follows the amp".into(), TEXT_DIM),
    }
}

/// The signal chain: a card per stage, joined by arrows; a click picks the stage below.
fn chain(app: &mut ScytheApp, ui: &mut Ui, target: &Target, at: RigAt, s: &RigSettings) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), CHAIN_H), Sense::hover());
    let painter = ui.painter().clone();
    let gap = 22.0;
    let n = Stage::ALL.len() as f32;
    let w = ((rect.width() - gap * (n - 1.0)) / n).max(60.0);
    let (note, note_color) = cab_note(app, target, s);
    for (k, stage) in Stage::ALL.into_iter().enumerate() {
        let r = Rect::from_min_size(pos2(rect.left() + k as f32 * (w + gap), rect.top()), vec2(w, CHAIN_H));
        let on = match stage {
            Stage::Gate => s.gate.enabled,
            Stage::Amp => s.amp.enabled,
            Stage::Cab => s.cab.enabled,
            Stage::Eq => s.eq.enabled,
            Stage::Output => true,
        };
        let picked = app.nam.section == stage;
        let resp = ui.interact(r, Id::unique(("rig-stage", at.track, at.index, k)), Sense::click());
        let bg = if picked {
            RAISED_HI
        } else if resp.hovered() {
            theme::with_alpha(RAISED_HI, 200)
        } else {
            RAISED
        };
        painter.rect_filled(r, CornerRadius::same(12), bg);
        if picked {
            painter.rect_stroke(r, CornerRadius::same(12), Stroke::new(1.5, ACCENT), egui::StrokeKind::Inside);
        }
        if k > 0 {
            let y = r.center().y;
            let (a, b) = (r.left() - gap + 3.0, r.left() - 4.0);
            let color = if on { TEXT_MID } else { TEXT_DIM };
            painter.line_segment([pos2(a, y), pos2(b, y)], Stroke::new(2.0, color));
            painter.add(egui::Shape::convex_polygon(
                vec![pos2(b + 1.0, y), pos2(b - 6.0, y - 5.0), pos2(b - 6.0, y + 5.0)],
                color,
                Stroke::NONE,
            ));
        }
        let fg = if on { TEXT } else { TEXT_DIM };
        painter.text(
            r.left_top() + vec2(12.0, 16.0),
            Align2::LEFT_CENTER,
            stage.label(),
            theme::semibold(12.0),
            if on { ACCENT } else { TEXT_DIM },
        );
        let mut switched = false;
        if stage != Stage::Output {
            let sw = Rect::from_min_size(pos2(r.right() - 10.0 - 30.0, r.top() + 8.0), vec2(30.0, 16.0));
            let tip = match stage {
                Stage::Cab => "Cab on or off (switching it by hand stops it following the amp model)",
                _ => "Stage on or off (bypass)",
            };
            if switch(ui, sw, on, ACCENT).on_hover_text(tip).clicked() {
                app.set_rig_stage(at, stage, !on);
                app.engine.wake_effects();
                switched = true;
            }
        }
        let (main, sub) = match stage {
            Stage::Gate => (db_text(s.gate.threshold_db), "threshold".to_owned()),
            Stage::Amp => (
                previewed(app, target, false)
                    .or_else(|| s.amp.model.as_deref().map(|m| shown_name(app, m)))
                    .unwrap_or_else(|| "No model".into()),
                format!("in {} · out {}", db_text(s.amp.input_db), db_text(s.amp.output_db)),
            ),
            Stage::Cab => {
                let first = previewed(app, target, true).or_else(|| s.cab.ir.as_deref().map(|a| shown_name(app, a)));
                let main = match (first, &s.cab.ir2) {
                    (Some(a), Some(b)) => format!("{a} + {}", shown_name(app, b)),
                    (Some(a), None) => a,
                    (None, _) => "No impulse response".into(),
                };
                (main, note.clone())
            }
            Stage::Eq => {
                let cuts = match (s.eq.low_cut, s.eq.high_cut) {
                    (Some(l), Some(h)) => format!("{} – {}", hz_text(l), hz_text(h)),
                    (Some(l), None) => format!("cut below {}", hz_text(l)),
                    (None, Some(h)) => format!("cut above {}", hz_text(h)),
                    (None, None) => "no cuts".into(),
                };
                (
                    cuts,
                    format!(
                        "bass {} · mid {} · treble {}",
                        trim(s.eq.bass_db),
                        trim(s.eq.mid_db),
                        trim(s.eq.treble_db)
                    ),
                )
            }
            Stage::Output => (db_text(s.output_db), "level".to_owned()),
        };
        let inner = r.width() - 24.0;
        let main = widgets::truncate(ui, &main, &theme::semibold(13.0), inner);
        painter.text(r.left_top() + vec2(12.0, 46.0), Align2::LEFT_CENTER, main, theme::semibold(13.0), fg);
        let sub_color = if stage == Stage::Cab { note_color } else { TEXT_DIM };
        let sub = widgets::truncate(ui, &sub, &theme::regular(11.0), inner);
        painter.text(r.left_top() + vec2(12.0, 68.0), Align2::LEFT_CENTER, sub, theme::regular(11.0), sub_color);
        if resp.clicked() && !switched {
            app.nam.section = stage;
        }
    }
    end_block(ui, rect);
}

/// A row of knobs; each returns its new value while it is dragged.
struct Row {
    rect: Rect,
    x: f32,
}

impl Row {
    fn new(ui: &mut Ui) -> Self {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), KNOBS_H), Sense::hover());
        Self { x: rect.left(), rect }
    }

    #[allow(clippy::too_many_arguments)]
    fn knob(
        &mut self,
        app: &mut ScytheApp,
        ui: &mut Ui,
        at: RigAt,
        id: &str,
        label: &str,
        value: f64,
        range: (f64, f64),
        default: f64,
        text: &str,
        tip: &str,
        set: impl FnOnce(&mut RigSettings, f64),
    ) {
        let r = Rect::from_min_size(pos2(self.x, self.rect.top()), vec2(KNOB_W - 4.0, KNOBS_H));
        self.x += KNOB_W;
        let (resp, changed) =
            knob(ui, r, Id::unique(("rig-knob", at.track, at.index, id)), value, range, default, label, text);
        let resp = resp.on_hover_text(tip);
        if let Some(v) = changed {
            app.rig_change(at, label, resp.dragged(), |s| set(s, v));
        }
        if resp.drag_stopped() {
            app.end_gesture();
        }
    }

    /// A knob over a frequency, turned on a log scale; the end of the range switches it off.
    #[allow(clippy::too_many_arguments)]
    fn cut(
        &mut self,
        app: &mut ScytheApp,
        ui: &mut Ui,
        at: RigAt,
        label: &str,
        value: Option<f64>,
        range: (f64, f64),
        off_at_top: bool,
        set: impl FnOnce(&mut RigSettings, Option<f64>),
    ) {
        let off = if off_at_top { range.1 } else { range.0 };
        let v = value.unwrap_or(off).clamp(range.0, range.1).log2();
        let text = value.map_or_else(|| "off".into(), hz_text);
        let tip = if off_at_top {
            "Turn fully right to switch the cut off; double-click for off"
        } else {
            "Turn fully left to switch the cut off; double-click for off"
        };
        self.knob(app, ui, at, label, label, v, (range.0.log2(), range.1.log2()), off.log2(), &text, tip, |s, v| {
            let hz = 2f64.powf(v).round();
            let is_off = if off_at_top { hz >= range.1 - 1.0 } else { hz <= range.0 + 1.0 };
            set(s, (!is_off).then_some(hz));
        });
    }

    fn end(self, ui: &mut Ui) -> Rect {
        end_block(ui, self.rect);
        Rect::from_min_max(pos2(self.x + 8.0, self.rect.top()), self.rect.max)
    }
}

fn cab(app: &mut ScytheApp, ui: &mut Ui, target: &Target, at: RigAt, s: &RigSettings) {
    let mut row = Row::new(ui);
    let level_tip = "The cab's level; drag, Shift for fine steps, double-click for 0 dB";
    row.knob(
        app,
        ui,
        at,
        "cab-level",
        "Level",
        s.cab.level_db,
        (-24.0, 12.0),
        0.0,
        &db_text(s.cab.level_db),
        level_tip,
        |s, v| s.cab.level_db = v,
    );
    if s.cab.ir2.is_some() {
        let text = format!("{:.0}%", s.cab.blend * 100.0);
        row.knob(
            app,
            ui,
            at,
            "cab-blend",
            "Blend",
            s.cab.blend,
            (0.0, 1.0),
            0.5,
            &text,
            "Share of the second IR",
            |s, v| s.cab.blend = v,
        );
    }
    row.cut(app, ui, at, "Low cut", s.eq.low_cut, LOW_CUT, false, |s, v| {
        s.eq.low_cut = v;
        s.eq.enabled = true;
    });
    row.cut(app, ui, at, "High cut", s.eq.high_cut, HIGH_CUT, true, |s, v| {
        s.eq.high_cut = v;
        s.eq.enabled = true;
    });
    let rest = row.end(ui);
    let previewing = previewed(app, target, true);
    let line = |k: usize, label: &str, file: Option<&str>| -> egui::Response {
        let r = Rect::from_min_size(pos2(rest.left(), rest.top() + 4.0 + k as f32 * 22.0), vec2(rest.width(), 20.0));
        let shown = if k == 0 { previewing.clone() } else { None };
        let name = shown.unwrap_or_else(|| file.map_or_else(|| "none".into(), |f| shown_name(app, f)));
        let text = widgets::truncate(ui, &format!("{label}  {name}"), &theme::regular(12.0), r.width() - 90.0);
        ui.painter().text(
            r.left_center(),
            Align2::LEFT_CENTER,
            text,
            theme::regular(12.0),
            if file.is_some() { TEXT } else { TEXT_DIM },
        );
        ui.interact(r, Id::unique(("rig-ir-line", k, at.track, at.index)), Sense::hover())
    };
    line(0, "IR 1", s.cab.ir.as_deref()).on_hover_text(s.cab.ir.clone().unwrap_or_default());
    line(1, "IR 2", s.cab.ir2.as_deref()).on_hover_text(
        "A second impulse response mixed in with Blend: right-click an IR in the list for \"Load as second IR\"",
    );
    if s.cab.ir2.is_some() {
        let r = Rect::from_min_size(pos2(rest.right() - 76.0, rest.top() + 25.0), vec2(72.0, 18.0));
        if text_pill(ui, r, "Remove", theme::regular(11.0), BG, TEXT_MID).clicked() {
            app.rig_change(at, "Remove second IR", false, |s| {
                s.cab.ir2 = None;
                s.cab.blend = 0.0;
            });
        }
    }
    let (note, color) = cab_note(app, target, s);
    let r = Rect::from_min_size(pos2(rest.left(), rest.top() + 50.0), vec2(rest.width(), 18.0));
    let follow = Rect::from_min_size(pos2(r.right() - 110.0, r.top()), vec2(106.0, 18.0));
    ui.painter().text(r.left_center(), Align2::LEFT_CENTER, format!("Cab {note}"), theme::regular(11.0), color);
    let (bg, fg) = if s.cab.auto { (theme::with_alpha(ACCENT, 70), TEXT) } else { (BG, TEXT_DIM) };
    if text_pill(ui, follow, "Follow amp", theme::regular(11.0), bg, fg)
        .on_hover_text(
            "On: a capture with a cabinet (amp-cab, full rig) switches the cab off, an amp-only capture switches it \
             on. Switching the cab by hand turns this off.",
        )
        .clicked()
    {
        app.rig_change(at, "Cab follows amp", false, |s| s.cab.auto = !s.cab.auto);
    }
}

fn gate(app: &mut ScytheApp, ui: &mut Ui, at: RigAt, s: &RigSettings) {
    let mut row = Row::new(ui);
    let tip = "The gate closes below this level; drag, Shift for fine steps";
    row.knob(
        app,
        ui,
        at,
        "gate",
        "Threshold",
        s.gate.threshold_db,
        (-90.0, 0.0),
        -50.0,
        &db_text(s.gate.threshold_db),
        tip,
        |s, v| {
            s.gate.threshold_db = v;
            s.gate.enabled = true;
        },
    );
    let rest = row.end(ui);
    note(
        ui,
        rest,
        "A noise gate before the amp (ReaGate): it keeps a high-gain amp quiet between notes. Its attack, \
         release and hysteresis are in ReaGate's own editor in the container.",
    );
}

fn eq(app: &mut ScytheApp, ui: &mut Ui, at: RigAt, s: &RigSettings) {
    let mut row = Row::new(ui);
    row.cut(app, ui, at, "Low cut", s.eq.low_cut, LOW_CUT, false, |s, v| s.eq.low_cut = v);
    let tone = "Drag, Shift for fine steps, double-click for 0 dB";
    row.knob(app, ui, at, "bass", "Bass", s.eq.bass_db, (-12.0, 12.0), 0.0, &db_text(s.eq.bass_db), tone, |s, v| {
        s.eq.bass_db = v
    });
    row.knob(app, ui, at, "mid", "Mid", s.eq.mid_db, (-12.0, 12.0), 0.0, &db_text(s.eq.mid_db), tone, |s, v| {
        s.eq.mid_db = v
    });
    row.knob(
        app,
        ui,
        at,
        "treble",
        "Treble",
        s.eq.treble_db,
        (-12.0, 12.0),
        0.0,
        &db_text(s.eq.treble_db),
        tone,
        |s, v| s.eq.treble_db = v,
    );
    row.cut(app, ui, at, "High cut", s.eq.high_cut, HIGH_CUT, true, |s, v| s.eq.high_cut = v);
    let rest = row.end(ui);
    note(ui, rest, "After the cab (ReaEQ): low and high cut, bass at 120 Hz, mid at 800 Hz and treble from 3.2 kHz.");
}

fn output(app: &mut ScytheApp, ui: &mut Ui, at: RigAt, s: &RigSettings) {
    let mut row = Row::new(ui);
    let tip = "The rig's output level; drag, Shift for fine steps, double-click for 0 dB";
    row.knob(app, ui, at, "out", "Level", s.output_db, (-24.0, 12.0), 0.0, &db_text(s.output_db), tip, |s, v| {
        s.output_db = v
    });
    let rest = row.end(ui);
    note(ui, rest, "The level the rig hands on, after the EQ; it stays when stages are switched off.");
}

fn note(ui: &mut Ui, rect: Rect, text: &str) {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(0.0, 8.0))));
    child.add(egui::Label::new(egui::RichText::new(text).color(TEXT_DIM)).wrap());
}

/// The name of what a running preview plays in the amp (or with `ir`, the cab).
fn previewed(app: &ScytheApp, target: &Target, ir: bool) -> Option<String> {
    let p = app.nam.preview.as_ref().filter(|p| p.target == *target && p.ir == ir && !p.comparing)?;
    Some(p.model().name.clone()).filter(|n| !n.is_empty())
}
