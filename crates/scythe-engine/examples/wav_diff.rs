//! Compares two WAV files sample by sample: peak and RMS of the difference, relative to the
//! first file's RMS, plus the offset that aligns them best within ±64 samples.
fn read(path: &str) -> (Vec<f64>, usize) {
    let mut r = hound::WavReader::open(path).unwrap();
    let spec = r.spec();
    let s = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().map(|s| s.unwrap() as f64).collect(),
        hound::SampleFormat::Int => {
            let scale = (1u64 << (spec.bits_per_sample - 1)) as f64;
            r.samples::<i32>().map(|s| s.unwrap() as f64 / scale).collect()
        }
    };
    (s, spec.channels as usize)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (a, ca) = read(&args[0]);
    let (b, cb) = read(&args[1]);
    assert_eq!(ca, cb, "channel counts differ");
    let frames = (a.len() / ca).min(b.len() / cb);
    let diff_at = |off: i64| {
        let (mut peak, mut sum) = (0.0f64, 0.0);
        for f in 64..frames.saturating_sub(64) {
            let g = (f as i64 + off) as usize;
            for c in 0..ca {
                let d = a[f * ca + c] - b[g * ca + c];
                peak = peak.max(d.abs());
                sum += d * d;
            }
        }
        (peak, (sum / (frames * ca) as f64).sqrt())
    };
    let best = (-64..=64).min_by(|x, y| diff_at(*x).1.total_cmp(&diff_at(*y).1)).unwrap();
    let rms_a = (a.iter().map(|s| s * s).sum::<f64>() / a.len() as f64).sqrt();
    let (peak, rms) = diff_at(0);
    let (bpeak, brms) = diff_at(best);
    let db = |v: f64| 20.0 * v.max(1e-12).log10();
    println!("aligned: peak diff {:.1} dBFS, rms diff {:.1} dB below signal", db(peak), db(rms_a) - db(rms));
    if let Some(win) = std::env::var("WINDOW").ok().and_then(|v| v.parse::<usize>().ok()) {
        for start in (0..frames).step_by(win) {
            let end = (start + win).min(frames);
            let (mut s, mut d) = (0.0, 0.0);
            for f in start..end {
                for c in 0..ca {
                    s += a[f * ca + c].powi(2);
                    d += (a[f * ca + c] - b[f * cb + c]).powi(2);
                }
            }
            println!(
                "  {:>8.3}s signal {:>6.1} dB, diff {:>6.1} dB",
                start as f64 / 48000.0,
                10.0 * (s / (end - start) as f64).max(1e-20).log10(),
                10.0 * (d / (end - start) as f64).max(1e-20).log10()
            );
        }
    }
    println!(
        "best offset {best:+}: peak diff {:.1} dBFS, rms diff {:.1} dB below signal",
        db(bpeak),
        db(rms_a) - db(brms)
    );
}
