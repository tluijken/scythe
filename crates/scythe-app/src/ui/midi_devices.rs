//! The virtual MIDI keyboard and the MIDI devices window (enabled ports, aliases, clock
//! output and external timecode synchronization).

use egui::{Align2, CornerRadius, Id, Rect, Sense, Ui, pos2, vec2};
use scythe_core::prefs::MidiPrefs;
use scythe_session::api::midi::{ChaseKind, MidiCommand, sync_info};

use crate::app::ScytheApp;
use crate::theme::{self, ACCENT, BG, PANEL, RAISED, TEXT, TEXT_DIM};
use crate::ui::widgets::{switch, text_pill};

/// What the keyboard remembers between frames: the held note, octave, velocity and channel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyboardState {
    pub held: Option<u8>,
    pub octave: i32,
    pub velocity: u8,
    pub channel: u8,
}

impl Default for KeyboardState {
    fn default() -> Self {
        Self { held: None, octave: 4, velocity: 96, channel: 1 }
    }
}

pub fn show(app: &mut ScytheApp, ui: &mut Ui) {
    keyboard(app, ui);
    devices(app, ui);
}

fn window(title: &str, id: &str, ui: &Ui) -> egui::Window<'static> {
    let frame = egui::Frame::window(ui.style()).fill(PANEL).corner_radius(CornerRadius::same(18));
    egui::Window::new(title.to_owned()).id(Id::unique(id.to_owned())).frame(frame).resizable(false).collapsible(false)
}

fn send_note(app: &mut ScytheApp, note: u8, velocity: u8) {
    let channel = app.view.keyboard.channel;
    let _ = scythe_session::api::midi::execute(
        &mut app.session,
        MidiCommand::Keyboard { note, velocity, channel: Some(channel) },
    );
}

fn keyboard(app: &mut ScytheApp, ui: &mut Ui) {
    if !app.view.keyboard_open {
        if let Some(note) = app.view.keyboard.held.take() {
            send_note(app, note, 0);
        }
        return;
    }
    let mut open = true;
    let ctx = ui.ctx().clone();
    window("Virtual MIDI keyboard", "virtual-keyboard", ui).open(&mut open).show(&ctx, |ui| {
        let state = app.view.keyboard;
        ui.horizontal(|ui| {
            let (bg, fg) = (RAISED, TEXT);
            let r = ui.allocate_space(vec2(0.0, 26.0)).1;
            let row = Rect::from_min_size(r.min, vec2(640.0, 26.0));
            if text_pill(ui, Rect::from_min_size(row.min, vec2(28.0, 26.0)), "‹", theme::semibold(14.0), bg, fg)
                .on_hover_text("Octave down")
                .clicked()
            {
                app.view.keyboard.octave = (state.octave - 1).max(0);
            }
            let label = format!("C{}", state.octave - 1);
            ui.painter().text(row.min + vec2(52.0, 13.0), Align2::CENTER_CENTER, label, theme::mono(12.0), TEXT);
            if text_pill(
                ui,
                Rect::from_min_size(row.min + vec2(76.0, 0.0), vec2(28.0, 26.0)),
                "›",
                theme::semibold(14.0),
                bg,
                fg,
            )
            .on_hover_text("Octave up")
            .clicked()
            {
                app.view.keyboard.octave = (state.octave + 1).min(8);
            }
            let ch = Rect::from_min_size(row.min + vec2(120.0, 0.0), vec2(70.0, 26.0));
            let resp = text_pill(ui, ch, &format!("Ch {}", state.channel), theme::mono(11.0), bg, fg)
                .on_hover_text("MIDI channel (click for the next, right-click for the previous)");
            if resp.clicked() {
                app.view.keyboard.channel = state.channel % 16 + 1;
            }
            if resp.secondary_clicked() {
                app.view.keyboard.channel = (state.channel + 14) % 16 + 1;
            }
            let vel = Rect::from_min_size(row.min + vec2(200.0, 0.0), vec2(90.0, 26.0));
            let resp =
                ui.interact(vel, Id::unique("vkb-velocity"), Sense::drag()).on_hover_text("Velocity (drag up or down)");
            ui.painter().rect_filled(vel, CornerRadius::same(13), bg);
            ui.painter().text(
                vel.center(),
                Align2::CENTER_CENTER,
                format!("Vel {}", state.velocity),
                theme::mono(11.0),
                fg,
            );
            if resp.dragged() {
                let v = state.velocity as f32 - resp.drag_delta().y;
                app.view.keyboard.velocity = v.clamp(1.0, 127.0) as u8;
            }
            if !app.keyboard_in_use() {
                ui.painter().text(
                    row.min + vec2(300.0, 13.0),
                    Align2::LEFT_CENTER,
                    "Arm a track with the keyboard as its MIDI input to hear it",
                    theme::regular(11.0),
                    TEXT_DIM,
                );
            }
        });
        ui.add_space(8.0);
        piano(app, ui);
    });
    if !open {
        app.view.keyboard_open = false;
    }
}

/// Three octaves of keys from the chosen octave; pressing one plays it until released.
fn piano(app: &mut ScytheApp, ui: &mut Ui) {
    const WHITE: [u8; 7] = [0, 2, 4, 5, 7, 9, 11];
    const OCTAVES: usize = 3;
    let (w, h) = (22.0, 110.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w * 7.0 * OCTAVES as f32, h), Sense::click_and_drag());
    let base = (app.view.keyboard.octave * 12).clamp(0, 127 - 12 * OCTAVES as i32) as u8;
    let black: Vec<(u8, Rect)> = (0..OCTAVES * 7)
        .filter(|k| ![2, 6].contains(&(k % 7)))
        .map(|k| {
            let note = base + (k / 7) as u8 * 12 + WHITE[k % 7] + 1;
            let x = rect.left() + (k + 1) as f32 * w - w * 0.3;
            (note, Rect::from_min_size(pos2(x, rect.top()), vec2(w * 0.6, h * 0.62)))
        })
        .collect();
    let white: Vec<(u8, Rect)> = (0..OCTAVES * 7)
        .map(|k| {
            let note = base + (k / 7) as u8 * 12 + WHITE[k % 7];
            (note, Rect::from_min_size(pos2(rect.left() + k as f32 * w, rect.top()), vec2(w - 1.0, h)))
        })
        .collect();
    let pointer = resp.interact_pointer_pos().filter(|_| resp.is_pointer_button_down_on());
    let under = pointer.and_then(|p| {
        black.iter().find(|(_, r)| r.contains(p)).or_else(|| white.iter().find(|(_, r)| r.contains(p))).map(|k| k.0)
    });
    let held = app.view.keyboard.held;
    if under != held {
        if let Some(note) = held {
            send_note(app, note, 0);
        }
        if let Some(note) = under {
            send_note(app, note, app.view.keyboard.velocity);
        }
        app.view.keyboard.held = under;
    }
    let painter = ui.painter();
    for (note, r) in &white {
        let fill = if Some(*note) == under { ACCENT } else { egui::Color32::from_gray(232) };
        painter.rect_filled(*r, CornerRadius { nw: 0, ne: 0, sw: 4, se: 4 }, fill);
        if note % 12 == 0 {
            let label = format!("C{}", *note as i32 / 12 - 1);
            painter.text(r.center_bottom() - vec2(0.0, 10.0), Align2::CENTER_CENTER, label, theme::mono(9.0), BG);
        }
    }
    for (note, r) in &black {
        let fill = if Some(*note) == under { ACCENT } else { BG };
        painter.rect_filled(*r, CornerRadius { nw: 0, ne: 0, sw: 3, se: 3 }, fill);
    }
}

fn devices(app: &mut ScytheApp, ui: &mut Ui) {
    if !app.view.midi_devices_open {
        return;
    }
    let mut open = true;
    let ctx = ui.ctx().clone();
    window("MIDI devices and sync", "midi-devices", ui).open(&mut open).show(&ctx, |ui| {
        ui.set_width(520.0);
        heading(ui, "Inputs");
        let inputs = app.midi_devices.inputs.clone();
        if inputs.is_empty() {
            note(ui, "No MIDI inputs found");
        }
        for p in inputs {
            let enabled = app.prefs.midi.input_enabled(p.number);
            let row = device_row(ui, &p, enabled, &[]);
            if row.0 {
                device_cmd(app, false, p.number, Some(!enabled), None);
            }
        }
        ui.add_space(6.0);
        heading(ui, "Outputs");
        let outputs = app.midi_devices.outputs.clone();
        if outputs.is_empty() {
            note(ui, "No MIDI outputs found");
        }
        for p in outputs {
            let m = app.prefs.midi;
            let enabled = m.output_enabled(p.number);
            let clock = MidiPrefs::bit(m.clock, p.number);
            let spp = !MidiPrefs::bit(m.clock_no_spp, p.number);
            let (toggle, extra) = device_row(ui, &p, enabled, &[("Clock", clock), ("SPP", spp)]);
            if toggle {
                device_cmd(app, true, p.number, Some(!enabled), None);
            }
            match extra {
                Some(0) => device_cmd(app, true, p.number, None, Some((Some(!clock), None))),
                Some(1) => device_cmd(app, true, p.number, None, Some((None, Some(!spp)))),
                _ => {}
            }
        }
        ui.add_space(4.0);
        let rescan = ui.allocate_space(vec2(120.0, 26.0)).1;
        if text_pill(ui, rescan, "Rescan", theme::semibold(12.0), RAISED, TEXT)
            .on_hover_text("Look for MIDI ports again; known devices keep their numbers")
            .clicked()
        {
            app.rescan_midi_devices();
        }
        ui.add_space(10.0);
        heading(ui, "External timecode synchronization");
        sync_section(app, ui);
    });
    if !open {
        app.view.midi_devices_open = false;
    }
}

fn heading(ui: &mut Ui, text: &str) {
    let r = ui.allocate_space(vec2(ui.available_width(), 22.0)).1;
    ui.painter().text(r.left_center(), Align2::LEFT_CENTER, text, theme::semibold(12.0), TEXT_DIM);
}

fn note(ui: &mut Ui, text: &str) {
    let r = ui.allocate_space(vec2(ui.available_width(), 18.0)).1;
    ui.painter().text(r.left_center(), Align2::LEFT_CENTER, text, theme::regular(11.0), TEXT_DIM);
}

/// A device: its number, name (with an alias field), the enable switch and extra pills.
/// Returns whether the switch was clicked and which pill was.
fn device_row(
    ui: &mut Ui,
    p: &scythe_engine::midi_io::MidiPort,
    enabled: bool,
    pills: &[(&str, bool)],
) -> (bool, Option<usize>) {
    let r = ui.allocate_space(vec2(ui.available_width(), 34.0)).1;
    ui.painter().rect_filled(r, CornerRadius::same(10), RAISED);
    ui.painter().text(
        r.left_center() + vec2(10.0, 0.0),
        Align2::LEFT_CENTER,
        format!("{:>2}", p.number + 1),
        theme::mono(11.0),
        TEXT_DIM,
    );
    let fg = if p.present { TEXT } else { TEXT_DIM };
    let name = if p.present { p.label().to_owned() } else { format!("{} (not present)", p.label()) };
    let name = crate::ui::widgets::truncate(ui, &name, &theme::regular(12.0), r.width() - 230.0);
    ui.painter().text(r.left_center() + vec2(36.0, 0.0), Align2::LEFT_CENTER, name, theme::regular(12.0), fg);
    let toggled =
        switch(ui, Rect::from_min_size(pos2(r.right() - 48.0, r.center().y - 9.0), vec2(36.0, 18.0)), enabled, ACCENT)
            .on_hover_text(if p.name != p.label() { p.name.as_str() } else { "Enable this device" })
            .clicked();
    let mut picked = None;
    for (k, (label, on)) in pills.iter().enumerate() {
        let pr = Rect::from_min_size(pos2(r.right() - 160.0 + k as f32 * 54.0, r.center().y - 10.0), vec2(50.0, 20.0));
        let fg = if *on { ACCENT } else { TEXT_DIM };
        if text_pill(ui, pr, label, theme::mono(10.0), BG, fg).clicked() {
            picked = Some(k);
        }
    }
    (toggled, picked)
}

fn device_cmd(
    app: &mut ScytheApp,
    output: bool,
    number: u8,
    enabled: Option<bool>,
    clock: Option<(Option<bool>, Option<bool>)>,
) {
    let (clock, spp) = clock.unwrap_or_default();
    let kind = if output {
        scythe_session::api::midi::DeviceKind::Output
    } else {
        scythe_session::api::midi::DeviceKind::Input
    };
    let command = MidiCommand::Device { kind, number, enabled, alias: None, clock, spp, reset: None };
    if let Err(e) = scythe_session::api::midi::execute(&mut app.session, command) {
        app.notify(e.message);
    }
}

fn sync_section(app: &mut ScytheApp, ui: &mut Ui) {
    let info = sync_info(&app.session);
    let row = ui.allocate_space(vec2(ui.available_width(), 30.0)).1;
    ui.painter().text(row.left_center(), Align2::LEFT_CENTER, "Chase incoming timecode", theme::regular(12.0), TEXT);
    let toggle = switch(
        ui,
        Rect::from_min_size(pos2(row.right() - 48.0, row.center().y - 9.0), vec2(36.0, 18.0)),
        info.enabled,
        ACCENT,
    )
    .on_hover_text("Start, move and stop with the timecode (REAPER: Toggle external timecode synchronization)")
    .clicked();
    let row = ui.allocate_space(vec2(ui.available_width(), 30.0)).1;
    let kinds = [(ChaseKind::Mtc, "MTC"), (ChaseKind::Ltc, "LTC"), (ChaseKind::Clock, "MIDI clock")];
    let mut source = None;
    for (k, (kind, label)) in kinds.iter().enumerate() {
        let r = Rect::from_min_size(row.min + vec2(k as f32 * 96.0, 2.0), vec2(90.0, 24.0));
        let (bg, fg) = if info.source == *kind { (ACCENT, BG) } else { (RAISED, TEXT_DIM) };
        if text_pill(ui, r, label, theme::semibold(11.0), bg, fg).clicked() {
            source = Some(*kind);
        }
    }
    let input_label = match info.source {
        ChaseKind::Ltc => format!("Audio in {}", info.channel + 1),
        _ => info.device.map_or("All MIDI inputs".to_owned(), |d| {
            app.midi_devices.input_label(scythe_core::midi_route::MidiDevice::Port(d))
        }),
    };
    let r = Rect::from_min_size(pos2(row.right() - 200.0, row.top() + 2.0), vec2(200.0, 24.0));
    let resp = text_pill(ui, r, &input_label, theme::mono(10.0), BG, TEXT)
        .on_hover_text("Where the timecode comes in (click for the next)");
    let mut device = None;
    let mut channel = None;
    if resp.clicked() {
        match info.source {
            ChaseKind::Ltc => channel = Some((info.channel + 1) % 8),
            _ => {
                let present: Vec<u8> = app.midi_devices.inputs.iter().filter(|p| p.present).map(|p| p.number).collect();
                device = Some(match info.device.and_then(|d| present.iter().position(|&n| n == d)) {
                    Some(i) if i + 1 < present.len() => present[i + 1],
                    Some(_) => 63,
                    None => present.first().copied().unwrap_or(63),
                });
            }
        }
    }
    let row = ui.allocate_space(vec2(ui.available_width(), 30.0)).1;
    ui.painter().text(
        row.left_center(),
        Align2::LEFT_CENTER,
        format!(
            "Offset {:.3} s · resync after {} ms · freewheel {} ms",
            info.offset, info.resync_ms, info.freewheel_ms
        ),
        theme::mono(11.0),
        TEXT_DIM,
    );
    if toggle || source.is_some() || device.is_some() || channel.is_some() {
        let command = MidiCommand::SyncSet {
            enabled: toggle.then_some(!info.enabled),
            source,
            device,
            channel,
            offset: None,
            resync_ms: None,
            freewheel_ms: None,
            frame_rate: None,
        };
        if let Err(e) = scythe_session::api::midi::execute(&mut app.session, command) {
            app.notify(e.message);
        }
    }
}
