//! Plays a project on the silent null device in real time and reports the audio callback load
//! (render time over the block period), to check that heavy projects keep up without sound.
use std::sync::Arc;
use std::time::Duration;

use scythe_core::rpp::{read_project, resolve_media_path};
use scythe_engine::{Engine, MediaPool};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("project");
    let start: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let project = read_project(path.as_ref()).unwrap();
    let pool = Arc::new(MediaPool::default());
    let mut engine = Engine::start_null(pool.clone(), 48_000, None);
    for take in project.tracks.iter().flat_map(|t| &t.items).flat_map(|i| &i.takes) {
        if let Some(f) = take.source.file() {
            let _ = pool.load(&resolve_media_path(&project, f), 48_000);
        }
    }
    engine.sync(&project);
    engine.seek(start);
    engine.play();
    let mut peak = 0.0f32;
    let mut sum = 0.0f32;
    let samples = 50;
    for _ in 0..samples {
        std::thread::sleep(Duration::from_millis(100));
        let load = engine.transport().load();
        peak = peak.max(load);
        sum += load;
    }
    engine.stop();
    println!(
        "playing from {start} s: average load {:.0} %, peak {:.0} % (of one 512-frame block period)",
        sum / samples as f32 * 100.0,
        peak * 100.0
    );
    std::thread::sleep(Duration::from_millis(3500));
    let idle = engine.transport().load();
    println!("stopped for 3.5 s: load {:.0} %", idle * 100.0);
}
