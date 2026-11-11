//! Scythe EQ's editor: the analyzer behind the curve of every band and of the whole EQ, a
//! handle per band, a piano strip under the frequency axis, the selected band's controls and
//! the global ones.
//!
//! Mouse: double-click empty space adds a band (a cut near either end), double-click a handle
//! bypasses it; drag handles (Shift: fine), drag empty space to select several, or drag a
//! highlighted spectrum peak to make a band there; the wheel sets Q; Alt-click solos a band
//! while held; right-click for a band's menu. Keys: arrows nudge the selection, Delete removes
//! it.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Align2, Color32, CornerRadius, Id, Mesh, Pos2, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};
use scythe_engine::fx::FxOwner;
use scythe_engine::probe::{Side, Watch};
use scythe_engine::scythe_eq::filters::{Design, Prototype, SLOPES, Shape as BandShape, slope_label};
use scythe_engine::scythe_eq::matching::collisions;
use scythe_engine::scythe_eq::params::{self as p, BANDS, CHARACTERS, GLOBAL, Placement, RESOLUTIONS, band, global};
use scythe_engine::scythe_eq::{BandSettings, Meters, ScytheEq};

use super::analyzer::{self, Analyzer, Curve, POINTS, Settings, Source, grid_hz, grid_pos, note_name};
use super::model::{Plot, Scale, hz_text};
use super::{Ctx, band_color, split_top, value_box};
use crate::theme::{self, ACCENT, BG, PANEL, RAISED_HI, TEXT, TEXT_DIM, TEXT_MID};
use crate::ui::widgets;

const POST: Color32 = Color32::from_rgb(0xc9, 0xd6, 0xdf);
const SIDECHAIN: Color32 = Color32::from_rgb(0xf9, 0x26, 0x72);
const OTHER: Color32 = Color32::from_rgb(0x66, 0xd9, 0xef);
const COLLISION: Color32 = Color32::from_rgb(0xff, 0x5f, 0x45);
const PIANO_H: f32 = 16.0;

/// What the editor keeps between frames.
pub struct EqView {
    analyzer: Analyzer,
    curves: [Curve; 4],
    /// The curves as drawn: eased towards the analyzer's so a 60 Hz display stays fluid.
    shown: [Vec<f32>; 4],
    sidechain_watch: Option<Watch>,
    other_watch: Option<Watch>,
    pub selected: BTreeSet<usize>,
    drag: Option<Drag>,
    /// The band soloed while Alt is held on it.
    alt_solo: Option<usize>,
    /// EQ scale: ± this many dB.
    pub range_db: f64,
    /// Another Scythe EQ, or a track's output when the path is empty, to show (and find
    /// collisions with).
    pub other: Option<(FxOwner, Vec<usize>)>,
    pub collisions: bool,
    pub frozen: bool,
    last: Instant,
    ui_cost: Duration,
    profile: Option<Instant>,
    pub match_panel: Option<super::eq_match::MatchPanel>,
}

#[derive(Clone)]
enum Drag {
    /// Moving bands, from where each was.
    Bands { start: Pos2, from: Vec<(usize, f64, f64)> },
    /// Selecting with a rubber band.
    Select { start: Pos2 },
    /// A band just made from a spectrum peak, its gain following the pointer.
    Grab { band: usize, start: Pos2, from_db: f64 },
}

impl EqView {
    pub fn new() -> Self {
        Self {
            analyzer: Analyzer::start(),
            curves: Default::default(),
            shown: std::array::from_fn(|_| vec![analyzer::FLOOR; POINTS]),
            sidechain_watch: None,
            other_watch: None,
            selected: BTreeSet::new(),
            drag: None,
            alt_solo: None,
            range_db: 12.0,
            other: None,
            collisions: true,
            frozen: false,
            last: Instant::now(),
            ui_cost: Duration::ZERO,
            profile: std::env::var_os("SCYTHE_EQ_PROFILE").map(|_| Instant::now()),
            match_panel: None,
        }
    }
}

impl EqView {
    /// The input's and the other instance's curves when the analyzer has them, for EQ Match.
    pub(super) fn capture_curves(&self) -> (Option<&[f32]>, Option<&[f32]>) {
        let live = |s: usize| self.curves[s].live.then_some(self.curves[s].db.as_slice());
        (live(0), live(3))
    }
}

impl Default for EqView {
    fn default() -> Self {
        Self::new()
    }
}

/// The values of every parameter of the running EQ, the meters and the latency.
struct State {
    values: Vec<f64>,
    meters: Arc<Meters>,
    latency: usize,
    sample_rate: f64,
}

impl State {
    fn v(&self, id: u32) -> f64 {
        self.values.get(id as usize).copied().unwrap_or(0.0)
    }

    fn g(&self, o: u32) -> f64 {
        self.v(GLOBAL + o)
    }

    fn band(&self, b: usize) -> BandSettings {
        BandSettings::read(&self.values, b)
    }

    fn used(&self) -> impl Iterator<Item = usize> + '_ {
        (0..BANDS).filter(|b| self.v(p::band_param(*b, band::USED)) >= 0.5)
    }

    /// The design a band plays now: its gain moved by the dynamic gain change.
    fn live_design(&self, b: usize) -> Design {
        let s = self.band(b);
        let dynamic = if s.dynamic == 1 { self.meters.dynamic(b) as f64 } else { 0.0 };
        Design { gain_db: s.design.gain_db + dynamic, ..s.design }
    }
}

fn eq_plot(rect: Rect, range: f64) -> Plot {
    Plot::new(rect, Scale::log(analyzer::LO, analyzer::HI), Scale::lin(-range, range))
}

/// The analyzer's own scale: 0 dB at the top down to `range` below, with its tilt.
struct Levels {
    top: f32,
    range: f32,
    tilt: f32,
}

impl Levels {
    fn y(&self, plot: &Plot, hz: f64, db: f32) -> f32 {
        let tilted = db + self.tilt * (hz / 1000.0).log2() as f32;
        let t = ((self.top - tilted) / self.range).clamp(0.0, 1.02);
        plot.rect.top() + t * plot.rect.height()
    }
}

pub fn show(cx: &mut Ctx, ui: &mut Ui, rect: Rect) {
    let started = Instant::now();
    let Some(st) = cx
        .inst
        .with(|e| {
            let latency = e.latency();
            let eq = e.as_any_mut().and_then(|a| a.downcast_mut::<ScytheEq>())?;
            Some(State { values: eq.values().to_vec(), meters: eq.meters(), latency, sample_rate: eq.sample_rate() })
        })
        .flatten()
    else {
        return;
    };
    if cx.ed.eq.is_none() {
        cx.ed.eq = Some(Box::default());
    }
    feed_analyzer(cx, &st);
    let mut view = cx.ed.eq.take().unwrap();
    view.selected.retain(|b| st.v(p::band_param(*b, band::USED)) >= 0.5);
    cx.ed.sel = view.selected.iter().next().copied();

    let bottom_h = 74.0;
    let panel_h = 76.0;
    let (graph_area, rest) = split_top(rect, rect.height() - bottom_h - panel_h - 16.0, 8.0);
    let (panel, bottom) = split_top(rest, panel_h, 8.0);
    let (graph, piano) = split_top(graph_area, graph_area.height() - PIANO_H, 0.0);
    let plot = eq_plot(graph, view.range_db);
    let levels = Levels {
        top: 6.0,
        range: [66.0, 96.0, 126.0][st.g(global::ANALYZER_RANGE) as usize % 3],
        tilt: st.g(global::ANALYZER_TILT) as f32,
    };
    draw_background(ui, &plot, &levels);
    draw_analyzer(ui, &plot, &levels, &mut view, &st);
    draw_bands(ui, &plot, &view, &st);
    let handles = handle_points(&plot, &st);
    for &(b, at) in &handles {
        draw_handle(ui, graph, at, b, &st, view.selected.contains(&b));
    }
    piano_strip(ui, piano, &plot, &view, &st, cx.ed.hovered);
    graph_input(cx, ui, &plot, &levels, &mut view, &st, &handles);
    for &(b, at) in handles.iter().filter(|(_, at)| graph.contains(*at)) {
        let id = Id::unique(("scythe-eq-handle", &cx.ed.owner, &cx.ed.path, b));
        let r = ui.interact(Rect::from_center_size(at, vec2(16.0, 16.0)), id, Sense::hover());
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Band {}", b + 1)));
    }
    band_panel(cx, ui, panel, &mut view, &st);
    bottom_bar(cx, ui, bottom, &mut view, &st);
    if view.match_panel.is_some() {
        super::eq_match::window(cx, ui, &mut view, &st.values);
    }
    view.ui_cost = started.elapsed();
    if let Some(t) = view.profile
        && t.elapsed() > Duration::from_secs(2)
    {
        eprintln!(
            "scythe-eq profile: editor {:.2} ms per frame, analyzer {:.2} ms per update",
            view.ui_cost.as_secs_f64() * 1000.0,
            view.analyzer.cost().as_secs_f64() * 1000.0
        );
        view.profile = Some(Instant::now());
    }
    cx.ed.eq = Some(view);
    ui.ctx().request_repaint_after(Duration::from_millis(16));
}

fn feed_analyzer(cx: &mut Ctx, st: &State) {
    let capture = cx.ed.eq.as_ref().and_then(|v| v.match_panel.as_ref()).and_then(|m| m.capturing());
    if let Some(Some(reference)) = &capture
        && let Some(v) = cx.ed.eq.as_mut()
    {
        v.other = Some(reference.clone());
    }
    let mode = st.g(global::ANALYZER) as usize;
    let pre_on = matches!(mode, 1 | 3 | 5 | 7) || capture.is_some();
    let settings = Settings {
        resolution: st.g(global::ANALYZER_RESOLUTION) as usize,
        speed: st.g(global::ANALYZER_SPEED) as usize,
        peak_hold: st.g(global::ANALYZER_PEAK_HOLD) >= 0.5,
        frozen: cx.ed.eq.as_ref().is_some_and(|v| v.frozen),
    };
    let probe = cx.inst.probe.clone();
    let other = cx.ed.eq.as_ref().and_then(|v| v.other.clone()).and_then(|(owner, path)| {
        if path.is_empty() {
            return super::eq_match::track_probe(cx.app, &owner);
        }
        let slots = super::chain_slots(cx.app, &owner);
        cx.app.engine.fx().instance_at(&owner, &slots, &path).map(|i| i.probe.clone())
    });
    let uses_sidechain = st.used().any(|b| {
        let s = st.band(b);
        s.dynamic > 0 && s.external
    });
    let view = cx.ed.eq.as_mut().unwrap();
    let a = &view.analyzer;
    a.set_settings(settings);
    a.set_source(Source::Pre, pre_on.then(|| (probe.clone(), Side::Pre)));
    a.set_source(Source::Post, matches!(mode, 2 | 3 | 6 | 7).then(|| (probe.clone(), Side::Post)));
    let sc = (uses_sidechain || mode >= 4).then(|| st.meters.sidechain.clone());
    if sc.is_some() && view.sidechain_watch.is_none() {
        view.sidechain_watch = sc.as_ref().map(|p| p.watch());
    }
    if sc.is_none() {
        view.sidechain_watch = None;
    }
    a.set_source(Source::Sidechain, sc.map(|p| (p, Side::Pre)));
    match &other {
        Some(o) if view.other_watch.as_ref().is_none_or(|w| !Arc::ptr_eq(w.probe(), o)) => {
            view.other_watch = Some(o.watch());
        }
        None => view.other_watch = None,
        _ => {}
    }
    a.set_source(Source::Other, other.map(|o| (o, Side::Post)));
    let dt = view.last.elapsed().as_secs_f32().min(0.1);
    view.last = Instant::now();
    let ease = 1.0 - (-dt / 0.03).exp();
    for (s, source) in [Source::Pre, Source::Post, Source::Sidechain, Source::Other].into_iter().enumerate() {
        view.analyzer.read(source, &mut view.curves[s]);
        let peak_hold = settings.peak_hold;
        let target = if peak_hold { &view.curves[s].peak } else { &view.curves[s].db };
        for (d, t) in view.shown[s].iter_mut().zip(target) {
            *d += (t - *d) * ease;
        }
    }
}

fn draw_background(ui: &Ui, plot: &Plot, levels: &Levels) {
    let painter = ui.painter_at(plot.rect.expand(1.0));
    // A faint vertical gradient gives the graph depth without drawing attention.
    let r = plot.rect;
    let mut mesh = Mesh::default();
    let top = Color32::from_rgb(0x1f, 0x20, 0x1c);
    mesh.colored_vertex(r.left_top(), top);
    mesh.colored_vertex(r.right_top(), top);
    mesh.colored_vertex(r.right_bottom(), BG);
    mesh.colored_vertex(r.left_bottom(), BG);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(Shape::mesh(mesh));
    let line = theme::with_alpha(RAISED_HI, 150);
    let faint = theme::with_alpha(RAISED_HI, 70);
    for decade in [10.0, 100.0, 1000.0, 10000.0] {
        for k in 1..10 {
            let hz = decade * k as f64;
            if hz > analyzer::HI {
                break;
            }
            let x = plot.sx(hz);
            let major = k == 1 || k == 2 || k == 5;
            painter.line_segment(
                [pos2(x, r.top()), pos2(x, r.bottom())],
                Stroke::new(1.0, if major { line } else { faint }),
            );
            if major && x > r.left() + 4.0 && x < r.right() - 30.0 {
                painter.text(
                    pos2(x + 3.0, r.bottom() - 3.0),
                    Align2::LEFT_BOTTOM,
                    hz_text(hz),
                    theme::regular(10.0),
                    TEXT_DIM,
                );
            }
        }
    }
    let step = if plot.y.hi > 20.0 {
        6.0
    } else if plot.y.hi > 8.0 {
        3.0
    } else {
        1.0
    };
    let mut db = -plot.y.hi;
    while db <= plot.y.hi + 1e-9 {
        let y = plot.sy(db);
        let zero = db.abs() < 1e-9;
        let stroke = if zero { Stroke::new(1.0, theme::with_alpha(TEXT_DIM, 110)) } else { Stroke::new(1.0, faint) };
        painter.line_segment([pos2(r.left(), y), pos2(r.right(), y)], stroke);
        if y > r.top() + 8.0 && y < r.bottom() - 14.0 {
            let text = if zero { "0".to_owned() } else { format!("{db:+.0}") };
            painter.text(pos2(r.left() + 4.0, y - 1.0), Align2::LEFT_BOTTOM, text, theme::regular(10.0), TEXT_DIM);
        }
        db += step;
    }
    // The analyzer's scale on the right.
    let mut a = levels.top - 6.0;
    while a >= levels.top - levels.range {
        let t = (levels.top - a) / levels.range;
        let y = r.top() + t * r.height();
        if y > r.top() + 8.0 && y < r.bottom() - 14.0 {
            painter.text(
                pos2(r.right() - 4.0, y),
                Align2::RIGHT_CENTER,
                format!("{a:.0}"),
                theme::regular(9.0),
                theme::with_alpha(TEXT_DIM, 140),
            );
        }
        a -= if levels.range > 100.0 { 24.0 } else { 12.0 };
    }
}

/// The analyzer curve of `db` per grid point as screen points across the plot.
fn curve_points(plot: &Plot, levels: &Levels, db: &[f32]) -> Vec<Pos2> {
    let (lo, hi) = (grid_pos(plot.x.lo).max(0.0) as usize, (grid_pos(plot.x.hi).ceil() as usize).min(POINTS - 1));
    (lo..=hi)
        .map(|i| {
            let hz = grid_hz(i);
            pos2(plot.sx(hz), levels.y(plot, hz, db[i]))
        })
        .collect()
}

/// A filled spectrum: a gradient from `color` at the curve to transparent at the bottom.
fn gradient_area(ui: &Ui, clip: Rect, points: &[Pos2], color: Color32, alpha_top: u8) {
    let mut mesh = Mesh::default();
    let bottom = clip.bottom();
    let span = clip.height().max(1.0);
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a.y >= bottom && b.y >= bottom {
            continue;
        }
        let i = mesh.vertices.len() as u32;
        let shade = |y: f32| {
            let t = 1.0 - ((y - clip.top()) / span).clamp(0.0, 1.0);
            theme::with_alpha(color, (alpha_top as f32 * (0.25 + 0.75 * t)) as u8)
        };
        mesh.colored_vertex(a, shade(a.y));
        mesh.colored_vertex(b, shade(b.y));
        mesh.colored_vertex(pos2(b.x, bottom), theme::with_alpha(color, 0));
        mesh.colored_vertex(pos2(a.x, bottom), theme::with_alpha(color, 0));
        mesh.add_triangle(i, i + 1, i + 2);
        mesh.add_triangle(i, i + 2, i + 3);
    }
    ui.painter_at(clip).add(Shape::mesh(mesh));
}

fn draw_analyzer(ui: &Ui, plot: &Plot, levels: &Levels, view: &mut EqView, st: &State) {
    let clip = plot.rect;
    let painter = ui.painter_at(clip);
    let mode = st.g(global::ANALYZER) as usize;
    if mode == 0 && view.other.is_none() {
        return;
    }
    let live = |s: usize| view.curves[s].live || view.shown[s].iter().any(|d| *d > analyzer::FLOOR + 1.0);
    if view.other.is_some() && live(3) {
        let pts = curve_points(plot, levels, &view.shown[3]);
        gradient_area(ui, clip, &pts, OTHER, 26);
        painter.add(Shape::line(pts, Stroke::new(1.0, theme::with_alpha(OTHER, 150))));
        if view.collisions && live(1) {
            draw_collisions(ui, plot, levels, view);
        }
    }
    if live(2) {
        let pts = curve_points(plot, levels, &view.shown[2]);
        gradient_area(ui, clip, &pts, SIDECHAIN, 30);
        painter.add(Shape::line(pts, Stroke::new(1.0, theme::with_alpha(SIDECHAIN, 160))));
    }
    if live(1) {
        let pts = curve_points(plot, levels, &view.shown[1]);
        gradient_area(ui, clip, &pts, POST, 70);
        painter.add(Shape::line(pts, Stroke::new(1.2, theme::with_alpha(POST, 170))));
    }
    if live(0) {
        let pts = curve_points(plot, levels, &view.shown[0]);
        painter.add(Shape::line(pts, Stroke::new(1.0, theme::with_alpha(TEXT_DIM, 120))));
    }
    if st.g(global::ANALYZER_PEAK_HOLD) >= 0.5 && live(1) {
        let pts = curve_points(plot, levels, &view.curves[1].peak);
        painter.add(Shape::line(pts, Stroke::new(1.0, theme::with_alpha(TEXT, 70))));
    }
}

/// Where this instance's output and the other instance's both stand out over the same
/// frequencies: shaded by how evenly matched they are there.
fn draw_collisions(ui: &Ui, plot: &Plot, levels: &Levels, view: &EqView) {
    let mut mesh = Mesh::default();
    let (ours, theirs) = (&view.shown[1], &view.shown[3]);
    for (lo, hi, strength) in collisions(ours, theirs, levels.top - levels.range * 0.55) {
        let (x0, x1) = (plot.sx(grid_hz(lo)), plot.sx(grid_hz(hi)));
        let top = plot.rect.top();
        let i = mesh.vertices.len() as u32;
        let c = theme::with_alpha(COLLISION, (40.0 + 110.0 * strength) as u8);
        let clear = theme::with_alpha(COLLISION, 0);
        mesh.colored_vertex(pos2(x0, plot.rect.bottom()), c);
        mesh.colored_vertex(pos2(x1, plot.rect.bottom()), c);
        mesh.colored_vertex(pos2(x1, top + plot.rect.height() * 0.35), clear);
        mesh.colored_vertex(pos2(x0, top + plot.rect.height() * 0.35), clear);
        mesh.add_triangle(i, i + 1, i + 2);
        mesh.add_triangle(i, i + 2, i + 3);
    }
    ui.painter_at(plot.rect).add(Shape::mesh(mesh));
}

fn band_curve(d: &Design, xs: &[f64]) -> Vec<f64> {
    let p = Prototype::design(d);
    xs.iter().map(|&f| 20.0 * p.magnitude(f).max(1e-9).log10()).collect()
}

fn draw_bands(ui: &Ui, plot: &Plot, view: &EqView, st: &State) {
    let xs = plot.xs(2.0);
    let clip = plot.rect;
    let painter = ui.painter_at(clip);
    let zero = plot.sy(0.0);
    let scale = st.g(global::OUTPUT);
    let mut total = vec![scale; xs.len()];
    let any_solo = st.used().any(|b| st.band(b).solo);
    for b in st.used() {
        let s = st.band(b);
        let color = band_color(b);
        let static_curve = band_curve(&s.design, &xs);
        let live = st.live_design(b);
        let live_curve = if s.dynamic == 1 { band_curve(&live, &xs) } else { static_curve.clone() };
        if s.enabled {
            for (t, c) in total.iter_mut().zip(&live_curve) {
                *t += c;
            }
        }
        let on = s.enabled && (!any_solo || s.solo);
        let selected = view.selected.contains(&b);
        let pts: Vec<Pos2> =
            xs.iter().zip(&static_curve).map(|(&f, &db)| plot.point(f, db.clamp(-60.0, 60.0))).collect();
        let cut = matches!(s.design.shape, BandShape::LowCut | BandShape::HighCut | BandShape::BandPass);
        let alpha = match (on, selected, cut) {
            (false, _, _) => 8,
            (_, true, _) => 60,
            (_, false, true) => 12,
            _ => 26,
        };
        fill_between(ui, clip, &pts, zero, theme::with_alpha(color, alpha));
        let stroke = Stroke::new(if selected { 1.6 } else { 1.0 }, theme::with_alpha(color, if on { 190 } else { 60 }));
        painter.add(Shape::line(pts.clone(), stroke));
        if s.dynamic > 0 && s.design.shape.has_gain() {
            // The range the band can move through, and where it stands now.
            let reach = Design { gain_db: s.design.gain_db + s.range, ..s.design };
            let reach_pts: Vec<Pos2> =
                xs.iter().zip(band_curve(&reach, &xs)).map(|(&f, db)| plot.point(f, db.clamp(-60.0, 60.0))).collect();
            painter.add(Shape::dashed_line(&reach_pts, Stroke::new(1.0, theme::with_alpha(color, 120)), 4.0, 4.0));
            let live_pts: Vec<Pos2> =
                xs.iter().zip(&live_curve).map(|(&f, &db)| plot.point(f, db.clamp(-60.0, 60.0))).collect();
            band_fill(ui, clip, &pts, &live_pts, theme::with_alpha(color, 110));
            painter.add(Shape::line(live_pts, Stroke::new(1.4, theme::with_alpha(color, 230))));
        }
    }
    let pts: Vec<Pos2> = xs.iter().zip(&total).map(|(&f, &db)| plot.point(f, db.clamp(-60.0, 60.0))).collect();
    painter.add(Shape::line(pts.clone(), Stroke::new(4.0, theme::with_alpha(BG, 160))));
    painter.add(Shape::line(pts, Stroke::new(2.2, ACCENT)));
}

/// The area between a curve and the 0 dB line.
fn fill_between(ui: &Ui, clip: Rect, points: &[Pos2], base: f32, color: Color32) {
    let mut mesh = Mesh::default();
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        if (a.y - base).abs() < 0.3 && (b.y - base).abs() < 0.3 {
            continue;
        }
        let i = mesh.vertices.len() as u32;
        mesh.colored_vertex(a, color);
        mesh.colored_vertex(b, color);
        mesh.colored_vertex(pos2(b.x, base), color);
        mesh.colored_vertex(pos2(a.x, base), color);
        mesh.add_triangle(i, i + 1, i + 2);
        mesh.add_triangle(i, i + 2, i + 3);
    }
    ui.painter_at(clip).add(Shape::mesh(mesh));
}

/// The area between two curves over the same x positions.
fn band_fill(ui: &Ui, clip: Rect, a: &[Pos2], b: &[Pos2], color: Color32) {
    let mut mesh = Mesh::default();
    for (wa, wb) in a.windows(2).zip(b.windows(2)) {
        if (wa[0].y - wb[0].y).abs() < 0.3 && (wa[1].y - wb[1].y).abs() < 0.3 {
            continue;
        }
        let i = mesh.vertices.len() as u32;
        mesh.colored_vertex(wa[0], color);
        mesh.colored_vertex(wa[1], color);
        mesh.colored_vertex(wb[1], color);
        mesh.colored_vertex(wb[0], color);
        mesh.add_triangle(i, i + 1, i + 2);
        mesh.add_triangle(i, i + 2, i + 3);
    }
    ui.painter_at(clip).add(Shape::mesh(mesh));
}

/// The handle position of band `b`: its frequency and gain, or for shapes without a gain the
/// curve's level there.
fn handle_value(st: &State, b: usize) -> (f64, f64) {
    let s = st.band(b);
    let db = if s.design.shape.has_gain() {
        s.design.gain_db
    } else {
        match s.design.shape {
            BandShape::LowCut | BandShape::HighCut | BandShape::BandPass => 0.0,
            _ => (20.0 * Prototype::design(&s.design).magnitude(s.design.freq).max(1e-9).log10()).max(-12.0),
        }
    };
    (s.design.freq, db)
}

fn handle_points(plot: &Plot, st: &State) -> Vec<(usize, Pos2)> {
    st.used()
        .map(|b| {
            let (hz, db) = handle_value(st, b);
            let at = plot.point(hz, db.clamp(-plot.y.hi, plot.y.hi));
            (b, pos2(at.x.clamp(plot.rect.left() + 6.0, plot.rect.right() - 6.0), at.y))
        })
        .collect()
}

fn draw_handle(ui: &Ui, clip: Rect, at: Pos2, b: usize, st: &State, selected: bool) {
    let s = st.band(b);
    let painter = ui.painter_at(clip.expand(12.0));
    let color = if s.enabled { band_color(b) } else { theme::SWITCH_OFF };
    if selected {
        painter.circle_filled(at, 13.0, theme::with_alpha(color, 55));
    }
    if s.dynamic > 0 {
        let live = st.live_design(b).gain_db - s.design.gain_db;
        painter.circle_stroke(at, 10.5, Stroke::new(1.5, theme::with_alpha(color, 200)));
        if live.abs() > 0.05 {
            let dy = -(live as f32).clamp(-30.0, 30.0) * 0.8;
            painter.line_segment([at, at + vec2(0.0, dy)], Stroke::new(2.0, color));
        }
    }
    painter.circle_filled(at, 8.0, color);
    painter.circle_stroke(at, 8.0, Stroke::new(1.5, if selected { TEXT } else { BG }));
    painter.text(at, Align2::CENTER_CENTER, (b + 1).to_string(), theme::semibold(9.0), BG);
    let badge = match s.placement {
        Placement::Stereo => None,
        Placement::Left => Some("L"),
        Placement::Right => Some("R"),
        Placement::Mid => Some("M"),
        Placement::Side => Some("S"),
    };
    if let Some(badge) = badge {
        let c = at + vec2(9.0, -9.0);
        painter.circle_filled(c, 5.5, PANEL);
        painter.text(c, Align2::CENTER_CENTER, badge, theme::semibold(8.0), color);
    }
    if s.solo {
        painter.circle_stroke(at, 13.0, Stroke::new(1.5, theme::YELLOW));
    }
}

fn is_black(note: i64) -> bool {
    matches!(note.rem_euclid(12), 1 | 3 | 6 | 8 | 10)
}

fn piano_strip(ui: &Ui, rect: Rect, plot: &Plot, view: &EqView, st: &State, hovered: bool) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, theme::with_alpha(TEXT, 210));
    let hz_of = |n: f64| 440.0 * 2f64.powf((n - 69.0) / 12.0);
    let hover_note = ui
        .input(|i| i.pointer.hover_pos())
        .filter(|p| hovered && plot.rect.union(rect).contains(*p))
        .map(|p| (69.0 + 12.0 * (plot.values(p).0 / 440.0).log2()).round() as i64);
    let selected_notes: Vec<i64> =
        view.selected.iter().map(|b| (69.0 + 12.0 * (st.band(*b).design.freq / 440.0).log2()).round() as i64).collect();
    for n in 0..=135i64 {
        let (lo, hi) = (hz_of(n as f64 - 0.5), hz_of(n as f64 + 0.5));
        let (x0, x1) = (plot.sx(lo), plot.sx(hi));
        if x1 < rect.left() || x0 > rect.right() {
            continue;
        }
        let r = Rect::from_min_max(pos2(x0, rect.top()), pos2(x1, rect.bottom()));
        let black = is_black(n);
        let mark = if hover_note == Some(n) {
            Some(ACCENT)
        } else if selected_notes.contains(&n) {
            Some(theme::with_alpha(ACCENT, 140))
        } else {
            None
        };
        if black {
            let k = Rect::from_min_max(r.min, pos2(r.max.x, rect.top() + rect.height() * 0.62));
            painter.rect_filled(k, CornerRadius::ZERO, mark.unwrap_or(Color32::from_rgb(0x26, 0x27, 0x22)));
        } else {
            if let Some(m) = mark {
                painter.rect_filled(r, CornerRadius::ZERO, m);
            }
            if n % 12 == 0 || n % 12 == 5 {
                painter.line_segment(
                    [pos2(x0, rect.top()), pos2(x0, rect.bottom())],
                    Stroke::new(1.0, theme::with_alpha(BG, 90)),
                );
            }
            if n % 12 == 0 && x1 - x0 > 2.5 {
                painter.text(
                    pos2(x0 + 1.0, rect.bottom() - 1.0),
                    Align2::LEFT_BOTTOM,
                    format!("C{}", n / 12 - 1),
                    theme::regular(8.0),
                    BG,
                );
            }
        }
    }
}

fn hit(handles: &[(usize, Pos2)], p: Pos2) -> Option<usize> {
    handles
        .iter()
        .map(|(b, a)| (*b, a.distance(p)))
        .filter(|(_, d)| *d <= 12.0)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(b, _)| b)
}

/// The spectrum peak near the pointer that a drag would turn into a band: a local maximum of
/// the post curve standing 6 dB over its neighbourhood, within 24 points sideways.
fn grab_candidate(plot: &Plot, levels: &Levels, view: &EqView, p: Pos2) -> Option<(f64, f32, Pos2)> {
    let curve = &view.shown[1];
    let at = grid_pos(plot.values(p).0).round() as isize;
    let span = 12isize;
    let mut best: Option<(usize, f32)> = None;
    for i in (at - span).max(4)..(at + span).min(POINTS as isize - 5) {
        let i = i as usize;
        let v = curve[i];
        if v <= analyzer::FLOOR + 10.0 || v < curve[i - 1] || v < curve[i + 1] {
            continue;
        }
        let around = (curve[i - 4] + curve[i + 4]) / 2.0;
        let prominence = v - around;
        if prominence >= 6.0 && best.is_none_or(|(_, b)| prominence > b) {
            best = Some((i, prominence));
        }
    }
    let (i, _) = best?;
    let hz = grid_hz(i);
    let point = pos2(plot.sx(hz), levels.y(plot, hz, curve[i]));
    (point.distance(p) < 48.0).then_some((hz, curve[i], point))
}

fn put(cx: &mut Ctx, id: u32, v: f64) {
    cx.edit("Adjust Scythe EQ", |eq: &mut ScytheEq| eq.set(id, v));
}

fn put_once(cx: &mut Ctx, label: &str, id: u32, v: f64) {
    cx.edit_once(label, |eq: &mut ScytheEq| eq.set(id, v));
}

/// Adds a band, as one undo step; returns it.
fn add_band(cx: &mut Ctx, st: &State, shape: BandShape, hz: f64, db: f64, q: f64) -> Option<usize> {
    let free = (0..BANDS).find(|b| st.v(p::band_param(*b, band::USED)) < 0.5)?;
    cx.edit_once("Add EQ band", |eq: &mut ScytheEq| {
        let id = |o: u32| p::band_param(free, o);
        for o in 0..band::COUNT {
            eq.set(id(o), p::default(id(o)));
        }
        eq.set(id(band::SHAPE), shape.index() as f64);
        eq.set(id(band::FREQ), hz);
        eq.set(id(band::GAIN), if shape.has_gain() { db } else { 0.0 });
        eq.set(id(band::Q), q);
        eq.set(id(band::SLOPE), if matches!(shape, BandShape::LowCut | BandShape::HighCut) { 3.0 } else { 1.0 });
        eq.set(id(band::USED), 1.0);
    });
    Some(free)
}

fn remove_bands(cx: &mut Ctx, bands: &[usize]) {
    let bands = bands.to_vec();
    cx.edit_once("Remove EQ band", |eq: &mut ScytheEq| {
        for b in &bands {
            eq.set(p::band_param(*b, band::USED), 0.0);
            eq.set(p::band_param(*b, band::SOLO), 0.0);
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn graph_input(
    cx: &mut Ctx,
    ui: &mut Ui,
    plot: &Plot,
    levels: &Levels,
    view: &mut EqView,
    st: &State,
    handles: &[(usize, Pos2)],
) {
    let area = plot.rect;
    let resp = ui.interact(area, Id::unique(("scythe-eq-graph", &cx.ed.owner, &cx.ed.path)), Sense::click_and_drag());
    let resp = resp.on_hover_cursor(egui::CursorIcon::Crosshair);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, "Scythe EQ graph"));
    let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
    let (shift, alt, command) = ui.input(|i| (i.modifiers.shift, i.modifiers.alt, i.modifiers.command));
    let under = pointer.and_then(|p| hit(handles, p));
    let grab = match (pointer, under, &view.drag) {
        (Some(p), None, None) if st.g(global::ANALYZER) >= 1.0 => grab_candidate(plot, levels, view, p),
        _ => None,
    };

    // Alt held on a band solos it until released.
    if alt
        && resp.is_pointer_button_down_on()
        && view.alt_solo.is_none()
        && let Some(b) = widgets::drag_origin(ui, &resp).or(pointer).and_then(|p| hit(handles, p))
    {
        view.alt_solo = Some(b);
        put_once(cx, "Solo EQ band", p::band_param(b, band::SOLO), 1.0);
    }
    if let Some(b) = view.alt_solo
        && !resp.is_pointer_button_down_on()
    {
        put_once(cx, "Solo EQ band", p::band_param(b, band::SOLO), 0.0);
        view.alt_solo = None;
    }

    if resp.drag_started() && view.alt_solo.is_none() {
        let origin = widgets::drag_origin(ui, &resp).or(pointer).unwrap_or(area.center());
        match hit(handles, origin) {
            Some(b) => {
                if !view.selected.contains(&b) {
                    if !(shift || command) {
                        view.selected.clear();
                    }
                    view.selected.insert(b);
                }
                let from = view.selected.iter().map(|k| {
                    let (hz, db) = handle_value(st, *k);
                    (*k, hz, db)
                });
                view.drag = Some(Drag::Bands { start: origin, from: from.collect() });
            }
            None => match grab_candidate(plot, levels, view, origin) {
                Some((hz, _, _)) => {
                    if let Some(b) = add_band(cx, st, BandShape::Bell, hz, 0.0, 4.0) {
                        view.selected = [b].into();
                        view.drag = Some(Drag::Grab { band: b, start: origin, from_db: 0.0 });
                    }
                }
                None => {
                    if !(shift || command) {
                        view.selected.clear();
                    }
                    view.drag = Some(Drag::Select { start: origin });
                }
            },
        }
    }
    if let (Some(drag), Some(p)) = (view.drag.clone(), pointer)
        && resp.dragged()
    {
        match drag {
            Drag::Bands { start, from } => {
                let fine = if shift { 0.15 } else { 1.0 };
                let (hz0, db0) = plot.values(start);
                let (hz1, db1) = plot.values(start + (p - start) * fine);
                let ratio = hz1 / hz0;
                let ddb = db1 - db0;
                for (b, hz, db) in from {
                    put(cx, p::band_param(b, band::FREQ), (hz * ratio).clamp(10.0, 30000.0));
                    if st.band(b).design.shape.has_gain() {
                        put(cx, p::band_param(b, band::GAIN), (db + ddb).clamp(-30.0, 30.0));
                    }
                }
            }
            Drag::Grab { band: b, start, from_db } => {
                let ddb = plot.values(p).1 - plot.values(start).1;
                put(cx, p::band_param(b, band::GAIN), (from_db + ddb).clamp(-30.0, 30.0));
            }
            Drag::Select { start } => {
                let r = Rect::from_two_pos(start, p);
                ui.painter_at(area).rect_filled(r, CornerRadius::same(3), theme::with_alpha(ACCENT, 24));
                ui.painter_at(area).rect_stroke(
                    r,
                    CornerRadius::same(3),
                    Stroke::new(1.0, theme::with_alpha(ACCENT, 140)),
                    egui::StrokeKind::Inside,
                );
                for (b, at) in handles {
                    if r.contains(*at) {
                        view.selected.insert(*b);
                    }
                }
            }
        }
    }
    if resp.drag_stopped() {
        view.drag = None;
        cx.end();
    }
    if resp.clicked() && view.alt_solo.is_none() && !alt {
        match under {
            Some(b) => {
                if shift || command {
                    if !view.selected.remove(&b) {
                        view.selected.insert(b);
                    }
                } else {
                    view.selected = [b].into();
                }
            }
            None => view.selected.clear(),
        }
    }
    if resp.double_clicked() {
        match under {
            Some(b) => {
                let on = st.band(b).enabled;
                put_once(cx, "Bypass EQ band", p::band_param(b, band::ENABLED), if on { 0.0 } else { 1.0 });
            }
            None => {
                if let Some(pt) = pointer {
                    let (hz, db) = plot.values(pt);
                    let shape = if hz < 30.0 {
                        BandShape::LowCut
                    } else if hz > 16000.0 {
                        BandShape::HighCut
                    } else {
                        BandShape::Bell
                    };
                    if let Some(b) = add_band(cx, st, shape, hz.clamp(10.0, 30000.0), db, 1.0) {
                        view.selected = [b].into();
                    }
                }
            }
        }
    }
    if resp.hovered() {
        let wheel = super::wheel(ui);
        let target: Vec<usize> = match under {
            Some(b) if !view.selected.contains(&b) => vec![b],
            Some(_) => view.selected.iter().copied().collect(),
            None => Vec::new(),
        };
        if wheel != 0.0 && !target.is_empty() {
            let step: f64 = if shift { 1.02 } else { 1.12 };
            cx.app.end_gesture();
            for b in target {
                let q = st.band(b).design.q * step.powf(wheel as f64);
                put(cx, p::band_param(b, band::Q), q.clamp(0.025, 40.0));
            }
            cx.end();
        }
    }
    let menu_band = under.or(view.selected.iter().next().copied());
    let menu_point = pointer;
    resp.context_menu(|ui| match menu_band {
        Some(b) if under.is_some() => band_menu(cx, ui, view, st, b),
        _ => {
            ui.label(egui::RichText::new("Add band").color(TEXT_DIM));
            for shape in BandShape::ALL {
                if ui.button(shape.label()).clicked() {
                    if let Some(pt) = menu_point {
                        let (hz, db) = plot.values(pt);
                        if let Some(b) = add_band(cx, st, shape, hz.clamp(10.0, 30000.0), db, 1.0) {
                            view.selected = [b].into();
                        }
                    }
                    ui.close();
                }
            }
        }
    });

    // Hover readout: frequency, note and the analyzer level there.
    if let Some(pt) = resp.hover_pos().filter(|_| view.drag.is_none()) {
        let painter = ui.painter_at(area);
        painter.line_segment(
            [pos2(pt.x, area.top()), pos2(pt.x, area.bottom())],
            Stroke::new(1.0, theme::with_alpha(TEXT, 40)),
        );
        let hz = plot.values(pt).0;
        let i = grid_pos(hz).round().clamp(0.0, (POINTS - 1) as f64) as usize;
        let level = view.shown[1][i].max(view.shown[0][i]);
        let mut text = format!("{} · {}", hz_text(hz), note_name(hz));
        if level > analyzer::FLOOR + 10.0 {
            text.push_str(&format!(" · {level:.1} dB"));
        }
        if let Some(b) = under {
            let s = st.band(b);
            text = format!(
                "{} {} · {} · {:+.1} dB · Q {:.2}",
                s.design.shape.label(),
                b + 1,
                hz_text(s.design.freq),
                s.design.gain_db,
                s.design.q
            );
            if s.dynamic > 0 {
                text.push_str(&format!(" · dyn {:+.1}", st.meters.dynamic(b)));
            }
        }
        readout(ui, area, pt, &text);
        if let Some((_, _, at)) = grab {
            painter.circle_stroke(at, 7.0, Stroke::new(1.5, ACCENT));
            painter.circle_filled(at, 3.0, ACCENT);
            painter.text(at + vec2(0.0, -12.0), Align2::CENTER_BOTTOM, "drag to grab", theme::regular(10.0), ACCENT);
        }
    }
    if cx.ed.hovered && cx.ed.typing.is_none() && !view.selected.is_empty() {
        arrow_keys(cx, ui, view, st);
    }
}

fn readout(ui: &Ui, area: Rect, at: Pos2, text: &str) {
    let w = widgets::text_width(ui, text, theme::regular(11.0)) + 16.0;
    let x = (at.x + 12.0).min(area.right() - w - 4.0).max(area.left() + 4.0);
    let y = (at.y - 30.0).max(area.top() + 4.0);
    let r = Rect::from_min_size(pos2(x, y), vec2(w, 20.0));
    let painter = ui.painter();
    painter.rect_filled(r, CornerRadius::same(6), theme::with_alpha(PANEL, 235));
    painter.text(r.center(), Align2::CENTER_CENTER, text, theme::regular(11.0), TEXT);
}

/// Arrows move the selected bands a twelfth of an octave or half a dB (Shift: finer);
/// Delete removes them.
fn arrow_keys(cx: &mut Ctx, ui: &Ui, view: &mut EqView, st: &State) {
    let (l, r, u, d, fine, del) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::ArrowLeft),
            i.key_pressed(egui::Key::ArrowRight),
            i.key_pressed(egui::Key::ArrowUp),
            i.key_pressed(egui::Key::ArrowDown),
            i.modifiers.shift,
            i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace),
        )
    });
    if del {
        let bands: Vec<usize> = view.selected.iter().copied().collect();
        remove_bands(cx, &bands);
        view.selected.clear();
        return;
    }
    let side = r as i32 - l as i32;
    let vert = u as i32 - d as i32;
    if side == 0 && vert == 0 {
        return;
    }
    for &b in &view.selected.clone() {
        let s = st.band(b);
        if side != 0 {
            let step = if fine { 1.0 / 100.0 } else { 1.0 / 12.0 };
            put(cx, p::band_param(b, band::FREQ), (s.design.freq * 2f64.powf(side as f64 * step)).clamp(10.0, 30000.0));
        }
        if vert != 0 && s.design.shape.has_gain() {
            let step = if fine { 0.1 } else { 0.5 };
            put(
                cx,
                p::band_param(b, band::GAIN),
                (st.v(p::band_param(b, band::GAIN)) + vert as f64 * step).clamp(-30.0, 30.0),
            );
        }
    }
    cx.ed.key_gesture = true;
}

fn band_menu(cx: &mut Ctx, ui: &mut Ui, view: &mut EqView, st: &State, b: usize) {
    let s = st.band(b);
    ui.label(egui::RichText::new(format!("Band {}", b + 1)).color(TEXT_DIM));
    ui.menu_button("Shape", |ui| {
        for shape in BandShape::ALL {
            if ui.radio(s.design.shape == shape, shape.label()).clicked() {
                put_once(cx, "Change EQ band shape", p::band_param(b, band::SHAPE), shape.index() as f64);
                ui.close();
            }
        }
    });
    if s.design.shape.has_slope() {
        ui.menu_button("Slope", |ui| {
            for (i, _) in SLOPES.iter().enumerate() {
                if ui.radio(s.design.slope == i, slope_label(i)).clicked() {
                    put_once(cx, "Change EQ band slope", p::band_param(b, band::SLOPE), i as f64);
                    ui.close();
                }
            }
        });
    }
    ui.menu_button("Stereo", |ui| {
        for pl in Placement::ALL {
            if ui.radio(s.placement == pl, pl.label()).clicked() {
                put_once(cx, "Change EQ band placement", p::band_param(b, band::STEREO), pl as usize as f64);
                ui.close();
            }
        }
    });
    if s.design.shape.has_gain() {
        ui.menu_button("Dynamics", |ui| {
            for (i, name) in p::DYNAMIC_MODES.iter().enumerate() {
                if ui.radio(s.dynamic as usize == i, *name).clicked() {
                    put_once(cx, "Change EQ band dynamics", p::band_param(b, band::DYNAMIC), i as f64);
                    ui.close();
                }
            }
        });
    }
    if ui.button(if s.enabled { "Bypass" } else { "Enable" }).clicked() {
        put_once(cx, "Bypass EQ band", p::band_param(b, band::ENABLED), if s.enabled { 0.0 } else { 1.0 });
        ui.close();
    }
    if ui.button(if s.solo { "Unsolo" } else { "Solo" }).clicked() {
        put_once(cx, "Solo EQ band", p::band_param(b, band::SOLO), if s.solo { 0.0 } else { 1.0 });
        ui.close();
    }
    if ui.button("Remove").clicked() {
        remove_bands(cx, &[b]);
        view.selected.remove(&b);
        ui.close();
    }
}

/// A pill showing `current` that opens a menu of `options`; returns the one picked.
fn choice(ui: &mut Ui, rect: Rect, id: Id, label: &str, options: &[String], current: usize) -> Option<usize> {
    let text = format!("{label} {}", options.get(current).map_or("", |s| s.as_str()));
    let resp = ui.interact(rect, id, Sense::click());
    let hot = resp.hovered();
    ui.painter().rect_filled(rect, CornerRadius::same(8), if hot { RAISED_HI } else { BG });
    let shown = widgets::truncate(ui, &text, &theme::regular(11.0), rect.width() - 18.0);
    ui.painter().text(rect.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, shown, theme::regular(11.0), TEXT);
    let c = rect.right_center() - vec2(10.0, 0.0);
    ui.painter().add(Shape::convex_polygon(
        vec![c + vec2(-3.5, -2.0), c + vec2(3.5, -2.0), c + vec2(0.0, 2.5)],
        TEXT_DIM,
        Stroke::NONE,
    ));
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, text.clone()));
    let mut picked = None;
    egui::Popup::menu(&resp).show(|ui| {
        for (i, o) in options.iter().enumerate() {
            if ui.radio(i == current, o.as_str()).clicked() {
                picked = Some(i);
                ui.close();
            }
        }
    });
    picked
}

/// A small on/off pill.
fn toggle(ui: &mut Ui, rect: Rect, id: Id, label: &str, on: bool, color: Color32) -> bool {
    let resp = ui.interact(rect, id, Sense::click());
    let fill = if on {
        color
    } else if resp.hovered() {
        RAISED_HI
    } else {
        BG
    };
    ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        theme::regular(11.0),
        if on { BG } else { TEXT_MID },
    );
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, on, label));
    resp.clicked()
}

/// Lays out boxes left to right in a row.
struct Row {
    x: f32,
    top: f32,
    h: f32,
    right: f32,
}

impl Row {
    fn next(&mut self, w: f32) -> Rect {
        let r = Rect::from_min_size(pos2(self.x, self.top), vec2(w.min((self.right - self.x).max(0.0)), self.h));
        self.x += w + 6.0;
        r
    }
}

fn names<T: AsRef<str>>(v: impl IntoIterator<Item = T>) -> Vec<String> {
    v.into_iter().map(|s| s.as_ref().to_owned()).collect()
}

fn band_panel(cx: &mut Ctx, ui: &mut Ui, rect: Rect, view: &mut EqView, st: &State) {
    super::card(ui, rect);
    let Some(b) = view.selected.iter().next().copied() else {
        let hint =
            "Double-click the graph to add a band · drag a spectrum peak to grab it · Alt-click a band to solo it";
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, hint, theme::regular(12.0), TEXT_DIM);
        return;
    };
    let s = st.band(b);
    let (owner, path) = (cx.ed.owner.clone(), cx.ed.path.clone());
    let key = |what: &str| Id::unique(("scythe-eq-panel", &owner, &path, what));
    let color = band_color(b);
    ui.painter().circle_filled(rect.left_center() + vec2(18.0, 0.0), 9.0, color);
    ui.painter().text(
        rect.left_center() + vec2(18.0, 0.0),
        Align2::CENTER_CENTER,
        (b + 1).to_string(),
        theme::semibold(10.0),
        BG,
    );
    let more = view.selected.len() - 1;
    if more > 0 {
        ui.painter().text(
            rect.left_center() + vec2(18.0, 18.0),
            Align2::CENTER_CENTER,
            format!("+{more}"),
            theme::regular(9.0),
            TEXT_DIM,
        );
    }
    let mut row = Row { x: rect.left() + 36.0, top: rect.top() + 8.0, h: 26.0, right: rect.right() - 8.0 };
    let shapes = names(BandShape::ALL.iter().map(|s| s.label()));
    if let Some(i) = choice(ui, row.next(128.0), key("shape"), "", &shapes, s.design.shape.index()) {
        put_once(cx, "Change EQ band shape", p::band_param(b, band::SHAPE), i as f64);
    }
    if s.design.shape.has_slope() {
        let slopes: Vec<String> = (0..SLOPES.len()).map(slope_label).collect();
        if let Some(i) = choice(ui, row.next(112.0), key("slope"), "", &slopes, s.design.slope) {
            put_once(cx, "Change EQ band slope", p::band_param(b, band::SLOPE), i as f64);
        }
    }
    let stereo = names(Placement::ALL.iter().map(|p| p.label()));
    if let Some(i) = choice(ui, row.next(86.0), key("stereo"), "", &stereo, s.placement as usize) {
        put_once(cx, "Change EQ band placement", p::band_param(b, band::STEREO), i as f64);
    }
    value_box(cx, ui, row.next(128.0), p::band_param(b, band::FREQ), "Freq");
    if s.design.shape.has_gain() {
        value_box(cx, ui, row.next(118.0), p::band_param(b, band::GAIN), "Gain");
    }
    value_box(cx, ui, row.next(96.0), p::band_param(b, band::Q), "Q");
    if toggle(ui, row.next(52.0), key("on"), "On", s.enabled, ACCENT) {
        put_once(cx, "Bypass EQ band", p::band_param(b, band::ENABLED), if s.enabled { 0.0 } else { 1.0 });
    }
    if toggle(ui, row.next(52.0), key("solo"), "Solo", s.solo, theme::YELLOW) {
        put_once(cx, "Solo EQ band", p::band_param(b, band::SOLO), if s.solo { 0.0 } else { 1.0 });
    }
    if toggle(ui, row.next(64.0), key("remove"), "Remove", false, ACCENT) {
        remove_bands(cx, &[b]);
        view.selected.remove(&b);
        return;
    }
    let mut row = Row { x: rect.left() + 36.0, top: rect.top() + 42.0, h: 26.0, right: rect.right() - 8.0 };
    if !s.design.shape.has_gain() {
        ui.painter().text(
            pos2(row.x, row.top + 13.0),
            Align2::LEFT_CENTER,
            "Dynamics need a bell, shelf or tilt",
            theme::regular(11.0),
            TEXT_DIM,
        );
        return;
    }
    let modes = names(p::DYNAMIC_MODES);
    if let Some(i) = choice(ui, row.next(116.0), key("dynamic"), "Dyn", &modes, s.dynamic as usize) {
        put_once(cx, "Change EQ band dynamics", p::band_param(b, band::DYNAMIC), i as f64);
    }
    if s.dynamic == 0 {
        return;
    }
    value_box(cx, ui, row.next(118.0), p::band_param(b, band::RANGE), "Range");
    value_box(cx, ui, row.next(132.0), p::band_param(b, band::THRESHOLD), "Thresh");
    if toggle(ui, row.next(48.0), key("auto"), "Auto", s.auto_threshold, ACCENT) {
        put_once(
            cx,
            "Auto threshold",
            p::band_param(b, band::AUTO_THRESHOLD),
            if s.auto_threshold { 0.0 } else { 1.0 },
        );
    }
    value_box(cx, ui, row.next(110.0), p::band_param(b, band::ATTACK), "Att");
    value_box(cx, ui, row.next(110.0), p::band_param(b, band::RELEASE), "Rel");
    let sources = names(["Internal", "External"]);
    if let Some(i) = choice(ui, row.next(108.0), key("sc"), "SC", &sources, s.external as usize) {
        put_once(cx, "Change EQ band sidechain", p::band_param(b, band::SIDECHAIN), i as f64);
    }
    if toggle(ui, row.next(62.0), key("audition"), "Listen", s.audition, SIDECHAIN) {
        put_once(cx, "Audition EQ sidechain", p::band_param(b, band::AUDITION), if s.audition { 0.0 } else { 1.0 });
    }
    // The band's live gain change.
    let live = st.meters.dynamic(b);
    let r = row.next(70.0);
    ui.painter().text(r.left_center(), Align2::LEFT_CENTER, format!("{live:+.1} dB"), theme::mono(11.0), color);
}

fn bottom_bar(cx: &mut Ctx, ui: &mut Ui, rect: Rect, view: &mut EqView, st: &State) {
    let (owner, path) = (cx.ed.owner.clone(), cx.ed.path.clone());
    let key = |what: &str| Id::unique(("scythe-eq-bar", &owner, &path, what));
    let mut row = Row { x: rect.left(), top: rect.top(), h: 26.0, right: rect.right() };
    let phases = names(["Zero latency", "Natural phase", "Linear phase"]);
    let phase = st.g(global::PHASE) as usize;
    if let Some(i) = choice(ui, row.next(150.0), key("phase"), "", &phases, phase) {
        put_once(cx, "Change EQ phase mode", GLOBAL + global::PHASE, i as f64);
    }
    if phase == 2 {
        let res = names(RESOLUTIONS);
        if let Some(i) = choice(ui, row.next(118.0), key("res"), "Res", &res, st.g(global::RESOLUTION) as usize) {
            put_once(cx, "Change linear phase resolution", GLOBAL + global::RESOLUTION, i as f64);
        }
    }
    let ms = st.latency as f64 * 1000.0 / st.sample_rate.max(1.0);
    let r = row.next(96.0);
    ui.painter().text(
        r.left_center(),
        Align2::LEFT_CENTER,
        format!("{} smp · {ms:.1} ms", st.latency),
        theme::regular(10.0),
        TEXT_DIM,
    );
    let chars = names(CHARACTERS);
    if let Some(i) = choice(ui, row.next(104.0), key("character"), "", &chars, st.g(global::CHARACTER) as usize) {
        put_once(cx, "Change EQ character", GLOBAL + global::CHARACTER, i as f64);
    }
    let gq = st.g(global::GAIN_Q) >= 0.5;
    if toggle(ui, row.next(64.0), key("gainq"), "Gain-Q", gq, ACCENT) {
        put_once(cx, "Gain-Q interaction", GLOBAL + global::GAIN_Q, if gq { 0.0 } else { 1.0 });
    }
    let ranges = names(["±3 dB", "±6 dB", "±12 dB", "±30 dB"]);
    let at = [3.0, 6.0, 12.0, 30.0].iter().position(|r| *r == view.range_db).unwrap_or(2);
    if let Some(i) = choice(ui, row.next(86.0), key("scale"), "", &ranges, at) {
        view.range_db = [3.0, 6.0, 12.0, 30.0][i];
    }
    let auto = st.g(global::AUTO_GAIN) >= 0.5;
    if toggle(ui, row.next(54.0), key("autogain"), "Auto", auto, ACCENT) {
        put_once(cx, "Auto gain", GLOBAL + global::AUTO_GAIN, if auto { 0.0 } else { 1.0 });
    }
    value_box(cx, ui, row.next(118.0), GLOBAL + global::GAIN_SCALE, "Scale");
    value_box(cx, ui, row.next(122.0), GLOBAL + global::OUTPUT, "Out");
    value_box(cx, ui, row.next(98.0), GLOBAL + global::PAN, "Pan");
    value_box(cx, ui, row.next(98.0), GLOBAL + global::WIDTH, "Width");

    let mut row = Row { x: rect.left(), top: rect.top() + 34.0, h: 26.0, right: rect.right() };
    let sources = names(["Off", "Pre", "Post", "Pre+Post", "SC", "Pre+SC", "Post+SC", "All"]);
    if let Some(i) = choice(ui, row.next(126.0), key("analyzer"), "Analyzer", &sources, st.g(global::ANALYZER) as usize)
    {
        put_once(cx, "Analyzer source", GLOBAL + global::ANALYZER, i as f64);
    }
    let res = names(["Low", "Medium", "High", "Maximum"]);
    if let Some(i) = choice(ui, row.next(118.0), key("ares"), "Res", &res, st.g(global::ANALYZER_RESOLUTION) as usize) {
        put_once(cx, "Analyzer resolution", GLOBAL + global::ANALYZER_RESOLUTION, i as f64);
    }
    let speeds = names(["Very slow", "Slow", "Medium", "Fast", "Very fast"]);
    if let Some(i) = choice(ui, row.next(120.0), key("speed"), "Speed", &speeds, st.g(global::ANALYZER_SPEED) as usize)
    {
        put_once(cx, "Analyzer speed", GLOBAL + global::ANALYZER_SPEED, i as f64);
    }
    let tilts = names(["0 dB/oct", "1.5 dB/oct", "3 dB/oct", "4.5 dB/oct", "6 dB/oct"]);
    let tilt_at = (st.g(global::ANALYZER_TILT) / 1.5).round() as usize;
    if let Some(i) = choice(ui, row.next(118.0), key("tilt"), "Tilt", &tilts, tilt_at.min(4)) {
        put_once(cx, "Analyzer tilt", GLOBAL + global::ANALYZER_TILT, i as f64 * 1.5);
    }
    let ranges = names(["60 dB", "90 dB", "120 dB"]);
    if let Some(i) = choice(ui, row.next(96.0), key("arange"), "", &ranges, st.g(global::ANALYZER_RANGE) as usize) {
        put_once(cx, "Analyzer range", GLOBAL + global::ANALYZER_RANGE, i as f64);
    }
    let hold = st.g(global::ANALYZER_PEAK_HOLD) >= 0.5;
    if toggle(ui, row.next(48.0), key("hold"), "Hold", hold, ACCENT) {
        put_once(cx, "Analyzer peak hold", GLOBAL + global::ANALYZER_PEAK_HOLD, if hold { 0.0 } else { 1.0 });
    }
    if toggle(ui, row.next(58.0), key("freeze"), "Freeze", view.frozen, ACCENT) {
        view.frozen = !view.frozen;
    }
    let others = super::eq_match::compare_sources(cx);
    let mut labels = vec!["None".to_owned()];
    labels.extend(others.iter().map(|o| o.label.clone()));
    let current = view
        .other
        .as_ref()
        .and_then(|o| others.iter().position(|x| (x.owner.clone(), x.path.clone()) == *o))
        .map_or(0, |i| i + 1);
    if let Some(i) = choice(ui, row.next(150.0), key("compare"), "Compare", &labels, current) {
        view.other = i.checked_sub(1).and_then(|i| others.get(i)).map(|o| (o.owner.clone(), o.path.clone()));
    }
    if view.other.is_some() && toggle(ui, row.next(76.0), key("collisions"), "Collisions", view.collisions, COLLISION) {
        view.collisions = !view.collisions;
    }
    if toggle(ui, row.next(60.0), key("match"), "Match", view.match_panel.is_some(), ACCENT) {
        view.match_panel = if view.match_panel.is_some() { None } else { Some(Default::default()) };
    }
    if toggle(ui, row.next(76.0), key("instances"), "Instances", cx.app.view.eq_instances, ACCENT) {
        cx.app.view.eq_instances = !cx.app.view.eq_instances;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collisions_need_both_signals_close_and_loud() {
        let mut ours = vec![-120.0f32; 40];
        let mut theirs = vec![-120.0f32; 40];
        for i in 10..20 {
            ours[i] = -20.0;
            theirs[i] = -22.0;
        }
        for i in 25..35 {
            ours[i] = -20.0;
            theirs[i] = -40.0;
        }
        let c = collisions(&ours, &theirs, -60.0);
        assert_eq!(c.len(), 1);
        assert_eq!((c[0].0, c[0].1), (10, 19));
        assert!(c[0].2 > 0.5);
    }

    #[test]
    fn piano_keys_follow_the_octave_pattern() {
        let black: Vec<i64> = (60..72).filter(|n| is_black(*n)).collect();
        assert_eq!(black, vec![61, 63, 66, 68, 70]);
    }
}
