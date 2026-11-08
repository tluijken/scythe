use std::path::Path;

use scythe_core::render_settings::{CdImage, Format, Normalize, NormalizeSource, RenderSettings};
use scythe_core::{Project, TimeRange};

use crate::fx::FxRegistry;
use crate::loudness::LoudnessScan;
use crate::media::MediaPool;
use crate::render::{MeterRegistry, RenderGraph, RenderScratch};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitDepth {
    Int16,
    Int24,
    Int32,
    Float32,
}

impl BitDepth {
    /// REAPER's WAV bit depths: 32 is float, 33 is 32-bit integer.
    pub fn from_bits(bits: u16) -> Self {
        match bits {
            ..=16 => Self::Int16,
            33 => Self::Int32,
            32.. => Self::Float32,
            _ => Self::Int24,
        }
    }
}

/// How a render is written. Everything but WAV goes through a command-line encoder (`lame`,
/// `flac`, `oggenc`, `opusenc`, `wavpack`) that reads an intermediate WAV; CD images and DDP
/// filesets are laid out from a 16-bit WAV by [`crate::disc`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Encoding {
    Wav(BitDepth),
    Mp3 { kbps: u32 },
    Flac { depth: BitDepth, level: u32 },
    Vorbis { vbr: bool, quality: f32, kbps: u32 },
    Opus { kbps: f32, mode: u8, complexity: u8 },
    WavPack { depth: BitDepth, mode: u32 },
    CdImage(CdImage),
    Ddp,
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
            Format::Opus { kbps, mode, complexity } => Self::Opus { kbps, mode, complexity },
            Format::WavPack { bits, float, mode } => {
                let depth = match (float, bits) {
                    (true, _) => BitDepth::Float32,
                    (false, 32) => BitDepth::Int32,
                    (false, bits) => BitDepth::from_bits(bits.min(24)),
                };
                Self::WavPack { depth, mode }
            }
            Format::CdImage(cd) => Self::CdImage(cd),
            Format::Ddp => Self::Ddp,
            Format::Other(_) => Self::Wav(BitDepth::Int24),
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Wav(_) => "wav",
            Self::Mp3 { .. } => "mp3",
            Self::Flac { .. } => "flac",
            Self::Vorbis { .. } => "ogg",
            Self::Opus { .. } => "opus",
            Self::WavPack { .. } => "wv",
            Self::CdImage(_) => "cue",
            Self::Ddp => "DAT",
        }
    }

    /// The sample rate this encoding is always written at, whatever the project asks for.
    pub fn sample_rate(self) -> Option<u32> {
        matches!(self, Self::CdImage(_) | Self::Ddp).then_some(crate::disc::RATE)
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
    #[error(transparent)]
    Io(#[from] std::io::Error),
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
    render_wav(project, pool, range, sample_rate, 2, depth, path, Finish::default(), progress)
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
    bounce_finished(project, pool, range, sample_rate, channels, encoding, path, Finish::default(), progress)
}

/// Like [`bounce`], normalising and dithering as `finish` says.
#[allow(clippy::too_many_arguments)]
pub fn bounce_finished(
    project: &Project,
    pool: &MediaPool,
    range: TimeRange,
    sample_rate: u32,
    channels: u16,
    encoding: Encoding,
    path: &Path,
    finish: Finish,
    progress: impl FnMut(f32),
) -> Result<(), BounceError> {
    let depth = match encoding {
        Encoding::Wav(depth) => {
            return render_wav(project, pool, range, sample_rate, channels, depth, path, finish, progress);
        }
        Encoding::Flac { depth, .. } | Encoding::WavPack { depth, .. } => depth,
        Encoding::Mp3 { .. } | Encoding::Vorbis { .. } | Encoding::Opus { .. } => BitDepth::Int24,
        Encoding::CdImage(_) | Encoding::Ddp => BitDepth::Int16,
    };
    if encoding.sample_rate().is_some_and(|r| r != sample_rate) {
        return Err(BounceError::Encoder(format!(
            "{} renders are written at {} Hz",
            encoding.extension().to_uppercase(),
            crate::disc::RATE
        )));
    }
    let wav = path.with_extension("scythe-render.wav");
    let encoded = match encoding {
        Encoding::CdImage(cd) => render_wav(project, pool, range, sample_rate, 2, depth, &wav, finish, progress)
            .and_then(|()| Ok(crate::disc::write_cd_image(&project.markers, range, &wav, &cd, path)?)),
        Encoding::Ddp => render_wav(project, pool, range, sample_rate, 2, depth, &wav, finish, progress)
            .and_then(|()| Ok(crate::disc::write_ddp(&project.markers, &wav, path)?)),
        _ => render_wav(project, pool, range, sample_rate, channels, depth, &wav, finish, progress)
            .and_then(|()| encode(encoding, channels, &wav, path)),
    };
    let _ = std::fs::remove_file(&wav);
    encoded
}

fn encode(encoding: Encoding, channels: u16, wav: &Path, out: &Path) -> Result<(), BounceError> {
    let mut cmd = match encoding {
        Encoding::Wav(_) | Encoding::CdImage(_) | Encoding::Ddp => unreachable!("written without an encoder"),
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
        Encoding::Opus { kbps, mode, complexity } => {
            // REAPER's bitrate is per channel.
            let total = kbps * channels.max(1) as f32;
            let mode = ["--vbr", "--cvbr", "--hard-cbr"][mode.min(2) as usize];
            let mut c = std::process::Command::new("opusenc");
            c.args(["--quiet", mode, "--bitrate", &format!("{total}"), "--comp", &complexity.to_string()]);
            c.arg(wav).arg(out);
            c
        }
        Encoding::WavPack { mode, .. } => {
            let mut c = std::process::Command::new("wavpack");
            c.args(["-q", "-y"]);
            match mode {
                1 | 2 => {
                    c.arg("-f");
                }
                3 => {
                    c.arg("-h");
                }
                4 => {
                    c.arg("-hh");
                }
                _ => {}
            }
            c.arg(wav).arg("-o").arg(out);
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

/// What happens to a render after the mix: normalisation, then dither when the samples are
/// stored as integers.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Finish {
    pub normalize: Option<Normalize>,
    pub dither: bool,
    pub noise_shape: bool,
}

impl Finish {
    pub fn from_settings(s: &RenderSettings) -> Self {
        Self { normalize: s.normalize, dither: s.dither, noise_shape: s.noise_shape }
    }
}

/// Renders the mix of `range` block by block into `sink`.
fn render_mix(
    project: &Project,
    pool: &MediaPool,
    range: TimeRange,
    sample_rate: u32,
    mut sink: impl FnMut(&[f32], &[f32], &[Vec<f32>]) -> Result<(), BounceError>,
    mut progress: impl FnMut(f32),
) -> Result<(), BounceError> {
    if range.is_empty() {
        return Err(BounceError::EmptyRange);
    }
    let mut fx = FxRegistry::new(sample_rate);
    fx.project_dir = project.path.as_deref().and_then(Path::parent).map(Into::into);
    let graph = RenderGraph::build(project, pool, &mut MeterRegistry::default(), &mut fx, sample_rate);
    let start = (range.start * sample_rate as f64).round() as i64;
    let total = (range.length() * sample_rate as f64).round() as i64;
    let mut scratch = RenderScratch::new(project.tracks.len());
    let mut l = vec![0.0f32; RenderScratch::MAX_BLOCK];
    let mut r = vec![0.0f32; RenderScratch::MAX_BLOCK];
    // Master channels 3 and up, for multichannel renders.
    let mut extra = vec![vec![0.0f32; RenderScratch::MAX_BLOCK]; graph.master_channels - 2];
    let mut done = 0i64;
    // Pre-roll through the plugin latency so lookahead effects have heard the first frames.
    let mut pre = graph.latency();
    while pre > 0 {
        let n = (pre as usize).min(l.len());
        graph.render(start - pre, &mut l[..n], &mut r[..n], &mut scratch);
        pre -= n as i64;
    }
    while done < total {
        let n = ((total - done) as usize).min(l.len());
        graph.render(start + done, &mut l[..n], &mut r[..n], &mut scratch);
        graph.master_extra(n, &mut extra);
        sink(&l[..n], &r[..n], &extra)?;
        done += n as i64;
        progress(done as f32 / total as f32);
    }
    Ok(())
}

/// TPDF dither of one LSB, optionally with first-order noise shaping that pushes the error
/// towards high frequencies.
struct Dither {
    rng: u64,
    shape: bool,
    error: [f32; 64],
}

impl Dither {
    fn uniform(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 40) as f32 / (1u64 << 24) as f32
    }

    fn quantize(&mut self, s: f32, channel: usize, bits: u32) -> i32 {
        let scale = (1i64 << (bits - 1)) as f32;
        let v = s * scale - if self.shape { self.error[channel] } else { 0.0 };
        let noise = self.uniform() - self.uniform();
        let q = (v + noise + 0.5).floor().clamp(-scale, scale - 1.0);
        self.error[channel] = q - v;
        q as i32
    }
}

/// Writes rendered blocks with a gain, mixing down to mono when asked.
struct SampleWriter {
    writer: hound::WavWriter<std::io::BufWriter<std::fs::File>>,
    depth: BitDepth,
    channels: u16,
    gain: f32,
    dither: Option<Dither>,
}

impl SampleWriter {
    fn create(
        path: &Path,
        sample_rate: u32,
        channels: u16,
        depth: BitDepth,
        finish: Finish,
    ) -> Result<Self, BounceError> {
        let (bits, format) = match depth {
            BitDepth::Int16 => (16, hound::SampleFormat::Int),
            BitDepth::Int24 => (24, hound::SampleFormat::Int),
            BitDepth::Int32 => (32, hound::SampleFormat::Int),
            BitDepth::Float32 => (32, hound::SampleFormat::Float),
        };
        let channels = channels.clamp(1, 64);
        let spec = hound::WavSpec { channels, sample_rate, bits_per_sample: bits, sample_format: format };
        let dither = (finish.dither && matches!(depth, BitDepth::Int16 | BitDepth::Int24)).then_some(Dither {
            rng: 0x9e37_79b9_7f4a_7c15,
            shape: finish.noise_shape,
            error: [0.0; 64],
        });
        Ok(Self { writer: hound::WavWriter::create(path, spec)?, depth, channels, gain: 1.0, dither })
    }

    /// Writes a block: the master's first pair, and `extra` channels for renders with more.
    fn write(&mut self, l: &[f32], r: &[f32], extra: &[Vec<f32>]) -> Result<(), BounceError> {
        for i in 0..l.len() {
            let sample = |c: usize| match c {
                0 => l[i],
                1 => r[i],
                c => extra.get(c - 2).map_or(0.0, |b| b[i]),
            };
            let mono = [0.5 * (l[i] + r[i]) * self.gain];
            let mut frame = [0.0f32; 64];
            for (c, f) in frame.iter_mut().enumerate().take(self.channels as usize) {
                *f = sample(c) * self.gain;
            }
            let samples = if self.channels == 1 { &mono[..] } else { &frame[..self.channels as usize] };
            for (c, &s) in samples.iter().enumerate() {
                match (self.depth, &mut self.dither) {
                    (BitDepth::Float32, _) => self.writer.write_sample(s)?,
                    (BitDepth::Int16, Some(d)) => self.writer.write_sample(d.quantize(s, c, 16) as i16)?,
                    (BitDepth::Int24, Some(d)) => self.writer.write_sample(d.quantize(s, c, 24))?,
                    (BitDepth::Int16, None) => self.writer.write_sample(quantize(s, 16) as i16)?,
                    (BitDepth::Int24, None) => self.writer.write_sample(quantize(s, 24))?,
                    (BitDepth::Int32, _) => self.writer.write_sample(quantize(s, 32))?,
                }
            }
        }
        Ok(())
    }
}

/// Levels of a render, for normalising it.
struct Measure {
    peak: f32,
    true_peak: f32,
    sum_squares: f64,
    samples: u64,
    loudness: LoudnessScan,
    /// The last few samples per channel, for the oversampled true peak.
    history: [[f32; 8]; 2],
}

impl Measure {
    fn new(sample_rate: u32) -> Self {
        Self {
            peak: 0.0,
            true_peak: 0.0,
            sum_squares: 0.0,
            samples: 0,
            loudness: LoudnessScan::new(sample_rate),
            history: [[0.0; 8]; 2],
        }
    }

    fn push(&mut self, l: &[f32], r: &[f32]) {
        self.loudness.push(l, r);
        for (c, ch) in [l, r].into_iter().enumerate() {
            for &s in ch {
                self.peak = self.peak.max(s.abs());
                self.sum_squares += (s as f64) * (s as f64);
                self.samples += 1;
                let h = &mut self.history[c];
                h.rotate_left(1);
                h[7] = s;
                // Three points between h[3] and h[4], by a Hann-windowed sinc over 8 samples.
                for k in 1..4 {
                    let frac = k as f32 / 4.0;
                    let mut v = 0.0;
                    for (j, &x) in h.iter().enumerate() {
                        let d = j as f32 - 3.0 - frac;
                        let w = 0.5 + 0.5 * (std::f32::consts::PI * d / 4.0).cos();
                        let sinc =
                            if d == 0.0 { 1.0 } else { (std::f32::consts::PI * d).sin() / (std::f32::consts::PI * d) };
                        v += x * sinc * w;
                    }
                    self.true_peak = self.true_peak.max(v.abs());
                }
                self.true_peak = self.true_peak.max(s.abs());
            }
        }
    }

    /// The gain that brings the measure to the target.
    fn gain(&self, n: Normalize) -> f32 {
        let amp = |db: f64| 10f64.powf(db / 20.0);
        let level = match n.source {
            NormalizeSource::Peak => self.peak as f64,
            NormalizeSource::TruePeak => self.true_peak as f64,
            // Both channels summed, as loudness is; REAPER's RMS reads within about 1 % of it.
            NormalizeSource::Rms => (2.0 * self.sum_squares / self.samples.max(1) as f64).sqrt(),
            NormalizeSource::LufsIntegrated => amp(self.loudness.integrated()),
            NormalizeSource::LufsMomentaryMax => amp(self.loudness.max_momentary()),
            NormalizeSource::LufsShortTermMax => amp(self.loudness.max_short_term()),
        };
        if level > 0.0 && level.is_finite() { (n.target / level) as f32 } else { 1.0 }
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
    finish: Finish,
    progress: impl FnMut(f32),
) -> Result<(), BounceError> {
    let Some(normalize) = finish.normalize else {
        let mut out = SampleWriter::create(path, sample_rate, channels, depth, finish)?;
        render_mix(project, pool, range, sample_rate, |l, r, extra| out.write(l, r, extra), progress)?;
        return Ok(out.writer.finalize()?);
    };
    // Normalising needs the whole render measured first; it is kept in a float file rather
    // than rendered twice, so effects that do not repeat themselves cannot miss the target.
    let raw = path.with_extension("scythe-unnormalized.wav");
    // The temporary file keeps every channel the output needs (a mono output is mixed later).
    let kept = channels.max(2);
    let mut measure = Measure::new(sample_rate);
    let first = (|| {
        let mut tmp = SampleWriter::create(&raw, sample_rate, kept, BitDepth::Float32, Finish::default())?;
        render_mix(
            project,
            pool,
            range,
            sample_rate,
            |l, r, extra| {
                measure.push(l, r);
                tmp.write(l, r, extra)
            },
            progress,
        )?;
        Ok::<(), BounceError>(tmp.writer.finalize()?)
    })();
    let second = first.and_then(|()| {
        let mut out = SampleWriter::create(path, sample_rate, channels, depth, finish)?;
        out.gain = measure.gain(normalize);
        let mut reader = hound::WavReader::open(&raw)?;
        let mut samples = reader.samples::<f32>();
        let mut chans: Vec<Vec<f32>> = (0..kept).map(|_| Vec::with_capacity(RenderScratch::MAX_BLOCK)).collect();
        'blocks: loop {
            chans.iter_mut().for_each(Vec::clear);
            while chans[0].len() < RenderScratch::MAX_BLOCK {
                for c in chans.iter_mut() {
                    let Some(s) = samples.next() else { break 'blocks };
                    c.push(s?);
                }
            }
            let (pair, extra) = chans.split_at(2);
            out.write(&pair[0], &pair[1], extra)?;
        }
        if !chans[0].is_empty() {
            let len = chans.iter().map(Vec::len).min().unwrap_or(0);
            chans.iter_mut().for_each(|c| c.truncate(len));
            let (pair, extra) = chans.split_at(2);
            out.write(&pair[0], &pair[1], extra)?;
        }
        Ok(out.writer.finalize()?)
    });
    let _ = std::fs::remove_file(&raw);
    second
}

/// Full scale is 2^(bits-1), rounding halves up, as REAPER writes integer samples.
fn quantize(s: f32, bits: u32) -> i32 {
    let scale = (1i64 << (bits - 1)) as f64;
    (s as f64 * scale + 0.5).floor().clamp(-scale, scale - 1.0) as i32
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
    fn dither_adds_noise_of_an_lsb() {
        let path = std::env::temp_dir().join(format!("scythe-dither-{}.wav", std::process::id()));
        let finish = Finish { dither: true, ..Finish::default() };
        let mut w = SampleWriter::create(&path, 8000, 1, BitDepth::Int16, finish).unwrap();
        let level = 1000.25 / 32767.0;
        w.write(&[level; 4000], &[level; 4000], &[]).unwrap();
        w.writer.finalize().unwrap();
        let samples: Vec<i32> =
            hound::WavReader::open(&path).unwrap().samples::<i16>().map(|s| s.unwrap() as i32).collect();
        std::fs::remove_file(&path).ok();
        assert!(samples.iter().all(|s| (999..=1002).contains(s)), "TPDF stays within a step either side");
        let mean = samples.iter().sum::<i32>() as f64 / samples.len() as f64;
        assert!((mean - 1000.25).abs() < 0.05, "the level survives on average: {mean}");
    }

    #[test]
    fn multichannel_master_renders_every_channel() {
        let dir = std::env::temp_dir().join(format!("scythe-bounce-mc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("quad.wav");
        let spec = hound::WavSpec {
            channels: 4,
            sample_rate: 8000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(&src, spec).unwrap();
        for _ in 0..8000 {
            for c in 1..=4 {
                w.write_sample(c as f32 / 10.0).unwrap();
            }
        }
        w.finalize().unwrap();

        let mut p = Project::default();
        p.master.channels = 4;
        p.master.volume = 0.5;
        let t = p.add_track("t");
        p.track_mut(t).unwrap().channels = 4;
        let mut item = Item::new(0.0, 1.0, Take::from_file(src.clone()));
        (item.fade_in, item.fade_out) = (scythe_core::Fade::default(), scythe_core::Fade::default());
        p.track_mut(t).unwrap().items.push(item);
        let pool = MediaPool::default();
        pool.load(&src, 8000).unwrap();
        let out = dir.join("out.wav");
        let wav = Encoding::Wav(BitDepth::Float32);
        bounce(&p, &pool, TimeRange::new(0.0, 1.0), 8000, 4, wav, &out, |_| {}).unwrap();
        let data = decode_file(&out).unwrap();
        assert_eq!(data.channels.len(), 4);
        let at = |c: usize| data.channels[c][4000];
        assert!(
            (0..4).all(|c| (at(c) - (c + 1) as f32 / 20.0).abs() < 1e-6),
            "{:?}",
            (0..4).map(at).collect::<Vec<_>>()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn quantize_clips() {
        assert_eq!(quantize(2.0, 16), 32767);
        assert_eq!(quantize(-1.0, 16), -32768);
        assert_eq!(quantize(0.0, 24), 0);
        // REAPER's rounding, checked against its 16- and 24-bit renders.
        assert_eq!((quantize(0.5 / 32768.0, 16), quantize(-0.5 / 32768.0, 16)), (1, 0));
    }
}
