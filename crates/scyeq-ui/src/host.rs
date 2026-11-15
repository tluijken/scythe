//! What ScyEQ-Pro's editor needs from whoever runs the EQ: Scythe, or a plugin host through
//! the plugin build.

use std::path::{Path, PathBuf};
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
    /// Asks for an audio file to match to; `None` when none was picked, or when the answer comes
    /// later through [`EqHost::picked_reference`].
    fn pick_reference(&mut self) -> Option<PathBuf> {
        None
    }
    /// A file picked since [`EqHost::pick_reference`] returned without one; asked every frame
    /// while EQ Match is open.
    fn picked_reference(&mut self) -> Option<PathBuf> {
        None
    }
    /// Decodes a reference file for EQ Match, on a thread of its own. `None` when files cannot
    /// be matched to.
    fn decode_reference(&self) -> Option<DecodeFn> {
        None
    }
    /// The instance list opens while `instances` is set: Scythe shows one for the whole project,
    /// a plugin shows [`crate::instances_window`] with it.
    fn view(&mut self) -> &mut ViewState;
    /// Whether a compare or EQ Match source the host no longer offers is let go; otherwise it
    /// stays selected.
    fn forgets_gone_sources(&self) -> bool {
        false
    }
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
    /// The colour of the instance's track, when the host gave one.
    pub color: Option<[u8; 3]>,
}

/// A signal to compare and match with.
#[derive(Clone, Debug, PartialEq)]
pub struct CompareSource {
    pub id: Id,
    pub label: String,
    pub color: Option<[u8; 3]>,
}

/// A decoded reference file: planar channels (one or two).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReferenceAudio {
    pub channels: Vec<Vec<f32>>,
    pub sample_rate: u32,
}

pub type DecodeFn = fn(&Path) -> Result<ReferenceAudio, String>;

/// Every ScyEQ-Pro the host runs, for the instance list. `id`s are [`Instance::id`]s.
pub trait InstanceList {
    /// Every instance, in the order to list them.
    fn instance_list(&self) -> Vec<Instance>;
    /// An instance's parameter values; `None` leaves it out of the list.
    fn instance_values(&self, id: Id) -> Option<Vec<f64>>;
    /// A gesture on an instance's parameter, as [`EqHost::begin`], [`EqHost::set`] and
    /// [`EqHost::end`] are on the editor's own.
    fn begin_instance(&mut self, id: Id, param: u32);
    fn set_instance(&mut self, id: Id, param: u32, value: f64);
    fn end_instance(&mut self, id: Id, param: u32);
    /// Opening and bypassing the instances, where the list offers them; a plugin can do neither
    /// for another plugin.
    fn controls(&mut self) -> Option<&mut dyn InstanceControls> {
        None
    }
    /// How the list's window is framed.
    fn window(&self) -> WindowFn {
        egui_window
    }
}

/// Opening and bypassing the instances of an [`InstanceList`].
pub trait InstanceControls {
    /// Whether the instance is on (not bypassed).
    fn instance_enabled(&self, id: Id) -> bool;
    fn toggle_instance_bypass(&mut self, id: Id);
    /// Whether the instance's editor is open.
    fn instance_open(&self, id: Id) -> bool;
    fn toggle_instance_open(&mut self, id: Id);
}

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
