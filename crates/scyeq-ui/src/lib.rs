//! ScyEQ-Pro's editor, the same in Scythe and in the plugin: drawn with egui from the DSP of
//! `scyeq_dsp`, talking to whoever runs the EQ through [`EqHost`].

pub mod analyzer;
pub mod controls;
pub mod editor;
mod eq_match;
pub mod fonts;
pub mod host;
pub mod model;
pub mod theme;
pub mod widgets;

pub use editor::{EditorState, show};
pub use host::{CompareSource, EqHost, Instance, MeasureFn, Reference, ViewState, WindowFn, WindowSpec};
