//! Paths as the user sees and types them, and the per-user folders settings and caches live
//! in. The XDG variables win everywhere, so tests can redirect them; Windows and macOS fall back
//! to their own folders (`%APPDATA%`, `~/Library/Application Support`), where REAPER keeps its
//! resources too.

use std::path::{Path, PathBuf};

fn var(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// The user's home folder: `$HOME`, on Windows `%USERPROFILE%`.
pub fn home() -> Option<PathBuf> {
    var("HOME").or_else(|| if cfg!(windows) { var("USERPROFILE") } else { None })
}

fn mac_library(sub: &str) -> Option<PathBuf> {
    if cfg!(target_os = "macos") { Some(home()?.join("Library").join(sub)) } else { None }
}

/// Settings: `$XDG_CONFIG_HOME`, `%APPDATA%`, `~/Library/Application Support` or `~/.config`.
pub fn config_home() -> Option<PathBuf> {
    var("XDG_CONFIG_HOME")
        .or_else(|| if cfg!(windows) { var("APPDATA") } else { None })
        .or_else(|| mac_library("Application Support"))
        .or_else(|| Some(home()?.join(".config")))
}

/// Data the program keeps: `$XDG_DATA_HOME`, `%LOCALAPPDATA%`, `~/Library/Application Support`
/// or `~/.local/share`.
pub fn data_home() -> Option<PathBuf> {
    var("XDG_DATA_HOME")
        .or_else(|| if cfg!(windows) { var("LOCALAPPDATA") } else { None })
        .or_else(|| mac_library("Application Support"))
        .or_else(|| Some(home()?.join(".local/share")))
}

/// Caches: `$XDG_CACHE_HOME`, `%LOCALAPPDATA%`, `~/Library/Caches` or `~/.cache`.
pub fn cache_home() -> Option<PathBuf> {
    var("XDG_CACHE_HOME")
        .or_else(|| if cfg!(windows) { var("LOCALAPPDATA") } else { None })
        .or_else(|| mac_library("Caches"))
        .or_else(|| Some(home()?.join(".cache")))
}

/// Where plugins are installed for all users and for one: on Windows `%CommonProgramFiles%\<sub>`
/// and `%LOCALAPPDATA%\Programs\Common\<sub>`, on macOS `/Library/Audio/Plug-Ins/<sub>` and
/// `~/Library/Audio/Plug-Ins/<sub>`; nothing elsewhere.
pub fn system_plugin_dirs(sub: &str) -> Vec<PathBuf> {
    let roots = if cfg!(windows) {
        vec![var("CommonProgramFiles"), var("LOCALAPPDATA").map(|l| l.join("Programs").join("Common"))]
    } else if cfg!(target_os = "macos") {
        let plug_ins = Path::new("Library/Audio/Plug-Ins");
        vec![Some(Path::new("/").join(plug_ins)), home().map(|h| h.join(plug_ins))]
    } else {
        Vec::new()
    };
    roots.into_iter().flatten().map(|d| d.join(sub)).collect()
}

/// The executable inside a macOS bundle (`Foo.clap`, `Foo.vst3`, `Foo.vst`):
/// `Contents/MacOS/Foo`, or the only file there when it is named otherwise.
pub fn bundle_executable(bundle: &Path) -> Option<PathBuf> {
    let dir = bundle.join("Contents").join("MacOS");
    let named = dir.join(bundle.file_stem()?);
    if named.is_file() {
        return Some(named);
    }
    let mut files = std::fs::read_dir(&dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_file());
    let only = files.next()?;
    files.next().is_none().then_some(only)
}

/// [`Path::canonicalize`] without Windows' verbatim prefix: `C:\dir` instead of `\\?\C:\dir`,
/// which shows as typed and still takes `/` when joined to.
pub fn canonical(path: &Path) -> std::io::Result<PathBuf> {
    let resolved = path.canonicalize()?;
    #[cfg(windows)]
    if let Some(plain) = resolved.to_str().and_then(|s| s.strip_prefix(r"\\?\"))
        && plain.as_bytes().get(1) == Some(&b':')
    {
        return Ok(PathBuf::from(plain));
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_paths_are_absolute_and_plain() {
        let here = canonical(Path::new(".")).unwrap();
        assert!(here.is_absolute());
        assert!(!here.to_string_lossy().starts_with(r"\\?\"));
        assert_eq!(canonical(&here.join("..")).unwrap(), canonical(here.parent().unwrap()).unwrap());
    }
}
