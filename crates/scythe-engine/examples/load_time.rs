//! Times loading every media file of a project into a pool, at a given engine rate.
//! cargo run --release -p scythe-engine --example load_time -- <project.RPP> [rate]
//! It reads and fills the media cache as the app does; `SCYTHE_MEDIA_CACHE=0` times it without.
use std::time::Instant;

use rayon::prelude::*;
use scythe_core::rpp::{read_project, resolve_media_path};
use scythe_engine::MediaPool;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("project");
    let rate: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(48_000);
    let project = read_project(path.as_ref()).unwrap();
    let mut files: Vec<_> = project
        .tracks
        .iter()
        .flat_map(|t| &t.items)
        .flat_map(|i| &i.takes)
        .filter_map(|t| t.source.file().map(|f| resolve_media_path(&project, f)))
        .collect();
    files.sort();
    files.dedup();
    let pool = MediaPool::with_resample(scythe_core::render_settings::ResampleMode::PLAYBACK)
        .with_disk_cache(scythe_engine::media_cache::DiskCache::for_user());
    let start = Instant::now();
    let mut times: Vec<_> = files
        .par_iter()
        .map(|f| {
            let t = Instant::now();
            let r = pool.load(f, rate);
            (t.elapsed().as_secs_f64(), f.clone(), r.err().map(|e| e.to_string()))
        })
        .collect();
    let total = start.elapsed().as_secs_f64();
    times.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (t, f, e) in times.iter().take(8) {
        println!("{t:7.2} s  {}  {}", f.display(), e.as_deref().unwrap_or(""));
    }
    let failed: Vec<_> = times.iter().filter(|t| t.2.is_some()).collect();
    for (_, f, e) in &failed {
        println!("FAILED {}: {}", f.display(), e.as_deref().unwrap_or(""));
    }
    println!("{} files at {rate} Hz in {total:.2} s ({} failed)", files.len(), failed.len());
}
