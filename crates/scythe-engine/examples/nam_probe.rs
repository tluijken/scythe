//! Loads NAM models, checks their structure and runs a second of a 440 Hz sine at -18 dBFS
//! through each, printing output level, the model's stated loudness and the processing speed.
use std::time::Instant;

use scythe_engine::nam::NamModel;

fn main() {
    let (mut ok, mut failed) = (0, 0);
    let verbose = std::env::var_os("VERBOSE").is_some();
    let sweep = std::env::var_os("SWEEP").is_some();
    for path in std::env::args().skip(1) {
        match NamModel::load(path.as_ref()) {
            Ok(mut m) => {
                ok += 1;
                if !verbose {
                    continue;
                }
                if sweep {
                    let sr = m.sample_rate.unwrap_or(48000.0) as f32;
                    let amp = 10f32.powf(-30.0 / 20.0) * std::f32::consts::SQRT_2;
                    let levels: Vec<String> = [100.0f32, 300.0, 1000.0, 3000.0, 6000.0, 10000.0]
                        .iter()
                        .map(|&f| {
                            m.reset();
                            let n = (sr * 0.5) as usize;
                            let mut out: Vec<f32> =
                                (0..n).map(|i| amp * (std::f32::consts::TAU * f * i as f32 / sr).sin()).collect();
                            m.process(&mut out);
                            // Level of the fundamental only (single-bin DFT over the second half).
                            let (mut re, mut im) = (0.0f64, 0.0f64);
                            for (i, v) in out.iter().enumerate().skip(n / 2) {
                                let w = std::f64::consts::TAU * f as f64 * i as f64 / sr as f64;
                                re += *v as f64 * w.cos();
                                im += *v as f64 * w.sin();
                            }
                            let mag = 2.0 * (re * re + im * im).sqrt() / (n / 2) as f64 / amp as f64;
                            format!("{f:>6.0} Hz {:>6.1} dB", 20.0 * mag.max(1e-9).log10())
                        })
                        .collect();
                    println!("{path}\n  {}", levels.join(" | "));
                    continue;
                }
                let sr = m.sample_rate.unwrap_or(48000.0);
                let n = sr as usize;
                let amp = 10f32.powf(-18.0 / 20.0) * std::f32::consts::SQRT_2;
                let t = Instant::now();
                let mut out: Vec<f32> =
                    (0..n).map(|i| amp * (std::f32::consts::TAU * 440.0 * i as f32 / sr as f32).sin()).collect();
                m.process(&mut out);
                let secs = t.elapsed().as_secs_f64();
                let tail = &out[n / 2..];
                let rms = (tail.iter().map(|v| v * v).sum::<f32>() / tail.len() as f32).sqrt();
                let finite = out.iter().all(|v| v.is_finite());
                println!(
                    "{:>6.1} dB out (model loudness {:?}), {:.2}x realtime, finite {finite}: {path}",
                    20.0 * rms.max(1e-9).log10(),
                    m.loudness.map(|l| (l * 10.0).round() / 10.0),
                    1.0 / secs
                );
            }
            Err(e) => {
                failed += 1;
                println!("FAILED {path}: {e}");
            }
        }
    }
    println!("{ok} loaded, {failed} failed");
}
