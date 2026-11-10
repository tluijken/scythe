//! Opens the default input through an engine for half a second and reports how many frames
//! arrived. The file is deleted straight away; only the count is printed.
use std::sync::Arc;

use scythe_core::TrackId;
use scythe_engine::record::{RecordTarget, Recording};
use scythe_engine::{Engine, MediaPool};

fn main() {
    let path = std::env::temp_dir().join("scythe-record-probe.wav");
    let target = RecordTarget { track: TrackId(1), channels: vec![0], path: path.clone() };
    let mut engine = Engine::start(Arc::new(MediaPool::default()));
    let started = engine.record_input().and_then(|tap| Recording::start(tap, vec![target]).map_err(|e| e.to_string()));
    match started {
        Ok(rec) => {
            println!("device: {} @ {} Hz", rec.device, rec.sample_rate);
            std::thread::sleep(std::time::Duration::from_millis(500));
            let takes = rec.finish().expect("finish");
            println!("frames: {}, input latency {:.2} ms", takes[0].frames, takes[0].input_latency * 1000.0);
        }
        Err(e) => println!("error: {e}"),
    }
    let _ = std::fs::remove_file(&path);
}
