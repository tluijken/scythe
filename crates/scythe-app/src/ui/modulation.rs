//! Parameter modulation, link and learn for one effect parameter (REAPER's "Parameter
//! modulation/MIDI link" window and its Learn dialog), and the MIDI/OSC shortcuts to actions
//! shown in the settings window.

use egui::{RichText, Ui};
use scythe_core::TrackId;
use scythe_core::modulation::{
    self as m, LEARN_FOCUSED_FX, LEARN_LFO_RETRIGGER, LEARN_SELECTED_TRACK, LEARN_SOFT_TAKEOVER, LEARN_VISIBLE_FX,
    LFO_SHAPES, LearnMode, LinkSource, ParamLearn, ParamLink, ParamModulation,
};
use scythe_session::api::modulation::param_state;
use scythe_session::api::{Target, modulation::is_js};
use scythe_session::learn::{Heard, Trigger};

use crate::app::ScytheApp;
use crate::theme::{self, ACCENT, TEXT_DIM, TEXT_MID};
use crate::ui::window::{WindowKind, scythe_window};

/// The parameter the window shows.
#[derive(Debug, Clone, PartialEq)]
pub struct ModWindow {
    pub track: TrackId,
    pub fx: usize,
    pub param: usize,
    /// Waiting for a MIDI or OSC message to learn.
    learning: bool,
}

impl ModWindow {
    pub fn new(track: TrackId, fx: usize, param: usize) -> Self {
        Self { track, fx, param, learning: false }
    }
}

/// Whether a parameter has modulation or a binding, for the dot on its row.
pub fn has_controls(slot: &scythe_core::FxSlot, param: usize) -> bool {
    m::modulation(slot, param).is_some_and(|m| m.is_active()) || m::learn(slot, param).is_some()
}

/// The parameter's index in REAPER's order: a JSFX slider's number, else its position.
pub fn reaper_index(slot: &scythe_core::FxSlot, id: u32, position: usize) -> usize {
    if is_js(slot) { id as usize } else { position }
}

fn edit_modulation(app: &mut ScytheApp, w: &ModWindow, f: impl FnOnce(&mut ParamModulation)) {
    let Some(slot) = app.project.track(w.track).and_then(|t| t.fx.get(w.fx)).cloned() else { return };
    let mut md = m::modulation(&slot, w.param).unwrap_or_else(|| {
        let (name, value) = param_state(&app.session, Target::Track(w.track), w.fx, &slot, w.param);
        let shown = scythe_engine::stock::shown_name(&slot.name);
        let fx_name = shown.split_once(": ").map_or(&*shown, |(_, n)| n);
        let fx_name = fx_name.rsplit_once(" (").map_or(fx_name, |(n, _)| n);
        ParamModulation::new(w.param.to_string(), format!("{name} / {fx_name}"), value)
    });
    f(&mut md);
    let key = format!("mod-{}-{}-{}", w.track.0, w.fx, w.param);
    let (track, fx) = (w.track, w.fx);
    app.edit_merging("Parameter modulation", &key, |p| {
        if let Some(s) = p.track_mut(track).and_then(|t| t.fx.get_mut(fx)) {
            m::set_modulation(s, &md);
        }
    });
}

fn edit_learn(app: &mut ScytheApp, w: &ModWindow, f: impl FnOnce(&mut ParamLearn)) {
    let Some(slot) = app.project.track(w.track).and_then(|t| t.fx.get(w.fx)).cloned() else { return };
    let Some(mut l) = m::learn(&slot, w.param) else { return };
    f(&mut l);
    let (track, fx) = (w.track, w.fx);
    app.edit("Learn parameter", |p| {
        if let Some(s) = p.track_mut(track).and_then(|t| t.fx.get_mut(fx)) {
            m::set_learn(s, &l);
        }
    });
}

fn percent(ui: &mut Ui, value: &mut f64) -> bool {
    let mut pct = *value * 100.0;
    let changed = ui.add(egui::Slider::new(&mut pct, 0.0..=100.0).suffix(" %").max_decimals(1)).changed();
    if changed {
        *value = pct / 100.0;
    }
    changed
}

fn direction(ui: &mut Ui, id: &str, value: &mut i32) -> bool {
    let before = *value;
    egui::ComboBox::from_id_salt(id)
        .selected_text(match *value {
            1 => "Positive",
            -1 => "Negative",
            _ => "Centered",
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(value, 1, "Positive");
            ui.selectable_value(value, 0, "Centered");
            ui.selectable_value(value, -1, "Negative");
        });
    *value != before
}

pub fn show(app: &mut ScytheApp, ui: &mut Ui) {
    let Some(mut w) = app.view.modulation.take() else { return };
    let Some(slot) = app.project.track(w.track).and_then(|t| t.fx.get(w.fx)).cloned() else { return };
    let (name, value) = param_state(&app.session, Target::Track(w.track), w.fx, &slot, w.param);
    let range = app
        .engine
        .fx()
        .instance(Some(w.track), &slot, w.fx)
        .and_then(|i| i.with(|e| scythe_engine::modulation::native_range(e, w.param)))
        .unwrap_or((0.0, 1.0));
    let mut open = true;
    scythe_window("param-modulation", format!("Modulation · {name}"), WindowKind::Panel).default_width(420.0).show(
        ui.ctx(),
        &mut open,
        |ui| {
            let md = m::modulation(&slot, w.param);
            let current = md.clone().unwrap_or_else(|| ParamModulation::new(w.param.to_string(), "", value));
            let mut next = current.clone();
            let mut enabled = md.as_ref().is_some_and(|m| !m.disabled);
            let mut create = false;
            if ui.checkbox(&mut enabled, "Enable parameter modulation, baseline value").changed() {
                next.disabled = !enabled;
                create = md.is_none() && enabled;
            }
            ui.add(egui::Slider::new(&mut next.base, range.0..=range.1).text("Baseline"));
            ui.separator();
            lfo(ui, &mut next.lfo);
            ui.separator();
            audio(ui, &mut next.acs);
            ui.separator();
            link(app, ui, &w, &mut next);
            if next != current || create {
                let base_moved = next.base != current.base;
                edit_modulation(app, &w, |md| {
                    *md = ParamModulation { param: md.param.clone(), label: md.label.clone(), ..next }
                });
                if base_moved && let Some(inst) = app.engine.fx().instance(Some(w.track), &slot, w.fx) {
                    inst.with_mods(|s| s.clear_base(w.param));
                }
            }
            if md.is_some() && ui.button("Remove modulation").clicked() {
                let (track, fx, param) = (w.track, w.fx, w.param);
                app.edit("Remove parameter modulation", |p| {
                    if let Some(s) = p.track_mut(track).and_then(|t| t.fx.get_mut(fx)) {
                        m::remove_modulation(s, param);
                    }
                });
            }
            ui.separator();
            learn(app, ui, &mut w, &slot);
        },
    );
    if open {
        app.view.modulation = Some(w);
    } else if w.learning {
        app.listen_for_control(false);
    }
}

fn lfo(ui: &mut Ui, l: &mut m::Lfo) {
    ui.checkbox(&mut l.active, RichText::new("LFO").strong());
    if !l.active {
        return;
    }
    egui::Grid::new("mod-lfo").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Shape");
        egui::ComboBox::from_id_salt("lfo-shape").selected_text(LFO_SHAPES[l.shape.min(5) as usize]).show_ui(
            ui,
            |ui| {
                for (i, s) in LFO_SHAPES.iter().enumerate() {
                    ui.selectable_value(&mut l.shape, i as u8, *s);
                }
            },
        );
        ui.end_row();
        ui.label("Speed");
        ui.horizontal(|ui| {
            if l.tempo_sync {
                let mut qn = l.period_qn();
                if ui.add(egui::Slider::new(&mut qn, 0.25..=8.0).suffix(" QN").logarithmic(true)).changed() {
                    l.speed = m::Lfo::speed_for_period_qn(qn);
                }
            } else {
                let mut hz = l.hz();
                if ui
                    .add(egui::Slider::new(&mut hz, m::LFO_MIN_HZ..=m::LFO_MAX_HZ).suffix(" Hz").logarithmic(true))
                    .changed()
                {
                    l.speed = m::Lfo::speed_for_hz(hz);
                }
            }
            ui.checkbox(&mut l.tempo_sync, "Tempo sync");
        });
        ui.end_row();
        ui.label("Strength");
        percent(ui, &mut l.strength);
        ui.end_row();
        ui.label("Phase");
        ui.add(egui::Slider::new(&mut l.phase, 0.0..=1.0));
        ui.end_row();
        ui.label("Direction");
        direction(ui, "lfo-dir", &mut l.direction);
        ui.end_row();
        ui.label("Phase reset");
        egui::ComboBox::from_id_salt("lfo-free")
            .selected_text(if l.free { "Free-running" } else { "On seek/loop (follows the position)" })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut l.free, false, "On seek/loop (follows the position)");
                ui.selectable_value(&mut l.free, true, "Free-running");
            });
        ui.end_row();
    });
}

fn audio(ui: &mut Ui, a: &mut m::AudioControl) {
    ui.checkbox(&mut a.active, RichText::new("Audio control signal (sidechain)").strong());
    if !a.active {
        return;
    }
    egui::Grid::new("mod-acs").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Track audio channel");
        ui.horizontal(|ui| {
            let label = |c: i32, stereo: bool| match c {
                c if c < 0 => "None".to_owned(),
                c if stereo => format!("{}/{}", c + 1, c + 2),
                c => format!("{}", c + 1),
            };
            egui::ComboBox::from_id_salt("acs-chan").selected_text(label(a.channel, a.stereo)).show_ui(ui, |ui| {
                for c in -1..16 {
                    ui.selectable_value(&mut a.channel, c, label(c, a.stereo));
                }
            });
            ui.checkbox(&mut a.stereo, "Stereo");
        });
        ui.end_row();
        ui.label("Attack");
        ui.add(egui::Slider::new(&mut a.attack_ms, 0.0..=1000.0).suffix(" ms"));
        ui.end_row();
        ui.label("Release");
        ui.add(egui::Slider::new(&mut a.release_ms, 0.0..=1000.0).suffix(" ms"));
        ui.end_row();
        ui.label("Min volume");
        ui.add(egui::Slider::new(&mut a.db_low, -60.0..=12.0).suffix(" dB"));
        ui.end_row();
        ui.label("Max volume");
        ui.add(egui::Slider::new(&mut a.db_high, -60.0..=12.0).suffix(" dB"));
        ui.end_row();
        ui.label("Strength");
        percent(ui, &mut a.strength);
        ui.end_row();
        ui.label("Direction");
        direction(ui, "acs-dir", &mut a.direction);
        ui.end_row();
        ui.label("Curve");
        ui.horizontal(|ui| {
            ui.add(egui::Slider::new(&mut a.x2, 0.0..=1.0).text("x"));
            ui.add(egui::Slider::new(&mut a.y2, 0.0..=1.0).text("y"));
        });
        ui.end_row();
    });
}

fn link(app: &ScytheApp, ui: &mut Ui, w: &ModWindow, md: &mut ParamModulation) {
    let mut active = md.link_active && md.link.is_some();
    if ui.checkbox(&mut active, RichText::new("Link from MIDI or FX parameter").strong()).changed() {
        md.link_active = active;
        if active && md.link.is_none() {
            let source = if w.fx > 0 {
                LinkSource::Fx { fx: ParamLink::fx_token(w.fx - 1, w.fx), param: 0 }
            } else {
                LinkSource::Midi { bus: 0, channel: 0, status: 0xb0, data1: 1 }
            };
            md.link = Some(ParamLink { scale: 1.0, offset: 0.0, source, tail: String::new() });
        }
    }
    let Some(l) = md.link.as_mut().filter(|_| active) else { return };
    let chain: Vec<String> = app
        .project
        .track(w.track)
        .map(|t| t.fx.iter().map(|s| scythe_engine::stock::shown_name(&s.name).into_owned()).collect())
        .unwrap_or_default();
    egui::Grid::new("mod-link").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Source");
        let current = match &l.source {
            LinkSource::Midi { .. } => "MIDI".to_owned(),
            LinkSource::Fx { .. } => {
                l.source_fx(w.fx).and_then(|i| chain.get(i)).cloned().unwrap_or_else(|| "(missing)".into())
            }
        };
        egui::ComboBox::from_id_salt("link-src").selected_text(current).show_ui(ui, |ui| {
            if ui.selectable_label(matches!(l.source, LinkSource::Midi { .. }), "MIDI").clicked() {
                l.source = LinkSource::Midi { bus: 0, channel: 0, status: 0xb0, data1: 1 };
            }
            for (i, name) in chain.iter().enumerate().filter(|(i, _)| *i != w.fx) {
                if ui.selectable_label(l.source_fx(w.fx) == Some(i), format!("{}: {name}", i + 1)).clicked() {
                    l.source = LinkSource::Fx { fx: ParamLink::fx_token(i, w.fx), param: 0 };
                }
            }
        });
        ui.end_row();
        match &mut l.source {
            LinkSource::Fx { param, .. } => {
                ui.label("Parameter");
                let mut p = (*param).max(0) as usize;
                if ui.add(egui::DragValue::new(&mut p).range(0..=1023).prefix("#")).changed() {
                    *param = p as i64;
                }
                ui.end_row();
            }
            LinkSource::Midi { channel, status, data1, .. } => {
                ui.label("Message");
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("link-midi")
                        .selected_text(if *status & 0xf0 == 0xe0 { "Pitch bend" } else { "CC" })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(status, 0xb0, "CC");
                            ui.selectable_value(status, 0xe0, "Pitch bend");
                        });
                    if *status & 0xf0 == 0xb0 {
                        ui.add(egui::DragValue::new(data1).range(0..=127).prefix("CC "));
                    }
                    ui.add(
                        egui::DragValue::new(channel)
                            .range(0..=16)
                            .prefix("ch ")
                            .custom_formatter(|v, _| if v == 0.0 { "omni".into() } else { format!("{v}") }),
                    );
                });
                ui.end_row();
            }
        }
        ui.label("Offset");
        ui.add(egui::Slider::new(&mut l.offset, -1.0..=1.0));
        ui.end_row();
        ui.label("Scale");
        ui.add(egui::Slider::new(&mut l.scale, -1.0..=1.0));
        ui.end_row();
    });
}

fn learn(app: &mut ScytheApp, ui: &mut Ui, w: &mut ModWindow, slot: &scythe_core::FxSlot) {
    ui.label(RichText::new("MIDI/OSC learn").strong());
    let current = m::learn(slot, w.param);
    if w.learning {
        match app.learn.heard.clone() {
            Some(heard) => {
                w.learning = false;
                let spec = current.as_ref().map_or_else(|| w.param.to_string(), |l| l.param.clone());
                let mut l = match &heard {
                    Heard::Midi { status, data1, .. } => ParamLearn::midi(spec, *status, *data1, LearnMode::Absolute),
                    Heard::Osc { address } => {
                        let mut l = ParamLearn::midi(spec, 0, 0, LearnMode::Absolute);
                        l.osc = address.clone();
                        l
                    }
                };
                if let Some(old) = &current {
                    l.flags = old.flags;
                    if matches!(heard, Heard::Midi { .. }) {
                        l.set_mode(old.mode());
                    }
                }
                let (track, fx) = (w.track, w.fx);
                app.edit("Learn parameter", |p| {
                    if let Some(s) = p.track_mut(track).and_then(|t| t.fx.get_mut(fx)) {
                        m::set_learn(s, &l);
                    }
                });
                app.sync();
            }
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Move a control: waiting for a MIDI or OSC message…").color(TEXT_MID));
                    if ui.button("Cancel").clicked() {
                        w.learning = false;
                        app.listen_for_control(false);
                    }
                });
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
                return;
            }
        }
    }
    let current = app.project.track(w.track).and_then(|t| t.fx.get(w.fx)).and_then(|s| m::learn(s, w.param));
    ui.horizontal(|ui| {
        let text = match &current {
            Some(l) if l.status() >= 0x80 => scythe_session::learn::describe_midi(l.status(), l.data1()),
            Some(l) if !l.osc.is_empty() => format!("OSC {}", l.osc),
            _ => "Not learned".into(),
        };
        ui.label(RichText::new(text).color(if current.is_some() { ACCENT } else { TEXT_DIM }));
        if ui.button("Learn").on_hover_text("Bind the next MIDI or OSC message to this parameter").clicked() {
            w.learning = true;
            app.listen_for_control(true);
        }
        if current.is_some() && ui.button("Remove").clicked() {
            let (track, fx, param) = (w.track, w.fx, w.param);
            app.edit("Remove parameter learn", |p| {
                if let Some(s) = p.track_mut(track).and_then(|t| t.fx.get_mut(fx)) {
                    m::remove_learn(s, param);
                }
            });
        }
    });
    let Some(l) = current else { return };
    let mut next = l.clone();
    if l.status() >= 0x80 {
        let mut mode = l.mode();
        egui::ComboBox::from_id_salt("learn-mode").selected_text(mode.label()).show_ui(ui, |ui| {
            for c in LearnMode::CHOICES {
                ui.selectable_value(&mut mode, c, c.label());
            }
        });
        next.set_mode(mode);
    }
    for (bit, text) in [
        (LEARN_SOFT_TAKEOVER, "Soft takeover (absolute mode only)"),
        (LEARN_SELECTED_TRACK, "Enable only when track is selected"),
        (LEARN_FOCUSED_FX, "Enable only when effect window is focused"),
        (LEARN_VISIBLE_FX, "Enable only when effect window is open"),
        (LEARN_LFO_RETRIGGER, "Retrigger the LFO instead of setting the value"),
    ] {
        let mut on = next.flags & bit != 0;
        if ui.checkbox(&mut on, text).changed() {
            next.flags = if on { next.flags | bit } else { next.flags & !bit };
        }
    }
    if next != l {
        edit_learn(app, w, |x| *x = next);
    }
    ui.label(
        RichText::new(
            "Learned controls come from MIDI inputs enabled for control and OSC devices that allow learning.",
        )
        .color(TEXT_DIM)
        .font(theme::regular(11.0)),
    );
}

/// The MIDI/OSC shortcuts to actions, with a Learn button, for the settings window.
#[derive(Debug, Clone, Default)]
pub struct ShortcutDraft {
    action: String,
    learning: bool,
}

pub fn shortcuts(app: &mut ScytheApp, ui: &mut Ui, draft: &mut ShortcutDraft) {
    ui.label(RichText::new("MIDI/OSC shortcuts to actions").strong());
    let names = action_names(&app.session);
    let label = |a: &str| names.iter().find(|(id, _)| id == a).map_or(a.to_owned(), |(_, n)| n.clone());
    let mut remove = None;
    for (i, b) in app.learn.bindings.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(b.trigger.describe()).color(ACCENT));
            ui.label(label(&b.action));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Remove").clicked() {
                    remove = Some(i);
                }
            });
        });
    }
    if let Some(i) = remove {
        let gone = app.learn.bindings[i].clone();
        app.unbind_actions(|b| *b != gone);
    }
    if draft.action.is_empty() {
        draft.action = names.first().map(|(id, _)| id.clone()).unwrap_or_default();
    }
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("shortcut-action").width(260.0).selected_text(label(&draft.action)).show_ui(
            ui,
            |ui| {
                for (id, name) in &names {
                    ui.selectable_value(&mut draft.action, id.clone(), name);
                }
            },
        );
        if draft.learning {
            ui.spinner();
            ui.label(RichText::new("Press a pedal or button…").color(TEXT_MID));
            if ui.button("Cancel").clicked() {
                draft.learning = false;
                app.listen_for_control(false);
            }
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
        } else if ui.button("Learn").on_hover_text("Bind the next MIDI or OSC message to this action").clicked() {
            draft.learning = true;
            app.listen_for_control(true);
        }
    });
    if draft.learning
        && let Some(heard) = app.learn.heard.take()
    {
        draft.learning = false;
        app.bind_action(Trigger::from(&heard), draft.action.clone());
    }
}

fn action_names(session: &scythe_session::Session) -> Vec<(String, String)> {
    scythe_session::actions::list(session).into_iter().map(|a| (a.command, a.name)).collect()
}
