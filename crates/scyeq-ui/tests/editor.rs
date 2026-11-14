//! The editor against a host that records what it is asked: a drag is one gesture per
//! parameter, a new band one edit.

use std::sync::Arc;

use egui::{Id, Pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use scyeq_dsp::params::{self as p, band};
use scyeq_dsp::probe::FxProbe;
use scyeq_dsp::{Meters, ScytheEq};
use scyeq_ui::{CompareSource, EditorState, EqHost, Instance, ViewState};

#[derive(Debug, Clone, PartialEq)]
enum Call {
    Begin(u32),
    Set(u32, f64),
    End(u32),
    Edit(String),
}

struct Fake {
    eq: ScytheEq,
    calls: Vec<Call>,
    view: ViewState,
}

impl Fake {
    /// An EQ with band 1 a bell at 1 kHz.
    fn new() -> Self {
        let mut eq = ScytheEq::new(48_000);
        let id = |o| p::band_param(0, o);
        eq.set(id(band::USED), 1.0);
        eq.set(id(band::FREQ), 1000.0);
        eq.set(id(band::GAIN), 0.0);
        Self { eq, calls: Vec::new(), view: ViewState::default() }
    }

    fn count(&self, call: impl Fn(&Call) -> bool) -> usize {
        self.calls.iter().filter(|c| call(c)).count()
    }
}

impl EqHost for Fake {
    fn values(&self) -> &[f64] {
        self.eq.values()
    }

    fn begin(&mut self, id: u32) {
        self.calls.push(Call::Begin(id));
    }

    fn set(&mut self, id: u32, value: f64) {
        self.calls.push(Call::Set(id, value));
        self.eq.set(id, value);
    }

    fn end(&mut self, id: u32) {
        self.calls.push(Call::End(id));
    }

    fn edit(&mut self, label: &str, f: &mut dyn FnMut(&mut ScytheEq)) {
        self.calls.push(Call::Edit(label.to_owned()));
        f(&mut self.eq);
    }

    fn meters(&self) -> Option<Arc<Meters>> {
        Some(self.eq.meters())
    }

    fn probe(&self) -> Option<Arc<FxProbe>> {
        None
    }

    fn latency(&self) -> usize {
        0
    }

    fn sample_rate(&self) -> f64 {
        48_000.0
    }

    fn instances(&self) -> Vec<Instance> {
        Vec::new()
    }

    fn compare_sources(&self) -> Vec<CompareSource> {
        Vec::new()
    }

    fn source_probe(&self, _id: Id) -> Option<Arc<FxProbe>> {
        None
    }

    fn view(&mut self) -> &mut ViewState {
        &mut self.view
    }
}

type State = (Fake, EditorState);

fn harness() -> Harness<'static, State> {
    Harness::builder()
        .with_size(vec2(1180.0, 680.0))
        .with_step_dt(1.0 / 30.0)
        .with_context_setup(scyeq_ui::theme::install)
        .build_ui_state(
            |ui, (host, state): &mut State| scyeq_ui::show(ui, host, state),
            (Fake::new(), EditorState::default()),
        )
}

fn button(h: &mut Harness<State>, pos: Pos2, pressed: bool) {
    h.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
}

#[test]
fn dragging_a_band_sends_one_gesture() {
    let mut h = harness();
    h.step();
    let from = h.get_by_label("Band 1").rect().center();
    let to = from + vec2(60.0, -30.0);
    h.event(egui::Event::PointerMoved(from));
    h.step();
    button(&mut h, from, true);
    for i in 1..=8 {
        h.event(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 8.0)));
        h.step();
    }
    button(&mut h, to, false);
    h.step();

    let freq = p::band_param(0, band::FREQ);
    let host = &h.state().0;
    assert_eq!(host.count(|c| *c == Call::Begin(freq)), 1, "{:?}", host.calls);
    assert!(host.count(|c| matches!(c, Call::Set(id, _) if *id == freq)) >= 1, "{:?}", host.calls);
    assert_eq!(host.count(|c| *c == Call::End(freq)), 1, "{:?}", host.calls);
    let first_set = host.calls.iter().position(|c| matches!(c, Call::Set(id, _) if *id == freq));
    let begin = host.calls.iter().position(|c| *c == Call::Begin(freq));
    let end = host.calls.iter().position(|c| *c == Call::End(freq));
    assert!(begin < first_set && first_set < end, "{:?}", host.calls);
    assert!(host.eq.value(freq) > 1100.0, "the band moved up in frequency: {}", host.eq.value(freq));
    assert_eq!(host.count(|c| matches!(c, Call::Edit(_))), 0, "{:?}", host.calls);
}

#[test]
fn adding_a_band_is_one_edit() {
    let mut h = harness();
    h.step();
    let graph = h.get_by_label("ScyEQ-Pro graph").rect();
    let at = graph.center() + vec2(-200.0, -40.0);
    h.event(egui::Event::PointerMoved(at));
    h.step();
    for _ in 0..2 {
        button(&mut h, at, true);
        button(&mut h, at, false);
    }
    h.step();

    let host = &h.state().0;
    assert_eq!(host.calls, vec![Call::Edit("Add EQ band".to_owned())]);
    assert_eq!(host.eq.value(p::band_param(1, band::USED)), 1.0, "band 2 is in use");
    assert!(h.state().1.selected().contains(&1), "the new band is selected");
}
