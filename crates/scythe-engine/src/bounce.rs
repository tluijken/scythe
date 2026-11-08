use std::path::Path;

use scythe_core::{Project, TimeRange};

use crate::fx::FxRegistry;
use crate::media::MediaPool;
use crate::render::{MeterRegistry, RenderGraph, RenderScratch};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitDepth {
    Int16,
    Int24,
    Float32,
}

#[derive(Debug, thiserror::Error)]
pub enum BounceError {
    #[error(transparent)]
    Wav(#[from] hound::Error),
    #[error("nothing to render: the range is empty")]
    EmptyRange,
}

/// Renders `range` of the project to a stereo WAV file, faster than real time. Media must
/// already be loaded into `pool`.
pub fn bounce_to_wav(
    project: &Project,
    pool: &MediaPool,
    range: TimeRange,
    sample_rate: u32,
    depth: BitDepth,
    path: &Path,
    mut progress: impl FnMut(f32),
) -> Result<(), BounceError> {
    if range.is_empty() {
        return Err(BounceError::EmptyRange);
    }
    let mut fx = FxRegistry::new(sample_rate);
    fx.project_dir = project.path.as_deref().and_then(Path::parent).map(Into::into);
    let graph = RenderGraph::build(project, pool, &mut MeterRegistry::default(), &mut fx, sample_rate);
    let (bits, format) = match depth {
        BitDepth::Int16 => (16, hound::SampleFormat::Int),
        BitDepth::Int24 => (24, hound::SampleFormat::Int),
        BitDepth::Float32 => (32, hound::SampleFormat::Float),
    };
    let spec = hound::WavSpec { channels: 2, sample_rate, bits_per_sample: bits, sample_format: format };
    let mut writer = hound::WavWriter::create(path, spec)?;

    let start = (range.start * sample_rate as f64).round() as i64;
    let total = (range.length() * sample_rate as f64).round() as i64;
    let mut scratch = RenderScratch::new(project.tracks.len());
    let mut l = vec![0.0f32; RenderScratch::MAX_BLOCK];
    let mut r = vec![0.0f32; RenderScratch::MAX_BLOCK];
    let mut done = 0i64;
    while done < total {
        let n = ((total - done) as usize).min(l.len());
        graph.render(start + done, &mut l[..n], &mut r[..n], &mut scratch);
        for i in 0..n {
            for s in [l[i], r[i]] {
                match depth {
                    BitDepth::Float32 => writer.write_sample(s)?,
                    BitDepth::Int16 => writer.write_sample(quantize(s, 16) as i16)?,
                    BitDepth::Int24 => writer.write_sample(quantize(s, 24))?,
                }
            }
        }
        done += n as i64;
        progress(done as f32 / total as f32);
    }
    writer.finalize()?;
    Ok(())
}

fn quantize(s: f32, bits: u32) -> i32 {
    let max = ((1i64 << (bits - 1)) - 1) as f32;
    (s.clamp(-1.0, 1.0) * max).round() as i32
}

/// Renders `range` and encodes it to MP3 with the `lame` command-line encoder (constant bitrate).
/// The intermediate WAV is removed afterwards.
pub fn bounce_to_mp3(
    project: &Project,
    pool: &MediaPool,
    range: TimeRange,
    sample_rate: u32,
    kbps: u32,
    path: &Path,
    progress: impl FnMut(f32),
) -> Result<(), String> {
    let wav = path.with_extension("scythe-render.wav");
    bounce_to_wav(project, pool, range, sample_rate, BitDepth::Int24, &wav, progress).map_err(|e| e.to_string())?;
    let status = std::process::Command::new("lame")
        .args(["--quiet", "--cbr", "-b", &kbps.to_string()])
        .arg(&wav)
        .arg(path)
        .status();
    let _ = std::fs::remove_file(&wav);
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("lame failed ({s})")),
        Err(e) => Err(format!("MP3 needs the `lame` encoder on PATH: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::decode_file;
    use scythe_core::{Item, Take};

    #[test]
    fn bounce_renders_project_audio() {
        let dir = std::env::temp_dir().join(format!("scythe-bounce-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 8000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(&src, spec).unwrap();
        for _ in 0..8000 {
            w.write_sample(0.5f32).unwrap();
        }
        w.finalize().unwrap();

        let mut p = Project::default();
        let t = p.add_track("t");
        p.track_mut(t).unwrap().pan = -1.0;
        let mut item = Item::new(0.5, 0.5, Take::from_file(src.clone()));
        item.fade_in = scythe_core::Fade::default();
        item.fade_out = scythe_core::Fade::default();
        p.track_mut(t).unwrap().items.push(item);

        let pool = MediaPool::default();
        pool.load(&src, 8000).unwrap();
        let out = dir.join("out.wav");
        let mut last = 0.0;
        bounce_to_wav(&p, &pool, TimeRange::new(0.0, 1.0), 8000, BitDepth::Float32, &out, |f| last = f).unwrap();
        assert_eq!(last, 1.0);

        let data = decode_file(&out).unwrap();
        assert_eq!(data.frames(), 8000);
        assert_eq!(data.channels[0][3999], 0.0);
        assert!((data.channels[0][4000] - 0.5).abs() < 1e-6);
        assert_eq!(data.channels[1][4000], 0.0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn quantize_clips() {
        assert_eq!(quantize(2.0, 16), 32767);
        assert_eq!(quantize(-1.0, 16), -32767);
        assert_eq!(quantize(0.0, 24), 0);
    }
}
