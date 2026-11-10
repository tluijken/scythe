//! Robustness: every project of a corpus opens in the window, draws, plays a moment and stops
//! without a panic. The corpus is `SCYTHE_ITEST_CORPUS`, folders separated by `:` searched
//! for `.RPP` files (`~/music` when unset), and `SCYTHE_ITEST_PROBES`, folders whose
//! sub-folders hold probe projects (`<dir>/*/*.RPP`). Nothing in either is written to.

use std::panic::{AssertUnwindSafe, catch_unwind};

use super::*;

fn dirs(var: &str, default: impl FnOnce() -> Vec<PathBuf>) -> Vec<PathBuf> {
    match std::env::var_os(var) {
        Some(v) => std::env::split_paths(&v).filter(|p| !p.as_os_str().is_empty()).collect(),
        None => default(),
    }
}

fn find_projects(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && depth > 0 {
            find_projects(&path, depth - 1, out);
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("rpp")) {
            out.push(path);
        }
    }
}

/// Opens `path`, draws the arrange view, the mixer, the setlist and monitor pages, plays a
/// few frames and stops.
fn exercise(name: &str, path: &Path) {
    let mut t = TestApp::open(name, path);
    t.frames(4);
    t.app().toggle_play();
    t.frames(6);
    t.app().stop();
    for page in ["Setlist (L)", "Monitor mixes (in-ear)", "Arrange"] {
        if t.has(page) {
            t.click(page);
        }
        t.frames(3);
    }
    assert!(!t.app().session.dirty, "only looking at {} changed it", path.display());
}

#[test]
fn every_corpus_project_opens_draws_and_plays() {
    let home = || std::env::var_os("HOME").map(|h| vec![PathBuf::from(h).join("music")]).unwrap_or_default();
    let mut files = Vec::new();
    for dir in dirs("SCYTHE_ITEST_CORPUS", home) {
        find_projects(&dir, 8, &mut files);
    }
    open_all("corpus", files);
}

#[test]
fn every_probe_project_opens_draws_and_plays() {
    let mut files = Vec::new();
    for dir in dirs("SCYTHE_ITEST_PROBES", Vec::new) {
        find_projects(&dir, 1, &mut files);
    }
    open_all("probes", files);
}

fn open_all(what: &str, mut files: Vec<PathBuf>) {
    files.sort();
    if files.is_empty() {
        eprintln!("no {what} projects found (see the module documentation): nothing to open");
        return;
    }
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(6);
    let failures: Vec<String> = std::thread::scope(|s| {
        let chunks: Vec<_> = files.chunks(files.len().div_ceil(threads)).collect();
        let handles: Vec<_> = chunks
            .into_iter()
            .enumerate()
            .map(|(c, chunk)| {
                s.spawn(move || {
                    let mut failed = Vec::new();
                    for (k, path) in chunk.iter().enumerate() {
                        if let Err(e) = catch_unwind(AssertUnwindSafe(|| exercise(&format!("{what}/{c}-{k}"), path))) {
                            let why = e
                                .downcast_ref::<String>()
                                .cloned()
                                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                                .unwrap_or_default();
                            failed.push(format!("{}: {why}", path.display()));
                        }
                    }
                    failed
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    eprintln!("{} {what} projects, {} failed", files.len(), failures.len());
    assert!(failures.is_empty(), "{} of {} projects failed:\n{}", failures.len(), files.len(), failures.join("\n"));
}
