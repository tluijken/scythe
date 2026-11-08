//! Loads `.RPP` files, writes them back in memory and reports every line whose content did not
//! survive. Never writes to disk.
//!
//! cargo run -p scythe-core --example rpp_roundtrip -- path/to/project.RPP …

use std::collections::BTreeMap;

use scythe_core::rpp::{project_from_rpp, project_to_rpp};

/// Normalises a line so that only meaningful differences remain: numbers are compared as f64
/// (REAPER prints more digits than needed) and quote style is ignored.
fn normalize(line: &str) -> String {
    scythe_core::rpp::tokenize(line)
        .into_iter()
        .map(|t| t.parse::<f64>().map(|v| format!("{v:?}")).unwrap_or(t))
        .collect::<Vec<_>>()
        .join(" ")
}

fn line_counts(text: &str) -> BTreeMap<String, i64> {
    let mut counts = BTreeMap::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        *counts.entry(normalize(line)).or_insert(0) += 1;
    }
    counts
}

fn main() {
    let mut failures = 0;
    for path in std::env::args().skip(1) {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                println!("{path}: unreadable: {e}");
                failures += 1;
                continue;
            }
        };
        let project = match project_from_rpp(&text) {
            Ok(p) => p,
            Err(e) => {
                println!("{path}: PARSE ERROR: {e}");
                failures += 1;
                continue;
            }
        };
        let out = project_to_rpp(&project);
        let reparsed = project_from_rpp(&out).expect("own output must parse");
        let stable = project_to_rpp(&reparsed).lines().skip(1).eq(out.lines().skip(1));

        let before = line_counts(&text);
        let after = line_counts(&out);
        let mut lost: BTreeMap<String, i64> = BTreeMap::new();
        for (line, n) in &before {
            let missing = n - after.get(line).copied().unwrap_or(0);
            if missing > 0 {
                let key = line.split_whitespace().next().unwrap_or("").to_owned();
                *lost.entry(key).or_insert(0) += missing;
            }
        }
        let items: usize = project.tracks.iter().map(|t| t.items.len()).sum();
        let fx: usize = project.tracks.iter().map(|t| t.fx.len()).sum();
        println!(
            "{path}: {} tracks, {items} items, {fx} fx, {} markers, {:.0} bpm, stable={stable}, {} of {} lines changed",
            project.tracks.len(),
            project.markers.len(),
            project.tempo.bpm,
            lost.values().sum::<i64>(),
            before.values().sum::<i64>(),
        );
        for (key, n) in &lost {
            println!("    {n:>5} × {key}");
        }
        if !stable {
            failures += 1;
        }
    }
    std::process::exit(if failures > 0 { 1 } else { 0 });
}
