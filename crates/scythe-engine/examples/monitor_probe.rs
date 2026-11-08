//! Opens the output and the live input the way monitoring does, with an empty project (so
//! nothing is audible), and reports whether the input stream could be opened.
use std::sync::Arc;

use scythe_engine::{Engine, MediaPool};

fn main() {
    let mut engine = Engine::start(Arc::new(MediaPool::default()));
    println!("output: {} @ {} Hz", engine.info().name, engine.sample_rate());
    match engine.set_monitoring(true) {
        Ok(()) => println!("input stream open"),
        Err(e) => println!("input unavailable: {e}"),
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    engine.set_monitoring(false).unwrap();
    println!("input stream closed");
}
