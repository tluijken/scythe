//! Prints peak and RMS level per channel of WAV files.
fn main() {
    for path in std::env::args().skip(1) {
        let mut r = hound::WavReader::open(&path).unwrap();
        let spec = r.spec();
        let scale = if spec.sample_format == hound::SampleFormat::Float {
            1.0
        } else {
            (1u64 << (spec.bits_per_sample - 1)) as f64
        };
        let samples: Vec<f64> = match spec.sample_format {
            hound::SampleFormat::Float => r.samples::<f32>().map(|s| s.unwrap() as f64).collect(),
            hound::SampleFormat::Int => r.samples::<i32>().map(|s| s.unwrap() as f64 / scale).collect(),
        };
        let ch = spec.channels as usize;
        for c in 0..ch {
            let it = samples.iter().skip(c).step_by(ch);
            let (mut peak, mut sum, mut n) = (0.0f64, 0.0, 0usize);
            for &s in it {
                peak = peak.max(s.abs());
                sum += s * s;
                n += 1;
            }
            let db = |v: f64| 20.0 * v.max(1e-12).log10();
            println!(
                "{path} ch{c}: peak {:.1} dBFS, rms {:.1} dBFS, {:.3} s at {} Hz",
                db(peak),
                db((sum / n as f64).sqrt()),
                n as f64 / spec.sample_rate as f64,
                spec.sample_rate
            );
        }
    }
}
