//! The instance list: every ScyEQ-Pro with its curve and bands, editable in place.

use egui::{Align2, CornerRadius, Pos2, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};
use scyeq_dsp::BandSettings;
use scyeq_dsp::filters::{Design, Prototype};
use scyeq_dsp::params::{self as p, BANDS, GLOBAL, band, global};

use crate::host::{Instance, InstanceList, WindowSpec};
use crate::model::{Plot, Scale, hz_text};
use crate::theme::{self, ACCENT, BG, RAISED, RAISED_HI, TEXT, TEXT_DIM, TEXT_MID, band_color};
use crate::widgets;

/// The instance list's window while `open`; its close button clears `open`.
pub fn instances_window(ctx: &egui::Context, list: &mut dyn InstanceList, open: &mut bool) {
    if !*open {
        return;
    }
    let spec = WindowSpec {
        key: "scythe-eq-instances",
        id: egui::Id::unique("scythe-eq-instances"),
        title: "ScyEQ-Pro instances",
        default_size: vec2(760.0, 420.0),
    };
    let frame = list.window();
    frame(ctx, &spec, open, &mut |ui| {
        let all = list.instance_list();
        if all.is_empty() {
            ui.label(egui::RichText::new("No ScyEQ-Pro in this project").color(TEXT_DIM));
            return;
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            for inst in all {
                instance_row(list, ui, &inst);
                ui.add_space(6.0);
            }
        });
    });
}

fn instance_row(list: &mut dyn InstanceList, ui: &mut Ui, inst: &Instance) {
    let Some(values) = list.instance_values(inst.id) else { return };
    if values.len() < p::TOTAL {
        return;
    }
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 92.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(10), RAISED);
    let name_rect = Rect::from_min_size(rect.min + vec2(10.0, 8.0), vec2(150.0, 20.0));
    let mut name_at = name_rect.left_center();
    if let Some(color) = inst.color {
        widgets::color_dot(ui.painter(), name_at + vec2(4.0, 0.0), color);
        name_at.x += 14.0;
    }
    ui.painter().text(name_at, Align2::LEFT_CENTER, &inst.label, theme::semibold(12.0), TEXT);
    let phase = ["Zero latency", "Natural", "Linear"][values[(GLOBAL + global::PHASE) as usize] as usize % 3];
    ui.painter().text(
        name_rect.left_center() + vec2(0.0, 18.0),
        Align2::LEFT_CENTER,
        phase,
        theme::regular(10.0),
        TEXT_DIM,
    );
    let key = |what: &str| inst.id.with(("eq-instances", what));
    if list.controls_instances() {
        open_and_bypass(list, ui, rect, inst);
    }
    let curve =
        Rect::from_min_max(pos2(rect.left() + 160.0, rect.top() + 8.0), pos2(rect.left() + 400.0, rect.bottom() - 8.0));
    let plot = Plot::new(curve, Scale::log(20.0, 20000.0), Scale::lin(-15.0, 15.0));
    ui.painter().rect_filled(curve, CornerRadius::same(6), BG);
    ui.painter().line_segment(
        [pos2(curve.left(), plot.sy(0.0)), pos2(curve.right(), plot.sy(0.0))],
        Stroke::new(1.0, RAISED_HI),
    );
    let designs: Vec<Design> =
        (0..BANDS).map(|b| BandSettings::read(&values, b)).filter(|s| s.active()).map(|s| s.design).collect();
    let protos: Vec<Prototype> = designs.iter().map(Prototype::design).collect();
    let pts: Vec<Pos2> = plot
        .xs(2.0)
        .into_iter()
        .map(|f| {
            let db: f64 = protos.iter().map(|p| 20.0 * p.magnitude(f).max(1e-9).log10()).sum();
            plot.point(f, db.clamp(-15.0, 15.0))
        })
        .collect();
    ui.painter_at(curve).add(Shape::line(pts, Stroke::new(1.6, ACCENT)));
    // Band chips: click bypasses, drag up and down sets the gain.
    let mut x = curve.right() + 10.0;
    let mut y = rect.top() + 8.0;
    for b in (0..BANDS).filter(|b| values[p::band_param(*b, band::USED) as usize] >= 0.5) {
        let s = BandSettings::read(&values, b);
        let text = if s.design.shape.has_gain() {
            format!("{} {} {:+.1}", b + 1, hz_text(s.design.freq), s.design.gain_db)
        } else {
            format!("{} {} {}", b + 1, s.design.shape.label(), hz_text(s.design.freq))
        };
        let w = widgets::text_width(ui, &text, theme::regular(10.0)) + 14.0;
        if x + w > rect.right() - 8.0 {
            x = curve.right() + 10.0;
            y += 26.0;
        }
        if y + 22.0 > rect.bottom() {
            break;
        }
        let r = Rect::from_min_size(pos2(x, y), vec2(w, 22.0));
        let resp = ui.interact(r, key(&format!("band{b}")), Sense::click_and_drag());
        let color = band_color(b);
        let fill = if s.enabled { theme::with_alpha(color, if resp.hovered() { 90 } else { 55 }) } else { BG };
        ui.painter().rect_filled(r, CornerRadius::same(6), fill);
        ui.painter().text(
            r.center(),
            Align2::CENTER_CENTER,
            &text,
            theme::regular(10.0),
            if s.enabled { TEXT } else { TEXT_DIM },
        );
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{} band {}", inst.label, b + 1))
        });
        if resp.clicked() {
            let id = p::band_param(b, band::ENABLED);
            list.begin_instance(inst.id, id);
            list.set_instance(inst.id, id, if s.enabled { 0.0 } else { 1.0 });
            list.end_instance(inst.id, id);
        }
        if s.design.shape.has_gain() {
            let id = p::band_param(b, band::GAIN);
            if resp.drag_started() {
                list.begin_instance(inst.id, id);
            }
            if resp.dragged() {
                let gain = values[id as usize] - resp.drag_delta().y as f64 * 0.1;
                list.set_instance(inst.id, id, gain.clamp(-30.0, 30.0));
            }
            if resp.drag_stopped() {
                list.end_instance(inst.id, id);
            }
        }
        x += w + 6.0;
    }
}

fn open_and_bypass(list: &mut dyn InstanceList, ui: &mut Ui, rect: Rect, inst: &Instance) {
    let open_r = Rect::from_min_size(rect.min + vec2(10.0, 50.0), vec2(56.0, 24.0));
    let is_open = list.instance_open(inst.id);
    if widgets::text_pill(
        ui,
        open_r,
        if is_open { "Close" } else { "Open" },
        theme::regular(11.0),
        if is_open { ACCENT } else { RAISED_HI },
        if is_open { BG } else { TEXT_MID },
    )
    .clicked()
    {
        list.toggle_instance_open(inst.id);
    }
    let enabled = list.instance_enabled(inst.id);
    let on_r = Rect::from_min_size(rect.min + vec2(72.0, 50.0), vec2(70.0, 24.0));
    if widgets::text_pill(
        ui,
        on_r,
        if enabled { "On" } else { "Bypassed" },
        theme::regular(11.0),
        if enabled { RAISED_HI } else { theme::SWITCH_OFF },
        TEXT_MID,
    )
    .clicked()
    {
        list.toggle_instance_bypass(inst.id);
    }
}
