//! The audio device window, after REAPER's Preferences > Audio > Device page: the audio system,
//! the input and output devices with how many channels to open, the rate and block size, and
//! the names the channels go by. Apply opens the new device while the project stays open.

use egui::{RichText, Ui, vec2};
use scythe_core::prefs::AudioSystem;
use scythe_engine::DeviceRequest;
use scythe_engine::devices::{COMMON_BLOCK_SIZES, COMMON_RATES, Capabilities, DeviceEntry};

use crate::app::ScytheApp;
use crate::theme::{self, ACCENT, TEXT, TEXT_DIM, YELLOW};
use crate::ui::window::{WindowKind, scythe_window};

/// The window's state while it is open: what the host offers and the choice not yet applied.
#[derive(Debug, Clone, Default)]
pub struct AudioDeviceWindow {
    systems: Vec<String>,
    devices: Vec<DeviceEntry>,
    pub draft: Draft,
    outputs: Option<Capabilities>,
    inputs: Option<Capabilities>,
    /// Why the last Apply failed.
    pub error: Option<String>,
    /// The channel names being edited: (input side, drafts).
    names: Option<(bool, Vec<String>)>,
}

/// The device page's fields.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Draft {
    /// The audio host (`ALSA`, `JACK`, `Dummy`).
    pub system: String,
    pub output: Option<String>,
    pub input: Option<String>,
    pub input_channels: u16,
    pub output_channels: u16,
    pub sample_rate: u32,
    pub block_size: u32,
}

impl Draft {
    /// The fields as the device playing now fills them.
    fn current(app: &ScytheApp) -> Self {
        let r = app.engine.devices();
        let info = app.engine.info();
        let system =
            r.host.clone().unwrap_or_else(|| if info.host.is_empty() { "ALSA".into() } else { info.host.clone() });
        Self {
            system,
            output: r.output.clone(),
            input: r.input.clone(),
            input_channels: r.input_channels.unwrap_or(info.inputs.len() as u16),
            output_channels: r.output_channels.unwrap_or(info.outputs.len().max(2) as u16),
            sample_rate: r.sample_rate.unwrap_or(info.sample_rate),
            block_size: r.buffer_size.or(info.block_size).unwrap_or(512),
        }
    }

    fn system(&self) -> Option<AudioSystem> {
        AudioSystem::from_host(&self.system)
    }

    /// What Apply asks the engine for. JACK's rate and block size are the server's.
    pub fn request(&self, no_input: bool) -> DeviceRequest {
        let jack = self.system() == Some(AudioSystem::Jack);
        let dummy = self.system() == Some(AudioSystem::Dummy);
        DeviceRequest {
            host: Some(self.system.clone()),
            output: self.output.clone().filter(|_| !jack && !dummy),
            input: self.input.clone().filter(|_| !jack && !dummy),
            sample_rate: (!jack).then_some(self.sample_rate),
            buffer_size: (!jack).then_some(self.block_size),
            output_channels: Some(if dummy { 2 } else { self.output_channels.max(1) }),
            input_channels: Some(if dummy { 0 } else { self.input_channels }),
            no_input,
        }
    }
}

impl AudioDeviceWindow {
    pub fn open(app: &ScytheApp) -> Self {
        let mut w = Self { draft: Draft::current(app), ..Default::default() };
        w.systems = scythe_engine::devices::hosts();
        w.devices = scythe_engine::devices::list();
        // A session on a silent clock opened no device on purpose (`--null-audio`); its window
        // opens none to ask either, until a device is chosen.
        if !app.engine.info().host.is_empty() {
            w.query();
        }
        w
    }

    fn refresh(&mut self) {
        self.systems = scythe_engine::devices::hosts();
        self.devices = scythe_engine::devices::list();
        self.query();
    }

    /// What the chosen devices can be opened with; a device that is busy (often the one
    /// playing) says nothing, and the common values are offered.
    fn query(&mut self) {
        let r = self.draft.request(false);
        self.outputs = scythe_engine::devices::capabilities(&r, false).ok();
        self.inputs = scythe_engine::devices::capabilities(&r, true).ok();
    }
}

pub fn toggle(app: &mut ScytheApp) {
    app.view.audio_device = match app.view.audio_device {
        Some(_) => None,
        None => Some(AudioDeviceWindow::open(app)),
    };
}

pub fn show(app: &mut ScytheApp, ui: &mut Ui) {
    let Some(mut state) = app.view.audio_device.take() else { return };
    let mut open = true;
    scythe_window("audio-device", "Audio device", WindowKind::Dialog).default_width(560.0).show(
        ui.ctx(),
        &mut open,
        |ui| {
            ui.set_width(560.0);
            device_page(app, ui, &mut state);
            ui.separator();
            channel_names(app, ui, &mut state);
        },
    );
    if open {
        app.view.audio_device = Some(state);
    }
}

fn device_page(app: &mut ScytheApp, ui: &mut Ui, state: &mut AudioDeviceWindow) {
    let before = state.draft.clone();
    let system = state.draft.system();
    egui::Grid::new("audio-device-grid").num_columns(2).spacing(vec2(12.0, 8.0)).show(ui, |ui| {
        ui.label("Audio system:");
        let shown = system.map_or(state.draft.system.clone(), |s| s.label().to_owned());
        egui::ComboBox::from_id_salt("audio-system")
            .selected_text(shown)
            .width(260.0)
            .show_ui(ui, |ui| {
                for host in &state.systems {
                    let label = AudioSystem::from_host(host).map_or(host.clone(), |s| s.label().to_owned());
                    if ui.selectable_label(state.draft.system.eq_ignore_ascii_case(host), label).clicked() {
                        if !state.draft.system.eq_ignore_ascii_case(host) {
                            (state.draft.output, state.draft.input) = (None, None);
                        }
                        state.draft.system = host.clone();
                    }
                }
            })
            .response
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Audio system"));
        ui.end_row();

        let dummy = system == Some(AudioSystem::Dummy);
        let jack = system == Some(AudioSystem::Jack);
        channels_row(ui, "Input channels:", &mut state.draft.input_channels, 0, state.inputs.as_ref(), !dummy);
        ui.end_row();
        if !jack && !dummy {
            ui.label("Input device:");
            device_combo(ui, "Input device", &state.devices, &state.draft.system, true, &mut state.draft.input);
            ui.end_row();
        }
        channels_row(ui, "Output channels:", &mut state.draft.output_channels, 1, state.outputs.as_ref(), !dummy);
        ui.end_row();
        if !jack && !dummy {
            ui.label("Output device:");
            device_combo(ui, "Output device", &state.devices, &state.draft.system, false, &mut state.draft.output);
            ui.end_row();
        }
        if !jack {
            let rates = state.outputs.as_ref().map(|c| c.sample_rates.clone()).filter(|r| !r.is_empty());
            let rates = rates.unwrap_or_else(|| COMMON_RATES.to_vec());
            ui.label("Sample rate:");
            choice(ui, "Sample rate", &rates, &mut state.draft.sample_rate, |r| format!("{r} Hz"));
            ui.end_row();
            let sizes: Vec<u32> = match state.outputs.as_ref().and_then(|c| c.buffer_sizes) {
                Some((min, max)) => COMMON_BLOCK_SIZES.into_iter().filter(|b| (min..=max).contains(b)).collect(),
                None => COMMON_BLOCK_SIZES.to_vec(),
            };
            let sizes = if sizes.is_empty() { COMMON_BLOCK_SIZES.to_vec() } else { sizes };
            ui.label("Blocksize:");
            choice(ui, "Blocksize", &sizes, &mut state.draft.block_size, |b| format!("{b} frames"));
            ui.end_row();
        }
    });
    if system == Some(AudioSystem::Jack) {
        note(ui, "Sample rate and block size come from the JACK server; Scythe connects to its system ports.");
    }
    if system == Some(AudioSystem::Dummy) {
        note(ui, "No device: playback runs on a silent clock, as REAPER's Dummy Audio.");
    }
    let changed_device = (&before.system, &before.output, &before.input)
        != (&state.draft.system, &state.draft.output, &state.draft.input);
    if changed_device {
        state.query();
        state.draft.input_channels = default_count(state.inputs.as_ref(), 2);
        state.draft.output_channels = default_count(state.outputs.as_ref(), 2);
    }

    ui.add_space(6.0);
    let info = app.engine.info().clone();
    let health = app.engine.health();
    let block = health.block_size.map_or("device default".to_owned(), |b| format!("{b} frames"));
    let latency = if health.output_latency > 0.0 {
        format!(" · {:.1} ms", health.output_latency * 1000.0)
    } else {
        String::new()
    };
    let now = format!(
        "Playing: {} · {} Hz · {block}{latency} · {} in / {} out",
        info.name,
        info.sample_rate,
        info.inputs.len(),
        info.outputs.len()
    );
    ui.label(RichText::new(now).color(if health.silent { YELLOW } else { TEXT }));
    if let Some(reason) = &health.reason {
        ui.label(RichText::new(reason).color(YELLOW).font(theme::regular(11.0)));
    }
    if let Some(e) = &state.error {
        ui.label(RichText::new(format!("Could not open the device: {e}")).color(theme::PINK));
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui.button("Apply").on_hover_text("Open these devices now; the project stays open").clicked() {
            apply(app, state);
        }
        if ui.button("Refresh").on_hover_text("Look for audio devices again").clicked() {
            state.refresh();
        }
        if ui.button("Reset").on_hover_text("Show what is playing now").clicked() {
            state.draft = Draft::current(app);
            state.error = None;
            state.query();
        }
    });
}

/// Applies the draft; on failure the previous device plays on and the reason shows.
pub fn apply(app: &mut ScytheApp, state: &mut AudioDeviceWindow) {
    let request = state.draft.request(app.no_input);
    app.close_plugin_windows();
    match app.change_audio_device(request, true) {
        Ok(()) => {
            state.error = None;
            state.names = None;
            let info = app.engine.info();
            let msg = format!("Audio: {} at {} Hz", info.name, info.sample_rate);
            app.notify(msg);
        }
        Err(e) => state.error = Some(e),
    }
}

fn default_count(caps: Option<&Capabilities>, fallback: u16) -> u16 {
    caps.map_or(fallback, |c| scythe_engine::devices::stream_channels(&c.channels, c.default_channels, None))
}

fn channels_row(ui: &mut Ui, label: &str, value: &mut u16, min: u16, caps: Option<&Capabilities>, enabled: bool) {
    ui.label(label);
    ui.horizontal(|ui| {
        let most = caps.and_then(|c| c.channels.iter().copied().max()).unwrap_or(64).max(min);
        let resp = ui.add_enabled(enabled, egui::DragValue::new(value).range(min..=most).speed(0.2));
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::SpinButton, enabled, label.trim_end_matches(':')));
        if let Some(c) = caps
            && let Some(m) = c.channels.iter().copied().max()
        {
            ui.label(RichText::new(format!("of {m}")).color(TEXT_DIM));
        }
    });
}

fn device_combo(
    ui: &mut Ui,
    name: &str,
    devices: &[DeviceEntry],
    host: &str,
    input: bool,
    chosen: &mut Option<String>,
) {
    let offered: Vec<&DeviceEntry> = devices
        .iter()
        .filter(|d| d.host.eq_ignore_ascii_case(host) && if input { d.input } else { d.output })
        .collect();
    let label = |d: &DeviceEntry| if d.id == d.name { d.id.clone() } else { format!("{} ; {}", d.id, d.name) };
    let shown = match chosen {
        None => "Default".to_owned(),
        Some(c) => offered.iter().find(|d| d.id == *c || d.name == *c).map_or(c.clone(), |d| label(d)),
    };
    egui::ComboBox::from_id_salt(name)
        .selected_text(shown)
        .width(380.0)
        .show_ui(ui, |ui| {
            if ui.selectable_label(chosen.is_none(), "Default").clicked() {
                *chosen = None;
            }
            for d in offered {
                if ui.selectable_label(chosen.as_deref() == Some(d.id.as_str()), label(d)).clicked() {
                    *chosen = Some(d.id.clone());
                }
            }
        })
        .response
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, name));
}

fn choice(ui: &mut Ui, name: &str, values: &[u32], value: &mut u32, label: impl Fn(u32) -> String) {
    let mut values = values.to_vec();
    if !values.contains(value) {
        values.push(*value);
        values.sort_unstable();
    }
    egui::ComboBox::from_id_salt(name)
        .selected_text(label(*value))
        .width(160.0)
        .show_ui(ui, |ui| {
            for v in values {
                ui.selectable_value(value, v, label(v));
            }
        })
        .response
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, name));
}

fn note(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).color(TEXT_DIM).font(theme::regular(11.0)));
}

/// REAPER's "Input/Output channel name aliasing": a switch per side and a list to name each
/// channel.
fn channel_names(app: &mut ScytheApp, ui: &mut Ui, state: &mut AudioDeviceWindow) {
    ui.label(RichText::new("Channel naming").color(TEXT_DIM).font(theme::semibold(12.0)));
    for input in [true, false] {
        ui.horizontal(|ui| {
            let mut on = app.channel_aliasing(input);
            let label = if input { "Input channel name aliasing" } else { "Output channel name aliasing" };
            if ui.checkbox(&mut on, label).changed() {
                app.set_channel_aliasing(input, on);
            }
            let editing = state.names.as_ref().is_some_and(|(i, _)| *i == input);
            let button = if input { "Edit input names" } else { "Edit output names" };
            if ui.selectable_label(editing, button).clicked() {
                state.names = if editing {
                    None
                } else {
                    let aliases = app.channel_aliases(input);
                    let count = if input { app.engine.info().inputs.len() } else { app.engine.info().outputs.len() };
                    Some((input, (0..count).map(|i| aliases.alias(i).unwrap_or("").to_owned()).collect()))
                };
            }
        });
    }
    let Some((input, drafts)) = &mut state.names else { return };
    let input = *input;
    let hardware = if input { app.engine.info().inputs.clone() } else { app.engine.info().outputs.clone() };
    if hardware.is_empty() {
        note(ui, if input { "The device has no inputs open." } else { "The device has no outputs open." });
        return;
    }
    egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
        egui::Grid::new(("channel-names", input)).num_columns(3).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
            ui.label(RichText::new("#").color(TEXT_DIM));
            ui.label(RichText::new("Hardware name").color(TEXT_DIM));
            ui.label(RichText::new("Aliased name").color(TEXT_DIM));
            ui.end_row();
            for (i, hw) in hardware.iter().enumerate() {
                ui.label(RichText::new((i + 1).to_string()).color(TEXT_DIM));
                ui.label(hw);
                let Some(draft) = drafts.get_mut(i) else { break };
                let field = egui::TextEdit::singleline(draft).hint_text(hw.as_str()).desired_width(200.0);
                let resp = ui.add(field);
                let what = format!("{} {} alias", if input { "Input" } else { "Output" }, i + 1);
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, what.as_str()));
                if resp.lost_focus() && app.channel_aliases(input).alias(i).unwrap_or("") != draft.trim() {
                    app.set_channel_alias(input, i, draft);
                }
                ui.end_row();
            }
        });
    });
    ui.label(
        RichText::new("Names show in track inputs, hardware outputs, monitor mixes and ReaInsert.")
            .color(ACCENT)
            .font(theme::regular(11.0)),
    );
}
