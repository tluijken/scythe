//! The audio device window, after REAPER's Preferences > Audio > Device page and the device
//! parts of its Audio page: the audio system and each system's settings, closing the device
//! while stopped, and channel naming and remapping. Apply opens the new device while the
//! project stays open.

use egui::{Align2, CornerRadius, Rect, RichText, Ui, pos2, vec2};
use scythe_core::prefs::{
    AudioSystem, CHANMAP_SECTION, CLOSE_INACTIVE, CLOSE_INACTIVE_ARMED, ChannelAliases, JACK_NO_CONNECT, JACK_NO_START,
};
use scythe_engine::DeviceRequest;
use scythe_engine::Health;
use scythe_engine::devices::{BACKEND_PERIODS, COMMON_BLOCK_SIZES, COMMON_RATES, Capabilities, DeviceEntry};

use crate::app::ScytheApp;
use crate::theme::{self, ACCENT, BG, RAISED, RAISED_HI, TEXT, TEXT_DIM, YELLOW};
use crate::ui::widgets::{self, switch, text_pill};
use crate::ui::window::{WindowKind, scythe_window};

const WIDTH: f32 = 600.0;
const LABEL_W: f32 = 150.0;
const ROW_H: f32 = 34.0;
/// REAPER's ALSA "RT priority" choices; 0 is "non-RT".
const RT_PRIORITIES: [u32; 11] = [0, 10, 20, 30, 40, 50, 60, 70, 80, 90, 99];

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
    /// The channel map being edited: input side or output, and the draft.
    map: Option<(bool, ChannelAliases)>,
}

/// The device page's fields.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Draft {
    /// The audio host (`ALSA`, `JACK`, `Dummy`, `PulseAudio`).
    pub system: String,
    pub output: Option<String>,
    pub input: Option<String>,
    pub input_channels: u16,
    pub output_channels: u16,
    pub sample_rate: u32,
    pub block_size: u32,
    pub bits: u32,
    pub rt_priority: u32,
    pub jack_autostart: bool,
    pub jack_launch: String,
    pub jack_connect: bool,
    pub rate_override: bool,
}

impl Draft {
    /// The fields as the device playing now and the preferences fill them.
    fn current(app: &ScytheApp) -> Self {
        let r = app.engine.devices();
        let info = app.engine.info();
        let a = &app.prefs.audio;
        let system =
            r.host.clone().unwrap_or_else(|| if info.host.is_empty() { "ALSA".into() } else { info.host.clone() });
        let jack = a.jack_options.unwrap_or(JACK_NO_START);
        Self {
            system,
            output: r.output.clone(),
            input: r.input.clone(),
            input_channels: r.input_channels.unwrap_or(info.inputs.len() as u16),
            output_channels: r.output_channels.unwrap_or(info.outputs.len().max(2) as u16),
            sample_rate: r.sample_rate.unwrap_or(info.sample_rate),
            block_size: r.buffer_size.or(info.block_size).unwrap_or(512),
            bits: a.bits.unwrap_or(32),
            rt_priority: a.rt_priority.unwrap_or(scythe_engine::realtime::PRIORITY as u32),
            jack_autostart: jack & JACK_NO_START == 0,
            jack_launch: a.jack_launch.clone().unwrap_or_default(),
            jack_connect: jack & JACK_NO_CONNECT == 0,
            rate_override: a.project_rate_override(),
        }
    }

    fn system(&self) -> Option<AudioSystem> {
        AudioSystem::from_host(&self.system)
    }

    /// What Apply asks the engine for. JACK's rate and block size are the server's.
    pub fn request(&self, no_input: bool) -> DeviceRequest {
        let system = self.system();
        let jack = system == Some(AudioSystem::Jack);
        let dummy = system == Some(AudioSystem::Dummy);
        let alsa = system == Some(AudioSystem::Alsa);
        DeviceRequest {
            host: Some(self.system.clone()),
            output: self.output.clone().filter(|_| alsa),
            input: self.input.clone().filter(|_| alsa),
            sample_rate: (!jack).then_some(self.sample_rate),
            buffer_size: (!jack).then_some(self.block_size),
            output_channels: Some(if dummy { 2 } else { self.output_channels.max(1) }),
            input_channels: Some(if dummy { 0 } else { self.input_channels }),
            bits: (alsa || system == Some(AudioSystem::PulseAudio)).then_some(self.bits as u16),
            jack_start: (jack && self.jack_autostart).then(|| self.jack_launch.clone()),
            jack_no_connect: jack && !self.jack_connect,
            no_input,
        }
    }
}

impl Draft {
    /// Takes the newly chosen device's own values where the draft asks for what it cannot do:
    /// its channel counts, its rate (the nearest one) and its bit depth.
    pub fn adopt(&mut self, outputs: Option<&Capabilities>, inputs: Option<&Capabilities>) {
        self.input_channels = default_count(inputs, 2);
        self.output_channels = default_count(outputs, 2);
        let Some(out) = outputs else { return };
        if !out.sample_rates.is_empty() && !out.sample_rates.contains(&self.sample_rate) {
            let ranges: Vec<(u32, u32)> = out.sample_rates.iter().map(|&r| (r, r)).collect();
            self.sample_rate = scythe_engine::devices::nearest_rate(&ranges, self.sample_rate);
        }
        let bits = u16::try_from(self.bits).unwrap_or(0);
        if let Some(&most) = out.bits.last().filter(|_| !out.bits.contains(&bits)) {
            self.bits = most.into();
        }
    }
}

/// REAPER's latency figure for the open device (`~2.9/2.9ms`, from the block size and periods),
/// and the delays the device measures, for a tooltip.
pub fn latency_lines(health: &Health) -> Option<(String, String)> {
    let figure = scythe_engine::devices::latency_text(health.block_latency?);
    let mut measured = format!("Measured by the device: output {:.1} ms", health.output_latency * 1000.0);
    if let Some(input) = health.input_latency {
        measured.push_str(&format!(", input {:.1} ms", input * 1000.0));
    }
    Some((figure, measured))
}

/// Says the system default device plays (no device chosen), so the interface still has to be
/// picked: `System default (PipeWire)`.
pub fn default_note(app: &ScytheApp) -> Option<String> {
    if !app.plays_system_default() {
        return None;
    }
    let label = scythe_engine::devices::default_label(scythe_engine::devices::sound_server());
    Some(format!("{label} is playing: choose your audio interface on the ALSA system for the lowest latency"))
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
    /// playing) says nothing, and REAPER's values are offered.
    fn query(&mut self) {
        let r = DeviceRequest { jack_start: None, ..self.draft.request(false) };
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
    error_window(app, ui);
    let Some(mut state) = app.view.audio_device.take() else { return };
    let mut open = true;
    let height = ui.ctx().content_rect().height() - 140.0;
    scythe_window("audio-device", "Audio device", WindowKind::Dialog).default_width(WIDTH).show(
        ui.ctx(),
        &mut open,
        |ui| {
            ui.set_width(WIDTH);
            egui::ScrollArea::vertical().max_height(height).min_scrolled_height(height.min(640.0)).show(ui, |ui| {
                // Rows space themselves; widgets placed inside a row must not push the next one down.
                ui.spacing_mut().item_spacing.y = 0.0;
                device_page(app, ui, &mut state);
                closing(app, ui);
                channel_naming(app, ui, &mut state);
            });
        },
    );
    if open {
        app.view.audio_device = Some(state);
    }
}

/// REAPER's "Error opening devices" window: the device did not open at start, or stayed busy.
fn error_window(app: &mut ScytheApp, ui: &mut Ui) {
    if let Some(e) = app.take_device_error() {
        app.view.audio_error = Some(e);
    }
    let Some(reason) = app.view.audio_error.clone() else { return };
    let mut open = true;
    let (mut retry, mut settings, mut close) = (false, false, false);
    scythe_window("audio-error", "Error opening devices", WindowKind::Dialog).default_width(440.0).show(
        ui.ctx(),
        &mut open,
        |ui| {
            ui.set_width(440.0);
            ui.label(RichText::new("There was an error opening the audio hardware.").color(TEXT));
            ui.label(RichText::new(&reason).color(TEXT_DIM).font(theme::regular(11.0)));
            ui.add_space(8.0);
            let r = ui.allocate_space(vec2(ui.available_width(), 30.0)).1;
            let pill = |x: f32, w: f32| Rect::from_min_size(pos2(r.left() + x, r.top()), vec2(w, 28.0));
            retry = text_pill(ui, pill(0.0, 90.0), "Retry", theme::semibold(12.0), ACCENT, BG).clicked();
            settings = text_pill(ui, pill(98.0, 130.0), "Audio device", theme::semibold(12.0), RAISED, TEXT).clicked();
            close = text_pill(ui, pill(r.width() - 90.0, 90.0), "Close", theme::semibold(12.0), RAISED, TEXT).clicked();
        },
    );
    if !open || close {
        app.view.audio_error = None;
    }
    if retry {
        let request = app.engine.devices().clone();
        match app.change_audio_device(request, false) {
            Ok(()) => app.view.audio_error = None,
            Err(e) => app.view.audio_error = Some(e),
        }
    }
    if settings {
        app.view.audio_error = None;
        if app.view.audio_device.is_none() {
            toggle(app);
        }
    }
}

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(6.0);
    let r = ui.allocate_space(vec2(ui.available_width(), 22.0)).1;
    ui.painter().text(r.left_center(), Align2::LEFT_CENTER, title, theme::semibold(12.0), TEXT_DIM);
}

fn note(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).color(TEXT_DIM).font(theme::regular(11.0)));
}

/// A row of the page: its label on the left, the rest for the control.
fn row(ui: &mut Ui, label: &str) -> Rect {
    let r = ui.allocate_space(vec2(ui.available_width(), ROW_H)).1;
    ui.painter().rect_filled(r, CornerRadius::same(10), RAISED);
    ui.painter().text(r.left_center() + vec2(12.0, 0.0), Align2::LEFT_CENTER, label, theme::regular(12.0), TEXT);
    Rect::from_min_max(pos2(r.left() + LABEL_W, r.top() + 5.0), pos2(r.right() - 8.0, r.bottom() - 5.0))
}

/// Ends a row whose control area is `r`: widgets placed in it leave the cursor inside the row,
/// so the next row starts a gap below it.
fn end_row(ui: &mut Ui, r: Rect) {
    ui.advance_cursor_after_rect(Rect::from_min_max(r.min, pos2(r.right(), r.bottom() + 9.0)));
}

/// A dropdown pill named `name` showing `shown`; returns the index picked from `items`.
fn choice(ui: &mut Ui, rect: Rect, name: &str, shown: &str, items: &[(String, bool)]) -> Option<usize> {
    let text = widgets::truncate(ui, shown, &theme::regular(12.0), rect.width() - 30.0);
    let resp = text_pill(ui, rect, "", theme::regular(12.0), BG, TEXT);
    let p = ui.painter();
    p.text(rect.left_center() + vec2(12.0, 0.0), Align2::LEFT_CENTER, text, theme::regular(12.0), TEXT);
    let c = pos2(rect.right() - 13.0, rect.center().y);
    let arrow = vec![c + vec2(-4.0, -2.0), c + vec2(4.0, -2.0), c + vec2(0.0, 3.0)];
    p.add(egui::Shape::convex_polygon(arrow, TEXT_DIM, egui::Stroke::NONE));
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, name));
    let mut picked = None;
    egui::Popup::menu(&resp).show(|ui| {
        egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
            for (i, (label, on)) in items.iter().enumerate() {
                if ui.selectable_label(*on, label).clicked() {
                    picked = Some(i);
                }
            }
        });
    });
    picked
}

/// A row with `label` and a switch at its right end; true when the switch is clicked.
fn switch_row(ui: &mut Ui, label: &str, on: bool, enabled: bool) -> bool {
    let r = ui.allocate_space(vec2(ui.available_width(), ROW_H)).1;
    ui.painter().rect_filled(r, CornerRadius::same(10), RAISED);
    let fg = if enabled { TEXT } else { TEXT_DIM };
    ui.painter().text(r.left_center() + vec2(12.0, 0.0), Align2::LEFT_CENTER, label, theme::regular(12.0), fg);
    let sw = Rect::from_min_size(pos2(r.right() - 50.0, r.center().y - 10.0), vec2(38.0, 20.0));
    let resp = switch(ui, sw, on && enabled, if enabled { ACCENT } else { RAISED_HI });
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, enabled, on, label));
    end_row(ui, r.shrink2(vec2(0.0, 5.0)));
    enabled && resp.clicked()
}

fn number(ui: &mut Ui, rect: Rect, name: &str, value: &mut u16, range: std::ops::RangeInclusive<u16>, enabled: bool) {
    let field = Rect::from_min_size(rect.min, vec2(80.0, rect.height()));
    ui.painter().rect_filled(field, CornerRadius::same(255), BG);
    let resp = ui
        .put(field.shrink2(vec2(10.0, 2.0)), |ui: &mut Ui| {
            ui.add_enabled(enabled, egui::DragValue::new(value).range(range).speed(0.2))
        })
        .on_hover_text("Drag or type the number of channels to open");
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::SpinButton, enabled, name));
}

fn device_page(app: &mut ScytheApp, ui: &mut Ui, state: &mut AudioDeviceWindow) {
    let before = state.draft.clone();
    let system = state.draft.system();
    section(ui, "Audio device settings");
    let items: Vec<(String, bool)> = state
        .systems
        .iter()
        .map(|h| {
            let label = AudioSystem::from_host(h).map_or(h.clone(), |s| s.label().to_owned());
            (label, state.draft.system.eq_ignore_ascii_case(h))
        })
        .collect();
    let shown = system.map_or(state.draft.system.clone(), |s| s.label().to_owned());
    let r = row(ui, "Audio system");
    if let Some(i) = choice(ui, Rect::from_min_size(r.min, vec2(260.0, r.height())), "Audio system", &shown, &items) {
        let host = state.systems[i].clone();
        if !state.draft.system.eq_ignore_ascii_case(&host) {
            (state.draft.output, state.draft.input) = (None, None);
        }
        state.draft.system = host;
    }
    end_row(ui, r);

    let jack = system == Some(AudioSystem::Jack);
    let dummy = system == Some(AudioSystem::Dummy);
    let alsa = system == Some(AudioSystem::Alsa);
    for (input, label) in [(true, "Input channels"), (false, "Output channels")] {
        let r = row(ui, label);
        let caps = if input { state.inputs.as_ref() } else { state.outputs.as_ref() };
        let most = caps.and_then(|c| c.channels.iter().copied().max());
        let top = most.unwrap_or(64);
        let value = if input { &mut state.draft.input_channels } else { &mut state.draft.output_channels };
        if dummy {
            *value = if input { 0 } else { 2 };
        }
        number(ui, r, label, value, if input { 0..=top } else { 1..=top.max(1) }, !dummy);
        if let Some(m) = most {
            let at = pos2(r.left() + 92.0, r.center().y);
            ui.painter().text(at, Align2::LEFT_CENTER, format!("of {m}"), theme::regular(11.0), TEXT_DIM);
        }
        if alsa {
            let device = Rect::from_min_max(pos2(r.left() + 140.0, r.top()), r.max);
            let name = if input { "Input device" } else { "Output device" };
            let chosen = if input { &mut state.draft.input } else { &mut state.draft.output };
            device_choice(ui, device, name, &state.devices, &state.draft.system, input, chosen);
        }
        end_row(ui, r);
    }
    if !jack {
        let rates = state.outputs.as_ref().map(|c| c.sample_rates.clone()).filter(|r| !r.is_empty());
        let rates = rates.unwrap_or_else(|| COMMON_RATES.to_vec());
        value_row(ui, "Sample rate", &rates, &mut state.draft.sample_rate, |r| format!("{r} Hz"));
        let sizes: Vec<u32> = match state.outputs.as_ref().and_then(|c| c.buffer_sizes) {
            Some((min, max)) => COMMON_BLOCK_SIZES.into_iter().filter(|b| (min..=max).contains(b)).collect(),
            None => COMMON_BLOCK_SIZES.to_vec(),
        };
        let sizes = if sizes.is_empty() { COMMON_BLOCK_SIZES.to_vec() } else { sizes };
        value_row(ui, "Blocksize", &sizes, &mut state.draft.block_size, |b| format!("{b} frames"));
    }
    if alsa {
        let offered: Vec<u32> =
            state.outputs.as_ref().map_or(Vec::new(), |c| c.bits.iter().map(|&b| b.into()).collect());
        let bits = if offered.is_empty() { vec![16, 24, 32] } else { offered };
        value_row(ui, "Bit depth", &bits, &mut state.draft.bits, |b| format!("{b} bit"));
    }
    if alsa {
        let r = row(ui, "Periods");
        let text = format!("{BACKEND_PERIODS}, as the audio backend opens ALSA devices");
        ui.painter().text(r.left_center(), Align2::LEFT_CENTER, text, theme::regular(12.0), TEXT_DIM);
        end_row(ui, r);
        let label = |p: u32| match p {
            0 => "0 (non-RT)".to_owned(),
            40 => "40 (suggested setting for RT use)".to_owned(),
            50 => "50 (default hardware priority)".to_owned(),
            99 => "99 (maximum priority)".to_owned(),
            p => p.to_string(),
        };
        value_row(ui, "RT priority", &RT_PRIORITIES, &mut state.draft.rt_priority, label);
        if state.draft.rt_priority == 0 {
            // REAPER rides out a late audio thread on its anticipative FX buffering; Scythe renders
            // in the callback, so without real-time priority the desktop can make it miss blocks.
            note(
                ui,
                "Without real-time priority the audio thread can be held up by other programs, which gives dropouts at small block sizes.",
            );
        }
    }
    if jack {
        let r = row(ui, "RT priority");
        let at = r.left_center();
        ui.painter().text(at, Align2::LEFT_CENTER, "(automatic from JACK)", theme::regular(12.0), TEXT_DIM);
        end_row(ui, r);
        if switch_row(ui, "Auto-start jackd", state.draft.jack_autostart, true) {
            state.draft.jack_autostart ^= true;
        }
        if state.draft.jack_autostart {
            let r = row(ui, "Launch command");
            let field =
                egui::TextEdit::singleline(&mut state.draft.jack_launch).hint_text("blank: as libjack starts it");
            let resp = ui.put(r, field);
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Launch command"));
            end_row(ui, r);
        }
        if switch_row(ui, "Auto-connect jack audio channels to hardware", state.draft.jack_connect, true) {
            state.draft.jack_connect ^= true;
        }
    }
    if !jack && switch_row(ui, "Allow projects to override device sample rate", state.draft.rate_override, true) {
        state.draft.rate_override ^= true;
    }
    let a = app.prefs.audio.clone();
    let pm = a.disable_power_management == Some(true);
    if switch_row(ui, "Disable power management (/dev/cpu_dma_latency)", pm, true) {
        app.change_preferences(|p| p.audio.disable_power_management = Some(!pm));
    }
    let locked = a.lock_memory == Some(true);
    if switch_row(ui, "Lock process memory with mlockall() (caution)", locked, true) {
        app.change_preferences(|p| p.audio.lock_memory = Some(!locked));
    }
    match system {
        Some(AudioSystem::Jack) => note(ui, "Sample rate and block size come from the JACK server."),
        Some(AudioSystem::Dummy) => note(ui, "No device: playback runs on a silent clock, as REAPER's Dummy Audio."),
        _ => {}
    }

    let changed_device = (&before.system, &before.output, &before.input)
        != (&state.draft.system, &state.draft.output, &state.draft.input);
    if changed_device {
        state.query();
        state.draft.adopt(state.outputs.as_ref(), state.inputs.as_ref());
    }

    ui.add_space(4.0);
    let info = app.engine.info().clone();
    let health = app.engine.health();
    let block = health.block_size.map_or("device default".to_owned(), |b| format!("{b} frames"));
    let (latency, measured) = match latency_lines(&health) {
        Some((figure, measured)) => (format!(" · {figure}"), measured),
        None => (String::new(), String::new()),
    };
    let now = format!(
        "Playing: {} · {} Hz · {block}{latency} · {} in / {} out",
        info.name,
        info.sample_rate,
        app.input_names().len(),
        app.output_names().len()
    );
    let resp = ui.label(RichText::new(now).color(if health.silent { YELLOW } else { TEXT }));
    if !measured.is_empty() {
        resp.on_hover_text(format!("Latency from the block size and periods, as REAPER shows it.\n{measured}"));
    }
    if let Some(note) = default_note(app) {
        ui.label(RichText::new(note).color(YELLOW).font(theme::regular(11.0)));
    }
    ui.add_space(4.0);
    if let Some(reason) = &health.reason {
        ui.label(RichText::new(reason).color(YELLOW).font(theme::regular(11.0)));
    }
    if let Some(e) = &state.error {
        ui.label(RichText::new(format!("Could not open the device: {e}")).color(theme::PINK));
    }
    ui.add_space(4.0);
    let r = ui.allocate_space(vec2(ui.available_width(), 30.0)).1;
    let pill = |x: f32, w: f32| Rect::from_min_size(pos2(r.left() + x, r.top()), vec2(w, 28.0));
    if text_pill(ui, pill(0.0, 110.0), "Apply", theme::semibold(12.0), ACCENT, BG)
        .on_hover_text("Open these devices now; the project stays open")
        .clicked()
    {
        apply(app, state);
    }
    if text_pill(ui, pill(118.0, 100.0), "Refresh", theme::semibold(12.0), RAISED, TEXT)
        .on_hover_text("Look for audio devices again")
        .clicked()
    {
        state.refresh();
    }
    if text_pill(ui, pill(226.0, 100.0), "Reset", theme::semibold(12.0), RAISED, TEXT)
        .on_hover_text("Show what is playing now")
        .clicked()
    {
        state.draft = Draft::current(app);
        state.error = None;
        state.query();
    }
}

/// Applies the draft; on failure the previous device plays on and the reason shows.
pub fn apply(app: &mut ScytheApp, state: &mut AudioDeviceWindow) {
    let d = state.draft.clone();
    let request = d.request(app.no_input);
    app.change_preferences(|p| {
        let a = &mut p.audio;
        let mut options = a.jack_options.unwrap_or(JACK_NO_START);
        options = if d.jack_autostart { options & !JACK_NO_START } else { options | JACK_NO_START };
        options = if d.jack_connect { options & !JACK_NO_CONNECT } else { options | JACK_NO_CONNECT };
        a.jack_options = Some(options);
        a.jack_launch = Some(d.jack_launch.clone());
        a.rt_priority = Some(d.rt_priority);
        a.bits = Some(d.bits);
        a.project_rate_override = Some(d.rate_override);
    });
    app.close_plugin_windows();
    match app.change_audio_device(request, true) {
        Ok(()) => {
            state.error = None;
            state.map = None;
            if !app.waiting_for_device() {
                let info = app.engine.info();
                let msg = format!("Audio: {} at {} Hz", info.name, info.sample_rate);
                app.notify(msg);
            }
        }
        Err(e) => state.error = Some(e),
    }
}

fn default_count(caps: Option<&Capabilities>, fallback: u16) -> u16 {
    caps.map_or(fallback, |c| scythe_engine::devices::stream_channels(&c.channels, c.default_channels, None))
}

fn value_row(ui: &mut Ui, name: &str, values: &[u32], value: &mut u32, label: impl Fn(u32) -> String) {
    let mut values = values.to_vec();
    if !values.contains(value) {
        values.push(*value);
        values.sort_unstable();
    }
    let r = row(ui, name);
    let items: Vec<(String, bool)> = values.iter().map(|v| (label(*v), v == value)).collect();
    if let Some(i) = choice(ui, Rect::from_min_size(r.min, vec2(260.0, r.height())), name, &label(*value), &items) {
        *value = values[i];
    }
    end_row(ui, r);
}

fn device_choice(
    ui: &mut Ui,
    rect: Rect,
    name: &str,
    devices: &[DeviceEntry],
    host: &str,
    input: bool,
    chosen: &mut Option<String>,
) {
    let default = scythe_engine::devices::default_label(scythe_engine::devices::sound_server());
    let offered: Vec<&DeviceEntry> = devices
        .iter()
        .filter(|d| d.host.eq_ignore_ascii_case(host) && if input { d.input } else { d.output })
        .collect();
    let label = |d: &DeviceEntry| if d.id == d.name { d.id.clone() } else { format!("{} ; {}", d.id, d.name) };
    let shown = match chosen {
        None => default.clone(),
        Some(c) => offered.iter().find(|d| d.id == *c || d.name == *c).map_or(c.clone(), |d| label(d)),
    };
    let mut items = vec![(default.clone(), chosen.is_none())];
    items.extend(offered.iter().map(|d| (label(d), chosen.as_deref() == Some(d.id.as_str()))));
    match choice(ui, rect, name, &shown, &items) {
        Some(0) => *chosen = None,
        Some(i) => *chosen = Some(offered[i - 1].id.clone()),
        None => {}
    }
}

/// REAPER's Audio page: closing the device while stopped.
fn closing(app: &mut ScytheApp, ui: &mut Ui) {
    section(ui, "Closing the device");
    let bits = app.prefs.audio.close_inactive.unwrap_or(0);
    let on = bits & CLOSE_INACTIVE != 0;
    if switch_row(ui, "Close audio device when stopped and application is inactive", on, true) {
        app.change_preferences(|p| p.audio.close_inactive = Some(bits ^ CLOSE_INACTIVE));
    }
    let armed = bits & CLOSE_INACTIVE_ARMED != 0;
    if switch_row(ui, "Close audio device when inactive and tracks are record armed", armed, on) {
        app.change_preferences(|p| p.audio.close_inactive = Some(bits ^ CLOSE_INACTIVE_ARMED));
    }
    let stopped = app.prefs.audio.close_stopped == Some(true);
    if switch_row(ui, "Close audio device when stopped and active (less responsive)", stopped, true) {
        app.change_preferences(|p| p.audio.close_stopped = Some(!stopped));
    }
}

/// REAPER's "Input/Output channel name aliasing/remapping" and its channel map editor.
fn channel_naming(app: &mut ScytheApp, ui: &mut Ui, state: &mut AudioDeviceWindow) {
    section(ui, "Channel naming/mapping");
    for input in [true, false] {
        let label =
            if input { "Input channel name aliasing/remapping" } else { "Output channel name aliasing/remapping" };
        let on = app.channel_aliasing(input);
        let r = ui.allocate_space(vec2(ui.available_width(), ROW_H)).1;
        ui.painter().rect_filled(r, CornerRadius::same(10), RAISED);
        ui.painter().text(r.left_center() + vec2(12.0, 0.0), Align2::LEFT_CENTER, label, theme::regular(12.0), TEXT);
        let editing = state.map.as_ref().is_some_and(|(i, _)| *i == input);
        let button = if input { "Edit input names" } else { "Edit output names" };
        let edit = Rect::from_min_size(pos2(r.right() - 210.0, r.top() + 5.0), vec2(140.0, r.height() - 10.0));
        if text_pill(ui, edit, button, theme::regular(12.0), if editing { RAISED_HI } else { BG }, TEXT).clicked() {
            state.map = if editing { None } else { Some((input, app.channel_aliases(input))) };
        }
        let sw = Rect::from_min_size(pos2(r.right() - 50.0, r.center().y - 10.0), vec2(38.0, 20.0));
        let resp = switch(ui, sw, on, ACCENT);
        resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, on, label));
        if resp.clicked() {
            app.set_channel_aliasing(input, !on);
        }
        end_row(ui, r.shrink2(vec2(0.0, 5.0)));
    }
    let pairs = app.prefs.all_stereo_pairs.unwrap_or(true);
    if switch_row(ui, "Show non-standard stereo channel pairs (Input 2/Input 3 etc)", pairs, true) {
        app.change_preferences(|p| p.all_stereo_pairs = Some(!pairs));
    }
    map_editor(app, ui, state);
}

/// The channel map: per slot its alias and the hardware channel it plays, with REAPER's Add,
/// Remove, Reset, Load and Save.
fn map_editor(app: &mut ScytheApp, ui: &mut Ui, state: &mut AudioDeviceWindow) {
    let Some((input, map)) = &mut state.map else { return };
    let input = *input;
    let hardware = if input { app.engine.info().inputs.clone() } else { app.engine.info().outputs.clone() };
    let side = if input { "Input" } else { "Output" };
    let mut changed = false;
    egui::ScrollArea::vertical().id_salt(("channel-map", input)).max_height(300.0).min_scrolled_height(300.0).show(
        ui,
        |ui| {
            egui::Grid::new(("channel-map-grid", input)).num_columns(4).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
                for head in ["#", "Aliased name", "HW#", "HW name"] {
                    ui.label(RichText::new(head).color(TEXT_DIM));
                }
                ui.end_row();
                for i in 0..map.channels.len() {
                    ui.label(RichText::new((i + 1).to_string()).color(TEXT_DIM));
                    let hw = map.hardware(i);
                    let hw_name = hw.map_or("(not connected)".to_owned(), |h| {
                        hardware.get(h).cloned().unwrap_or_else(|| format!("{side} {}", h + 1))
                    });
                    let alias = &mut map.channels[i].1;
                    let field = egui::TextEdit::singleline(alias).hint_text(hw_name.as_str()).desired_width(170.0);
                    let resp = ui.add(field);
                    let what = format!("{side} {} alias", i + 1);
                    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, what.as_str()));
                    changed |= resp.lost_focus();
                    let shown = hw.map_or("-".to_owned(), |h| (h + 1).to_string());
                    let r = ui.allocate_space(vec2(64.0, 22.0)).1;
                    let mut items = vec![("(not connected)".to_owned(), hw.is_none())];
                    items.extend(hardware.iter().enumerate().map(|(k, n)| (format!("{}: {n}", k + 1), hw == Some(k))));
                    let name = format!("{side} {} hardware channel", i + 1);
                    if let Some(k) = choice(ui, r, &name, &shown, &items) {
                        map.set_hardware(i, k.checked_sub(1), hardware.len());
                        changed = true;
                    }
                    ui.label(hw_name);
                    ui.end_row();
                }
            });
        },
    );
    let r = ui.allocate_space(vec2(ui.available_width(), 30.0)).1;
    let pill = |x: f32| Rect::from_min_size(pos2(r.left() + x, r.top()), vec2(84.0, 26.0));
    let font = theme::regular(12.0);
    if text_pill(ui, pill(0.0), "Add", font.clone(), RAISED, TEXT).clicked() {
        map.add_slot(hardware.len());
        changed = true;
    }
    if text_pill(ui, pill(90.0), "Remove", font.clone(), RAISED, TEXT)
        .on_hover_text("Remove the last channel")
        .clicked()
    {
        map.remove_slot(map.channels.len().saturating_sub(1));
        changed = true;
    }
    if text_pill(ui, pill(180.0), "Reset", font.clone(), RAISED, TEXT).clicked() {
        *map = ChannelAliases::identity(hardware.len());
        changed = true;
    }
    let dir = scythe_session::preferences_path().with_file_name("ChanMaps");
    if text_pill(ui, pill(270.0), "Load...", font.clone(), RAISED, TEXT).clicked()
        && let Some(path) =
            rfd::FileDialog::new().add_filter("ChanMap files", &["ReaperChanMap"]).set_directory(&dir).pick_file()
    {
        match std::fs::read_to_string(&path).ok().and_then(|t| ChannelAliases::from_chanmap(&t)) {
            Some(loaded) => {
                *map = loaded;
                changed = true;
            }
            None => app.notify(format!("{} holds no [{CHANMAP_SECTION}] map", path.display())),
        }
    }
    if text_pill(ui, pill(360.0), "Save...", font, RAISED, TEXT).clicked() {
        let _ = std::fs::create_dir_all(&dir);
        if let Some(path) =
            rfd::FileDialog::new().add_filter("ChanMap files", &["ReaperChanMap"]).set_directory(&dir).save_file()
        {
            let path = path.with_extension("ReaperChanMap");
            if let Err(e) = std::fs::write(&path, map.to_chanmap()) {
                app.notify(format!("Cannot save {}: {e}", path.display()));
            }
        }
    }
    if changed {
        let map = map.clone();
        app.set_channel_map(input, map);
    }
}
