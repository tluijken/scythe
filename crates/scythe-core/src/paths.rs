//! Paths as the user sees and types them.

use std::path::{Path, PathBuf};

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
