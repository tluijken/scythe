//! Opens the default input for half a second and reports how many frames arrived. The file is
//! deleted straight away; only the count is printed.
use scythe_core::TrackId;
use scythe_engine::record::{RecordTarget, Recording};

fn main() {
    let path = std::env::temp_dir().join("scythe-record-probe.wav");
    let target = RecordTarget { track: TrackId(1), channels: vec![0], path: path.clone() };
    match Recording::start(vec![target], 48000) {
        Ok(rec) => {
            println!("device: {} @ {} Hz", rec.device, rec.sample_rate);
            std::thread::sleep(std::time::Duration::from_millis(500));
            let takes = rec.finish().expect("finish");
            println!("frames: {}", takes[0].frames);
        }
        Err(e) => println!("error: {e}"),
    }
    let _ = std::fs::remove_file(&path);
}
