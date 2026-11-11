//! Renders a project block by block as fast as it can and reports the block render times
//! (p50/p99/max against the block period), the realtime factor and which tracks and effects
//! take the time, like REAPER's performance meter.
//!
//! cargo run --release -p scythe-engine --example perf_probe -- x.RPP [--rate 44100]
//!     [--block 512] [--seconds 20] [--start 30] [--paced]
//!
//! `--paced` waits out each block period between blocks, as a device callback does, so the
//! track threads go to sleep between blocks and have to be woken.
use std::sync::Arc;
use std::time::{Duration, Instant};

use scythe_core::rpp::{read_project, resolve_media_path};
use scythe_engine::MediaPool;
use scythe_engine::fx::{FxOwner, FxRegistry};
use scythe_engine::perf::{FxCpu, chain_cpu};
use scythe_engine::render::{MeterRegistry, RenderGraph, RenderScratch};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("project");
    let (mut rate, mut block, mut seconds, mut start, mut paced) = (44_100u32, 512usize, 20.0f64, 30.0f64, false);
    while let Some(a) = args.next() {
        let mut v = || args.next().expect("value");
        match a.as_str() {
            "--rate" => rate = v().parse().unwrap(),
            "--block" => block = v().parse().unwrap(),
            "--seconds" => seconds = v().parse().unwrap(),
            "--start" => start = v().parse().unwrap(),
            "--paced" => paced = true,
            other => panic!("unknown argument {other}"),
        }
    }
    // Render on the track threads the audio callback uses.
    scythe_engine::realtime::mark_audio_thread();
    let project = read_project(path.as_ref()).unwrap();
    let pool = Arc::new(MediaPool::default());
    let t = Instant::now();
    for take in project.tracks.iter().flat_map(|t| &t.items).flat_map(|i| &i.takes) {
        if let Some(f) = take.source.file() {
            let _ = pool.load(&resolve_media_path(&project, f), rate);
        }
    }
    pool.prepare_project(&project);
    eprintln!("media loaded in {:.1} s", t.elapsed().as_secs_f64());
    let mut meters = MeterRegistry::default();
    let mut fx = FxRegistry::new(rate);
    fx.project_dir = project.path.as_deref().and_then(std::path::Path::parent).map(Into::into);
    let t = Instant::now();
    let graph = RenderGraph::build(&project, &pool, &mut meters, &mut fx, rate);
    eprintln!("graph built in {:.1} s", t.elapsed().as_secs_f64());
    let mut scratch = RenderScratch::new(graph.tracks.len() + 8);
    let (mut l, mut r) = (vec![0.0f32; block], vec![0.0f32; block]);
    let mut pos = (start * rate as f64) as i64;
    let blocks = (seconds * rate as f64 / block as f64) as usize;
    let period = Duration::from_secs_f64(block as f64 / rate as f64);
    // Warm up: first blocks allocate model state, fault in pages.
    for _ in 0..20 {
        graph.render(pos, &mut l, &mut r, &mut scratch);
        pos += block as i64;
    }
    let track_cpu0: Vec<u64> = graph.tracks.iter().map(|t| t.meter.cpu.total()).collect();
    let chains: Vec<Vec<FxCpu>> = graph
        .tracks
        .iter()
        .map(|t| project.track(t.id).map_or_else(Vec::new, |p| chain_cpu(&fx, &FxOwner::Track(t.id), &p.fx)))
        .collect();
    let fx_cpu0: Vec<Vec<u64>> = chains.iter().map(|c| c.iter().map(|f| f.instance.cpu.total()).collect()).collect();
    let mut times = Vec::with_capacity(blocks);
    let wall = Instant::now();
    let mut next = Instant::now();
    for _ in 0..blocks {
        let t = Instant::now();
        graph.render(pos, &mut l, &mut r, &mut scratch);
        times.push(t.elapsed());
        pos += block as i64;
        if paced {
            next += period;
            if let Some(wait) = next.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            } else {
                next = Instant::now();
            }
        }
    }
    let busy: Duration = times.iter().sum();
    let wall = wall.elapsed();
    times.sort();
    let q = |p: f64| times[((times.len() - 1) as f64 * p) as usize];
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let budget = period.as_secs_f64() * 1000.0;
    println!(
        "{rate} Hz, {block} frames ({budget:.2} ms): p50 {:.3} ms, p90 {:.3}, p99 {:.3}, max {:.3}; over budget {} of {}; realtime factor {:.1}x",
        ms(q(0.5)),
        ms(q(0.9)),
        ms(q(0.99)),
        ms(*times.last().unwrap()),
        times.iter().filter(|t| ms(**t) > budget).count(),
        times.len(),
        seconds / busy.as_secs_f64(),
    );
    let audio = seconds;
    let mut rows: Vec<(f64, String)> = Vec::new();
    let mut sum = 0.0;
    for (i, t) in graph.tracks.iter().enumerate() {
        let name = project.track(t.id).map_or(String::new(), |p| p.name.clone());
        let secs = (t.meter.cpu.total() - track_cpu0[i]) as f64 / 1e9;
        sum += secs;
        let fxs: Vec<String> = chains[i]
            .iter()
            .zip(&fx_cpu0[i])
            .map(|(f, &c0)| format!("{} {:.1}%", f.name, (f.instance.cpu.total() - c0) as f64 / 1e9 / audio * 100.0))
            .collect();
        rows.push((secs, format!("{:>6.2}%  {name:<20} {}", secs / audio * 100.0, fxs.join(", "))));
    }
    println!(
        "tracks (own time incl. fx, % of one core in real time): total {:.1}%, wall {:.1}%",
        sum / audio * 100.0,
        wall.as_secs_f64() / audio * 100.0
    );
    rows.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (_, row) in rows.iter().take(25) {
        println!("{row}");
    }
}
