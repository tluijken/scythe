//! ScyEQ-Pro in Scythe: its editor (`scyeq_ui`) on a running instance, the compare sources
//! the project offers, and the instance list, every ScyEQ-Pro in one window.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use egui::{Id, Rect, Ui, UiBuilder};
use scyeq_ui::{CompareSource, EqHost, InstanceList, ReferenceAudio, ViewState, WindowFn, WindowSpec};
use scythe_core::FxSlot;
use scythe_engine::fx::FxOwner;
use scythe_engine::probe::FxProbe;
use scythe_engine::scythe_eq::params::{self as p, band};
use scythe_engine::scythe_eq::{self, Meters, ScytheEq};

use super::{Ctx, chain_slots, slot_at};
use crate::app::ScytheApp;
use crate::ui::window::{WindowKind, scythe_window};

/// The editor of the ScyEQ-Pro `cx` shows, in `rect`.
pub fn show(cx: &mut Ctx, ui: &mut Ui, rect: Rect) {
    let Some((values, meters, latency, sample_rate)) = cx
        .inst
        .with(|e| {
            let latency = e.latency();
            let eq = e.as_any_mut().and_then(|a| a.downcast_mut::<ScytheEq>())?;
            Some((eq.values().to_vec(), eq.meters().clone(), latency, eq.sample_rate()))
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
        let now = self.cx.app.fx_gesture("Adjust ScyEQ-Pro", &self.cx.inst, |e| {
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
        project_instances(self.cx.app)
            .iter()
            .filter(|i| !(i.owner == self.cx.ed.owner && i.path == self.cx.ed.path))
            .map(|i| scyeq_ui::Instance { id: source_id(&i.owner, &i.path), label: i.label.clone(), color: None })
            .collect()
    }

    fn compare_sources(&self) -> Vec<CompareSource> {
        track_outputs(self.cx.app)
            .into_iter()
            .map(|(owner, label)| CompareSource { id: source_id(&owner, &[]), label, color: None })
            .collect()
    }

    fn source_probe(&self, id: Id) -> Option<Arc<FxProbe>> {
        let app = &*self.cx.app;
        if let Some((owner, _)) = track_outputs(app).into_iter().find(|(o, _)| source_id(o, &[]) == id) {
            return track_probe(app, &owner);
        }
        let instances = project_instances(app);
        let i = instances.iter().find(|i| source_id(&i.owner, &i.path) == id)?;
        let slots = chain_slots(app, &i.owner);
        app.engine.fx().instance_at(&i.owner, &slots, &i.path).map(|i| i.probe.clone())
    }

    fn pick_reference(&mut self) -> Option<PathBuf> {
        rfd::FileDialog::new().add_filter("Audio", scythe_decode::REFERENCE_EXTENSIONS).pick_file()
    }

    fn decode_reference(&self) -> Option<scyeq_ui::DecodeFn> {
        Some(decode_file)
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

fn decode_file(path: &Path) -> Result<ReferenceAudio, String> {
    let audio = scythe_engine::media::decode_file(path).map_err(|e| e.to_string())?;
    Ok(ReferenceAudio { channels: audio.channels, sample_rate: audio.sample_rate })
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

/// The session revision and load count an instance list was made at, and the list.
pub type Listed = (u64, u64, Rc<Vec<Instance>>);

/// [`all_instances`], listed anew only once the project changed: the editor and the instance
/// list ask for it every frame.
pub fn project_instances(app: &ScytheApp) -> Rc<Vec<Instance>> {
    let at = (app.revision(), app.loads());
    let mut cache = app.eq_instance_list.borrow_mut();
    if let Some((revision, loads, list)) = &*cache
        && (*revision, *loads) == at
    {
        return list.clone();
    }
    let list = Rc::new(all_instances(app));
    *cache = Some((at.0, at.1, list.clone()));
    list
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
/// The instance list, while `app.view.eq_instances`.
pub fn instances_window(app: &mut ScytheApp, ui: &mut Ui) {
    if !app.view.eq_instances {
        return;
    }
    let mut open = true;
    let ctx = ui.ctx().clone();
    let all = project_instances(app);
    scyeq_ui::instances_window(&ctx, &mut ProjectInstances { app, all }, &mut open);
    if !open {
        app.view.eq_instances = false;
        if app.session.in_gesture() {
            app.end_gesture();
        }
    }
}

/// The project's ScyEQ-Pro instances for the instance list, its changes undo steps of the
/// session.
struct ProjectInstances<'a> {
    app: &'a mut ScytheApp,
    all: Rc<Vec<Instance>>,
}

impl ProjectInstances<'_> {
    fn find(&self, id: Id) -> Option<&Instance> {
        self.all.iter().find(|i| source_id(&i.owner, &i.path) == id)
    }

    fn slot(&self, id: Id) -> Option<FxSlot> {
        let i = self.find(id)?;
        slot_at(&chain_slots(self.app, &i.owner), &i.path)
    }

    fn running(&self, id: Id) -> Option<Arc<scythe_engine::fx::FxInstance>> {
        let i = self.find(id)?;
        let slots = chain_slots(self.app, &i.owner);
        self.app.engine.fx().instance_at(&i.owner, &slots, &i.path)
    }
}

impl InstanceList for ProjectInstances<'_> {
    fn instance_list(&self) -> Vec<scyeq_ui::Instance> {
        self.all
            .iter()
            .map(|i| scyeq_ui::Instance { id: source_id(&i.owner, &i.path), label: i.label.clone(), color: None })
            .collect()
    }

    fn instance_values(&self, id: Id) -> Option<Vec<f64>> {
        let slot = self.slot(id)?;
        let running = self.running(id).and_then(|i| {
            i.with(|e| e.as_any_mut().and_then(|a| a.downcast_mut::<ScytheEq>()).map(|eq| eq.values().to_vec()))
                .flatten()
        });
        Some(running.or_else(|| scythe_eq::slot_values(&slot)).unwrap_or_else(p::defaults))
    }

    fn instance_enabled(&self, id: Id) -> bool {
        self.slot(id).is_some_and(|s| s.enabled)
    }

    fn toggle_instance_bypass(&mut self, id: Id) {
        let Some(i) = self.find(id).cloned() else { return };
        super::edit_slot(self.app, &i.owner, &i.path, "Toggle FX bypass", &|s| s.enabled = !s.enabled);
    }

    fn instance_open(&self, id: Id) -> bool {
        self.find(id).is_some_and(|i| super::is_open_at(self.app, &i.owner, &i.path))
    }

    fn toggle_instance_open(&mut self, id: Id) {
        let Some(i) = self.find(id).cloned() else { return };
        super::toggle_at(self.app, &i.owner, &i.path);
    }

    fn begin_instance(&mut self, _id: Id, _param: u32) {
        self.app.end_gesture();
    }

    fn set_instance(&mut self, id: Id, param: u32, value: f64) {
        let Some(fx) = self.running(id) else { return };
        let label = if param % p::BAND_STRIDE == band::ENABLED { "Bypass EQ band" } else { "Adjust EQ band gain" };
        self.app.fx_gesture(label, &fx, |e| e.set_param(param, value));
    }

    fn end_instance(&mut self, _id: Id, _param: u32) {
        self.app.end_gesture();
    }

    fn window(&self) -> WindowFn {
        panel
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_project_is_walked_again_only_after_it_changed() {
        let mut app = ScytheApp::with_devices(egui::Context::default(), &Default::default(), true);
        app.edit("Add track", |p| {
            let mut track = scythe_core::Track::new("Vocals");
            track.fx.push(scythe_eq::slot(None));
            p.tracks.push(track);
        });
        let first = project_instances(&app);
        assert_eq!(first.len(), 1);
        assert!(Rc::ptr_eq(&first, &project_instances(&app)), "listed again without a change");
        app.edit("Add FX", |p| p.tracks[0].fx.push(scythe_eq::slot(None)));
        let labels: Vec<String> = project_instances(&app).iter().map(|i| i.label.clone()).collect();
        assert_eq!(labels, ["Vocals · 1", "Vocals · 2"]);
    }
}
