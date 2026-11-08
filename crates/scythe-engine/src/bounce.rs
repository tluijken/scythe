use std::path::Path;

use scythe_core::render_settings::Format;
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

impl BitDepth {
    pub fn from_bits(bits: u16) -> Self {
        match bits {
            ..=16 => Self::Int16,
            32.. => Self::Float32,
            _ => Self::Int24,
        }
    }
}

/// How a render is written. Everything but WAV goes through a command-line encoder (`lame`,
/// `flac`, `oggenc`) that reads an intermediate WAV.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Encoding {
    Wav(BitDepth),
    Mp3 { kbps: u32 },
    Flac { depth: BitDepth, level: u32 },
    Vorbis { vbr: bool, quality: f32, kbps: u32 },
}

impl Encoding {
    /// The encoding for a project's render format; sinks without an encoder here become
    /// 24-bit WAV.
    pub fn from_format(format: &Format) -> Self {
        match *format {
            Format::Wav { bits } => Self::Wav(BitDepth::from_bits(bits)),
            Format::Mp3 { kbps } => Self::Mp3 { kbps },
            Format::Flac { bits, level } => Self::Flac { depth: BitDepth::from_bits(bits.min(24)), level },
            Format::Vorbis { vbr, quality, kbps } => Self::Vorbis { vbr, quality, kbps },
            Format::Other(_) => Self::Wav(BitDepth::Int24),
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Wav(_) => "wav",
            Self::Mp3 { .. } => "mp3",
            Self::Flac { .. } => "flac",
            Self::Vorbis { .. } => "ogg",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BounceError {
    #[error(transparent)]
    Wav(#[from] hound::Error),
    #[error("nothing to render: the range is empty")]
    EmptyRange,
    #[error("{0}")]
    Encoder(String),
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
    progress: impl FnMut(f32),
) -> Result<(), BounceError> {
    render_wav(project, pool, range, sample_rate, 2, depth, path, progress)
}

/// Renders `range` with `channels` (1 or 2) in `encoding`. A mono render is the average of
/// the master's two channels, as REAPER mixes it down.
#[allow(clippy::too_many_arguments)]
pub fn bounce(
    project: &Project,
    pool: &MediaPool,
    range: TimeRange,
    sample_rate: u32,
    channels: u16,
    encoding: Encoding,
    path: &Path,
    progress: impl FnMut(f32),
) -> Result<(), BounceError> {
    let depth = match encoding {
        Encoding::Wav(depth) => return render_wav(project, pool, range, sample_rate, channels, depth, path, progress),
        Encoding::Flac { depth, .. } => depth,
        Encoding::Mp3 { .. } | Encoding::Vorbis { .. } => BitDepth::Int24,
    };
    let wav = path.with_extension("scythe-render.wav");
    let rendered = render_wav(project, pool, range, sample_rate, channels, depth, &wav, progress);
    let encoded = rendered.and_then(|()| encode(encoding, &wav, path));
    let _ = std::fs::remove_file(&wav);
    encoded
}

fn encode(encoding: Encoding, wav: &Path, out: &Path) -> Result<(), BounceError> {
    let mut cmd = match encoding {
        Encoding::Wav(_) => unreachable!("WAV is written directly"),
        Encoding::Mp3 { kbps } => {
            let mut c = std::process::Command::new("lame");
            c.args(["--quiet", "--cbr", "-b", &kbps.to_string()]).arg(wav).arg(out);
            c
        }
        Encoding::Flac { level, .. } => {
            let mut c = std::process::Command::new("flac");
            c.args(["--silent", "--force", &format!("-{}", level.min(8)), "-o"]).arg(out).arg(wav);
            c
        }
        Encoding::Vorbis { vbr, quality, kbps } => {
            let mut c = std::process::Command::new("oggenc");
            c.arg("--quiet");
            if vbr {
                c.args(["-q", &format!("{:.1}", quality * 10.0)]);
            } else {
                c.args(["--managed", "-b", &kbps.to_string()]);
            }
            c.arg("-o").arg(out).arg(wav);
            c
        }
    };
    let program = cmd.get_program().to_string_lossy().into_owned();
    match cmd.status() {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(BounceError::Encoder(format!("{program} failed ({s})"))),
        Err(e) => Err(BounceError::Encoder(format!(
            "{} renders need the `{program}` encoder on PATH: {e}",
            encoding.extension().to_uppercase()
        ))),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_wav(
    project: &Project,
    pool: &MediaPool,
    range: TimeRange,
    sample_rate: u32,
    channels: u16,
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
    let channels = channels.clamp(1, 2);
    let spec = hound::WavSpec { channels, sample_rate, bits_per_sample: bits, sample_format: format };
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
            let frame = [l[i], r[i]];
            let mono = [0.5 * (l[i] + r[i])];
            for &s in if channels == 1 { &mono[..] } else { &frame[..] } {
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
    fn mono_renders_average_the_channels() {
        let dir = std::env::temp_dir().join(format!("scythe-mono-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 8000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(&src, spec).unwrap();
        for _ in 0..800 {
            w.write_sample(0.5f32).unwrap();
            w.write_sample(0.125f32).unwrap();
        }
        w.finalize().unwrap();
        let mut p = Project::default();
        let t = p.add_track("t");
        let mut item = Item::new(0.0, 0.1, Take::from_file(src.clone()));
        item.fade_in = scythe_core::Fade::default();
        item.fade_out = scythe_core::Fade::default();
        p.track_mut(t).unwrap().items.push(item);
        let pool = MediaPool::default();
        pool.load(&src, 8000).unwrap();
        let out = dir.join("out.wav");
        let encoding = Encoding::Wav(BitDepth::Float32);
        bounce(&p, &pool, TimeRange::new(0.0, 0.1), 8000, 1, encoding, &out, |_| {}).unwrap();
        let data = decode_file(&out).unwrap();
        assert_eq!(data.channels.len(), 1);
        assert!((data.channels[0][400] - 0.3125).abs() < 1e-6, "{}", data.channels[0][400]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn faster_takes_keep_their_pitch_when_asked() {
        let dir = std::env::temp_dir().join(format!("scythe-stretch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("sine.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 8000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(&src, spec).unwrap();
        for i in 0..16000 {
            w.write_sample(0.5 * (std::f32::consts::TAU * 440.0 * i as f32 / 8000.0).sin()).unwrap();
        }
        w.finalize().unwrap();
        let pool = MediaPool::default();
        pool.load(&src, 8000).unwrap();

        let crossings = |preserve: bool| {
            let mut take = Take::from_file(src.clone());
            take.playrate = 2.0;
            take.preserve_pitch = preserve;
            let mut p = Project::default();
            let t = p.add_track("t");
            p.track_mut(t).unwrap().items.push(Item::new(0.0, 1.0, take));
            pool.prepare_project(&p);
            let out = dir.join("out.wav");
            bounce(&p, &pool, TimeRange::new(0.0, 1.0), 8000, 1, Encoding::Wav(BitDepth::Float32), &out, |_| {})
                .unwrap();
            let x = &decode_file(&out).unwrap().channels[0];
            x[2000..6000].windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count() * 2
        };
        let kept = crossings(true);
        let doubled = crossings(false);
        assert!((kept as i32 - 440).abs() <= 4, "{kept} Hz");
        assert!((doubled as i32 - 880).abs() <= 4, "{doubled} Hz");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn quantize_clips() {
        assert_eq!(quantize(2.0, 16), 32767);
        assert_eq!(quantize(-1.0, 16), -32767);
        assert_eq!(quantize(0.0, 24), 0);
    }
}
