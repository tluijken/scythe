//! What ScyEQ-Pro's editor needs from whoever runs the EQ: Scythe, or a plugin host through
//! the plugin build.

use std::path::Path;
use std::sync::Arc;

use egui::{Id, Ui, Vec2};
use scyeq_dsp::probe::FxProbe;
use scyeq_dsp::{Meters, ScytheEq};

pub trait EqHost {
    /// Every parameter's value, by id.
    fn values(&self) -> &[f64];
    /// A gesture on parameter `id` starts; [`EqHost::set`] calls follow, then [`EqHost::end`].
    fn begin(&mut self, id: u32);
    fn set(&mut self, id: u32, value: f64);
    fn end(&mut self, id: u32);
    /// One undo step for a multi-value edit (add band, EQ Match apply).
    fn edit(&mut self, label: &str, f: &mut dyn FnMut(&mut ScytheEq));
    fn meters(&self) -> Option<Arc<Meters>>;
    /// The running EQ's own probe: its input and output for the analyzer.
    fn probe(&self) -> Option<Arc<FxProbe>>;
    /// Processing latency in samples.
    fn latency(&self) -> usize;
    fn sample_rate(&self) -> f64;
    /// The other ScyEQ-Pro instances, to compare and match with.
    fn instances(&self) -> Vec<Instance>;
    /// Signals other than ScyEQ-Pro instances to compare and match with (Scythe's tracks).
    fn compare_sources(&self) -> Vec<CompareSource>;
    /// The output of an instance or compare source.
    fn source_probe(&self, id: Id) -> Option<Arc<FxProbe>>;
    /// Measures a reference file for EQ Match; it runs on a thread of its own. `None` when
    /// files cannot be matched to.
    fn measure_reference(&self) -> Option<MeasureFn> {
        None
    }
    fn view(&mut self) -> &mut ViewState;
    /// A note shown over the graph, e.g. "made with a newer ScyEQ-Pro".
    fn notice(&self) -> Option<String> {
        None
    }
    /// Whether a parameter's menu offers modulation and learn.
    fn can_modulate(&self) -> bool {
        false
    }
    fn modulate(&mut self, _id: u32) {}
    /// How the editor's own windows (EQ Match) are framed.
    fn window(&self) -> WindowFn {
        egui_window
    }
}

/// Another ScyEQ-Pro instance.
#[derive(Clone, Debug, PartialEq)]
pub struct Instance {
    /// Stays the same from frame to frame while the instance does.
    pub id: Id,
    pub label: String,
}

/// A signal to compare and match with.
#[derive(Clone, Debug, PartialEq)]
pub struct CompareSource {
    pub id: Id,
    pub label: String,
}

/// A reference file's level per frequency band, from low to high.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reference {
    /// Centre frequency (Hz) and level (dB) per band.
    pub bands: Vec<(f64, f64)>,
}

pub type MeasureFn = fn(&Path) -> Result<Reference, String>;

/// What the host keeps for the editor beyond one editor's life.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewState {
    /// The instance list is open.
    pub instances: bool,
}

/// One of the editor's own windows.
pub struct WindowSpec<'a> {
    /// Names the kind of window, for a host that remembers where windows were.
    pub key: &'a str,
    pub id: Id,
    pub title: &'a str,
    pub default_size: Vec2,
}

/// Shows a window with `body` in it while `open`; the close button clears `open`.
pub type WindowFn = fn(&egui::Context, &WindowSpec, &mut bool, &mut dyn FnMut(&mut Ui));

pub fn egui_window(ctx: &egui::Context, spec: &WindowSpec, open: &mut bool, body: &mut dyn FnMut(&mut Ui)) {
    egui::Window::new(spec.title)
        .id(spec.id)
        .open(open)
        .collapsible(false)
        .default_size(spec.default_size)
        .show(ctx, |ui| body(ui));
}
