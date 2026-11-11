//! Scythe EQ across the project: EQ Match (bands fitted so this instance's input moves towards
//! another instance's signal or a reference file) and the instance list, every Scythe EQ in
//! one window.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

use egui::{Align2, CornerRadius, Id, Pos2, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};
use scythe_core::FxSlot;
use scythe_engine::fx::FxOwner;
use scythe_engine::scythe_eq::filters::{Design, Prototype};
use scythe_engine::scythe_eq::matching::{self, MatchOptions};
use scythe_engine::scythe_eq::params::{self as p, BANDS, GLOBAL, band, global};
use scythe_engine::scythe_eq::{self, BandSettings, ScytheEq};

use super::analyzer::{POINTS, grid_hz};
use super::model::{Plot, Scale, hz_text};
use super::scythe_eq::EqView;
use super::{Ctx, band_color, chain_slots, slot_at};
use crate::app::ScytheApp;
use crate::theme::{self, ACCENT, BG, RAISED, RAISED_HI, TEXT, TEXT_DIM, TEXT_MID};
use crate::ui::widgets;
use crate::ui::window::{WindowKind, scythe_window};

/// A Scythe EQ somewhere in the project.
#[derive(Clone, Debug, PartialEq)]
pub struct Instance {
    pub owner: FxOwner,
    pub path: Vec<usize>,
    pub label: String,
}

/// Every Scythe EQ in the project, in track order: each track's chain, input chain and take
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

/// The other Scythe EQs, for comparing and matching.
pub fn other_instances(cx: &Ctx) -> Vec<Instance> {
    all_instances(cx.app).into_iter().filter(|i| !(i.owner == cx.ed.owner && i.path == cx.ed.path)).collect()
}

/// What an instance can compare and match with: the other Scythe EQs, then every track's
/// and the master's output after its effects (an empty path).
pub fn compare_sources(cx: &Ctx) -> Vec<Instance> {
    let mut out: Vec<Instance> =
        other_instances(cx).into_iter().map(|i| Instance { label: format!("{} (Scythe EQ)", i.label), ..i }).collect();
    for t in &cx.app.project.tracks {
        let label = if t.name.is_empty() { "Track".to_owned() } else { t.name.clone() };
        out.push(Instance { owner: FxOwner::Track(t.id), path: Vec::new(), label });
    }
    out.push(Instance { owner: FxOwner::Master, path: Vec::new(), label: "Master".to_owned() });
    out
}

/// The tap on a track's (or the master's) output after its effects.
pub fn track_probe(app: &ScytheApp, owner: &FxOwner) -> Option<std::sync::Arc<scythe_engine::probe::FxProbe>> {
    let meters = app.engine.meters();
    match owner {
        FxOwner::Track(id) => meters.get(*id).map(|m| m.probe.clone()),
        FxOwner::Master => Some(meters.master.probe.clone()),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
enum Reference {
    #[default]
    None,
    Instance(Instance),
    File(PathBuf),
}

/// The EQ Match panel's state.
pub struct MatchPanel {
    reference: Reference,
    capturing: bool,
    /// Summed power per grid point of this instance's input and of the reference.
    current: Vec<f64>,
    current_frames: usize,
    other: Vec<f64>,
    other_frames: usize,
    /// A reference file's levels on the grid, once measured.
    file: Option<Vec<f64>>,
    job: Option<Receiver<Result<Vec<f64>, String>>>,
    amount: f64,
    smoothing: f64,
    max_bands: usize,
    proposal: Vec<Design>,
    target: Vec<f64>,
    error: Option<String>,
}

impl Default for MatchPanel {
    fn default() -> Self {
        Self {
            reference: Reference::None,
            capturing: false,
            current: vec![0.0; POINTS],
            current_frames: 0,
            other: vec![0.0; POINTS],
            other_frames: 0,
            file: None,
            job: None,
            amount: 100.0,
            smoothing: 1.0 / 3.0,
            max_bands: 8,
            proposal: Vec::new(),
            target: Vec::new(),
            error: None,
        }
    }
}

impl MatchPanel {
    /// Whether the analyzer must listen to the input and the reference instance.
    pub fn capturing(&self) -> Option<Option<(FxOwner, Vec<usize>)>> {
        self.capturing.then(|| match &self.reference {
            Reference::Instance(i) => Some((i.owner.clone(), i.path.clone())),
            _ => None,
        })
    }

    fn reference_levels(&self) -> Option<Vec<f64>> {
        match &self.reference {
            Reference::File(_) => self.file.clone(),
            Reference::Instance(_) if self.other_frames > 0 => {
                Some(self.other.iter().map(|s| 10.0 * (s / self.other_frames as f64).max(1e-20).log10()).collect())
            }
            _ => None,
        }
    }

    fn current_levels(&self) -> Option<Vec<f64>> {
        (self.current_frames > 0)
            .then(|| self.current.iter().map(|s| 10.0 * (s / self.current_frames as f64).max(1e-20).log10()).collect())
    }

    fn compute(&mut self) {
        let (Some(cur), Some(reference)) = (self.current_levels(), self.reference_levels()) else {
            self.proposal.clear();
            self.target.clear();
            return;
        };
        let freqs: Vec<f64> = (0..POINTS).map(grid_hz).collect();
        let o = MatchOptions {
            amount: self.amount / 100.0,
            smoothing: self.smoothing,
            max_bands: self.max_bands,
            ..Default::default()
        };
        // Levels too low to measure would only add noise to the difference.
        let usable: Vec<bool> = cur.iter().zip(&reference).map(|(c, r)| *c > -110.0 && *r > -110.0).collect();
        let cur: Vec<f64> = cur.iter().zip(&usable).map(|(c, u)| if *u { *c } else { 0.0 }).collect();
        let reference: Vec<f64> = reference.iter().zip(&usable).map(|(r, u)| if *u { *r } else { 0.0 }).collect();
        self.target = matching::target(&freqs, &cur, &reference, &o);
        self.proposal = matching::fit(&freqs, &self.target, &o);
    }
}

/// A reference file measured on the analyzer's grid: its third-octave levels interpolated.
fn measure_file(path: PathBuf) -> Receiver<Result<Vec<f64>, String>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let result = scythe_engine::media::decode_file(&path).map_err(|e| e.to_string()).map(|audio| {
            let a = scythe_engine::analysis::analyse(&audio.channels, audio.sample_rate, None, None);
            let hz: Vec<f64> = a.bands.iter().map(|b| b.hz.ln()).collect();
            let db: Vec<f64> = a.bands.iter().map(|b| b.level_db).collect();
            (0..POINTS)
                .map(|i| {
                    let x = grid_hz(i).ln();
                    match hz.iter().position(|h| *h >= x) {
                        Some(0) => db[0],
                        Some(k) => {
                            let t = (x - hz[k - 1]) / (hz[k] - hz[k - 1]);
                            db[k - 1] + t * (db[k] - db[k - 1])
                        }
                        None => *db.last().unwrap_or(&-150.0),
                    }
                })
                .collect()
        });
        let _ = tx.send(result);
    });
    rx
}

pub(super) fn window(cx: &mut Ctx, ui: &mut Ui, view: &mut EqView, values: &[f64]) {
    let Some(mut panel) = view.match_panel.take() else { return };
    let others = compare_sources(cx);
    if panel.capturing {
        let (pre, other) = view.capture_curves();
        if let Some(c) = pre {
            for (s, d) in panel.current.iter_mut().zip(c) {
                *s += 10f64.powf(*d as f64 / 10.0);
            }
            panel.current_frames += 1;
        }
        if let Some(c) = other.filter(|_| matches!(panel.reference, Reference::Instance(_))) {
            for (s, d) in panel.other.iter_mut().zip(c) {
                *s += 10f64.powf(*d as f64 / 10.0);
            }
            panel.other_frames += 1;
        }
    }
    if let Some(rx) = &panel.job
        && let Ok(result) = rx.try_recv()
    {
        match result {
            Ok(levels) => panel.file = Some(levels),
            Err(e) => panel.error = Some(e),
        }
        panel.job = None;
    }
    panel.compute();
    let mut open = true;
    let ctx = ui.ctx().clone();
    scythe_window("scythe-eq-match", "EQ Match".to_owned(), WindowKind::Panel)
        .id(Id::unique(("scythe-eq-match", &cx.ed.owner, &cx.ed.path)))
        .default_size(vec2(520.0, 360.0))
        .show(&ctx, &mut open, |ui| match_body(cx, ui, view, &mut panel, &others, values));
    if open {
        view.match_panel = Some(panel);
    } else if view.other.is_some() && matches!(panel.reference, Reference::Instance(_)) {
        view.other = None;
    }
}

fn match_body(
    cx: &mut Ctx,
    ui: &mut Ui,
    view: &mut EqView,
    panel: &mut MatchPanel,
    others: &[Instance],
    values: &[f64],
) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Reference").color(TEXT_DIM));
        let current = match &panel.reference {
            Reference::None => "Choose…".to_owned(),
            Reference::Instance(i) => i.label.clone(),
            Reference::File(f) => {
                f.file_name().map_or_else(|| f.display().to_string(), |n| n.to_string_lossy().into_owned())
            }
        };
        egui::ComboBox::from_id_salt(("eq-match-ref", &cx.ed.owner, &cx.ed.path)).selected_text(current).show_ui(
            ui,
            |ui| {
                for o in others {
                    if ui.selectable_label(panel.reference == Reference::Instance(o.clone()), &o.label).clicked() {
                        panel.reference = Reference::Instance(o.clone());
                        panel.other = vec![0.0; POINTS];
                        panel.other_frames = 0;
                        view.other = Some((o.owner.clone(), o.path.clone()));
                    }
                }
                if ui.selectable_label(matches!(panel.reference, Reference::File(_)), "Audio file…").clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("Audio", &["wav", "flac", "mp3", "ogg", "aif", "aiff"])
                        .pick_file()
                {
                    panel.file = None;
                    panel.job = Some(measure_file(path.clone()));
                    panel.reference = Reference::File(path);
                }
            },
        );
        let label = if panel.capturing { "Stop learning" } else { "Learn" };
        if ui
            .button(label)
            .on_hover_text("Average this EQ's input (and the reference track or instance) while playing")
            .clicked()
        {
            panel.capturing = !panel.capturing;
        }
        if ui.button("Reset").clicked() {
            panel.current = vec![0.0; POINTS];
            panel.current_frames = 0;
            panel.other = vec![0.0; POINTS];
            panel.other_frames = 0;
        }
    });
    let secs = |frames: usize| frames as f64 / 60.0;
    let status = match (&panel.reference, panel.job.is_some()) {
        (_, true) => "Measuring the file…".to_owned(),
        (Reference::None, _) => "Pick a track, another Scythe EQ or an audio file to match to".to_owned(),
        _ => format!(
            "Input learned {:.0} s{}",
            secs(panel.current_frames),
            if matches!(panel.reference, Reference::Instance(_)) {
                format!(" · reference {:.0} s", secs(panel.other_frames))
            } else {
                String::new()
            }
        ),
    };
    ui.label(egui::RichText::new(status).color(TEXT_MID));
    if let Some(e) = &panel.error {
        ui.label(egui::RichText::new(e).color(theme::PINK));
    }
    ui.horizontal(|ui| {
        ui.label("Amount");
        ui.add(egui::Slider::new(&mut panel.amount, 0.0..=100.0).suffix(" %"));
        ui.label("Smoothing");
        ui.add(egui::Slider::new(&mut panel.smoothing, 0.05..=2.0).logarithmic(true).suffix(" oct"));
    });
    ui.horizontal(|ui| {
        ui.label("Bands");
        ui.add(egui::Slider::new(&mut panel.max_bands, 1..=16));
    });
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), (ui.available_height() - 36.0).max(80.0)), Sense::hover());
    let plot = Plot::new(rect, Scale::log(20.0, 20000.0), Scale::lin(-12.0, 12.0));
    ui.painter().rect_filled(rect, CornerRadius::same(8), BG);
    ui.painter()
        .line_segment([pos2(rect.left(), plot.sy(0.0)), pos2(rect.right(), plot.sy(0.0))], Stroke::new(1.0, RAISED_HI));
    if !panel.target.is_empty() {
        let pts: Vec<Pos2> = (0..POINTS)
            .filter(|i| (20.0..=20000.0).contains(&grid_hz(*i)))
            .map(|i| plot.point(grid_hz(i), panel.target[i].clamp(-12.0, 12.0)))
            .collect();
        ui.painter_at(rect).add(Shape::dashed_line(&pts, Stroke::new(1.0, TEXT_DIM), 4.0, 3.0));
        let fit: Vec<Pos2> = plot
            .xs(2.0)
            .into_iter()
            .map(|f| {
                let db: f64 =
                    panel.proposal.iter().map(|d| 20.0 * Prototype::design(d).magnitude(f).max(1e-9).log10()).sum();
                plot.point(f, db.clamp(-12.0, 12.0))
            })
            .collect();
        ui.painter_at(rect).add(Shape::line(fit, Stroke::new(2.0, ACCENT)));
    }
    let free = (0..BANDS).filter(|b| values[p::band_param(*b, band::USED) as usize] < 0.5).count();
    ui.horizontal(|ui| {
        let n = panel.proposal.len().min(free);
        let apply = ui.add_enabled(n > 0, egui::Button::new(format!("Apply {n} band(s)")));
        if apply.clicked() {
            let proposal = panel.proposal.clone();
            cx.edit_once("EQ match", |eq: &mut ScytheEq| matching::apply_bands(eq, &proposal));
            panel.capturing = false;
        }
        if free < panel.proposal.len() {
            ui.label(egui::RichText::new(format!("only {free} free band(s)")).color(TEXT_DIM));
        }
    });
}

/// The instance list: every Scythe EQ with its curve and bands, editable in place.
pub fn instances_window(app: &mut ScytheApp, ui: &mut Ui) {
    if !app.view.eq_instances {
        return;
    }
    let mut open = true;
    let ctx = ui.ctx().clone();
    scythe_window("scythe-eq-instances", "Scythe EQ instances".to_owned(), WindowKind::Panel)
        .default_size(vec2(760.0, 420.0))
        .show(&ctx, &mut open, |ui| {
            let list = all_instances(app);
            if list.is_empty() {
                ui.label(egui::RichText::new("No Scythe EQ in this project").color(TEXT_DIM));
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
