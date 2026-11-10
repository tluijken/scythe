//! The AI agent light in the top bar and its details window: which agents are connected over
//! MCP, what they say they are doing, their recent tool calls with the undo steps they made,
//! and the user's controls over them (undo their last change, pause, disconnect).

use std::time::{Duration, Instant};

use egui::{CornerRadius, Rect, RichText, Sense, Ui, vec2};
use scythe_session::agents::{AgentCall, Light, now_ms};

use crate::app::ScytheApp;
use crate::theme::{self, ACCENT, PANEL, PINK, RAISED, TEXT, TEXT_DIM, TEXT_MID};
use crate::ui::widgets;
use crate::ui::window::{WindowKind, scythe_window};

/// The light's slot in the top bar.
pub const DOT_W: f32 = 32.0;
/// One pulse of the busy light.
const PULSE: f32 = 1.6;

fn state_word(light: Light) -> &'static str {
    match light {
        Light::Off => "off",
        Light::Idle => "idle",
        Light::Busy => "working",
        Light::Error => "error",
    }
}

/// The light in the title block: after the project's name when there is room before the
/// transport (`limit`), else as a badge on the logo. `title_w` is the block's width.
pub fn dot_beside(app: &mut ScytheApp, ui: &mut Ui, logo: Rect, title_w: f32, limit: f32) {
    let beside = Rect::from_center_size(
        egui::pos2(logo.left() + title_w + 6.0 + DOT_W / 2.0, logo.center().y),
        vec2(DOT_W, DOT_W),
    );
    let rect = if beside.right() <= limit - 4.0 {
        beside
    } else {
        Rect::from_center_size(logo.right_bottom() - vec2(3.0, 3.0), vec2(16.0, 16.0))
    };
    dot(app, ui, rect);
}

/// Draws the light in `rect` when an agent is connected; answers whether it took the room.
pub fn dot(app: &mut ScytheApp, ui: &mut Ui, rect: Rect) -> bool {
    let now = Instant::now();
    let light = app.agents.light(now);
    if light == Light::Off {
        return false;
    }
    let resp = ui.allocate_rect(rect, Sense::click());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, CornerRadius::same(255), if resp.hovered() { RAISED } else { PANEL });
    let centre = rect.center();
    let color = match light {
        Light::Busy => {
            let t = ui.input(|i| i.time) as f32;
            let phase = 0.5 - 0.5 * (t * std::f32::consts::TAU / PULSE).cos();
            painter.circle_filled(centre, 4.0 + 3.0 * phase, ACCENT.gamma_multiply(0.25 * (1.0 - phase)));
            ui.ctx().request_repaint_after(Duration::from_millis(33));
            ACCENT.gamma_multiply(0.7 + 0.3 * phase)
        }
        Light::Error => PINK,
        _ => TEXT_DIM.gamma_multiply(0.55),
    };
    painter.circle_filled(centre, 4.0, color);
    if app.agents.paused() {
        painter.circle_stroke(centre, 7.0, egui::Stroke::new(1.0, TEXT_DIM));
    }
    if let Some(wait) = app.agents.light_changes_in(now) {
        ui.ctx().request_repaint_after(wait);
    }
    let names: Vec<&str> = app.agents.clients().iter().map(|c| c.name.as_str()).collect();
    let mut tip = format!("Agent: {}", names.join(", "));
    if let Some((_, status)) = app.agents.status() {
        tip.push_str(&format!("\n{status}"));
    }
    if app.agents.paused() {
        tip.push_str("\nPaused: changes are refused");
    }
    tip.push_str("\nClick for details");
    let label = format!("Agent activity ({})", state_word(light));
    resp.widget_info(|| widgets::button_info(ui, &label));
    if resp.on_hover_text(tip).clicked() {
        app.view.agents = !app.view.agents;
    }
    true
}

/// "just now", "12 s ago".
fn since(ms: u64, now: u64) -> String {
    match ago(ms, now) {
        n if n == "now" => "just now".into(),
        n => format!("{n} ago"),
    }
}

/// "now", "12 s", "4 min", "2 h": how long ago `ms` was.
fn ago(ms: u64, now: u64) -> String {
    let s = now.saturating_sub(ms) / 1000;
    match s {
        0..=2 => "now".into(),
        3..=59 => format!("{s} s"),
        60..=3599 => format!("{} min", s / 60),
        _ => format!("{} h", s / 3600),
    }
}

pub fn show(app: &mut ScytheApp, ui: &mut Ui) {
    if !app.view.agents {
        return;
    }
    let mut open = true;
    scythe_window("agents", "Agents", WindowKind::Panel)
        .default_size(vec2(560.0, 520.0))
        .min_size(vec2(420.0, 320.0))
        .anchored()
        .show(ui.ctx(), &mut open, |ui| body(app, ui));
    if !open {
        app.view.agents = false;
    }
}

fn body(app: &mut ScytheApp, ui: &mut Ui) {
    let now = now_ms();
    if app.agents.clients().is_empty() {
        ui.label(RichText::new("No agent is connected.").color(TEXT_DIM));
    }
    let mut disconnect = None;
    for c in app.agents.clients().to_vec() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(&c.name).font(theme::semibold(14.0)).color(TEXT));
            let version = c.version.as_deref().map(|v| format!(" {v}")).unwrap_or_default();
            let busy = if c.running > 0 { " · working" } else { "" };
            ui.label(
                RichText::new(format!("{version} · {} · connected {}{busy}", c.transport, since(c.connected_ms, now)))
                    .color(TEXT_DIM),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Disconnect").on_hover_text("Refuse this agent's calls from now on").clicked() {
                    disconnect = Some(c.id.clone());
                }
            });
        });
        if let Some(status) = &c.status {
            ui.label(RichText::new(status).color(TEXT_MID).italics());
        }
        ui.add_space(4.0);
    }
    if let Some(id) = disconnect {
        app.session.agents.disconnect(Some(&id));
        app.notify("Agent disconnected");
    }
    ui.separator();
    controls(app, ui);
    ui.separator();
    calls(app, ui, now);
}

fn controls(app: &mut ScytheApp, ui: &mut Ui) {
    let paused = app.agents.paused();
    if paused {
        ui.label(RichText::new("Paused: agents can read the project, their changes are refused.").color(theme::YELLOW));
    }
    ui.horizontal_wrapped(|ui| {
        let undo = scythe_session::api::agents::agent_undo(&app.session);
        let tip = match &undo {
            Some(u) => format!("Undo {} ({})", u.label, u.agent),
            None => "The last undo step is not an agent's".into(),
        };
        if ui.add_enabled(undo.is_some(), egui::Button::new("Undo last agent change")).on_hover_text(tip).clicked() {
            app.undo();
        }
        let (label, tip) = if paused {
            ("Resume agents", "Let agents change the project again")
        } else {
            ("Pause agents", "Refuse agents' changes until you resume; reading stays allowed")
        };
        if ui.button(label).on_hover_text(tip).clicked() {
            app.session.agents.set_paused(!paused);
            app.notify(if paused { "Agents resumed" } else { "Agents paused: their changes are refused" });
        }
        let any = !app.agents.clients().is_empty();
        if ui.add_enabled(any, egui::Button::new("Disconnect all")).on_hover_text("Disconnect every agent").clicked() {
            app.session.agents.disconnect(None);
            app.notify("Agents disconnected");
        }
    });
}

fn calls(app: &mut ScytheApp, ui: &mut Ui, now: u64) {
    ui.label(RichText::new("Activity").font(theme::semibold(13.0)).color(TEXT_MID));
    let list: Vec<AgentCall> = app.agents.calls().rev().cloned().collect();
    if list.is_empty() {
        ui.label(RichText::new("No tool calls yet.").color(TEXT_DIM));
        return;
    }
    let several = app.agents.clients().iter().any(|c| c.name != app.agents.clients()[0].name);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for call in &list {
            row(ui, call, now, several);
        }
    });
}

fn row(ui: &mut Ui, call: &AgentCall, now: u64, several: bool) {
    let (color, word) = match call.ok {
        None => (ACCENT, "running"),
        Some(true) => (theme::GREEN.gamma_multiply(0.8), "done"),
        Some(false) => (PINK, "failed"),
    };
    ui.horizontal(|ui| {
        ui.add_sized(
            vec2(44.0, 18.0),
            egui::Label::new(RichText::new(ago(call.started_ms, now)).color(TEXT_DIM).monospace()),
        );
        let (dot, _) = ui.allocate_exact_size(vec2(10.0, 18.0), Sense::hover());
        ui.painter().circle_filled(dot.center(), 3.5, color);
        let who = if several { format!("{}: ", call.agent) } else { String::new() };
        ui.label(RichText::new(format!("{who}{}", call.summary)).color(TEXT)).on_hover_text(format!(
            "{} · {word}{}",
            call.tool,
            call.duration_ms.map(|d| format!(" in {d} ms")).unwrap_or_default()
        ));
    });
    // Under the summary: past the time and the mark (the label adds the second gap).
    let indent = 44.0 + 10.0 + ui.spacing().item_spacing.x;
    let note = |ui: &mut Ui, text: RichText| {
        let mut area = ui.available_rect_before_wrap();
        area.min.x += indent;
        ui.scope_builder(egui::UiBuilder::new().max_rect(area), |ui| ui.add(egui::Label::new(text).wrap()));
    };
    if let Some(error) = &call.error {
        note(ui, RichText::new(error).color(PINK).size(12.0));
    }
    if !call.undo.is_empty() {
        note(ui, RichText::new(format!("Undo: {}", call.undo.join(", "))).color(TEXT_DIM).size(12.0));
    }
}
