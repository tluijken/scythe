//! What plugin scans found per binary or bundle, kept between runs so only new or changed
//! ones are loaded again. Failures are kept too, so a plugin that hangs or crashes the scan is
//! not tried again until it changes or the cache is cleared.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// `$XDG_CACHE_HOME/scythe/<name>.json`.
pub fn default_path(name: &str) -> Option<PathBuf> {
    let base = scythe_core::paths::cache_home()?;
    Some(base.join("scythe").join(format!("{name}.json")))
}

/// What identifies a binary or bundle's version. Nix store files all carry the same
/// modification time, so the resolved path (which holds the store hash) counts too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    modified: u64,
    size: u64,
    target: PathBuf,
}

fn file_stamp(meta: &std::fs::Metadata) -> (u64, u64) {
    let modified = meta.modified().ok().and_then(|m| m.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
    (modified as u64, meta.len())
}

/// The stamp of a file, or of a folder from the files directly inside it (an LV2 bundle).
pub fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    let target = scythe_core::paths::canonical(path).unwrap_or_else(|_| path.to_owned());
    let (mut modified, mut size) = file_stamp(&meta);
    if meta.is_dir() {
        for entry in std::fs::read_dir(path).ok()?.flatten() {
            if let Ok(meta) = entry.metadata() {
                let (m, s) = file_stamp(&meta);
                modified = modified.max(m);
                size = size.wrapping_add(s);
            }
        }
    }
    Some(Stamp { modified, size, target })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound(deserialize = "T: DeserializeOwned"))]
struct Entry<T> {
    stamp: Stamp,
    found: Vec<T>,
    #[serde(default)]
    failed: Option<String>,
}

/// One binary or bundle's scan: its plugins, or why it could not be scanned.
pub type Outcome<T> = Result<Vec<T>, String>;

pub struct ScanCache<T> {
    path: Option<PathBuf>,
    old: HashMap<PathBuf, Entry<T>>,
    new: HashMap<PathBuf, Entry<T>>,
}

impl<T: Clone + Serialize + DeserializeOwned> ScanCache<T> {
    /// Reads the cache at `path`; a missing or unreadable file (or an older layout) starts
    /// empty, and `None` keeps nothing between runs.
    pub fn open(path: Option<PathBuf>) -> Self {
        let old = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self { path, old, new: HashMap::new() }
    }

    /// A cache at `path` that starts empty, for a scan that checks everything anew.
    pub fn empty(path: Option<PathBuf>) -> Self {
        Self { path, old: HashMap::new(), new: HashMap::new() }
    }

    /// Everything the cache holds, changed or not: what to show until a scan has checked.
    pub fn all(&self) -> impl Iterator<Item = &T> {
        self.old.values().flat_map(|e| e.found.iter())
    }

    /// What `file` held when it last had `stamp`, if it is unchanged since.
    pub fn lookup(&mut self, file: &Path, stamp: &Stamp) -> Option<Outcome<T>> {
        let entry = self.old.remove(file).filter(|e| e.stamp == *stamp)?;
        let outcome = match &entry.failed {
            Some(reason) => Err(reason.clone()),
            None => Ok(entry.found.clone()),
        };
        self.new.insert(file.to_owned(), entry);
        Some(outcome)
    }

    pub fn insert(&mut self, file: &Path, stamp: Stamp, outcome: &Outcome<T>) {
        let (found, failed) = match outcome {
            Ok(found) => (found.clone(), None),
            Err(reason) => (Vec::new(), Some(reason.clone())),
        };
        self.new.insert(file.to_owned(), Entry { stamp, found, failed });
    }

    /// Writes what this scan has seen so far; binaries it has not seen are forgotten.
    pub fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec(&self.new) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(tmp, path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reuses_unchanged_binaries_and_failures() {
        let dir = std::env::temp_dir().join(format!("scythe-scan-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (bin, bad, cache) = (dir.join("a.so"), dir.join("b.so"), dir.join("cache.json"));
        std::fs::write(&bin, b"one").unwrap();
        std::fs::write(&bad, b"hangs").unwrap();

        let mut c = ScanCache::<String>::open(Some(cache.clone()));
        let s = stamp(&bin).unwrap();
        assert_eq!(c.lookup(&bin, &s), None);
        c.insert(&bin, s, &Ok(vec!["first".into()]));
        c.insert(&bad, stamp(&bad).unwrap(), &Err("timed out".into()));
        c.save();

        let mut c = ScanCache::<String>::open(Some(cache.clone()));
        assert_eq!(c.all().count(), 1);
        assert_eq!(c.lookup(&bin, &stamp(&bin).unwrap()), Some(Ok(vec!["first".to_owned()])));
        assert_eq!(c.lookup(&bad, &stamp(&bad).unwrap()), Some(Err("timed out".to_owned())));
        c.save();

        std::fs::write(&bin, b"changed").unwrap();
        let mut c = ScanCache::<String>::open(Some(cache.clone()));
        assert_eq!(c.lookup(&bin, &stamp(&bin).unwrap()), None);

        std::fs::write(&cache, br#"{"/x.so":[1,2,["old layout"]]}"#).unwrap();
        assert_eq!(ScanCache::<String>::open(Some(cache)).all().count(), 0);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_bundle_changes_with_its_files() {
        let dir = std::env::temp_dir().join(format!("scythe-scan-stamp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.ttl"), b"a").unwrap();
        let before = stamp(&dir).unwrap();
        std::fs::write(dir.join("plugin.ttl"), b"more").unwrap();
        assert_ne!(stamp(&dir).unwrap(), before);
        std::fs::remove_dir_all(dir).ok();
    }
}
