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
