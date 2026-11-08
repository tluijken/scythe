use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("cannot open {path}: {source}")]
    Open { path: PathBuf, source: std::io::Error },
    #[error("unsupported or corrupt media {path}: {message}")]
    Decode { path: PathBuf, message: String },
    #[error("resampling failed: {0}")]
    Resample(String),
}

/// Decoded audio, planar, one `Vec` per channel.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioData {
    pub sample_rate: u32,
    pub channels: Vec<Vec<f32>>,
}

impl AudioData {
    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    pub fn duration(&self) -> f64 {
        self.frames() as f64 / self.sample_rate as f64
    }
}

pub fn decode_file(path: &Path) -> Result<AudioData, MediaError> {
    let decode_err = |message: String| MediaError::Decode { path: path.to_owned(), message };
    let file = File::open(path).map_err(|source| MediaError::Open { path: path.to_owned(), source })?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| decode_err(e.to_string()))?;
    let track = format.default_track(TrackType::Audio).ok_or_else(|| decode_err("no audio track".into()))?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| decode_err("no audio codec parameters".into()))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| decode_err(e.to_string()))?;

    let mut sample_rate = params.sample_rate.unwrap_or(0);
    let mut channels: Vec<Vec<f32>> = Vec::new();
    let mut planes: Vec<Vec<f32>> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(decode_err(e.to_string())),
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                sample_rate = buf.spec().rate();
                buf.copy_to_vecs_planar::<f32>(&mut planes);
                let frames = buf.frames();
                if channels.len() < planes.len() {
                    let have = channels.first().map_or(0, Vec::len);
                    channels.resize_with(planes.len(), || vec![0.0; have]);
                }
                for (dst, src) in channels.iter_mut().zip(&planes) {
                    dst.extend_from_slice(&src[..frames.min(src.len())]);
                }
            }
            // A corrupt packet is skipped; the rest of the stream is usually fine.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(decode_err(e.to_string())),
        }
    }
    if channels.is_empty() || sample_rate == 0 {
        return Err(decode_err("stream contains no audio".into()));
    }
    Ok(AudioData { sample_rate, channels })
}

/// Sample-rate conversion with a symmetric (zero-phase) Kaiser-windowed sinc, so converted
/// media stays sample-aligned with the original timeline the way REAPER's does.
pub fn resample(data: &AudioData, target_rate: u32) -> Result<AudioData, MediaError> {
    if data.sample_rate == target_rate || data.frames() == 0 {
        return Ok(AudioData { sample_rate: target_rate, channels: data.channels.clone() });
    }
    if data.sample_rate == 0 || target_rate == 0 {
        return Err(MediaError::Resample("zero sample rate".into()));
    }
    let kernel = SincKernel::new(data.sample_rate, target_rate);
    let ratio = data.sample_rate as f64 / target_rate as f64;
    let out_frames = (data.frames() as f64 / ratio).round() as usize;
    let channels =
        data.channels.iter().map(|ch| (0..out_frames).map(|i| kernel.sample(ch, i as f64 * ratio)).collect()).collect();
    Ok(AudioData { sample_rate: target_rate, channels })
}

/// REAPER's 192-point sinc resampler (`RENDER_RESAMPLE 3`, the render default), measured from
/// its renders of impulses: a Blackman-Harris window over 96 input frames each side, cut off
/// at the input's Nyquist frequency or 1/1.03 of the output's, read from a table of 32 steps
/// per input frame with linear interpolation between them.
struct SincKernel {
    /// The kernel from its centre outwards, `OVERSAMPLE` steps per input frame.
    table: Vec<f64>,
}

impl SincKernel {
    const HALF: usize = 96;
    const OVERSAMPLE: usize = 32;

    fn new(from: u32, to: u32) -> Self {
        Self::with_step(from as f64 / to as f64)
    }

    /// A kernel for reading the input `step` frames apart.
    fn with_step(step: f64) -> Self {
        use std::f64::consts::PI;
        let cutoff = if step > 1.0 { 1.0 / (step * 1.03) } else { 1.0 };
        let steps = Self::HALF * Self::OVERSAMPLE;
        let table = (0..=steps + 1)
            .map(|q| {
                let x = q as f64 / Self::OVERSAMPLE as f64;
                if q >= steps {
                    return 0.0;
                }
                let u = x / Self::HALF as f64;
                let window = 0.35875
                    + 0.48829 * (PI * u).cos()
                    + 0.14128 * (2.0 * PI * u).cos()
                    + 0.01168 * (3.0 * PI * u).cos();
                let sinc = if x == 0.0 { 1.0 } else { (PI * cutoff * x).sin() / (PI * cutoff * x) };
                cutoff * sinc * window
            })
            .collect();
        Self { table }
    }

    fn tap(&self, distance: f64) -> f64 {
        let t = distance.abs() * Self::OVERSAMPLE as f64;
        let lo = t as usize;
        if lo + 1 >= self.table.len() {
            return 0.0;
        }
        let f = t - lo as f64;
        self.table[lo] * (1.0 - f) + self.table[lo + 1] * f
    }

    fn sample(&self, input: &[f32], pos: f64) -> f32 {
        let base = pos.floor() as i64;
        let first = (base - Self::HALF as i64 + 1).max(0);
        let last = (base + Self::HALF as i64).min(input.len() as i64 - 1);
        let mut acc = 0.0f64;
        for idx in first..=last {
            acc += input[idx as usize] as f64 * self.tap(pos - idx as f64);
        }
        acc as f32
    }
}

/// Reads `frames` output frames from `input`, `step` input frames apart (band-limited).
pub(crate) fn resample_by_step(input: &[f32], step: f64, frames: usize) -> Vec<f32> {
    let kernel = SincKernel::with_step(step);
    (0..frames).map(|i| kernel.sample(input, i as f64 * step)).collect()
}

/// Min/max envelope of the channel-summed signal, used for waveform drawing.
#[derive(Debug, Clone, PartialEq)]
pub struct Peaks {
    pub frames_per_peak: usize,
    pub min: Vec<f32>,
    pub max: Vec<f32>,
}

impl Peaks {
    pub const DEFAULT_RESOLUTION: usize = 256;

    pub fn compute(data: &AudioData, frames_per_peak: usize) -> Self {
        let frames = data.frames();
        let n = frames.div_ceil(frames_per_peak);
        let (mut min, mut max) = (Vec::with_capacity(n), Vec::with_capacity(n));
        let scale = 1.0 / data.channels.len().max(1) as f32;
        for block in 0..n {
            let range = block * frames_per_peak..((block + 1) * frames_per_peak).min(frames);
            let (mut lo, mut hi) = (0f32, 0f32);
            for i in range {
                let s: f32 = data.channels.iter().map(|c| c[i]).sum::<f32>() * scale;
                lo = lo.min(s);
                hi = hi.max(s);
            }
            min.push(lo);
            max.push(hi);
        }
        Self { frames_per_peak, min, max }
    }

    /// Min/max over the frame range `[from, to)` of the source.
    pub fn range(&self, from: f64, to: f64) -> (f32, f32) {
        let a = (from / self.frames_per_peak as f64).floor().max(0.0) as usize;
        let b = ((to / self.frames_per_peak as f64).ceil() as usize).min(self.min.len());
        if a >= b {
            return (0.0, 0.0);
        }
        let lo = self.min[a..b].iter().copied().fold(0.0, f32::min);
        let hi = self.max[a..b].iter().copied().fold(0.0, f32::max);
        (lo, hi)
    }
}

/// A decoded file at the engine's sample rate, ready for the audio thread.
#[derive(Debug)]
pub struct Media {
    pub path: PathBuf,
    pub source_rate: u32,
    pub audio: Arc<AudioData>,
    pub peaks: Peaks,
}

impl Media {
    pub fn duration(&self) -> f64 {
        self.audio.duration()
    }
}

#[derive(Debug, Clone)]
pub enum MediaState {
    Ready(Arc<Media>),
    Failed(String),
}

/// Decoded media keyed by path. Decoding is slow, so `load` is meant for worker threads; the
/// audio thread only ever sees `Arc<AudioData>` handed over in a render graph.
#[derive(Debug, Default)]
pub struct MediaPool {
    entries: Mutex<HashMap<PathBuf, MediaState>>,
    /// Time-stretched or pitch-shifted copies, keyed by file and the two ratios' bits.
    stretched: Mutex<HashMap<(PathBuf, u64, u64), Arc<AudioData>>>,
}

impl MediaPool {
    /// The copy of `path` stretched by `time` and shifted by `pitch`, once prepared.
    pub fn stretched(&self, path: &Path, (time, pitch): (f64, f64)) -> Option<Arc<AudioData>> {
        self.stretched.lock().unwrap().get(&(path.to_owned(), time.to_bits(), pitch.to_bits())).cloned()
    }

    /// Computes the stretched copy (slow; for worker threads). `None` until the file is loaded.
    pub fn prepare_stretched(&self, path: &Path, ratios: (f64, f64)) -> Option<Arc<AudioData>> {
        if let Some(done) = self.stretched(path, ratios) {
            return Some(done);
        }
        let base = self.ready(path)?;
        let audio = Arc::new(crate::stretch::stretch(&base.audio, ratios.0, ratios.1));
        let key = (path.to_owned(), ratios.0.to_bits(), ratios.1.to_bits());
        self.stretched.lock().unwrap().insert(key, audio.clone());
        Some(audio)
    }

    /// Prepares every stretched copy the project's takes need; their files must be loaded.
    pub fn prepare_project(&self, project: &scythe_core::Project) {
        for take in project.tracks.iter().flat_map(|t| &t.items).flat_map(|i| &i.takes) {
            if let (Some(file), Some(ratios)) = (take.source.file(), take.stretch()) {
                self.prepare_stretched(&scythe_core::rpp::resolve_media_path(project, file), ratios);
            }
        }
    }

    pub fn get(&self, path: &Path) -> Option<MediaState> {
        self.entries.lock().unwrap().get(path).cloned()
    }

    pub fn ready(&self, path: &Path) -> Option<Arc<Media>> {
        match self.get(path)? {
            MediaState::Ready(m) => Some(m),
            MediaState::Failed(_) => None,
        }
    }

    pub fn load(&self, path: &Path, engine_rate: u32) -> Result<Arc<Media>, MediaError> {
        if let Some(m) = self.ready(path) {
            return Ok(m);
        }
        let result = decode_file(path).and_then(|raw| {
            let audio = resample(&raw, engine_rate)?;
            let peaks = Peaks::compute(&audio, Peaks::DEFAULT_RESOLUTION);
            Ok(Arc::new(Media { path: path.to_owned(), source_rate: raw.sample_rate, audio: Arc::new(audio), peaks }))
        });
        let state = match &result {
            Ok(m) => MediaState::Ready(m.clone()),
            Err(e) => MediaState::Failed(e.to_string()),
        };
        self.entries.lock().unwrap().insert(path.to_owned(), state);
        result
    }

    pub fn clear(&self) {
        self.entries.lock().unwrap().clear();
        self.stretched.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, freq: f32, secs: f32) -> Vec<f32> {
        (0..(rate as f32 * secs) as usize)
            .map(|i| (i as f32 / rate as f32 * freq * std::f32::consts::TAU).sin() * 0.5)
            .collect()
    }

    pub(crate) fn write_wav(path: &Path, rate: u32, channels: &[Vec<f32>]) {
        let spec = hound::WavSpec {
            channels: channels.len() as u16,
            sample_rate: rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..channels[0].len() {
            for c in channels {
                w.write_sample(c[i]).unwrap();
            }
        }
        w.finalize().unwrap();
    }

    #[test]
    fn decodes_wav() {
        let dir = std::env::temp_dir().join(format!("scythe-media-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone.wav");
        let tone = sine(48000, 440.0, 0.5);
        write_wav(&path, 48000, &[tone.clone(), tone.iter().map(|s| -s).collect()]);

        let data = decode_file(&path).unwrap();
        assert_eq!(data.sample_rate, 48000);
        assert_eq!(data.channels.len(), 2);
        assert_eq!(data.frames(), tone.len());
        assert!((data.channels[0][100] - tone[100]).abs() < 1e-6);
        assert!((data.channels[1][100] + tone[100]).abs() < 1e-6);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_is_an_error() {
        assert!(matches!(decode_file(Path::new("/nonexistent/x.wav")), Err(MediaError::Open { .. })));
    }

    #[test]
    fn resample_keeps_duration_and_pitch() {
        let data = AudioData { sample_rate: 44100, channels: vec![sine(44100, 1000.0, 1.0)] };
        let out = resample(&data, 48000).unwrap();
        assert_eq!(out.sample_rate, 48000);
        assert!((out.frames() as i64 - 48000).abs() < 64, "frames {}", out.frames());
        let crossings = out.channels[0].windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        assert!((crossings as i64 - 1000).abs() <= 2, "crossings {crossings}");
    }

    #[test]
    fn peaks_track_envelope() {
        let data = AudioData { sample_rate: 1000, channels: vec![vec![0.0, 0.5, -0.25, 1.0, 0.0, 0.0]] };
        let p = Peaks::compute(&data, 2);
        assert_eq!(p.max, [0.5, 1.0, 0.0]);
        assert_eq!(p.min, [0.0, -0.25, 0.0]);
        assert_eq!(p.range(0.0, 4.0), (-0.25, 1.0));
    }
}
