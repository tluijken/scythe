//! Browsing for projects: the folders the server was given and what lies under them, only
//! directories and .RPP files, never anything outside.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use scythe_session::api::ApiError;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub dir: bool,
    /// Bytes, for files.
    pub size: Option<u64>,
    /// Seconds since 1970.
    pub modified: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Listing {
    pub roots: Vec<Entry>,
    /// The folder listed, or `None` for the list of roots.
    pub path: Option<String>,
    /// The folder above, while still inside a root.
    pub parent: Option<String>,
    pub entries: Vec<Entry>,
}

fn entry(path: &Path, name: String) -> Option<Entry> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs());
    Some(Entry {
        name,
        path: path.display().to_string(),
        dir: meta.is_dir(),
        size: meta.is_file().then_some(meta.len()),
        modified,
    })
}

fn is_project(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("rpp"))
}

/// The roots that exist, resolved, so a path can be checked against them.
fn resolved(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots.iter().filter_map(|r| scythe_core::paths::canonical(r).ok()).filter(|r| r.is_dir()).collect()
}

/// Lists `path` (the roots when `None`). A path that resolves outside every root, through `..`
/// or a link, is refused.
pub fn list(roots: &[PathBuf], path: Option<&Path>) -> Result<Listing, ApiError> {
    let roots = resolved(roots);
    let root_entries: Vec<Entry> = roots
        .iter()
        .filter_map(|r| entry(r, r.file_name().map_or_else(|| r.display().to_string(), |n| n.to_string_lossy().into())))
        .collect();
    let Some(path) = path else {
        return Ok(Listing { entries: root_entries.clone(), roots: root_entries, path: None, parent: None });
    };
    let outside = || ApiError::forbidden("that folder is outside the folders this server offers");
    let dir = scythe_core::paths::canonical(path).map_err(|_| outside())?;
    let Some(root) = roots.iter().find(|r| dir.starts_with(r)) else { return Err(outside()) };
    if !dir.is_dir() {
        return Err(ApiError::invalid("that is not a folder"));
    }
    let mut entries: Vec<Entry> = std::fs::read_dir(&dir)
        .map_err(|e| ApiError::failed(format!("cannot read {}: {e}", dir.display())))?
        .filter_map(Result::ok)
        .filter_map(|d| {
            let name = d.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            let path = d.path();
            let inside = scythe_core::paths::canonical(&path).ok().is_some_and(|p| p.starts_with(root));
            let wanted = path.is_dir() || is_project(&path);
            (inside && wanted).then(|| entry(&path, name)).flatten()
        })
        .collect();
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    let parent = (dir != *root).then(|| dir.parent().map(|p| p.display().to_string())).flatten();
    Ok(Listing { roots: root_entries, path: Some(dir.display().to_string()), parent, entries })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_stays_inside_the_roots() {
        let base = std::env::temp_dir().join(format!("scythe-files-{}", std::process::id()));
        let root = base.join("live");
        let outside = base.join("private");
        std::fs::create_dir_all(root.join("set")).unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("set").join("Set.RPP"), "<REAPER_PROJECT\n>").unwrap();
        std::fs::write(root.join("notes.txt"), "").unwrap();
        std::fs::write(root.join("b.rpp"), "").unwrap();
        std::fs::write(outside.join("secret.RPP"), "").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
        let roots = vec![root.clone()];

        let top = list(&roots, None).unwrap();
        assert_eq!(top.entries.len(), 1);
        assert_eq!(top.entries[0].name, "live");

        let live = list(&roots, Some(&root)).unwrap();
        let names: Vec<&str> = live.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["set", "b.rpp"]);
        assert_eq!(live.parent, None);
        let set = list(&roots, Some(&root.join("set"))).unwrap();
        assert_eq!(set.entries[0].name, "Set.RPP");
        assert_eq!(set.parent.as_deref(), Some(scythe_core::paths::canonical(&root).unwrap().to_str().unwrap()));

        for path in
            [outside.clone(), root.join(".."), root.join("set/../../private"), root.join("escape"), PathBuf::from("/")]
        {
            let err = list(&roots, Some(&path)).unwrap_err();
            assert_eq!(err.kind, scythe_session::api::ErrorKind::Forbidden, "{}", path.display());
        }
        assert!(list(&roots, Some(&root.join("b.rpp"))).is_err());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
