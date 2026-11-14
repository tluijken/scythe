//! EQ Match: bands fitted so this instance's input moves towards another instance's signal
//! or a reference file.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

use egui::{CornerRadius, Id, Pos2, Sense, Shape, Stroke, Ui, pos2, vec2};
use scyeq_dsp::filters::{Design, Prototype};
use scyeq_dsp::matching::{self, MatchOptions};
use scyeq_dsp::params::{self as p, BANDS, band};
use scyeq_dsp::reference::{levels_at, third_octaves};

use crate::analyzer::{POINTS, grid_hz};
use crate::editor::{Cx, View};
use crate::host::{CompareSource, DecodeFn, WindowSpec};
use crate::model::{Plot, Scale};
use crate::theme::{self, ACCENT, BG, RAISED_HI, TEXT_DIM, TEXT_MID};
use crate::widgets;

#[derive(Clone, Debug, PartialEq, Default)]
enum Target {
    #[default]
    None,
    Source(CompareSource),
    File(PathBuf),
}

/// The EQ Match panel's state.
pub struct MatchPanel {
    reference: Target,
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
            reference: Target::None,
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
    pub fn capturing(&self) -> Option<Option<Id>> {
        let reference = match &self.reference {
            Target::Source(s) => Some(s.id),
            _ => None,
        };
        self.capturing.then_some(reference)
    }

    /// Drops a source reference that `known` no longer knows.
    pub(crate) fn forget_gone(&mut self, known: &dyn Fn(Id) -> bool) {
        if let Target::Source(s) = &self.reference
            && !known(s.id)
        {
            self.reference = Target::None;
            self.other = vec![0.0; POINTS];
            self.other_frames = 0;
        }
    }

    fn reference_levels(&self) -> Option<Vec<f64>> {
        match &self.reference {
            Target::File(_) => self.file.clone(),
            Target::Source(_) if self.other_frames > 0 => {
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

/// A reference file's third-octave levels on the analyzer's grid, measured on a thread of its
/// own.
fn measure_file(decode: DecodeFn, path: PathBuf) -> Receiver<Result<Vec<f64>, String>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let freqs: Vec<f64> = (0..POINTS).map(grid_hz).collect();
        let _ = tx.send(decode(&path).map(|a| levels_at(&third_octaves(&a.channels, a.sample_rate), &freqs)));
    });
    rx
}

pub(crate) fn window(cx: &mut Cx, ui: &mut Ui, view: &mut View, values: &[f64], others: &[CompareSource]) {
    let Some(mut panel) = view.match_panel.take() else { return };
    if panel.capturing {
        let (pre, other) = view.capture_curves();
        if let Some(c) = pre {
            for (s, d) in panel.current.iter_mut().zip(c) {
                *s += 10f64.powf(*d as f64 / 10.0);
            }
            panel.current_frames += 1;
        }
        if let Some(c) = other.filter(|_| matches!(panel.reference, Target::Source(_))) {
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
    let spec = WindowSpec {
        key: "scythe-eq-match",
        id: cx.salt.with("scythe-eq-match"),
        title: "EQ Match",
        default_size: vec2(520.0, 360.0),
    };
    let frame = cx.host.window();
    frame(&ctx, &spec, &mut open, &mut |ui| match_body(cx, ui, view, &mut panel, others, values));
    if open {
        view.match_panel = Some(panel);
    } else if view.other.is_some() && matches!(panel.reference, Target::Source(_)) {
        view.other = None;
    }
}

fn match_body(
    cx: &mut Cx,
    ui: &mut Ui,
    view: &mut View,
    panel: &mut MatchPanel,
    others: &[CompareSource],
    values: &[f64],
) {
    let decode = cx.host.decode_reference();
    let mut pick = false;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Reference").color(TEXT_DIM));
        let current = match &panel.reference {
            Target::None => "Choose…".to_owned(),
            Target::Source(i) => i.label.clone(),
            Target::File(f) => {
                f.file_name().map_or_else(|| f.display().to_string(), |n| n.to_string_lossy().into_owned())
            }
        };
        egui::ComboBox::from_id_salt(cx.salt.with("eq-match-ref")).selected_text(current).show_ui(ui, |ui| {
            for o in others {
                if reference_entry(ui, panel.reference == Target::Source(o.clone()), o).clicked() {
                    panel.reference = Target::Source(o.clone());
                    panel.other = vec![0.0; POINTS];
                    panel.other_frames = 0;
                    view.other = Some(o.id);
                }
            }
            if decode.is_some()
                && ui.selectable_label(matches!(panel.reference, Target::File(_)), "Audio file…").clicked()
            {
                pick = true;
            }
        });
        let picked = if pick { cx.host.pick_reference() } else { None };
        if let Some(decode) = decode
            && let Some(path) = picked.or_else(|| cx.host.picked_reference())
        {
            panel.file = None;
            panel.job = Some(measure_file(decode, path.clone()));
            panel.reference = Target::File(path);
        }
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
        (Target::None, _) => "Pick a track, another ScyEQ-Pro or an audio file to match to".to_owned(),
        _ => format!(
            "Input learned {:.0} s{}",
            secs(panel.current_frames),
            if matches!(panel.reference, Target::Source(_)) {
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
            cx.edit("EQ match", |eq| {
                matching::apply_bands(eq, &proposal);
            });
            panel.capturing = false;
        }
        if free < panel.proposal.len() {
            ui.label(egui::RichText::new(format!("only {free} free band(s)")).color(TEXT_DIM));
        }
    });
}

/// A reference to choose, full width as the dropdown lays it out, with the source's colour as
/// a dot in room the label leaves before its text.
fn reference_entry(ui: &mut Ui, selected: bool, source: &CompareSource) -> egui::Response {
    let Some(color) = source.color else { return ui.selectable_label(selected, &source.label) };
    let resp = ui.selectable_label(selected, format!("    {}", source.label));
    widgets::color_dot(ui.painter(), resp.rect.left_center() + vec2(8.0, 0.0), color);
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, ui.is_enabled(), selected, &source.label));
    resp
}
