//! The instance list against a host that records what it is asked.

use egui::{Id, Pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use scyeq_dsp::params::{self as p, band};
use scyeq_ui::{Instance, InstanceControls, InstanceList};

#[derive(Debug, Clone, PartialEq)]
enum Call {
    Begin(Id, u32),
    Set(Id, u32, f64),
    End(Id, u32),
}

struct Fake {
    values: Vec<f64>,
    calls: Vec<Call>,
    open: bool,
    /// Whether the list may open and bypass instances.
    controls: bool,
}

fn vocals() -> Id {
    Id::unique("vocals")
}

impl Fake {
    /// One instance, "Vocals", with band 1 a bell at 1 kHz.
    fn new() -> Self {
        let mut values = p::defaults();
        values[p::band_param(0, band::USED) as usize] = 1.0;
        values[p::band_param(0, band::FREQ) as usize] = 1000.0;
        values[p::band_param(0, band::GAIN) as usize] = 0.0;
        Self { values, calls: Vec::new(), open: true, controls: true }
    }
}

impl InstanceList for Fake {
    fn instance_list(&self) -> Vec<Instance> {
        vec![Instance { id: vocals(), label: "Vocals".to_owned(), color: None }]
    }

    fn instance_values(&self, _id: Id) -> Option<Vec<f64>> {
        Some(self.values.clone())
    }

    fn begin_instance(&mut self, id: Id, param: u32) {
        self.calls.push(Call::Begin(id, param));
    }

    fn set_instance(&mut self, id: Id, param: u32, value: f64) {
        self.calls.push(Call::Set(id, param, value));
        self.values[param as usize] = value;
    }

    fn end_instance(&mut self, id: Id, param: u32) {
        self.calls.push(Call::End(id, param));
    }

    fn controls(&mut self) -> Option<&mut dyn InstanceControls> {
        if self.controls { Some(self) } else { None }
    }
}

impl InstanceControls for Fake {
    fn instance_enabled(&self, _id: Id) -> bool {
        true
    }

    fn toggle_instance_bypass(&mut self, _id: Id) {}

    fn instance_open(&self, _id: Id) -> bool {
        false
    }

    fn toggle_instance_open(&mut self, _id: Id) {}
}

fn button(h: &mut Harness<Fake>, pos: Pos2, pressed: bool) {
    h.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
}

#[test]
fn dragging_an_instances_gain_is_one_gesture() {
    let mut h =
        Harness::builder().with_size(vec2(900.0, 500.0)).with_context_setup(scyeq_ui::theme::install).build_ui_state(
            |ui, list: &mut Fake| {
                let ctx = ui.ctx().clone();
                let mut open = list.open;
                scyeq_ui::instances_window(&ctx, list, &mut open);
                list.open = open;
            },
            Fake::new(),
        );
    h.step();
    let from = h.get_by_label("Vocals band 1").rect().center();
    let to = from + vec2(0.0, -40.0);
    h.event(egui::Event::PointerMoved(from));
    h.step();
    button(&mut h, from, true);
    for i in 1..=8 {
        h.event(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 8.0)));
        h.step();
    }
    button(&mut h, to, false);
    h.step();

    let gain = p::band_param(0, band::GAIN);
    let list = h.state();
    let calls = &list.calls;
    assert_eq!(calls.first(), Some(&Call::Begin(vocals(), gain)), "{calls:?}");
    assert_eq!(calls.last(), Some(&Call::End(vocals(), gain)), "{calls:?}");
    assert_eq!(calls.iter().filter(|c| matches!(c, Call::Begin(..))).count(), 1, "{calls:?}");
    assert_eq!(calls.iter().filter(|c| matches!(c, Call::End(..))).count(), 1, "{calls:?}");
    assert!(calls.iter().all(|c| !matches!(c, Call::Set(_, id, _) if id != &gain)), "{calls:?}");
    assert!(list.values[gain as usize] > 2.0, "dragging up raises the gain: {}", list.values[gain as usize]);
}

fn harness(list: Fake) -> Harness<'static, Fake> {
    Harness::builder().with_size(vec2(900.0, 500.0)).with_context_setup(scyeq_ui::theme::install).build_ui_state(
        |ui, list: &mut Fake| {
            let ctx = ui.ctx().clone();
            let mut open = list.open;
            scyeq_ui::instances_window(&ctx, list, &mut open);
            list.open = open;
        },
        list,
    )
}

#[test]
fn a_list_that_cannot_open_or_bypass_instances_offers_neither() {
    let mut h = harness(Fake::new());
    h.step();
    assert!(h.query_by_label("Open").is_some());
    assert!(h.query_by_label("On").is_some());

    let mut h = harness(Fake { controls: false, ..Fake::new() });
    h.step();
    assert!(h.query_by_label("Open").is_none());
    assert!(h.query_by_label("On").is_none());
    assert!(h.query_by_label("Vocals band 1").is_some(), "the bands are still there");
}

/// Bands beyond the room of a row are counted at its end instead of left out.
#[test]
fn bands_that_do_not_fit_are_counted() {
    let mut list = Fake::new();
    for b in 0..p::BANDS {
        list.values[p::band_param(b, band::USED) as usize] = 1.0;
        list.values[p::band_param(b, band::FREQ) as usize] = 100.0 * (b + 1) as f64;
        list.values[p::band_param(b, band::GAIN) as usize] = -3.5;
    }
    let mut h = harness(list);
    h.step();
    let shown = (1..=p::BANDS).filter(|b| h.query_by_label(&format!("Vocals band {b}")).is_some()).count();
    assert!(shown < p::BANDS, "this many bands do not fit a row");
    let hidden = p::BANDS - shown;
    let more = h.get_by_label(&format!("Vocals: {hidden} more bands")).rect();
    let last = h.get_by_label(&format!("Vocals band {shown}")).rect();
    assert!(!more.intersects(last), "{more:?} over {last:?}");
}
