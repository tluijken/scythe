//! Compiles every JSFX under the given folders (default: the JSFX search paths) and prints
//! the ones that do not compile, with the reason. `--bench name` instead times ten seconds of
//! stereo audio through one effect.
//!
//! cargo run -p scythe-engine --example jsfx_check [-- dir …] [-- --bench sstillwell/1175]

use std::path::{Path, PathBuf};

use scythe_engine::jsfx::{JsfxEffect, Script, search_paths};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_none_or(|x| x == "jsfx") {
            out.push(p);
        }
    }
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if let Some(name) = raw.iter().position(|a| a == "--bench").and_then(|i| raw.get(i + 1)) {
        use scythe_engine::fx::{Effect, ProcessContext};
        let roots = search_paths();
        let path = scythe_engine::jsfx::find(name, &roots).expect("effect not found");
        let mut fx = JsfxEffect::new(Script::load(&path, &roots).unwrap(), &[], 48000).unwrap();
        let ctx = ProcessContext {
            sample_rate: 48000,
            position: 0,
            playing: true,
            notes: None,
            live: &[],
            live_channel: None,
            bpm: 120.0,
        };
        let (mut l, mut r) = (vec![0.0f32; 512], vec![0.0f32; 512]);
        let started = std::time::Instant::now();
        for b in 0..(10 * 48000 / 512) {
            for (i, (a, c)) in l.iter_mut().zip(r.iter_mut()).enumerate() {
                *a = ((b * 512 + i) as f32 * 0.03).sin() * 0.5;
                *c = *a * 0.7;
            }
            fx.process(&mut l, &mut r, &ctx);
        }
        let t = started.elapsed().as_secs_f64();
        println!("{name}: 10 s of audio in {:.1} ms ({:.0}x realtime)", t * 1000.0, 10.0 / t);
        return;
    }
    let args: Vec<PathBuf> = raw.iter().map(PathBuf::from).collect();
    let roots = if args.is_empty() { search_paths() } else { args };
    let mut files = Vec::new();
    for r in &roots {
        walk(r, &mut files);
    }
    let (mut ok, mut failed) = (0, 0);
    for f in &files {
        let text = std::fs::read(f).unwrap_or_default();
        if !String::from_utf8_lossy(&text).lines().any(|l| l.trim_start().starts_with("desc:")) {
            continue;
        }
        match Script::load(f, &roots).and_then(|s| JsfxEffect::new(s, &[], 48000)) {
            Ok(_) => ok += 1,
            Err(e) => {
                failed += 1;
                println!("{}: {e}", f.display());
            }
        }
    }
    println!("{ok} compile, {failed} do not");
}
