//! Prints the left channel of WAV files side by side for a frame range.
fn read(path: &str) -> Vec<f64> {
    let mut r = hound::WavReader::open(path).unwrap();
    let spec = r.spec();
    let ch = spec.channels as usize;
    let s: Vec<f64> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().map(|s| s.unwrap() as f64).collect(),
        hound::SampleFormat::Int => {
            let scale = (1u64 << (spec.bits_per_sample - 1)) as f64;
            r.samples::<i32>().map(|s| s.unwrap() as f64 / scale).collect()
        }
    };
    s.chunks(ch).map(|c| c[0]).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (from, to): (usize, usize) = (args[0].parse().unwrap(), args[1].parse().unwrap());
    let files: Vec<Vec<f64>> = args[2..].iter().map(|p| read(p)).collect();
    for f in from..to {
        let row: Vec<String> =
            files.iter().map(|s| format!("{:>10.6}", s.get(f).copied().unwrap_or(f64::NAN))).collect();
        println!("{f:>7} {}", row.join(" "));
    }
}
