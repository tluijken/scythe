//! ScyEQ-Pro, Scythe's EQ, as a CLAP and VST3 plugin: the same DSP and the same state as in
//! Scythe, so a project sounds the same in either.

mod host;
#[cfg(unix)]
mod owner;
pub mod params;
mod plugin;
pub mod registry;

pub use plugin::{ScyEqPlugin, ScyEqShared};
pub use plugin_format;

/// Keeps the library that holds `code` loaded for good, as the editor does before its file dialog;
/// false when it could not. For the test that loads the built library.
///
/// # Safety
///
/// `code` must be null or point into code of a loaded image.
#[doc(hidden)]
pub unsafe fn pin_library_of(code: *const ()) -> bool {
    unsafe { host::pin(code) }
}

plugin_format::export_clap!(ScyEqPlugin);
plugin_format::export_vst3!(ScyEqPlugin);
