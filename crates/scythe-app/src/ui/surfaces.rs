//! The control surfaces window: OSC devices, Mackie Control and its extenders, HUI. The list is
//! REAPER's (`csurf_<i>` lines in the preferences) and changes apply at once.

use egui::{RichText, Ui};
use scythe_core::surface::{MidiKind, MidiSurface, OSC_RECEIVE, OSC_SEND, OscSurface, Surface};

use crate::app::ScytheApp;
use crate::theme::{self, ACCENT, TEXT_DIM, TEXT_MID};
use crate::ui::window::{WindowKind, scythe_window};

/// The window's state while it is open: the surface being edited and its draft.
#[derive(Debug, Clone, Default)]
pub struct SurfacesWindow {
    pub editing: Option<(Option<usize>, Surface)>,
    inputs: Vec<String>,
    outputs: Vec<String>,
    patterns: Vec<String>,
    shortcut: crate::ui::modulation::ShortcutDraft,
}

impl SurfacesWindow {
    pub fn open() -> Self {
        let mut w = Self::default();
        w.refresh();
        w
    }

    fn refresh(&mut self) {
        self.inputs = scythe_surface::midi::input_names();
        self.outputs = scythe_surface::midi::output_names();
        self.patterns = scythe_surface::osc::pattern_names();
    }
}

pub fn show(app: &mut ScytheApp, ui: &mut Ui) {
    let Some(mut state) = app.view.surfaces.take() else { return };
    let mut open = true;
    scythe_window("control-surfaces", "Control surfaces", WindowKind::Panel).default_width(520.0).show(
        ui.ctx(),
        &mut open,
        |ui| {
            let info = app.engine.info().clone();
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Audio: {} · {} Hz", info.name, info.sample_rate)).color(TEXT_DIM));
                if ui
                    .button("Audio device...")
                    .on_hover_text("Choose the audio system, devices, rate and channels")
                    .clicked()
                {
                    crate::ui::audio_device::toggle(app);
                }
            });
            ui.separator();
            list(app, ui, &mut state);
            ui.separator();
            match state.editing.take() {
                Some((index, mut draft)) => {
                    editor(ui, &state, &mut draft);
                    ui.horizontal(|ui| {
                        if ui.button(if index.is_some() { "Apply" } else { "Add" }).clicked() {
                            let s = draft.clone();
                            app.change_preferences(|p| match index {
                                Some(i) if i < p.surfaces.len() => p.surfaces[i] = s,
                                _ => p.surfaces.push(s),
                            });
                        } else if !ui.button("Cancel").clicked() {
                            state.editing = Some((index, draft));
                        }
                    });
                }
                None => {
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Add OSC device").clicked() {
                            let port = 8000 + app.prefs.surfaces.len() as u16;
                            state.editing =
                                Some((None, Surface::Osc(OscSurface { local_port: port, ..Default::default() })));
                        }
                        for kind in [MidiKind::Mcu, MidiKind::McuExtender, MidiKind::Hui] {
                            if ui.button(format!("Add {}", kind.label())).clicked() {
                                let offset = if kind == MidiKind::McuExtender { 8 } else { 0 };
                                state.editing =
                                    Some((None, Surface::Midi(MidiSurface { offset, ..MidiSurface::new(kind, 0, 0) })));
                            }
                        }
                        if ui.button("Rescan devices").clicked() {
                            state.refresh();
                        }
                    });
                }
            }
            ui.separator();
            crate::ui::modulation::shortcuts(app, ui, &mut state.shortcut);
            ui.separator();
            ui.label(
                RichText::new(
                    "OSC actions: /action/<id> runs a REAPER command id; /action/str _SCYTHE_SETLIST_NEXT (or \
                 _PLAY, _STOP, _PLAY_STOP, _PREVIOUS) drives the setlist from a footswitch. On a Mackie \
                 Control, user switch A plays or stops the setlist and B goes to the next song.",
                )
                .color(TEXT_DIM)
                .font(theme::regular(11.0)),
            );
        },
    );
    if open {
        app.view.surfaces = Some(state);
    }
}

fn list(app: &mut ScytheApp, ui: &mut Ui, state: &mut SurfacesWindow) {
    let surfaces = app.prefs.surfaces.clone();
    if surfaces.is_empty() {
        ui.label(RichText::new("No control surfaces yet.").color(TEXT_MID));
    }
    let mut remove = None;
    for (i, s) in surfaces.iter().enumerate() {
        let line = s.line();
        let status = app.session.surface_status.iter().find(|st| st.line == line).cloned();
        ui.horizontal(|ui| {
            let (dot, tip) = match &status {
                Some(st) if st.running => (ACCENT, "Running".to_owned()),
                Some(st) => (egui::Color32::from_rgb(0xe0, 0x6c, 0x5c), st.error.clone().unwrap_or_default()),
                None => (TEXT_DIM, "Starting".to_owned()),
            };
            ui.label(RichText::new("●").color(dot)).on_hover_text(&tip);
            ui.label(describe(s, state));
            if let Some(err) = status.as_ref().and_then(|st| st.error.clone()) {
                ui.label(RichText::new(err).color(TEXT_DIM).font(theme::regular(11.0)));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Remove").clicked() {
                    remove = Some(i);
                }
                if ui.small_button("Edit").clicked() {
                    state.editing = Some((Some(i), s.clone()));
                }
            });
        });
    }
    if let Some(i) = remove {
        app.change_preferences(|p| {
            if i < p.surfaces.len() {
                p.surfaces.remove(i);
            }
        });
        state.editing = None;
    }
}

fn describe(s: &Surface, state: &SurfacesWindow) -> String {
    match s {
        Surface::Midi(m) => {
            let name = |list: &[String], i: i64| {
                usize::try_from(i).ok().and_then(|i| list.get(i).cloned()).unwrap_or_else(|| format!("device {i}"))
            };
            format!("{} · in {} · out {}", m.kind.label(), name(&state.inputs, m.input), name(&state.outputs, m.output))
        }
        other => other.describe(),
    }
}

fn editor(ui: &mut Ui, state: &SurfacesWindow, draft: &mut Surface) {
    egui::Grid::new("surface-editor").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| match draft {
        Surface::Osc(o) => {
            ui.label("Name");
            ui.text_edit_singleline(&mut o.name);
            ui.end_row();
            ui.label("Listen on port");
            ui.add(egui::DragValue::new(&mut o.local_port).range(1..=65535));
            ui.end_row();
            ui.label("Device address");
            ui.text_edit_singleline(&mut o.device_ip);
            ui.end_row();
            ui.label("Device port");
            ui.add(egui::DragValue::new(&mut o.device_port).range(1..=65535));
            ui.end_row();
            ui.label("Messages");
            ui.horizontal(|ui| {
                let mut receive = o.flags & OSC_RECEIVE != 0;
                let mut send = o.flags & OSC_SEND != 0;
                ui.checkbox(&mut receive, "Receive");
                ui.checkbox(&mut send, "Send feedback");
                o.flags =
                    (o.flags & !(OSC_RECEIVE | OSC_SEND)) | (receive as i64 * OSC_RECEIVE) | (send as i64 * OSC_SEND);
            });
            ui.end_row();
            ui.label("Pattern file");
            let shown = if o.pattern.is_empty() { "Default".to_owned() } else { o.pattern.clone() };
            egui::ComboBox::from_id_salt("osc-pattern").selected_text(shown).show_ui(ui, |ui| {
                ui.selectable_value(&mut o.pattern, String::new(), "Default");
                for p in &state.patterns {
                    ui.selectable_value(&mut o.pattern, p.clone(), p);
                }
            });
            ui.end_row();
        }
        Surface::Midi(m) => {
            ui.label("Type");
            egui::ComboBox::from_id_salt("midi-kind").selected_text(m.kind.label()).show_ui(ui, |ui| {
                for kind in [MidiKind::Mcu, MidiKind::McuExtender, MidiKind::Hui] {
                    ui.selectable_value(&mut m.kind, kind, kind.label());
                }
            });
            ui.end_row();
            for (label, list, value) in
                [("MIDI input", &state.inputs, &mut m.input), ("MIDI output", &state.outputs, &mut m.output)]
            {
                ui.label(label);
                let name =
                    usize::try_from(*value).ok().and_then(|i| list.get(i).cloned()).unwrap_or_else(|| "None".into());
                egui::ComboBox::from_id_salt(label).selected_text(name).show_ui(ui, |ui| {
                    ui.selectable_value(value, -1, "None");
                    for (i, n) in list.iter().enumerate() {
                        ui.selectable_value(value, i as i64, n);
                    }
                });
                ui.end_row();
            }
            ui.label("Track offset");
            ui.add(egui::DragValue::new(&mut m.offset).range(0..=1024))
                .on_hover_text("8 for an extender to the right of an MCU");
            ui.end_row();
        }
        Surface::Other { kind, config } => {
            ui.label("REAPER line");
            ui.label(format!("{kind} {config}"));
            ui.end_row();
        }
    });
}
