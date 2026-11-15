//! Finds a library cargo built, for the tests that load the plugin: this crate's and Scythe's.

use std::path::PathBuf;

/// The library `name` cargo built last above the running test: beside it in `deps` with cargo's
/// old build layout, or in the profile directory, where the new layout (each unit in a directory
/// of its own) leaves only `cargo build`'s copy. The newest of those, as a build in one layout
/// leaves its file behind when the other is used later.
pub fn library(name: &str) -> PathBuf {
    let file = format!("{}{name}{}", std::env::consts::DLL_PREFIX, std::env::consts::DLL_SUFFIX);
    let exe = std::env::current_exe().unwrap();
    exe.ancestors()
        .skip(1)
        .map(|dir| dir.join(&file))
        .filter_map(|library| Some((std::fs::metadata(&library).ok()?.modified().ok()?, library)))
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, library)| library)
        .unwrap_or_else(|| panic!("{file} not built next to or above {}: cargo build -p scyeq-plugin", exe.display()))
}
