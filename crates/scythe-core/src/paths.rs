//! Paths as the user sees and types them, and the per-user folders settings and caches live
//! in. The XDG variables win everywhere, so tests can redirect them; Windows falls back to its
//! own folders (`%APPDATA%`, `%LOCALAPPDATA%`), where REAPER keeps its resources too.

use std::path::{Path, PathBuf};

fn var(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// The user's home folder: `$HOME`, on Windows `%USERPROFILE%`.
pub fn home() -> Option<PathBuf> {
    var("HOME").or_else(|| if cfg!(windows) { var("USERPROFILE") } else { None })
}

/// Settings: `$XDG_CONFIG_HOME`, `%APPDATA%` or `~/.config`.
pub fn config_home() -> Option<PathBuf> {
    var("XDG_CONFIG_HOME")
        .or_else(|| if cfg!(windows) { var("APPDATA") } else { None })
        .or_else(|| Some(home()?.join(".config")))
}

/// Data the program keeps: `$XDG_DATA_HOME`, `%LOCALAPPDATA%` or `~/.local/share`.
pub fn data_home() -> Option<PathBuf> {
    var("XDG_DATA_HOME")
        .or_else(|| if cfg!(windows) { var("LOCALAPPDATA") } else { None })
        .or_else(|| Some(home()?.join(".local/share")))
}

/// Caches: `$XDG_CACHE_HOME`, `%LOCALAPPDATA%` or `~/.cache`.
pub fn cache_home() -> Option<PathBuf> {
    var("XDG_CACHE_HOME")
        .or_else(|| if cfg!(windows) { var("LOCALAPPDATA") } else { None })
        .or_else(|| Some(home()?.join(".cache")))
}

/// Where plugins are installed for all users on Windows (`%CommonProgramFiles%\<sub>`) and for
/// one (`%LOCALAPPDATA%\Programs\Common\<sub>`); nothing elsewhere.
pub fn windows_plugin_dirs(sub: &str) -> Vec<PathBuf> {
    if !cfg!(windows) {
        return Vec::new();
    }
    [var("CommonProgramFiles"), var("LOCALAPPDATA").map(|l| l.join("Programs").join("Common"))]
        .into_iter()
        .flatten()
        .map(|d| d.join(sub))
        .collect()
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
