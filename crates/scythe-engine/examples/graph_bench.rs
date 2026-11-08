//! Times building the render graph for projects (media is not loaded, so this measures the
//! project-side work: MIDI note resolution, automation, routing).
use std::time::Instant;

use scythe_core::rpp::read_project;
use scythe_engine::MediaPool;
use scythe_engine::render::{MeterRegistry, RenderGraph};

fn main() {
    for path in std::env::args().skip(1) {
        let project = read_project(path.as_ref()).unwrap();
        let pool = MediaPool::default();
        let mut meters = MeterRegistry::default();
        let t = Instant::now();
        let g = RenderGraph::build(&project, &pool, &mut meters, &mut scythe_engine::fx::FxRegistry::new(48000), 48000);
        let items: usize = g.tracks.iter().map(|t| t.items.len()).sum();
        println!("{:>8.2} ms  {items:>4} items  {path}", t.elapsed().as_secs_f64() * 1000.0);
    }
}
