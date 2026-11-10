//! Graphical editors for REAPER's stock effects (ReaEQ, ReaComp, ReaGate …) in windows of
//! their own, opened from the Edit button of an FX slot. Each draws from the emulation's own
//! math and reads what the effect measures through its probe while the window is open; the
//! parameter rows stay one tab away. Every change goes through `Session::fx_gesture`, so a
//! drag is one undo step.

pub mod analyzer;
mod delay;
mod dynamics;
mod eq;
pub mod eq_match;
mod fir;
mod misc;
pub mod model;
pub mod scythe_eq;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use egui::{Align2, Color32, CornerRadius, Id, Mesh, Pos2, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};
use scythe_core::{FxSlot, TrackId};
use scythe_engine::clap_host::ParamInfo;
use scythe_engine::fx::{FxInstance, FxOwner, FxStatus};
use scythe_engine::probe::{self, Side, Spectrum, Watch};

use crate::app::ScytheApp;
use crate::theme::{self, ACCENT, BG, RAISED, RAISED_HI, TEXT, TEXT_DIM, TEXT_MID};
use crate::ui::widgets;
use crate::ui::window::{WindowKind, scythe_window};
use model::{Ballistic, History, Plot, amp_db};

/// Which editor a stock effect gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Eq,
    Comp,
    Xcomp,
    Gate,
    Limit,
    Delay,
    Fir,
    Verbate,
    Verb,
    Synth,
    Insert,
    ControlMidi,
    ScytheEq,
}

impl Kind {
    fn of_stock(name: &str) -> Option<Self> {
        Some(match name {
            "ReaEQ" => Self::Eq,
            "ReaComp" => Self::Comp,
            "ReaXcomp" => Self::Xcomp,
            "ReaGate" => Self::Gate,
            "ReaLimit" => Self::Limit,
            "ReaDelay" => Self::Delay,
            "ReaFir (FFT EQ+Dynamics Processor)" => Self::Fir,
            "ReaVerbate" => Self::Verbate,
            "ReaVerb" => Self::Verb,
            "ReaSynth" => Self::Synth,
            "ReaInsert" => Self::Insert,
            "ReaControlMIDI" => Self::ControlMidi,
            _ => return None,
        })
    }

    fn title(self) -> &'static str {
        match self {
            Self::Eq => "ReaEQ",
            Self::Comp => "ReaComp",
            Self::Xcomp => "ReaXcomp",
            Self::Gate => "ReaGate",
            Self::Limit => "ReaLimit",
            Self::Delay => "ReaDelay",
            Self::Fir => "ReaFIR",
            Self::Verbate => "ReaVerbate",
            Self::Verb => "ReaVerb",
            Self::Synth => "ReaSynth",
            Self::Insert => "ReaInsert",
            Self::ControlMidi => "ReaControlMIDI",
            Self::ScytheEq => "Scythe EQ",
        }
    }

    fn default_size(self) -> egui::Vec2 {
        match self {
            Self::Eq | Self::Fir | Self::Xcomp => vec2(820.0, 520.0),
            Self::Comp | Self::Gate | Self::Delay => vec2(760.0, 480.0),
            Self::Limit | Self::Verbate | Self::Verb | Self::Synth => vec2(680.0, 440.0),
            Self::Insert | Self::ControlMidi => vec2(560.0, 400.0),
            Self::ScytheEq => vec2(1180.0, 680.0),
        }
    }
}

/// The stock plugin a slot holds, by the plugin name its VST chunk stores (the slot's own name
/// may have been changed).
pub fn stock_name(slot: &FxSlot) -> Option<&'static str> {
    let display = slot.raw.iter().find_map(|n| match n {
        scythe_core::rpp::Node::Chunk(c) if c.name() == "VST" => c.args().first().cloned(),
        _ => None,
    })?;
    scythe_engine::stock::find(&display).map(|p| p.name)
}

/// The effects of `owner`'s chain: a track's own, the master's, a track's record input chain,
/// a take's or the monitoring chain.
pub(crate) fn chain_slots(app: &ScytheApp, owner: &FxOwner) -> Vec<FxSlot> {
    match owner {
        FxOwner::Track(t) => app.project.track(*t).map(|t| t.fx.clone()).unwrap_or_default(),
        FxOwner::Input(t) => app.project.track(*t).map(|t| t.input_fx()).unwrap_or_default(),
        FxOwner::Master => app.project.master.fx.clone(),
        FxOwner::Monitor => app.monitor_fx(),
        FxOwner::Take(item, k) => {
            app.project.find_item(*item).and_then(|(_, it)| it.takes.get(*k)).map(|t| t.fx()).unwrap_or_default()
        }
    }
}

/// The slot at `path`: an index, then indices into containers.
pub(crate) fn slot_at(slots: &[FxSlot], path: &[usize]) -> Option<FxSlot> {
    let (first, rest) = path.split_first()?;
    let slot = slots.get(*first)?.clone();
    if rest.is_empty() { Some(slot) } else { slot_at(&slot.container_children(), rest) }
}

fn slot_at_mut(slots: &mut [FxSlot], path: &[usize], f: &dyn Fn(&mut FxSlot)) -> bool {
    let Some((first, rest)) = path.split_first() else { return false };
    let Some(slot) = slots.get_mut(*first) else { return false };
    if rest.is_empty() {
        f(slot);
        return true;
    }
    let mut children = slot.container_children();
    let done = slot_at_mut(&mut children, rest, f);
    slot.set_container_children(&children);
    done
}

/// Changes the slot at `path` of `owner`'s chain as one undo step.
pub(crate) fn edit_slot(app: &mut ScytheApp, owner: &FxOwner, path: &[usize], label: &str, f: &dyn Fn(&mut FxSlot)) {
    let mut chain = chain_slots(app, owner);
    if !slot_at_mut(&mut chain, path, f) {
        return;
    }
    match owner.clone() {
        FxOwner::Track(t) => app.edit(label, |p| {
            if let Some(t) = p.track_mut(t) {
                t.fx = chain;
            }
        }),
        FxOwner::Master => app.edit(label, |p| p.master.fx = chain),
        FxOwner::Input(t) => app.edit(label, |p| {
            if let Some(t) = p.track_mut(t) {
                t.set_input_fx(&chain);
            }
        }),
        FxOwner::Take(item, k) => app.edit(label, |p| {
            if let Some(tk) = p.tracks.iter_mut().find_map(|t| t.item_mut(item)).and_then(|i| i.takes.get_mut(k)) {
                tk.set_fx(&chain);
            }
        }),
        FxOwner::Monitor => app.set_monitor_fx(chain),
    }
}

/// The editor kind of effect `index` on `track` (`None` for the master), when it is a stock
/// effect running as Scythe's emulation.
pub fn kind(app: &ScytheApp, track: Option<TrackId>, slot: &FxSlot, index: usize) -> Option<Kind> {
    kind_at(app, &FxOwner::of(track), slot, &[index])
}

/// The editor kind of the effect at `path` in `owner`'s chain.
pub fn kind_at(app: &ScytheApp, owner: &FxOwner, slot: &FxSlot, path: &[usize]) -> Option<Kind> {
    let kind =
        if scythe_engine::scythe_eq::is_slot(slot) { Kind::ScytheEq } else { Kind::of_stock(stock_name(slot)?)? };
    let status = app.engine.fx().status_at(owner, &chain_slots(app, owner), path).cloned();
    matches!(status, Some(FxStatus::Native(_))).then_some(kind)
}

pub fn is_open(app: &ScytheApp, track: Option<TrackId>, index: usize) -> bool {
    is_open_at(app, &FxOwner::of(track), &[index])
}

pub fn is_open_at(app: &ScytheApp, owner: &FxOwner, path: &[usize]) -> bool {
    app.view.fx_editors.iter().any(|e| e.owner == *owner && e.path == path)
}

/// Opens the editor of effect `index` on `track`, or closes it when open.
pub fn toggle(app: &mut ScytheApp, track: Option<TrackId>, index: usize) {
    toggle_at(app, &FxOwner::of(track), &[index]);
}

/// Opens the editor of the effect at `path` in `owner`'s chain (an input, take or monitoring
/// chain, or inside a container), or closes it when open.
pub fn toggle_at(app: &mut ScytheApp, owner: &FxOwner, path: &[usize]) {
    if is_open_at(app, owner, path) {
        app.view.fx_editors.retain(|e| !(e.owner == *owner && e.path == path));
        return;
    }
    let Some(slot) = slot_at(&chain_slots(app, owner), path) else { return };
    if let Some(kind) = kind_at(app, owner, &slot, path) {
        app.view.fx_editors.push(FxEditor::new(owner.clone(), path.to_vec(), &slot, kind));
    }
}

/// Whether an editor takes the arrow keys: the pointer is over it and something is selected.
pub fn takes_keys(app: &ScytheApp) -> bool {
    app.view.fx_editors.iter().any(|e| e.hovered && (e.sel.is_some() || e.focus.is_some()) || e.typing.is_some())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Editor,
    Parameters,
}

/// What a typed value goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Typing {
    Param(u32),
    Field(u64),
}

/// One open editor window.
pub struct FxEditor {
    /// The chain the effect is in and where: an index, then indices into containers.
    pub owner: FxOwner,
    pub path: Vec<usize>,
    /// The slot's raw name when opened: the window closes when another effect takes the slot.
    name: String,
    kind: Kind,
    tab: Tab,
    watch: Option<Watch>,
    pre: Spectrum,
    post: Spectrum,
    input: Ballistic,
    output: Ballistic,
    reduction: Ballistic,
    detector: Ballistic,
    /// Gain reduction (dB) per frame, or a gate's detector level.
    history: History,
    /// A gate's opening per frame.
    gate: History,
    bands: Vec<Ballistic>,
    /// The selected band, tap or point.
    sel: Option<usize>,
    /// The parameter arrow keys nudge.
    focus: Option<u32>,
    typing: Option<(Typing, String, bool)>,
    /// Arrow keys held change the effect as one gesture.
    key_gesture: bool,
    last: Instant,
    hovered: bool,
    /// A handle being dragged (editors' own drags).
    dragging: Option<usize>,
    /// The ReaXcomp band shown.
    band: usize,
    /// Scythe EQ's own view state.
    pub(crate) eq: Option<Box<scythe_eq::EqView>>,
}

impl std::fmt::Debug for FxEditor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FxEditor").field("owner", &self.owner).field("path", &self.path).finish()
    }
}

impl FxEditor {
    /// The track and slot when the effect is a track's own top-level one, which modulation and
    /// learn address.
    pub fn track_fx(&self) -> Option<(TrackId, usize)> {
        match (&self.owner, self.path.as_slice()) {
            (FxOwner::Track(t), [i]) => Some((*t, *i)),
            _ => None,
        }
    }

    fn new(owner: FxOwner, path: Vec<usize>, slot: &FxSlot, kind: Kind) -> Self {
        let mut post = Spectrum::new(4096);
        post.fall = 36.0;
        let mut pre = Spectrum::new(4096);
        pre.fall = 36.0;
        Self {
            owner,
            path,
            name: slot.name.clone(),
            kind,
            tab: Tab::Editor,
            watch: None,
            pre,
            post,
            input: Ballistic::default(),
            output: Ballistic::default(),
            reduction: Ballistic::zero(),
            detector: Ballistic::default(),
            history: History::new(360),
            gate: History::new(360),
            bands: Vec::new(),
            sel: None,
            focus: None,
            typing: None,
            key_gesture: false,
            last: Instant::now(),
            hovered: false,
            dragging: None,
            band: 0,
            eq: None,
        }
    }

    /// Takes what the probe gathered since the last frame.
    fn measure(&mut self, inst: &FxInstance, dt: f32) {
        if self.watch.as_ref().is_none_or(|w| !Arc::ptr_eq(w.probe(), &inst.probe)) {
            self.watch = Some(inst.probe.watch());
        }
        let p = &inst.probe;
        let fall = 24.0;
        self.input.update(amp_db(p.take(probe::INPUT)), dt, fall);
        self.output.update(amp_db(p.take(probe::OUTPUT)), dt, fall);
        let gr = p.take(probe::REDUCTION);
        self.reduction.db = if gr > self.reduction.db { gr } else { (self.reduction.db - 30.0 * dt).max(gr) };
        self.detector.update(amp_db(p.take(probe::DETECTOR)), dt, fall);
        let gate = p.take(probe::GATE);
        match self.kind {
            Kind::Gate => {
                self.history.push(self.detector.db);
                self.gate.push(gate);
            }
            Kind::Xcomp => {
                let n = probe::BANDS;
                self.bands.resize(n, Ballistic::zero());
                for (k, b) in self.bands.iter_mut().enumerate() {
                    let v = p.take(probe::BAND_REDUCTION + k);
                    b.db = if v > b.db { v } else { (b.db - 30.0 * dt).max(v) };
                }
            }
            Kind::Eq | Kind::Fir => {}
            _ => self.history.push(gr),
        }
        if matches!(self.kind, Kind::Eq | Kind::Fir | Kind::Xcomp) {
            self.pre.update(p, Side::Pre, dt);
            self.post.update(p, Side::Post, dt);
        }
    }
}

/// What an editor draws with this frame: the app, its state and the running effect.
pub(crate) struct Ctx<'a> {
    pub app: &'a mut ScytheApp,
    pub ed: &'a mut FxEditor,
    pub inst: Arc<FxInstance>,
    pub slot: FxSlot,
    pub params: Vec<ParamInfo>,
    values: HashMap<u32, (f64, String)>,
}

impl Ctx<'_> {
    pub fn info(&self, id: u32) -> Option<&ParamInfo> {
        self.params.iter().find(|p| p.id == id)
    }

    pub fn value(&self, id: u32) -> f64 {
        self.values.get(&id).map(|v| v.0).or_else(|| self.info(id).map(|p| p.default)).unwrap_or(0.0)
    }

    pub fn text(&self, id: u32) -> String {
        self.values.get(&id).map(|v| v.1.clone()).unwrap_or_default()
    }

    fn reload(&mut self) {
        let (params, values) = read_params(&self.inst);
        self.params = params;
        self.values = values;
    }

    /// Sets a parameter as part of the current gesture (a drag), ended by [`Ctx::end`].
    pub fn set(&mut self, id: u32, v: f64) {
        let label = self.info(id).map_or_else(|| "Adjust effect".to_owned(), |p| format!("Adjust {}", p.name));
        let shown = self.app.fx_gesture(&label, &self.inst, |e| {
            e.set_param(id, v);
            e.param(id)
        });
        if let Some(Some(shown)) = shown {
            self.values.insert(id, shown);
        }
    }

    /// Sets a parameter as an undo step of its own.
    pub fn set_once(&mut self, id: u32, v: f64) {
        self.app.end_gesture();
        self.set(id, v);
        self.app.end_gesture();
    }

    pub fn end(&mut self) {
        self.app.end_gesture();
    }

    /// Reads the effect as its stock type.
    pub fn read<T: 'static, R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        self.inst.with(|e| e.as_any_mut().and_then(|a| a.downcast_mut::<T>()).map(f)).flatten()
    }

    /// Changes the effect as its stock type within the current gesture.
    pub fn edit<T: 'static, R>(&mut self, label: &str, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        let out = self.app.fx_gesture(label, &self.inst, |e| e.as_any_mut().and_then(|a| a.downcast_mut::<T>()).map(f));
        self.reload();
        out.flatten()
    }

    /// Changes the effect as its stock type, as an undo step of its own.
    pub fn edit_once<T: 'static, R>(&mut self, label: &str, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        self.app.end_gesture();
        let out = self.edit(label, f);
        self.app.end_gesture();
        out
    }

    /// Opens the modulation and learn window on a parameter.
    pub fn modulate(&mut self, id: u32) {
        let Some((track, index)) = self.ed.track_fx() else { return };
        let Some(position) = self.params.iter().position(|p| p.id == id) else { return };
        let param = crate::ui::modulation::reaper_index(&self.slot, id, position);
        self.app.view.modulation = Some(crate::ui::modulation::ModWindow::new(track, index, param));
    }

    fn controlled(&self, id: u32) -> bool {
        let Some(position) = self.params.iter().position(|p| p.id == id) else { return false };
        crate::ui::modulation::has_controls(&self.slot, crate::ui::modulation::reaper_index(&self.slot, id, position))
    }
}

/// Whose effect a window shows, for its title.
fn owner_name(app: &ScytheApp, owner: &FxOwner, nested: bool) -> String {
    let track = |t: &TrackId| app.project.track(*t).map_or_else(|| "Track".to_owned(), |t| t.name.clone());
    let name = match owner {
        FxOwner::Track(t) => track(t),
        FxOwner::Master => "Master".to_owned(),
        FxOwner::Input(t) => format!("{} input FX", track(t)),
        FxOwner::Monitor => "Monitoring FX".to_owned(),
        FxOwner::Take(item, k) => {
            let take = app.project.find_item(*item).and_then(|(_, i)| i.takes.get(*k)).map(|t| t.name.clone());
            format!("take FX of {}", take.filter(|n| !n.is_empty()).unwrap_or_else(|| "item".to_owned()))
        }
    };
    if nested { format!("{name} · in container") } else { name }
}

fn read_params(inst: &FxInstance) -> (Vec<ParamInfo>, HashMap<u32, (f64, String)>) {
    inst.with(|e| {
        let params = e.params();
        let values = params.iter().filter_map(|p| Some((p.id, e.param(p.id)?))).collect();
        (params, values)
    })
    .unwrap_or_default()
}

pub fn show(app: &mut ScytheApp, ui: &mut Ui) {
    if app.view.fx_editors.is_empty() {
        return;
    }
    let mut editors = std::mem::take(&mut app.view.fx_editors);
    editors.retain_mut(|ed| window(app, ui, ed));
    editors.append(&mut app.view.fx_editors);
    app.view.fx_editors = editors;
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
}

fn window(app: &mut ScytheApp, ui: &mut Ui, ed: &mut FxEditor) -> bool {
    let slots = chain_slots(app, &ed.owner);
    let Some(slot) = slot_at(&slots, &ed.path).filter(|s| s.name == ed.name) else { return false };
    let Some(inst) = app.engine.fx().instance_at(&ed.owner, &slots, &ed.path) else { return false };
    let now = Instant::now();
    let dt = now.duration_since(ed.last).as_secs_f32().clamp(0.0, 0.25);
    ed.last = now;
    ed.measure(&inst, dt);
    let owner = owner_name(app, &ed.owner, ed.path.len() > 1);
    let (params, values) = read_params(&inst);
    let mut open = true;
    let ctx = ui.ctx().clone();
    let shown = scythe_window(
        format!("fx-editor-{}", ed.kind.title()),
        format!("{} · {}", ed.kind.title(), owner),
        WindowKind::Panel,
    )
    .id(Id::unique(("fx-editor", &ed.owner, &ed.path)))
    .default_size(ed.kind.default_size())
    .min_size(vec2(380.0, 280.0))
    .escape_closes(ed.typing.is_none())
    .show(&ctx, &mut open, |ui| {
        let mut cx = Ctx { app, ed, inst, slot, params, values };
        body(&mut cx, ui);
    });
    if let Some(s) = shown {
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        ed.hovered = pointer.is_some_and(|p| s.response.rect.contains(p));
    }
    if !open && app.session.in_gesture() {
        app.end_gesture();
    }
    open
}

fn body(cx: &mut Ctx, ui: &mut Ui) {
    let avail = ui.available_rect_before_wrap();
    let rect = Rect::from_min_size(avail.min, vec2(avail.width(), avail.height().max(200.0)));
    ui.allocate_rect(rect, Sense::hover());
    let bar = Rect::from_min_size(rect.min, vec2(rect.width(), 28.0));
    let tabs = [(Tab::Editor, "Editor"), (Tab::Parameters, "Parameters")];
    let mut x = bar.left();
    for (tab, label) in tabs {
        let w = widgets::text_width(ui, label, theme::semibold(12.0)) + 24.0;
        let r = Rect::from_min_size(pos2(x, bar.top()), vec2(w, 26.0));
        let on = cx.ed.tab == tab;
        if widgets::text_pill(
            ui,
            r,
            label,
            theme::semibold(12.0),
            if on { ACCENT } else { RAISED },
            if on { BG } else { TEXT_MID },
        )
        .clicked()
        {
            cx.ed.tab = tab;
            cx.ed.focus = None;
            cx.ed.typing = None;
        }
        x += w + 6.0;
    }
    let enabled = cx.slot.enabled;
    let sw = Rect::from_min_size(pos2(bar.right() - 40.0, bar.top() + 2.0), vec2(36.0, 22.0));
    if widgets::switch(ui, sw, enabled, ACCENT).on_hover_text("Bypass").clicked() {
        let (owner, path) = (cx.ed.owner.clone(), cx.ed.path.clone());
        edit_slot(cx.app, &owner, &path, "Toggle FX bypass", &|s| s.enabled = !s.enabled);
    }
    let label = if enabled { "on" } else { "bypassed" };
    painter_text(ui, pos2(sw.left() - 8.0, sw.center().y), Align2::RIGHT_CENTER, label, theme::regular(11.0), TEXT_DIM);
    let content = Rect::from_min_max(pos2(rect.left(), bar.bottom() + 8.0), rect.max);
    match cx.ed.tab {
        Tab::Parameters => parameters(cx, ui, content),
        Tab::Editor => match cx.ed.kind {
            Kind::Eq => eq::show(cx, ui, content),
            Kind::Comp => dynamics::comp(cx, ui, content),
            Kind::Xcomp => dynamics::xcomp(cx, ui, content),
            Kind::Gate => dynamics::gate(cx, ui, content),
            Kind::Limit => dynamics::limit(cx, ui, content),
            Kind::Delay => delay::show(cx, ui, content),
            Kind::Fir => fir::show(cx, ui, content),
            Kind::Verbate => misc::verbate(cx, ui, content),
            Kind::Verb => misc::verb(cx, ui, content),
            Kind::Synth => misc::synth(cx, ui, content),
            Kind::Insert => misc::insert(cx, ui, content),
            Kind::ControlMidi => misc::control_midi(cx, ui, content),
            Kind::ScytheEq => scythe_eq::show(cx, ui, content),
        },
    }
    keys(cx, ui);
}

/// Arrow keys nudge the focused control; held keys make one gesture.
fn keys(cx: &mut Ctx, ui: &Ui) {
    if cx.ed.typing.is_some() || !cx.ed.hovered {
        return;
    }
    let (up, down, held, fine) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::ArrowUp) || i.key_pressed(egui::Key::ArrowRight),
            i.key_pressed(egui::Key::ArrowDown) || i.key_pressed(egui::Key::ArrowLeft),
            [egui::Key::ArrowUp, egui::Key::ArrowDown, egui::Key::ArrowLeft, egui::Key::ArrowRight]
                .iter()
                .any(|k| i.key_down(*k)),
            i.modifiers.shift,
        )
    });
    if let Some(id) = cx.ed.focus
        && (up || down)
        && let Some(info) = cx.info(id).cloned()
    {
        let v = model::nudge(&info, cx.value(id), if up { 1.0 } else { -1.0 }, fine, model::is_switch(&cx.text(id)));
        cx.set(id, v);
        cx.ed.key_gesture = true;
    }
    if cx.ed.key_gesture && !held {
        cx.ed.key_gesture = false;
        cx.end();
    }
}

fn painter_text(ui: &Ui, pos: Pos2, align: Align2, text: impl ToString, font: egui::FontId, color: Color32) {
    ui.painter().text(pos, align, text, font, color);
}

/// The parameter rows, as in the inspector, in a scrolling list.
fn parameters(cx: &mut Ctx, ui: &mut Ui, rect: Rect) {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    egui::ScrollArea::vertical().id_salt(("fx-editor-params", &cx.ed.owner, &cx.ed.path)).show(&mut child, |ui| {
        let ids: Vec<u32> = cx.params.iter().map(|p| p.id).collect();
        for id in ids {
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
            param_row(cx, ui, r, id);
            ui.add_space(4.0);
        }
        if cx.params.is_empty() {
            ui.label(egui::RichText::new("This effect exposes no parameters").color(TEXT_DIM));
        }
    });
}

fn param_row(cx: &mut Ctx, ui: &mut Ui, r: Rect, id: u32) {
    let Some(info) = cx.info(id).cloned() else { return };
    let resp = ui.interact(r, Id::unique(("fx-ed-row", &cx.ed.owner, &cx.ed.path, id)), Sense::click_and_drag());
    let focused = cx.ed.focus == Some(id);
    let painter = ui.painter();
    painter.rect_filled(r, CornerRadius::same(10), if resp.hovered() || resp.dragged() { RAISED_HI } else { RAISED });
    if focused {
        painter.rect_stroke(r, CornerRadius::same(10), Stroke::new(1.0, ACCENT), egui::StrokeKind::Inside);
    }
    let frac = fraction(&info, cx.value(id));
    let bar = Rect::from_min_size(r.left_bottom() - vec2(0.0, 3.0), vec2(r.width() * frac, 3.0));
    painter.rect_filled(bar, CornerRadius::same(2), ACCENT);
    let name = widgets::truncate(ui, &info.name, &theme::regular(12.0), r.width() - 150.0);
    painter.text(r.left_center() + vec2(10.0, 0.0), Align2::LEFT_CENTER, name, theme::regular(12.0), TEXT);
    let value_rect = Rect::from_min_max(pos2(r.right() - 140.0, r.top()), pos2(r.right() - 32.0, r.bottom()));
    if !typing_box(cx, ui, value_rect, id) {
        ui.painter().text(value_rect.right_center(), Align2::RIGHT_CENTER, cx.text(id), theme::mono(11.0), TEXT_DIM);
    }
    let mod_rect = Rect::from_center_size(r.right_center() - vec2(16.0, 0.0), vec2(20.0, 18.0));
    if cx.ed.track_fx().is_some() {
        let controlled = cx.controlled(id);
        if widgets::text_pill(
            ui,
            mod_rect,
            "~",
            theme::mono(11.0),
            if controlled { ACCENT } else { RAISED_HI },
            if controlled { BG } else { TEXT_DIM },
        )
        .on_hover_text("Modulation, link and MIDI/OSC learn")
        .clicked()
        {
            cx.modulate(id);
        }
    }
    interact(cx, ui, &resp, id, Axis::Horizontal(r.width()));
}

/// Where `v` sits in the parameter's range, 0..1.
pub(crate) fn fraction(info: &ParamInfo, v: f64) -> f32 {
    let span = (info.max - info.min).max(1e-9);
    if info.log && info.min > 0.0 {
        (v.max(info.min) / info.min).ln() / (info.max / info.min).ln()
    } else {
        (v - info.min) / span
    }
    .clamp(0.0, 1.0) as f32
}

/// Which way a control is dragged, and over how many points the full range lies.
#[derive(Clone, Copy)]
pub(crate) enum Axis {
    Vertical(f32),
    Horizontal(f32),
}

/// What every parameter control does: drag (Shift for fine), wheel, click to take the arrow
/// keys, double-click to type a value, right-click for a menu with reset and modulation.
pub(crate) fn interact(cx: &mut Ctx, ui: &Ui, resp: &egui::Response, id: u32, axis: Axis) {
    let Some(info) = cx.info(id).cloned() else { return };
    let fine = ui.input(|i| i.modifiers.shift);
    if resp.clicked() || resp.drag_started() {
        cx.ed.focus = Some(id);
        cx.ed.sel = None;
    }
    if resp.dragged() {
        let d = resp.drag_delta();
        let v = cx.value(id);
        let nv = match axis {
            Axis::Vertical(span) => model::drag(&info, v, d.y, span, fine),
            Axis::Horizontal(span) => model::drag(&info, v, -d.x, span, fine),
        };
        if nv != v {
            cx.set(id, nv);
        }
    }
    if resp.drag_stopped() {
        cx.end();
    }
    if resp.hovered() {
        let wheel = wheel(ui);
        if wheel != 0.0 {
            let v = model::nudge(&info, cx.value(id), wheel.signum() as f64, fine, model::is_switch(&cx.text(id)));
            cx.set_once(id, v);
        }
    }
    if resp.double_clicked() {
        start_typing_param(cx, id);
    }
    resp.context_menu(|ui| param_menu(cx, ui, id));
}

pub(crate) fn start_typing_param(cx: &mut Ctx, id: u32) {
    let shown = cx.text(id);
    let number: String = shown.split_whitespace().next().unwrap_or("").trim_start_matches('+').to_owned();
    cx.ed.typing = Some((Typing::Param(id), number, true));
}

pub(crate) fn param_menu(cx: &mut Ctx, ui: &mut Ui, id: u32) {
    let name = cx.info(id).map(|p| p.name.clone()).unwrap_or_default();
    ui.label(egui::RichText::new(name).color(TEXT_DIM));
    if ui.button("Type a value…").clicked() {
        start_typing_param(cx, id);
        ui.close();
    }
    if ui.button("Reset to default").clicked() {
        if let Some(d) = cx.info(id).map(|p| p.default) {
            cx.set_once(id, d);
        }
        ui.close();
    }
    if cx.ed.track_fx().is_some() && ui.button("Modulate/Learn…").clicked() {
        cx.modulate(id);
        ui.close();
    }
}

/// The text field of a value being typed, when it is `id`'s; true while it shows.
pub(crate) fn typing_box(cx: &mut Ctx, ui: &mut Ui, rect: Rect, id: u32) -> bool {
    let Some((Typing::Param(at), mut text, first)) = cx.ed.typing.clone() else { return false };
    if at != id {
        return false;
    }
    let edit = egui::TextEdit::singleline(&mut text).font(theme::mono(12.0)).desired_width(rect.width());
    let resp = ui.put(rect.shrink2(vec2(0.0, (rect.height() - 22.0).max(0.0) / 2.0)), edit);
    if first {
        resp.request_focus();
    }
    let (enter, escape) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
    if escape {
        cx.ed.typing = None;
    } else if enter || (resp.lost_focus() && !first) {
        if let Some(v) = model::parse_value(&text, &cx.text(id))
            && let Some(info) = cx.info(id)
        {
            let v = v.clamp(info.min.min(info.max), info.max.max(info.min));
            cx.set_once(id, v);
        }
        cx.ed.typing = None;
    } else {
        cx.ed.typing = Some((Typing::Param(id), text, false));
    }
    true
}

/// A rotary control for parameter `id`.
pub(crate) fn knob(cx: &mut Ctx, ui: &mut Ui, rect: Rect, id: u32, label: &str) {
    let Some(info) = cx.info(id).cloned() else { return };
    let resp = ui.interact(rect, Id::unique(("fx-ed-knob", &cx.ed.owner, &cx.ed.path, id)), Sense::click_and_drag());
    let painter = ui.painter();
    let size = rect.width().min(rect.height() - 30.0).clamp(16.0, 60.0);
    let dial = Rect::from_center_size(pos2(rect.center().x, rect.top() + size / 2.0 + 2.0), vec2(size, size));
    let r = size / 2.0 - 3.0;
    let centre = dial.center();
    let (start, sweep) = (std::f32::consts::PI * 0.75, std::f32::consts::PI * 1.5);
    let frac = fraction(&info, cx.value(id));
    let arc = |from: f32, to: f32| -> Vec<Pos2> {
        let n = 40;
        (0..=n)
            .map(|k| {
                let a = start + sweep * (from + (to - from) * k as f32 / n as f32);
                centre + vec2(a.cos(), a.sin()) * r
            })
            .collect()
    };
    let hot = resp.hovered() || resp.dragged();
    painter.circle_filled(centre, (r - 5.0).max(2.0), if hot { RAISED_HI } else { RAISED });
    painter.add(Shape::line(arc(0.0, 1.0), Stroke::new(3.0, BG)));
    let zero: f32 = if info.min < 0.0 && (info.max + info.min).abs() < 1e-9 { 0.5 } else { 0.0 };
    painter.add(Shape::line(arc(zero.min(frac), zero.max(frac)), Stroke::new(3.0, ACCENT)));
    let a = start + sweep * frac;
    painter.line_segment(
        [centre + vec2(a.cos(), a.sin()) * (r * 0.35), centre + vec2(a.cos(), a.sin()) * (r - 6.0)],
        Stroke::new(2.0, TEXT),
    );
    if cx.ed.focus == Some(id) {
        painter.circle_stroke(centre, r + 2.0, Stroke::new(1.0, theme::with_alpha(ACCENT, 140)));
    }
    if cx.controlled(id) {
        painter.circle_filled(dial.right_top() + vec2(-2.0, 4.0), 3.0, ACCENT);
    }
    painter.text(
        pos2(rect.center().x, dial.bottom() + 8.0),
        Align2::CENTER_CENTER,
        widgets::truncate(ui, label, &theme::regular(11.0), rect.width() + 4.0),
        theme::regular(11.0),
        TEXT_DIM,
    );
    let value_rect =
        Rect::from_center_size(pos2(rect.center().x, dial.bottom() + 22.0), vec2(rect.width().max(60.0), 16.0));
    if !typing_box(cx, ui, value_rect, id) {
        let text = cx.text(id);
        let fits = |size: f32| widgets::text_width(ui, &text, theme::mono(size)) <= rect.width() + 2.0;
        let size = if fits(11.0) {
            Some(11.0)
        } else if fits(9.0) {
            Some(9.0)
        } else {
            None
        };
        if let Some(size) = size {
            ui.painter().text(value_rect.center(), Align2::CENTER_CENTER, text, theme::mono(size), TEXT);
        }
    }
    let resp = resp.on_hover_text(format!("{} {}: drag, scroll or type (double-click)", info.name, cx.text(id)));
    interact(cx, ui, &resp, id, Axis::Vertical(160.0));
}

/// A row of knobs filling `rect`, one per (id, label) that the effect has.
pub(crate) fn knob_row(cx: &mut Ctx, ui: &mut Ui, rect: Rect, knobs: &[(u32, &str)]) {
    let present: Vec<&(u32, &str)> = knobs.iter().filter(|(id, _)| cx.info(*id).is_some()).collect();
    if present.is_empty() {
        return;
    }
    let w = (rect.width() / present.len() as f32).min(110.0);
    let left = rect.center().x - w * present.len() as f32 / 2.0;
    for (k, (id, label)) in present.into_iter().enumerate() {
        let r = Rect::from_min_size(pos2(left + k as f32 * w, rect.top()), vec2(w, rect.height()));
        knob(cx, ui, r.shrink2(vec2(4.0, 0.0)), *id, label);
    }
}

/// A switch for an on/off parameter, with its label.
pub(crate) fn switch_param(cx: &mut Ctx, ui: &mut Ui, rect: Rect, id: u32, label: &str) {
    if cx.info(id).is_none() {
        return;
    }
    let on = cx.value(id) >= 0.5;
    let sw = Rect::from_min_size(pos2(rect.left(), rect.center().y - 10.0), vec2(34.0, 20.0));
    let resp = ui.interact(rect, Id::unique(("fx-ed-toggle", &cx.ed.owner, &cx.ed.path, id)), Sense::click());
    let painter = ui.painter();
    painter.rect_filled(sw, CornerRadius::same(255), if on { ACCENT } else { theme::SWITCH_OFF });
    let knob_x = if on { sw.right() - 10.0 } else { sw.left() + 10.0 };
    painter.circle_filled(pos2(knob_x, sw.center().y), 7.0, TEXT);
    painter.text(pos2(sw.right() + 8.0, rect.center().y), Align2::LEFT_CENTER, label, theme::regular(12.0), TEXT_MID);
    if resp.clicked() {
        cx.set_once(id, if on { 0.0 } else { 1.0 });
    }
    resp.context_menu(|ui| param_menu(cx, ui, id));
}

/// A compact value field: label, value, drag sideways, double-click to type.
pub(crate) fn value_box(cx: &mut Ctx, ui: &mut Ui, rect: Rect, id: u32, label: &str) {
    if cx.info(id).is_none() {
        return;
    }
    let resp = ui.interact(rect, Id::unique(("fx-ed-box", &cx.ed.owner, &cx.ed.path, id)), Sense::click_and_drag());
    let painter = ui.painter();
    let hot = resp.hovered() || resp.dragged();
    painter.rect_filled(rect, CornerRadius::same(8), if hot { RAISED_HI } else { BG });
    if cx.ed.focus == Some(id) {
        painter.rect_stroke(rect, CornerRadius::same(8), Stroke::new(1.0, ACCENT), egui::StrokeKind::Inside);
    }
    let needed =
        widgets::text_width(ui, label, theme::regular(11.0)) + widgets::text_width(ui, &cx.text(id), theme::mono(11.0));
    if needed + 24.0 <= rect.width() {
        ui.painter().text(
            rect.left_center() + vec2(8.0, 0.0),
            Align2::LEFT_CENTER,
            label,
            theme::regular(11.0),
            TEXT_DIM,
        );
    }
    let value_rect = Rect::from_min_max(pos2(rect.left() + rect.width() * 0.4, rect.top()), rect.max - vec2(6.0, 0.0));
    if !typing_box(cx, ui, value_rect, id) {
        ui.painter().text(
            rect.right_center() - vec2(8.0, 0.0),
            Align2::RIGHT_CENTER,
            cx.text(id),
            theme::mono(11.0),
            TEXT,
        );
    }
    let resp = if label.is_empty() { resp } else { resp.on_hover_text(label) };
    interact(cx, ui, &resp, id, Axis::Horizontal(240.0));
}

/// A rounded card behind a section.
pub(crate) fn card(ui: &Ui, rect: Rect) {
    ui.painter().rect_filled(rect, CornerRadius::same(12), RAISED);
}

pub(crate) fn section_title(ui: &Ui, rect: Rect, text: &str) {
    ui.painter().text(rect.left_top() + vec2(12.0, 10.0), Align2::LEFT_TOP, text, theme::semibold(12.0), TEXT_MID);
}

/// The graph's background: frequency lines when `freq`, dB lines every `step`.
pub(crate) fn grid(ui: &Ui, plot: &Plot, freq: bool, step: f64, unit: &str) {
    let painter = ui.painter_at(plot.rect.expand(1.0));
    painter.rect_filled(plot.rect, CornerRadius::same(8), BG);
    let line = theme::with_alpha(RAISED_HI, 200);
    if freq {
        for (hz, label) in model::freq_ticks(plot.x.lo, plot.x.hi) {
            let x = plot.sx(hz);
            painter.line_segment([pos2(x, plot.rect.top()), pos2(x, plot.rect.bottom())], Stroke::new(1.0, line));
            if x < plot.rect.right() - 36.0 {
                painter.text(
                    pos2(x + 3.0, plot.rect.bottom() - 3.0),
                    Align2::LEFT_BOTTOM,
                    label,
                    theme::regular(10.0),
                    TEXT_DIM,
                );
            }
        }
    }
    for db in model::db_ticks(plot.y.lo, plot.y.hi, step) {
        let y = plot.sy(db);
        let stroke = if db == 0.0 { Stroke::new(1.0, theme::with_alpha(TEXT_DIM, 90)) } else { Stroke::new(1.0, line) };
        painter.line_segment([pos2(plot.rect.left(), y), pos2(plot.rect.right(), y)], stroke);
        if y > plot.rect.top() + 8.0 && y < plot.rect.bottom() - 8.0 {
            painter.text(
                pos2(plot.rect.right() - 4.0, y - 1.0),
                Align2::RIGHT_BOTTOM,
                tick_label(db, step, unit),
                theme::regular(10.0),
                TEXT_DIM,
            );
        }
    }
}

/// A curve through `points`, clipped to `clip`.
pub(crate) fn polyline(ui: &Ui, clip: Rect, points: Vec<Pos2>, stroke: Stroke) {
    ui.painter_at(clip).add(Shape::line(points, stroke));
}

/// The area between `points` and the horizontal line at `base`.
pub(crate) fn area(ui: &Ui, clip: Rect, points: &[Pos2], base: f32, color: Color32) {
    let mut mesh = Mesh::default();
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
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

/// A spectrum drawn as a filled area behind a curve.
pub(crate) fn spectrum(ui: &Ui, plot: &Plot, s: &Spectrum, offset: f32, fill: Color32, line: Color32) {
    let xs = plot.xs(2.0);
    let points: Vec<Pos2> = xs
        .windows(2)
        .map(|w| {
            let db = s.level(w[0], w[1]) + offset;
            pos2(plot.sx((w[0] * w[1]).sqrt()), plot.sy(db as f64).min(plot.rect.bottom()))
        })
        .collect();
    area(ui, plot.rect, &points, plot.rect.bottom(), fill);
    polyline(ui, plot.rect, points, Stroke::new(1.0, line));
}

/// A vertical level meter from `lo` to `hi` dB; reduction meters hang from the top.
#[allow(clippy::too_many_arguments)]
pub(crate) fn meter(ui: &Ui, rect: Rect, db: f32, peak: Option<f32>, lo: f32, hi: f32, color: Color32, from_top: bool) {
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(4), BG);
    let t = ((db - lo) / (hi - lo)).clamp(0.0, 1.0);
    let fill = if from_top {
        Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + rect.height() * t))
    } else {
        Rect::from_min_max(pos2(rect.left(), rect.bottom() - rect.height() * t), rect.max)
    };
    if fill.height() > 0.5 {
        painter.rect_filled(fill, CornerRadius::same(4), color);
    }
    if let Some(p) = peak.filter(|p| *p > lo) {
        let t = ((p - lo) / (hi - lo)).clamp(0.0, 1.0);
        let y = if from_top { rect.top() + rect.height() * t } else { rect.bottom() - rect.height() * t };
        painter.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], Stroke::new(1.5, TEXT));
    }
}

/// A labelled meter with its reading under it.
pub(crate) fn labelled_meter(ui: &Ui, rect: Rect, label: &str, b: &Ballistic, reduction: bool) {
    let bar = Rect::from_min_max(rect.min + vec2(0.0, 16.0), rect.max - vec2(0.0, 18.0));
    ui.painter().text(
        pos2(rect.center().x, rect.top() + 6.0),
        Align2::CENTER_CENTER,
        label,
        theme::regular(10.0),
        TEXT_DIM,
    );
    let reading = if reduction {
        meter(ui, bar, b.db, None, 0.0, 24.0, theme::PINK, true);
        format!("-{:.1}", b.db.max(0.0))
    } else {
        meter(ui, bar, b.db, Some(b.peak), -60.0, 6.0, theme::GREEN, false);
        let zone = |from: f32, to: f32, color| {
            if b.db > from {
                let y = |db: f32| bar.bottom() - bar.height() * ((db + 60.0) / 66.0).clamp(0.0, 1.0);
                let r = Rect::from_min_max(pos2(bar.left(), y(b.db.min(to))), pos2(bar.right(), y(from)));
                ui.painter().rect_filled(r, CornerRadius::ZERO, color);
            }
        };
        zone(-12.0, 0.0, theme::YELLOW);
        zone(0.0, 6.0, theme::PINK);
        if b.peak <= model::FLOOR + 1.0 { "-inf".to_owned() } else { format!("{:.1}", b.peak) }
    };
    ui.painter().text(
        pos2(rect.center().x, rect.bottom() - 7.0),
        Align2::CENTER_CENTER,
        reading,
        theme::mono(10.0),
        TEXT,
    );
}

/// The last values of `h` across `rect`, newest on the right, between `lo` and `hi`.
pub(crate) fn history(ui: &Ui, rect: Rect, h: &History, lo: f32, hi: f32, color: Color32, from_top: bool) {
    let n = h.capacity().max(2);
    let start = n - h.len();
    let y = |v: f32| {
        let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
        if from_top { rect.top() + rect.height() * t } else { rect.bottom() - rect.height() * t }
    };
    let points: Vec<Pos2> = h
        .iter()
        .enumerate()
        .map(|(i, v)| pos2(rect.left() + (start + i) as f32 / (n - 1) as f32 * rect.width(), y(v)))
        .collect();
    if points.len() < 2 {
        return;
    }
    area(ui, rect, &points, if from_top { rect.top() } else { rect.bottom() }, theme::with_alpha(color, 50));
    polyline(ui, rect, points, Stroke::new(1.5, color));
}

/// A drag handle: a filled circle with a ring when selected.
pub(crate) fn handle(ui: &Ui, clip: Rect, at: Pos2, color: Color32, selected: bool, label: &str) {
    let painter = ui.painter_at(clip.expand(10.0));
    if selected {
        painter.circle_filled(at, 11.0, theme::with_alpha(color, 60));
    }
    painter.circle_filled(at, 7.0, color);
    painter.circle_stroke(at, 7.0, Stroke::new(1.5, if selected { TEXT } else { BG }));
    if !label.is_empty() {
        painter.text(at, Align2::CENTER_CENTER, label, theme::semibold(9.0), BG);
    }
}

/// Colours that tell bands apart.
pub(crate) const BAND_COLORS: [Color32; 8] = [
    Color32::from_rgb(0xfd, 0x97, 0x1f),
    Color32::from_rgb(0x66, 0xd9, 0xef),
    Color32::from_rgb(0xa6, 0xe2, 0x2e),
    Color32::from_rgb(0xae, 0x81, 0xff),
    Color32::from_rgb(0xf9, 0x26, 0x72),
    Color32::from_rgb(0xe6, 0xdb, 0x74),
    Color32::from_rgb(0x4e, 0xc9, 0xb0),
    Color32::from_rgb(0xff, 0x8c, 0xc6),
];

pub(crate) fn band_color(k: usize) -> Color32 {
    BAND_COLORS[k % BAND_COLORS.len()]
}

/// Splits `rect` into a left part `w` wide and the rest, with a gap.
pub(crate) fn split_left(rect: Rect, w: f32, gap: f32) -> (Rect, Rect) {
    let w = w.min(rect.width());
    (
        Rect::from_min_size(rect.min, vec2(w, rect.height())),
        Rect::from_min_max(pos2((rect.left() + w + gap).min(rect.right()), rect.top()), rect.max),
    )
}

/// Splits `rect` into a top part `h` tall and the rest, with a gap.
pub(crate) fn split_top(rect: Rect, h: f32, gap: f32) -> (Rect, Rect) {
    let h = h.min(rect.height());
    (
        Rect::from_min_size(rect.min, vec2(rect.width(), h)),
        Rect::from_min_max(pos2(rect.left(), (rect.top() + h + gap).min(rect.bottom())), rect.max),
    )
}

/// A value that is not a parameter (a delay tap's time, a curve point), shown in a
/// [`field_box`].
pub(crate) struct Field {
    /// Tells the field apart from the editor's others, for typing.
    pub key: u64,
    pub value: f64,
    pub min: f64,
    pub max: f64,
    pub log: bool,
    pub text: String,
}

/// What a [`field_box`] asks for: a new value, and whether the gesture ends with it.
pub(crate) struct FieldChange {
    pub value: Option<f64>,
    pub done: bool,
}

/// A value field like [`value_box`] for a [`Field`]: drag sideways, scroll, double-click to
/// type. The caller applies the value within a gesture and ends it when `done`.
pub(crate) fn field_box(cx: &mut Ctx, ui: &mut Ui, rect: Rect, f: &Field, label: &str) -> FieldChange {
    let info = ParamInfo { id: 0, name: label.into(), min: f.min, max: f.max, default: f.min, log: f.log };
    let resp =
        ui.interact(rect, Id::unique(("fx-ed-field", &cx.ed.owner, &cx.ed.path, f.key)), Sense::click_and_drag());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(8), if resp.hovered() || resp.dragged() { RAISED_HI } else { BG });
    if !label.is_empty() {
        painter.text(rect.left_center() + vec2(7.0, 0.0), Align2::LEFT_CENTER, label, theme::regular(10.0), TEXT_DIM);
    }
    let mut out = FieldChange { value: None, done: false };
    if let Some((Typing::Field(key), mut text, first)) = cx.ed.typing.clone()
        && key == f.key
    {
        let edit = egui::TextEdit::singleline(&mut text).font(theme::mono(11.0)).desired_width(rect.width());
        let r = ui.put(rect, edit);
        if first {
            r.request_focus();
        }
        let (enter, escape) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
        if escape {
            cx.ed.typing = None;
        } else if enter || (r.lost_focus() && !first) {
            out.value = model::parse_value(&text, &f.text).map(|v| v.clamp(f.min, f.max));
            out.done = true;
            cx.ed.typing = None;
        } else {
            cx.ed.typing = Some((Typing::Field(f.key), text, false));
        }
        return out;
    }
    ui.painter().text(rect.right_center() - vec2(7.0, 0.0), Align2::RIGHT_CENTER, &f.text, theme::mono(11.0), TEXT);
    let fine = ui.input(|i| i.modifiers.shift);
    if resp.dragged() {
        let v = model::drag(&info, f.value, -resp.drag_delta().x, 240.0, fine);
        if v != f.value {
            out.value = Some(v);
        }
    }
    out.done |= resp.drag_stopped();
    if resp.hovered() {
        let wheel = wheel(ui);
        if wheel != 0.0 {
            out.value = Some(model::nudge(&info, f.value, wheel.signum() as f64, fine, false));
            out.done = true;
        }
    }
    if resp.double_clicked() {
        let number = f.text.split_whitespace().next().unwrap_or("").trim_start_matches('+').to_owned();
        cx.ed.typing = Some((Typing::Field(f.key), number, true));
    }
    out
}

/// Wheel notches turned this frame, up positive.
pub(crate) fn wheel(ui: &Ui) -> f32 {
    ui.input(|i| {
        i.events
            .iter()
            .filter_map(|e| match e {
                egui::Event::MouseWheel { delta, .. } => Some(delta.y.signum()),
                _ => None,
            })
            .sum()
    })
}

/// Keys an editor takes while [`takes_keys`]: the arrows nudge, Delete removes.
pub fn is_editor_key(key: egui::Key) -> bool {
    use egui::Key::*;
    matches!(key, ArrowUp | ArrowDown | ArrowLeft | ArrowRight | Delete | Backspace)
}

/// Switches side by side, each as wide as its label.
pub(crate) fn switch_row(cx: &mut Ctx, ui: &mut Ui, rect: Rect, switches: &[(u32, &str)]) {
    let mut x = rect.left();
    for (id, label) in switches {
        if cx.info(*id).is_none() {
            continue;
        }
        let w = 34.0 + 8.0 + widgets::text_width(ui, label, theme::regular(12.0)) + 18.0;
        if x + w > rect.right() + 8.0 {
            break;
        }
        switch_param(cx, ui, Rect::from_min_size(pos2(x, rect.top()), vec2(w, rect.height().min(24.0))), *id, label);
        x += w;
    }
}

/// A grid line's label: dB with their sign, other units as plain numbers, with the decimals
/// the step needs.
pub(crate) fn tick_label(v: f64, step: f64, unit: &str) -> String {
    let decimals = (0..4)
        .find(|d| {
            let s = step * 10f64.powi(*d);
            (s - s.round()).abs() < 1e-9
        })
        .unwrap_or(3) as usize;
    let signed = unit.contains("dB") || (unit.is_empty() && step >= 1.0);
    if v == 0.0 {
        format!("{:.decimals$}{unit}", 0.0)
    } else if signed {
        format!("{v:+.decimals$}{unit}")
    } else {
        format!("{v:.decimals$}{unit}")
    }
}
