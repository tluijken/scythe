//! The performance meter (REAPER's View: Show performance meter window): how much of the
//! real-time budget the audio takes, the longest block, xruns, and per track the CPU it takes,
//! its longest block and plugin delay; hovering a track lists its effects.

use std::time::Duration;

use egui::{RichText, Ui, vec2};

use crate::app::ScytheApp;
use crate::theme::{self, TEXT_DIM};
use crate::ui::window::{WindowKind, scythe_window};

pub fn show(app: &mut ScytheApp, ui: &mut Ui) {
    if !app.view.performance {
        return;
    }
    let ctx = ui.ctx().clone();
    let mut open = true;
    let meter = app.performance().clone();
    scythe_window("performance-meter", "Performance meter", WindowKind::Panel).default_size(vec2(560.0, 460.0)).show(
        &ctx,
        &mut open,
        |ui| {
            let over = meter.rt_longest_block_ms > meter.block_ms && meter.block_ms > 0.0;
            ui.label(RichText::new(format!("RT CPU: {:.1}%", meter.rt_cpu)).font(theme::mono(12.0)));
            ui.label(
                RichText::new(format!("RT longest-block: {:.2}ms/{:.2}ms", meter.rt_longest_block_ms, meter.block_ms))
                    .font(theme::mono(12.0))
                    .color(if over { theme::YELLOW } else { ui.visuals().text_color() }),
            );
            ui.label(
                RichText::new(format!("RT xruns: {} · late blocks: {}", meter.xruns, meter.overloads))
                    .font(theme::mono(12.0)),
            );
            ui.label(
                RichText::new(format!("Tracks: {:.1}% of one core", meter.tracks_cpu))
                    .font(theme::mono(12.0))
                    .color(TEXT_DIM),
            );
            ui.add_space(6.0);
            let mut rows: Vec<_> = meter.tracks.iter().collect();
            rows.sort_by(|a, b| b.cpu.total_cmp(&a.cpu));
            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("performance-tracks").striped(true).spacing(vec2(14.0, 4.0)).show(ui, |ui| {
                    for head in ["#", "Track", "FX CPU", "FX longest-block", "# FX", "PDC"] {
                        ui.label(RichText::new(head).color(TEXT_DIM).size(11.0));
                    }
                    ui.end_row();
                    for t in rows {
                        let number = if t.index == 0 { "M".to_owned() } else { t.index.to_string() };
                        ui.label(RichText::new(number).color(TEXT_DIM));
                        let name = ui.label(&t.name);
                        if !t.fx.is_empty() {
                            let list: Vec<String> =
                                t.fx.iter()
                                    .map(|f| format!("{:>5.1}%  {}", f.cpu, scythe_engine::stock::shown_name(&f.name)))
                                    .collect();
                            name.on_hover_text(RichText::new(list.join("\n")).font(theme::mono(11.0)));
                        }
                        ui.label(RichText::new(format!("{:.1}%", t.cpu)).font(theme::mono(11.0)));
                        ui.label(RichText::new(format!("{:.2}ms", t.longest_block_ms)).font(theme::mono(11.0)));
                        ui.label(RichText::new(t.fx.len().to_string()).font(theme::mono(11.0)));
                        ui.label(RichText::new(t.pdc.to_string()).font(theme::mono(11.0)));
                        ui.end_row();
                    }
                });
            });
        },
    );
    ctx.request_repaint_after(Duration::from_millis(500));
    app.view.performance = open;
}
