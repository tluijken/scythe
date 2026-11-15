//! EQ Match's reference file, picked by a host that answers a frame or more later.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use egui::{Id, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use scyeq_dsp::probe::FxProbe;
use scyeq_dsp::{Meters, ScytheEq};
use scyeq_ui::{CompareSource, DecodeFn, EditorState, EqHost, Instance, ReferenceAudio, ViewState};

static DECODED: AtomicUsize = AtomicUsize::new(0);

fn decode(path: &Path) -> Result<ReferenceAudio, String> {
    assert_eq!(path, Path::new("/music/reference.wav"));
    DECODED.fetch_add(1, Ordering::SeqCst);
    Ok(ReferenceAudio { channels: vec![vec![0.0; 4800]], sample_rate: 48_000 })
}

struct Later {
    eq: ScytheEq,
    view: ViewState,
    asked: usize,
    /// What `picked_reference` answers once the frames run out.
    answer: Option<PathBuf>,
    frames_to_answer: usize,
}

impl EqHost for Later {
    fn values(&self) -> &[f64] {
        self.eq.values()
    }
    fn begin(&mut self, _: u32) {}
    fn set(&mut self, id: u32, value: f64) {
        self.eq.set(id, value);
    }
    fn end(&mut self, _: u32) {}
    fn edit(&mut self, _: &str, f: &mut dyn FnMut(&mut ScytheEq)) {
        f(&mut self.eq);
    }
    fn meters(&self) -> Option<Arc<Meters>> {
        None
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
    fn source_probe(&self, _: Id) -> Option<Arc<FxProbe>> {
        None
    }
    fn pick_reference(&mut self) -> Option<PathBuf> {
        self.asked += 1;
        self.frames_to_answer = 3;
        None
    }
    fn picked_reference(&mut self) -> Option<PathBuf> {
        if self.asked == 0 {
            return None;
        }
        if self.frames_to_answer > 0 {
            self.frames_to_answer -= 1;
            return None;
        }
        self.answer.take()
    }
    fn decode_reference(&self) -> Option<DecodeFn> {
        Some(decode)
    }
    fn view(&mut self) -> &mut ViewState {
        &mut self.view
    }
}

#[test]
fn a_reference_picked_later_is_measured() {
    let host = Later {
        eq: ScytheEq::new(48_000),
        view: ViewState::default(),
        asked: 0,
        answer: Some(PathBuf::from("/music/reference.wav")),
        frames_to_answer: 0,
    };
    let mut h = Harness::builder()
        .with_size(vec2(1180.0, 680.0))
        .with_step_dt(1.0 / 30.0)
        .with_context_setup(scyeq_ui::theme::install)
        .build_ui_state(
            |ui, (host, state): &mut (Later, EditorState)| scyeq_ui::show(ui, host, state),
            (host, EditorState::default()),
        );
    h.step();
    h.get_by_label("Match").click();
    h.run_steps(2);
    h.get_by_value("Choose…").click();
    h.run_steps(2);
    h.get_by_label("Audio file…").click();
    h.run_steps(2);
    assert_eq!(h.state().0.asked, 1);
    assert!(h.query_by_value("reference.wav").is_none(), "not picked yet");
    h.run_steps(4);
    assert!(h.query_by_value("reference.wav").is_some(), "the file the host picked later");
    for _ in 0..100 {
        if DECODED.load(Ordering::SeqCst) > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(DECODED.load(Ordering::SeqCst), 1, "decoded once");
    assert_eq!(h.state().0.asked, 1);
}

/// Offers one other instance, "Bass", until told to stop.
struct Offers {
    eq: ScytheEq,
    view: ViewState,
    offered: bool,
    forgets: bool,
    /// The sources the editor asked a probe of.
    asked: std::sync::Mutex<Vec<Id>>,
}

fn bass() -> Id {
    Id::unique("bass")
}

impl EqHost for Offers {
    fn values(&self) -> &[f64] {
        self.eq.values()
    }
    fn begin(&mut self, _: u32) {}
    fn set(&mut self, id: u32, value: f64) {
        self.eq.set(id, value);
    }
    fn end(&mut self, _: u32) {}
    fn edit(&mut self, _: &str, f: &mut dyn FnMut(&mut ScytheEq)) {
        f(&mut self.eq);
    }
    fn meters(&self) -> Option<Arc<Meters>> {
        None
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
        let bass = Instance { id: bass(), label: "Bass".to_owned(), color: Some([40, 200, 40]) };
        self.offered.then_some(bass).into_iter().collect()
    }
    fn compare_sources(&self) -> Vec<CompareSource> {
        Vec::new()
    }
    fn source_probe(&self, id: Id) -> Option<Arc<FxProbe>> {
        self.asked.lock().unwrap().push(id);
        None
    }
    fn view(&mut self) -> &mut ViewState {
        &mut self.view
    }
    fn forgets_gone_sources(&self) -> bool {
        self.forgets
    }
}

/// Bass picked as EQ Match reference, then no longer offered: whether the reference is still
/// shown, and whether the editor still asks for Bass's probe.
fn reference_after_bass_goes(forgets: bool) -> (bool, bool) {
    let host = Offers {
        eq: ScytheEq::new(48_000),
        view: ViewState::default(),
        offered: true,
        forgets,
        asked: Default::default(),
    };
    let mut h = Harness::builder()
        .with_size(vec2(1180.0, 680.0))
        .with_step_dt(1.0 / 30.0)
        .with_context_setup(scyeq_ui::theme::install)
        .build_ui_state(
            |ui, (host, state): &mut (Offers, EditorState)| scyeq_ui::show(ui, host, state),
            (host, EditorState::default()),
        );
    h.step();
    h.get_by_label("Match").click();
    h.run_steps(2);
    h.get_by_value("Choose…").click();
    h.run_steps(2);
    h.get_by_label("Bass (ScyEQ-Pro)").click();
    h.run_steps(2);
    assert!(h.query_by_value("Bass (ScyEQ-Pro)").is_some(), "Bass is the reference");
    assert!(h.state().0.asked.lock().unwrap().contains(&bass()));

    h.state_mut().0.offered = false;
    h.run_steps(2);
    h.state().0.asked.lock().unwrap().clear();
    h.run_steps(2);
    let shown = h.query_by_value("Bass (ScyEQ-Pro)").is_some();
    assert_eq!(h.query_by_value("Choose…").is_some(), !shown);
    (shown, h.state().0.asked.lock().unwrap().contains(&bass()))
}

#[test]
fn a_host_that_forgets_gone_sources_drops_the_reference() {
    assert_eq!(reference_after_bass_goes(true), (false, false));
}

#[test]
fn a_gone_source_stays_selected_by_default() {
    assert_eq!(reference_after_bass_goes(false), (true, true));
}

/// How many dots of `color` the last frame drew.
fn dots(h: &Harness<(Offers, EditorState)>, [r, g, b]: [u8; 3]) -> usize {
    fn count(shape: &egui::Shape, color: egui::Color32) -> usize {
        match shape {
            egui::Shape::Circle(c) => (c.fill == color) as usize,
            egui::Shape::Vec(shapes) => shapes.iter().map(|s| count(s, color)).sum(),
            _ => 0,
        }
    }
    h.output().shapes.iter().map(|s| count(&s.shape, egui::Color32::from_rgb(r, g, b))).sum()
}

/// The reference chosen shows its track's colour, as the compare choice beside it does.
#[test]
fn the_chosen_reference_shows_its_colour() {
    let host = Offers {
        eq: ScytheEq::new(48_000),
        view: ViewState::default(),
        offered: true,
        forgets: false,
        asked: Default::default(),
    };
    let mut h = Harness::builder()
        .with_size(vec2(1180.0, 680.0))
        .with_step_dt(1.0 / 30.0)
        .with_context_setup(scyeq_ui::theme::install)
        .build_ui_state(
            |ui, (host, state): &mut (Offers, EditorState)| scyeq_ui::show(ui, host, state),
            (host, EditorState::default()),
        );
    h.step();
    h.get_by_label("Match").click();
    h.run_steps(2);
    h.get_by_value("Choose…").click();
    h.run_steps(2);
    h.get_by_label("Bass (ScyEQ-Pro)").click();
    h.run_steps(2);
    assert!(h.query_by_label("Compare Bass (ScyEQ-Pro)").is_some(), "compared too");
    assert_eq!(dots(&h, [40, 200, 40]), 2, "on the compare choice and the reference");
}

fn decode_by_name(path: &Path) -> Result<ReferenceAudio, String> {
    match path.file_name().and_then(|n| n.to_str()) {
        Some("good.wav") => Ok(ReferenceAudio { channels: vec![vec![0.1; 4800]], sample_rate: 48_000 }),
        _ => Err("unsupported feature: core (probe): no suitable format reader found".to_owned()),
    }
}

/// Answers each pick at once with the next file, and offers one other instance, "Bass".
struct Picks {
    eq: ScytheEq,
    view: ViewState,
    files: Vec<PathBuf>,
}

impl EqHost for Picks {
    fn values(&self) -> &[f64] {
        self.eq.values()
    }
    fn begin(&mut self, _: u32) {}
    fn set(&mut self, id: u32, value: f64) {
        self.eq.set(id, value);
    }
    fn end(&mut self, _: u32) {}
    fn edit(&mut self, _: &str, f: &mut dyn FnMut(&mut ScytheEq)) {
        f(&mut self.eq);
    }
    fn meters(&self) -> Option<Arc<Meters>> {
        None
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
        vec![Instance { id: bass(), label: "Bass".to_owned(), color: None }]
    }
    fn compare_sources(&self) -> Vec<CompareSource> {
        Vec::new()
    }
    fn source_probe(&self, _: Id) -> Option<Arc<FxProbe>> {
        None
    }
    fn pick_reference(&mut self) -> Option<PathBuf> {
        (!self.files.is_empty()).then(|| self.files.remove(0))
    }
    fn decode_reference(&self) -> Option<DecodeFn> {
        Some(decode_by_name)
    }
    fn view(&mut self) -> &mut ViewState {
        &mut self.view
    }
}

fn picks(files: &[&str]) -> Harness<'static, (Picks, EditorState)> {
    let host = Picks {
        eq: ScytheEq::new(48_000),
        view: ViewState::default(),
        files: files.iter().map(|f| PathBuf::from("/music").join(f)).collect(),
    };
    let mut h = Harness::builder()
        .with_size(vec2(1180.0, 680.0))
        .with_step_dt(1.0 / 30.0)
        .with_context_setup(scyeq_ui::theme::install)
        .build_ui_state(
            |ui, (host, state): &mut (Picks, EditorState)| scyeq_ui::show(ui, host, state),
            (host, EditorState::default()),
        );
    h.step();
    h.get_by_label("Match").click();
    h.run_steps(2);
    h
}

/// Picks the next file through the reference menu, then waits for its measurement.
fn pick_next(h: &mut Harness<'static, (Picks, EditorState)>, current: &str) {
    h.get_by_value(current).click();
    h.run_steps(2);
    h.get_by_label("Audio file…").click();
    for _ in 0..200 {
        h.run_steps(1);
        if h.query_by_label_contains("Measuring the file").is_none() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the file was never measured");
}

const RAW: &str = "unsupported feature: core (probe): no suitable format reader found";

#[test]
fn a_file_measured_after_one_that_failed_clears_the_error() {
    let mut h = picks(&["notes.txt", "good.wav"]);
    pick_next(&mut h, "Choose…");
    assert!(h.query_by_label_contains(RAW).is_some(), "the failed file's error");
    pick_next(&mut h, "notes.txt");
    assert!(h.query_by_value("good.wav").is_some());
    assert!(h.query_by_label_contains(RAW).is_none(), "the error stays after a good file");
}

#[test]
fn choosing_an_instance_after_a_failed_file_clears_the_error() {
    let mut h = picks(&["notes.txt"]);
    pick_next(&mut h, "Choose…");
    assert!(h.query_by_label_contains(RAW).is_some());
    h.get_by_value("notes.txt").click();
    h.run_steps(2);
    h.get_by_label("Bass (ScyEQ-Pro)").click();
    h.run_steps(2);
    assert!(h.query_by_label_contains(RAW).is_none(), "the error stays with an instance as reference");
}
