//! ScyEQ-Pro in Scythe: its editor (`scyeq_ui`) on a running instance, the compare sources
//! the project offers, and the instance list, every ScyEQ-Pro in one window.

use std::path::Path;
use std::sync::Arc;

use egui::{Align2, CornerRadius, Id, Pos2, Rect, Sense, Shape, Stroke, Ui, UiBuilder, pos2, vec2};
use scyeq_ui::model::{Plot, Scale, hz_text};
use scyeq_ui::{CompareSource, EqHost, Reference, ViewState, WindowFn, WindowSpec};
use scythe_core::FxSlot;
use scythe_engine::fx::FxOwner;
use scythe_engine::probe::FxProbe;
use scythe_engine::scythe_eq::filters::{Design, Prototype};
use scythe_engine::scythe_eq::params::{self as p, BANDS, GLOBAL, band, global};
use scythe_engine::scythe_eq::{self, BandSettings, Meters, ScytheEq};

use super::{Ctx, band_color, chain_slots, slot_at};
use crate::app::ScytheApp;
use crate::theme::{self, ACCENT, BG, RAISED, RAISED_HI, TEXT, TEXT_DIM, TEXT_MID};
use crate::ui::widgets;
use crate::ui::window::{WindowKind, scythe_window};

/// The editor of the ScyEQ-Pro `cx` shows, in `rect`.
pub fn show(cx: &mut Ctx, ui: &mut Ui, rect: Rect) {
    let Some((values, meters, latency, sample_rate)) = cx
        .inst
        .with(|e| {
            let latency = e.latency();
            let eq = e.as_any_mut().and_then(|a| a.downcast_mut::<ScytheEq>())?;
            Some((eq.values().to_vec(), eq.meters(), latency, eq.sample_rate()))
        })
        .flatten()
    else {
        return;
    };
    let mut state = cx.ed.eq.take().unwrap_or_default();
    state.controls = std::mem::take(&mut cx.ed.controls);
    let view = ViewState { instances: cx.app.view.eq_instances };
    let mut host = Host { cx, values, meters, latency, sample_rate, view };
    scyeq_ui::show(&mut ui.new_child(UiBuilder::new().max_rect(rect)), &mut host, &mut state);
    let Host { cx, view, .. } = host;
    cx.app.view.eq_instances = view.instances;
    cx.ed.controls = std::mem::take(&mut state.controls);
    cx.ed.eq = Some(state);
}

/// The running ScyEQ-Pro behind an editor, its changes undo steps of the session.
struct Host<'c, 'a> {
    cx: &'c mut Ctx<'a>,
    values: Vec<f64>,
    meters: Arc<Meters>,
    latency: usize,
    sample_rate: f64,
    view: ViewState,
}

impl EqHost for Host<'_, '_> {
    fn values(&self) -> &[f64] {
        &self.values
    }

    fn begin(&mut self, _id: u32) {}

    fn set(&mut self, id: u32, value: f64) {
        let label = format!("Adjust {}", p::name(id));
        let now = self.cx.app.fx_gesture(&label, &self.cx.inst, |e| {
            e.as_any_mut().and_then(|a| a.downcast_mut::<ScytheEq>()).map(|eq| {
                eq.set(id, value);
                eq.value(id)
            })
        });
        if let Some(Some(v)) = now
            && let Some(at) = self.values.get_mut(id as usize)
        {
            *at = v;
        }
    }

    fn end(&mut self, _id: u32) {
        self.cx.app.end_gesture();
    }

    fn edit(&mut self, label: &str, f: &mut dyn FnMut(&mut ScytheEq)) {
        let values = self.cx.edit_once(label, |eq: &mut ScytheEq| {
            f(eq);
            eq.values().to_vec()
        });
        if let Some(values) = values {
            self.values = values;
        }
    }

    fn meters(&self) -> Option<Arc<Meters>> {
        Some(self.meters.clone())
    }

    fn probe(&self) -> Option<Arc<FxProbe>> {
        Some(self.cx.inst.probe.clone())
    }

    fn latency(&self) -> usize {
        self.latency
    }

    fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    fn instances(&self) -> Vec<scyeq_ui::Instance> {
        all_instances(self.cx.app)
            .into_iter()
            .filter(|i| !(i.owner == self.cx.ed.owner && i.path == self.cx.ed.path))
            .map(|i| scyeq_ui::Instance { id: source_id(&i.owner, &i.path), label: i.label })
            .collect()
    }

    fn compare_sources(&self) -> Vec<CompareSource> {
        track_outputs(self.cx.app)
            .into_iter()
            .map(|(owner, label)| CompareSource { id: source_id(&owner, &[]), label })
            .collect()
    }

    fn source_probe(&self, id: Id) -> Option<Arc<FxProbe>> {
        let app = &*self.cx.app;
        if let Some((owner, _)) = track_outputs(app).into_iter().find(|(o, _)| source_id(o, &[]) == id) {
            return track_probe(app, &owner);
        }
        let i = all_instances(app).into_iter().find(|i| source_id(&i.owner, &i.path) == id)?;
        let slots = chain_slots(app, &i.owner);
        app.engine.fx().instance_at(&i.owner, &slots, &i.path).map(|i| i.probe.clone())
    }

    fn measure_reference(&self) -> Option<scyeq_ui::MeasureFn> {
        Some(measure_file)
    }

    fn view(&mut self) -> &mut ViewState {
        &mut self.view
    }

    fn can_modulate(&self) -> bool {
        self.cx.ed.track_fx().is_some()
    }

    fn modulate(&mut self, id: u32) {
        self.cx.modulate(id);
    }

    fn window(&self) -> WindowFn {
        panel
    }
}

/// Where a compare source is: a ScyEQ-Pro's chain and slot, or a track's output (no path).
fn source_id(owner: &FxOwner, path: &[usize]) -> Id {
    Id::unique(("scyeq-source", owner, path))
}

/// Every track's and the master's output after its effects, with their names.
fn track_outputs(app: &ScytheApp) -> Vec<(FxOwner, String)> {
    let mut out: Vec<(FxOwner, String)> = app
        .project
        .tracks
        .iter()
        .map(|t| (FxOwner::Track(t.id), if t.name.is_empty() { "Track".to_owned() } else { t.name.clone() }))
        .collect();
    out.push((FxOwner::Master, "Master".to_owned()));
    out
}

/// The tap on a track's (or the master's) output after its effects.
fn track_probe(app: &ScytheApp, owner: &FxOwner) -> Option<Arc<FxProbe>> {
    let meters = app.engine.meters();
    match owner {
        FxOwner::Track(id) => meters.get(*id).map(|m| m.probe.clone()),
        FxOwner::Master => Some(meters.master.probe.clone()),
        _ => None,
    }
}

/// A reference file's third-octave levels.
fn measure_file(path: &Path) -> Result<Reference, String> {
    let audio = scythe_engine::media::decode_file(path).map_err(|e| e.to_string())?;
    let a = scythe_engine::analysis::analyse(&audio.channels, audio.sample_rate, None, None);
    Ok(Reference { bands: a.bands.iter().map(|b| (b.hz, b.level_db)).collect() })
}

fn panel(ctx: &egui::Context, spec: &WindowSpec, open: &mut bool, body: &mut dyn FnMut(&mut Ui)) {
    scythe_window(spec.key, spec.title, WindowKind::Panel).id(spec.id).default_size(spec.default_size).show(
        ctx,
        open,
        |ui| body(ui),
    );
}

/// A ScyEQ-Pro somewhere in the project.
#[derive(Clone, Debug, PartialEq)]
pub struct Instance {
    pub owner: FxOwner,
    pub path: Vec<usize>,
    pub label: String,
}

/// Every ScyEQ-Pro in the project, in track order: each track's chain, input chain and take
/// chains, then the master's and the monitoring chain, containers included. The label says
/// where: "Vocals", "Vocals · input", "Guitar · Rig › 3".
pub fn all_instances(app: &ScytheApp) -> Vec<Instance> {
    let mut out = Vec::new();
    for t in &app.project.tracks {
        let name = if t.name.is_empty() { "Track" } else { t.name.as_str() };
        add_chain(&mut out, FxOwner::Track(t.id), &t.fx, name);
        add_chain(&mut out, FxOwner::Input(t.id), &t.input_fx(), &format!("{name} · input"));
        for it in &t.items {
            for (k, take) in it.takes.iter().enumerate() {
                let label = if take.name.is_empty() {
                    format!("{name} · take {}", k + 1)
                } else {
                    format!("{name} · {}", take.name)
                };
                add_chain(&mut out, FxOwner::Take(it.id, k), &take.fx(), &label);
            }
        }
    }
    add_chain(&mut out, FxOwner::Master, &app.project.master.fx, "Master");
    add_chain(&mut out, FxOwner::Monitor, &app.monitor_fx(), "Monitoring");
    out
}

fn add_chain(out: &mut Vec<Instance>, owner: FxOwner, slots: &[FxSlot], name: &str) {
    let start = out.len();
    walk(out, &owner, slots, &mut Vec::new(), &mut Vec::new(), name);
    // A lone top-level instance goes by its chain's name alone.
    if out.len() - start == 1 && out[start].path.len() == 1 {
        out[start].label = name.to_owned();
    }
}

fn walk(
    out: &mut Vec<Instance>,
    owner: &FxOwner,
    slots: &[FxSlot],
    path: &mut Vec<usize>,
    crumbs: &mut Vec<String>,
    name: &str,
) {
    for (i, slot) in slots.iter().enumerate() {
        path.push(i);
        if scythe_eq::is_slot(slot) {
            crumbs.push((i + 1).to_string());
            out.push(Instance {
                owner: owner.clone(),
                path: path.clone(),
                label: format!("{name} · {}", crumbs.join(" › ")),
            });
            crumbs.pop();
        } else if slot.is_container() {
            crumbs.push(if slot.name.is_empty() { "Container".to_owned() } else { slot.name.clone() });
            walk(out, owner, &slot.container_children(), path, crumbs, name);
            crumbs.pop();
        }
        path.pop();
    }
}

/// The instance list: every ScyEQ-Pro with its curve and bands, editable in place.
pub fn instances_window(app: &mut ScytheApp, ui: &mut Ui) {
    if !app.view.eq_instances {
        return;
    }
    let mut open = true;
    let ctx = ui.ctx().clone();
    scythe_window("scythe-eq-instances", "ScyEQ-Pro instances".to_owned(), WindowKind::Panel)
        .default_size(vec2(760.0, 420.0))
        .show(&ctx, &mut open, |ui| {
            let list = all_instances(app);
            if list.is_empty() {
                ui.label(egui::RichText::new("No ScyEQ-Pro in this project").color(TEXT_DIM));
                return;
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                for inst in list {
                    instance_row(app, ui, &inst);
                    ui.add_space(6.0);
                }
            });
        });
    if !open {
        app.view.eq_instances = false;
        if app.session.in_gesture() {
            app.end_gesture();
        }
    }
}

fn instance_row(app: &mut ScytheApp, ui: &mut Ui, inst: &Instance) {
    let slots = chain_slots(app, &inst.owner);
    let Some(slot) = slot_at(&slots, &inst.path) else { return };
    let running = app.engine.fx().instance_at(&inst.owner, &slots, &inst.path);
    let values = running
        .as_ref()
        .and_then(|i| {
            i.with(|e| e.as_any_mut().and_then(|a| a.downcast_mut::<ScytheEq>()).map(|eq| eq.values().to_vec()))
                .flatten()
        })
        .or_else(|| scythe_eq::slot_values(&slot))
        .unwrap_or_else(p::defaults);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 92.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(10), RAISED);
    let name_rect = Rect::from_min_size(rect.min + vec2(10.0, 8.0), vec2(150.0, 20.0));
    ui.painter().text(name_rect.left_center(), Align2::LEFT_CENTER, &inst.label, theme::semibold(12.0), TEXT);
    let phase = ["Zero latency", "Natural", "Linear"][values[(GLOBAL + global::PHASE) as usize] as usize % 3];
    ui.painter().text(
        name_rect.left_center() + vec2(0.0, 18.0),
        Align2::LEFT_CENTER,
        phase,
        theme::regular(10.0),
        TEXT_DIM,
    );
    let key = |what: &str| Id::unique(("eq-instances", &inst.owner, &inst.path, what));
    let open_r = Rect::from_min_size(rect.min + vec2(10.0, 50.0), vec2(56.0, 24.0));
    let is_open = super::is_open_at(app, &inst.owner, &inst.path);
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
        super::toggle_at(app, &inst.owner, &inst.path);
    }
    let on_r = Rect::from_min_size(rect.min + vec2(72.0, 50.0), vec2(70.0, 24.0));
    if widgets::text_pill(
        ui,
        on_r,
        if slot.enabled { "On" } else { "Bypassed" },
        theme::regular(11.0),
        if slot.enabled { RAISED_HI } else { theme::SWITCH_OFF },
        TEXT_MID,
    )
    .clicked()
    {
        super::edit_slot(app, &inst.owner, &inst.path, "Toggle FX bypass", &|s| s.enabled = !s.enabled);
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
        if let Some(fx) = &running {
            if resp.clicked() {
                app.end_gesture();
                app.fx_gesture("Bypass EQ band", fx, |e| {
                    e.set_param(p::band_param(b, band::ENABLED), if s.enabled { 0.0 } else { 1.0 })
                });
                app.end_gesture();
            }
            if resp.dragged() && s.design.shape.has_gain() {
                let gain = values[p::band_param(b, band::GAIN) as usize] - resp.drag_delta().y as f64 * 0.1;
                app.fx_gesture("Adjust EQ band gain", fx, |e| {
                    e.set_param(p::band_param(b, band::GAIN), gain.clamp(-30.0, 30.0))
                });
            }
            if resp.drag_stopped() {
                app.end_gesture();
            }
        }
        x += w + 6.0;
    }
}
