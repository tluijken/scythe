//! Times loading media files the way playback does: decode, conversion to the engine rate with
//! the playback resampler, and peaks, per file and for all of them at once.
use std::path::PathBuf;
use std::time::Instant;

use scythe_core::render_settings::ResampleMode;
use scythe_engine::MediaPool;
use scythe_engine::media::{Peaks, decode_file, resample_with};

fn main() {
    let files: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    let mode = ResampleMode::PLAYBACK;
    for path in &files {
        let t = Instant::now();
        let raw = decode_file(path).unwrap();
        let decoded = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let audio = resample_with(&raw, 48_000, mode).unwrap();
        let resampled = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let _ = Peaks::compute(&audio, Peaks::DEFAULT_RESOLUTION);
        let peaks = t.elapsed().as_secs_f64();
        println!(
            "{:>7.1} s audio  decode {decoded:>6.3} s ({:>5.0}x)  resample {resampled:>6.3} s  peaks {peaks:>6.3} s  {}",
            raw.duration(),
            raw.duration() / decoded,
            path.file_name().unwrap().to_string_lossy()
        );
    }
    let pool = std::sync::Arc::new(MediaPool::with_resample(mode));
    let t = Instant::now();
    std::thread::scope(|s| {
        for path in &files {
            let pool = pool.clone();
            s.spawn(move || pool.load(path, 48_000).unwrap());
        }
    });
    println!("all {} files on one thread each: {:.3} s", files.len(), t.elapsed().as_secs_f64());
}
