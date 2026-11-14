//! ScyEQ-Pro, Scythe's EQ, as a CLAP and VST3 plugin: the same DSP and the same state as in
//! Scythe, so a project sounds the same in either.

pub mod params;
mod plugin;

pub use plugin::{ScyEqPlugin, ScyEqShared};
pub use plugin_format;

plugin_format::export_clap!(ScyEqPlugin);
plugin_format::export_vst3!(ScyEqPlugin);
