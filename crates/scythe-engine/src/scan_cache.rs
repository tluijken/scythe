//! What plugin scans found per binary, kept between runs so only new or changed binaries are
//! loaded again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// `$XDG_CACHE_HOME/scythe/<name>.json`.
pub fn default_path(name: &str) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("scythe").join(format!("{name}.json")))
}

/// Modification time (ns) and size: a binary with both unchanged is taken to be the same.
fn stamp(file: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(file).ok()?;
    let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos() as u64;
    Some((modified, meta.len()))
}

type Entries<T> = HashMap<PathBuf, (u64, u64, Vec<T>)>;

pub struct ScanCache<T> {
    path: Option<PathBuf>,
    old: Entries<T>,
    new: Entries<T>,
    loaded: usize,
}

impl<T: Clone + Serialize + DeserializeOwned> ScanCache<T> {
    /// Reads the cache at `path`; a missing or unreadable file starts empty, and `None` keeps
    /// nothing between runs.
    pub fn open(path: Option<PathBuf>) -> Self {
        let old = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self { path, old, new: HashMap::new(), loaded: 0 }
    }

    /// What `file` holds: from the cache when it has not changed, otherwise from `scan`.
    pub fn get(&mut self, file: &Path, scan: impl FnOnce() -> Vec<T>) -> Vec<T> {
        let stamp = stamp(file);
        let found = match (stamp, self.old.remove(file)) {
            (Some((m, s)), Some((cm, cs, found))) if (m, s) == (cm, cs) => found,
            _ => {
                self.loaded += 1;
                scan()
            }
        };
        if let Some((m, s)) = stamp {
            self.new.insert(file.to_owned(), (m, s, found.clone()));
        }
        found
    }

    /// Binaries that had to be loaded, rather than taken from the cache.
    pub fn loaded(&self) -> usize {
        self.loaded
    }

    /// Writes what this scan saw; binaries it did not see are forgotten.
    pub fn save(self) {
        let Some(path) = self.path else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec(&self.new) {
            let _ = std::fs::write(path, json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reuses_unchanged_binaries() {
        let dir = std::env::temp_dir().join(format!("scythe-scan-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (bin, cache) = (dir.join("a.so"), dir.join("cache.json"));
        std::fs::write(&bin, b"one").unwrap();

        let mut c = ScanCache::<String>::open(Some(cache.clone()));
        assert_eq!(c.get(&bin, || vec!["first".into()]), ["first"]);
        assert_eq!(c.loaded(), 1);
        c.save();

        let mut c = ScanCache::<String>::open(Some(cache.clone()));
        assert_eq!(c.get(&bin, || vec!["again".into()]), ["first"]);
        assert_eq!(c.loaded(), 0);
        c.save();

        std::fs::write(&bin, b"changed").unwrap();
        let mut c = ScanCache::<String>::open(Some(cache));
        assert_eq!(c.get(&bin, || vec!["new".into()]), ["new"]);
        std::fs::remove_dir_all(dir).ok();
    }
}
