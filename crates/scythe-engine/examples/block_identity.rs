//! Renders a project at 128-frame blocks and at larger ones and says where they differ: whether
//! its effects give the same audio whatever the block size, as anticipative FX relies on to
//! render ahead in larger chunks than the device block.
//!
//! cargo run --release -p scythe-engine --example block_identity -- x.RPP
use scythe_core::rpp::{read_project, resolve_media_path};
use scythe_engine::MediaPool;
use scythe_engine::fx::FxRegistry;
use scythe_engine::render::{MeterRegistry, RenderGraph, RenderScratch};
use std::sync::Arc;

fn render(
    project: &scythe_core::Project,
    pool: &MediaPool,
    block: usize,
    frames: usize,
    start: i64,
) -> Vec<(f32, f32)> {
    let mut fx = FxRegistry::new(44_100);
    fx.project_dir = project.path.as_deref().and_then(std::path::Path::parent).map(Into::into);
    let g = RenderGraph::build(project, pool, &mut MeterRegistry::default(), &mut fx, 44_100);
    let mut s = RenderScratch::new(g.tracks.len() + 8);
    let mut out = Vec::new();
    let (mut l, mut r) = (vec![0.0; block], vec![0.0; block]);
    let mut pos = start;
    while out.len() < frames {
        g.render(pos, &mut l, &mut r, &mut s);
        out.extend(l.iter().copied().zip(r.iter().copied()));
        pos += block as i64;
    }
    out.truncate(frames);
    out
}

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let project = read_project(path.as_ref()).unwrap();
    let pool = Arc::new(MediaPool::default());
    for take in project.tracks.iter().flat_map(|t| &t.items).flat_map(|i| &i.takes) {
        if let Some(f) = take.source.file() {
            let _ = pool.load(&resolve_media_path(&project, f), 44_100);
        }
    }
    pool.prepare_project(&project);
    let frames = 44_100 * 8;
    let a = render(&project, &pool, 128, frames, 44_100 * 30);
    for block in [512, 384, 1024] {
        let b = render(&project, &pool, block, frames, 44_100 * 30);
        let diffs = a.iter().zip(&b).filter(|(x, y)| x != y).count();
        let worst = a.iter().zip(&b).map(|(x, y)| (x.0 - y.0).abs().max((x.1 - y.1).abs())).fold(0.0f32, f32::max);
        let first = a.iter().zip(&b).position(|(x, y)| x != y);
        println!("128 vs {block}: {diffs} differing frames of {frames}, worst {worst:e}, first {first:?}");
    }
}
