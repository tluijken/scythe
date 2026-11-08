//! Renders projects that REAPER already rendered (with `-renderproject`) and compares the two
//! outputs sample by sample. The REAPER file is the project's own render target.
//!
//! cargo run -p scythe-engine --example null_check -- [--tolerance 0.001] a.RPP b.RPP …

use std::path::Path;

use scythe_core::render_settings::RenderSettings;
use scythe_engine::MediaPool;
use scythe_engine::bounce::{BitDepth, Encoding, Finish, bounce_finished};

fn read(path: &Path) -> Option<Vec<(f32, f32)>> {
    let mut r = hound::WavReader::open(path).ok()?;
    let spec = r.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().map(Result::unwrap).collect(),
        hound::SampleFormat::Int => {
            let scale = (1u64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>().map(|s| s.unwrap() as f32 / scale).collect()
        }
    };
    Some(samples.chunks(spec.channels as usize).map(|c| (c[0], *c.get(1).unwrap_or(&c[0]))).collect())
}

fn main() {
    let mut tolerance = 1e-3f32;
    let mut files = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--tolerance" {
            tolerance = args.next().and_then(|v| v.parse().ok()).expect("--tolerance <value>");
        } else {
            files.push(a);
        }
    }
    let (mut passed, mut total) = (0, 0);
    for file in files {
        let path = Path::new(&file);
        let project = match scythe_core::rpp::read_project(path) {
            Ok(p) => p,
            Err(e) => {
                println!("{file}: {e}");
                continue;
            }
        };
        let settings = RenderSettings::from_project(&project);
        let rate = settings.sample_rate.or(project.sample_rate).unwrap_or(48_000);
        let Ok(jobs) = settings.jobs(&project, 0) else { continue };
        let pool = MediaPool::default();
        for t in &project.tracks {
            for take in t.items.iter().flat_map(|i| &i.takes) {
                if let Some(f) = take.source.file() {
                    let _ = pool.load(&scythe_core::rpp::resolve_media_path(&project, f), rate);
                }
            }
        }
        pool.prepare_project(&project);
        for (range, reaper_out) in jobs {
            let Some(theirs) = read(&reaper_out) else {
                println!("{file}: no REAPER render at {}", reaper_out.display());
                continue;
            };
            let ours_path = reaper_out.with_extension("scythe.wav");
            // Dither is noise and cannot null; normalisation is compared.
            let finish = Finish { dither: false, noise_shape: false, ..Finish::from_settings(&settings) };
            let wav = Encoding::Wav(BitDepth::Float32);
            bounce_finished(&project, &pool, range, rate, 2, wav, &ours_path, finish, |_| {}).unwrap();
            let ours = read(&ours_path).unwrap();
            let (diff, at) = ours
                .iter()
                .zip(&theirs)
                .enumerate()
                .map(|(i, (a, b))| ((a.0 - b.0).abs().max((a.1 - b.1).abs()), i))
                .fold((0.0f32, 0), |m, v| if v.0 > m.0 { v } else { m });
            total += 1;
            let name = path.file_stem().map_or(file.clone(), |s| s.to_string_lossy().into_owned());
            if diff <= tolerance {
                passed += 1;
            } else {
                println!("{name:<40} DIFFERS max |diff| {diff:.5} at {:.4} s", at as f64 / rate as f64);
            }
        }
    }
    println!("{passed}/{total} within {tolerance}");
}
