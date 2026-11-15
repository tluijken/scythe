//! Where a host finds a plugin cargo built: the library itself, or on macOS a bundle made from it the
//! way `scripts/bundle.sh` lays one out.

use std::path::PathBuf;

mod library;

pub use library::library;

pub struct Built {
    /// What a host is given as the plugin's path: the library, or on macOS the bundle.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code, reason = "a VST3 host needs it on macOS only"))]
    pub path: PathBuf,
    /// The library a host loads.
    pub binary: PathBuf,
}

/// The library `name` cargo built, in a bundle with `extension` next to the test on macOS. Makes
/// the bundle anew, so call it once per process, before the library is loaded.
pub fn plugin(name: &str, extension: &str) -> Built {
    let library = library(name);
    #[cfg(target_os = "macos")]
    {
        let dir = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
        let path = dir.join(format!("{name}.{extension}"));
        let binary = bundle(&library, &path, name);
        Built { path, binary }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = extension;
        Built { path: library.clone(), binary: library }
    }
}

#[cfg(target_os = "macos")]
fn bundle(library: &std::path::Path, path: &std::path::Path, name: &str) -> PathBuf {
    // A new file rather than one written over: macOS keeps a file's code signature by its vnode and
    // kills a process that loads a signed file changed in place.
    let _ = std::fs::remove_dir_all(path);
    let macos = path.join("Contents/MacOS");
    std::fs::create_dir_all(&macos).unwrap();
    let binary = macos.join(name);
    std::fs::copy(library, &binary).unwrap();
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>{name}</string>
    <key>CFBundleIdentifier</key>
    <string>test.scyeq-plugin.{name}</string>
    <key>CFBundlePackageType</key>
    <string>BNDL</string>
</dict>
</plist>
"#
    );
    std::fs::write(path.join("Contents/Info.plist"), plist).unwrap();
    let signed = std::process::Command::new("codesign").args(["--force", "--sign", "-"]).arg(path).status().unwrap();
    assert!(signed.success(), "codesign failed for {}", path.display());
    binary
}
