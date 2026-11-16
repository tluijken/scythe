//! Checks that REAPER reads the states scythe writes for its built-in effects: renders a chain
//! of all of them in REAPER and in scythe and compares.
//!
//! cargo run -p scythe-engine --example builtin_ref -- <work dir> <reaper> <impulse response>

use std::path::PathBuf;

use scythe_core::rpp::{Node, project_from_rpp, project_to_rpp};
use scythe_core::{Item, Project, Take, TimeRange, Track};
use scythe_engine::MediaPool;
use scythe_engine::bounce::{BitDepth, bounce_to_wav};
use scythe_engine::builtin::{Builtin, eq_state, reverb_state};

const SR: u32 = 48000;

/// A built-in with parameters set the way the UI sets them, saved as Scythe saves it.
fn edited(builtin: Builtin, changes: &[(u32, f64)]) -> scythe_core::FxSlot {
    let stock = builtin.stock().expect("a stock plugin");
    let cx = scythe_engine::stock::StockContext { sample_rate: SR, project_dir: None, hardware: None };
    let mut fx = (stock.create)(&builtin.default_state(), &cx).unwrap();
    for &(id, value) in changes {
        fx.set_param(id, value);
    }
    builtin.slot_with(&fx.save_state().unwrap())
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("work dir"));
    let reaper = args.next().expect("reaper");
    let ir = PathBuf::from(args.next().expect("impulse response"));
    let noise = dir.join("builtin_noise.wav");
    let spec =
        hound::WavSpec { channels: 2, sample_rate: SR, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(&noise, spec).unwrap();
    for i in 0..SR as usize * 2 {
        let v = ((i as u64).wrapping_mul(2654435761) % 1000) as f32 / 2000.0 - 0.25;
        w.write_sample(v).unwrap();
        w.write_sample(-v).unwrap();
    }
    w.finalize().unwrap();

    let mut track = Track::new("t");
    track.items.push(Item::new(0.0, 2.0, Take::from_file(noise.clone())));
    track.items[0].fade_in = scythe_core::Fade::new(0.0);
    track.items[0].fade_out = scythe_core::Fade::new(0.0);
    track.fx = vec![
        edited(Builtin::Gate, &[(0, -30.0), (12, -6.0), (14, 5.0)]),
        Builtin::Eq.slot_with(&eq_state(&[(4, 120.0, 1.0, 0.8), (8, 2000.0, 2.0, 1.0), (1, 6000.0, 0.5, 0.8)])),
        edited(Builtin::Compressor, &[(0, -18.0), (14, 6.0), (15, 1.0), (16, 1.0)]),
        Builtin::Delay.slot(),
        Builtin::Reverb.slot_with(&reverb_state(Some(&ir), 0.5, 1.0)),
        edited(Builtin::Multiband, &[(2, -20.0), (14, -20.0), (26, -20.0), (38, -20.0), (16, 6.0)]),
        edited(Builtin::Limiter, &[(0, -6.0), (1, -1.0), (2, 30.0)]),
    ];
    let mut project = Project { sample_rate: Some(SR), ..Project::default() };
    project.tracks.push(track);
    let out = dir.join("builtin.reaper.wav");
    for line in [
        "SELECTION 0 2".to_owned(),
        format!("RENDER_FILE \"{}\"", out.display()),
        "RENDER_PATTERN \"\"".into(),
        format!("RENDER_FMT 0 2 {SR}"),
        "RENDER_RANGE 2 0 0 0 1000".into(),
        "RENDER_STEMS 0".into(),
        "RENDER_DITHER 0".into(),
    ] {
        project.extra.push(Node::Line(line));
    }
    let mut cfg = scythe_core::rpp::Chunk::new("RENDER_CFG", &[]);
    cfg.line(["ZXZhdyAAAQ=="]);
    project.extra.push(Node::Chunk(cfg));
    let text = project_to_rpp(&project);
    let rpp = dir.join("builtin.RPP");
    std::fs::write(&rpp, &text).unwrap();
    let _ = std::fs::remove_file(&out);
    let cfgfile = dir.join("reaper-cfg").join("reaper.ini");
    let _ = scythe_engine::process::command(&reaper)
        .args(["-cfgfile", &cfgfile.display().to_string(), "-nosplash", "-renderproject"])
        .arg(&rpp)
        .status();

    let mut ours_project = project_from_rpp(&text).unwrap();
    ours_project.path = Some(rpp);
    let pool = MediaPool::default();
    pool.load(&noise, SR).unwrap();
    let ours = dir.join("builtin.scythe.wav");
    bounce_to_wav(&ours_project, &pool, TimeRange::new(0.0, 2.0), SR, BitDepth::Float32, &ours, |_| {}).unwrap();
    let read =
        |p: &PathBuf| -> Vec<f32> { hound::WavReader::open(p).unwrap().samples::<f32>().map(Result::unwrap).collect() };
    let (a, b) = (read(&out), read(&ours));
    let rms = |v: &[f32]| (v.iter().map(|s| s * s).sum::<f32>() / v.len().max(1) as f32).sqrt();
    let diff: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x - y).collect();
    println!(
        "REAPER rendered {} samples; signal rms {:.4}, difference {:.1} dB below, max {:.5}",
        a.len(),
        rms(&a),
        20.0 * (rms(&a) / rms(&diff).max(1e-12)).log10(),
        diff.iter().fold(0.0f32, |m, v| m.max(v.abs()))
    );
}
