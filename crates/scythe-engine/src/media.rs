use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use scythe_core::Processing;
use scythe_core::render_settings::ResampleMode;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::codecs::audio::well_known::{CODEC_ID_MP1, CODEC_ID_MP2, CODEC_ID_MP3};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
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

/// Decodes a file the way a REAPER media item reads it.
pub fn decode_file(path: &Path) -> Result<AudioData, MediaError> {
    if crate::video::is_video_file(path) {
        return decode_with_ffmpeg(path).or_else(|_| decode(path, false));
    }
    decode(path, false)
}

/// The first audio stream of a video file, decoded by the `ffmpeg` command-line tool. REAPER
/// reads video files with FFmpeg, which applies their edit lists (an AAC encoder's priming
/// samples left out); Symphonia does not.
fn decode_with_ffmpeg(path: &Path) -> Result<AudioData, MediaError> {
    let decode_err = |message: String| MediaError::Decode { path: path.to_owned(), message };
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let wav = std::env::temp_dir().join(format!("scythe-video-audio-{}-{n}.wav", std::process::id()));
    let output = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-vn", "-c:a", "pcm_f32le", "-f", "wav"])
        .arg(&wav)
        .output()
        .map_err(|e| decode_err(format!("ffmpeg: {e}")));
    let decoded = output.and_then(|o| {
        if o.status.success() {
            decode(&wav, false)
        } else {
            Err(decode_err(String::from_utf8_lossy(&o.stderr).trim().to_owned()))
        }
    });
    let _ = std::fs::remove_file(&wav);
    decoded
}

/// Decodes a file the way ReaSamplOmatic5000 loads a sample: MP3 encoder delay and padding
/// (from the LAME header) are trimmed, as in gapless playback.
pub fn decode_gapless(path: &Path) -> Result<AudioData, MediaError> {
    decode(path, true)
}

fn decode(path: &Path, gapless: bool) -> Result<AudioData, MediaError> {
    let decode_err = |message: String| MediaError::Decode { path: path.to_owned(), message };
    let open_err = |source| MediaError::Open { path: path.to_owned(), source };
    let file = File::open(path).map_err(open_err)?;
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probe = |source: Box<dyn MediaSource>| {
        let mss = MediaSourceStream::new(source, Default::default());
        symphonia::default::get_probe()
            .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
            .map_err(|e| decode_err(e.to_string()))
    };

    let mut format = probe(Box::new(file))?;
    let is_mp3 = format
        .default_track(TrackType::Audio)
        .and_then(|t| t.codec_params.as_ref())
        .and_then(|p| p.audio())
        .is_some_and(|p| p.codec == CODEC_ID_MP3);
    if is_mp3 {
        let mut bytes = std::fs::read(path).map_err(open_err)?;
        if lower_lsf_mid_side_gain(&mut bytes) {
            format = probe(Box::new(std::io::Cursor::new(bytes)))?;
        }
    }
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
    // REAPER's MPEG decoder scales its output by 32767/32768. Media items keep the encoder delay
    // and padding and play a Xing/Info/VBRI header frame as one frame of silence.
    let mpeg = [CODEC_ID_MP1, CODEC_ID_MP2, CODEC_ID_MP3].contains(&params.codec);
    let untrimmed = mpeg && !gapless;
    let mut header_frame = untrimmed && starts_with_info_frame(path);

    let mut sample_rate = params.sample_rate.unwrap_or(0);
    let mut channels: Vec<Vec<f32>> = Vec::new();
    let mut planes: Vec<Vec<f32>> = Vec::new();
    loop {
        let mut packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(decode_err(e.to_string())),
        };
        if packet.track_id != track_id {
            continue;
        }
        if untrimmed {
            packet.trim_start = Default::default();
            packet.trim_end = Default::default();
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
                if std::mem::take(&mut header_frame) {
                    for dst in &mut channels {
                        dst.resize(dst.len() + frames, 0.0);
                    }
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
    if mpeg {
        // The sampler keeps the decoder's 16-bit output; items get it unrounded and unclipped.
        for s in channels.iter_mut().flatten() {
            *s = if gapless {
                (*s * 32767.0).round().clamp(-32768.0, 32767.0) / 32768.0
            } else {
                *s * (32767.0 / 32768.0)
            };
        }
    }
    Ok(AudioData { sample_rate, channels })
}

/// REAPER's MP3 decoder plays mid/side granules of MPEG-2 and 2.5 (LSF) Layer III streams
/// 3 dB low: their 1/sqrt(2) mid/side factor is applied twice. MPEG-1 is decoded correctly.
/// Lowering the global gain of such granules by 2 steps (2^(-2/4)) makes a correct decoder
/// produce the same samples. Returns whether any frame changed.
fn lower_lsf_mid_side_gain(bytes: &mut [u8]) -> bool {
    const LSF_BITRATES: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
    const LSF_RATES: [[u32; 3]; 2] = [[11025, 12000, 8000], [22050, 24000, 16000]];
    // (version, frame length) of a Layer III header at `at`, when it is one.
    let header = |bytes: &[u8], at: usize| -> Option<(u32, usize)> {
        let h = u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?);
        let (version, layer, bitrate, rate) = ((h >> 19) & 3, (h >> 17) & 3, (h >> 12) & 15, (h >> 10) & 3);
        if h >> 21 != 0x7ff || version == 1 || layer != 1 || bitrate == 0 || bitrate == 15 || rate == 3 {
            return None;
        }
        let padding = ((h >> 9) & 1) as usize;
        let len = if version == 3 {
            const BITRATES: [u32; 15] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
            const RATES: [u32; 3] = [44100, 48000, 32000];
            144_000 * BITRATES[bitrate as usize] / RATES[rate as usize]
        } else {
            72_000 * LSF_BITRATES[bitrate as usize] / LSF_RATES[(version / 2) as usize][rate as usize]
        };
        Some((version, len as usize + padding))
    };
    let mut at = 0;
    if bytes.starts_with(b"ID3") && bytes.len() >= 10 {
        let size = bytes[6..10].iter().fold(0usize, |n, &b| (n << 7) | (b & 0x7f) as usize);
        at = 10 + size + if bytes[5] & 0x10 != 0 { 10 } else { 0 };
    }
    let mut changed = false;
    let mut synced = false;
    while at + 4 <= bytes.len() {
        let Some((version, len)) = header(bytes, at) else {
            synced = false;
            at += 1;
            continue;
        };
        // A sync word found by scanning counts only when the next frame follows it.
        if !synced && at + len + 4 <= bytes.len() && header(bytes, at + len).is_none() {
            at += 1;
            continue;
        }
        synced = true;
        let (mode, mode_ext) = ((bytes[at + 3] >> 6) & 3, (bytes[at + 3] >> 4) & 3);
        let side_info = at + 4 + if bytes[at + 1] & 1 == 0 { 2 } else { 0 };
        if version != 3 && mode == 1 && mode_ext & 2 != 0 && side_info + 13 <= bytes.len() {
            // LSF side info: main_data_begin (8), private bits (2), then per channel 63 bits
            // with the 8-bit global gain at bit 21.
            for bit in [side_info * 8 + 31, side_info * 8 + 94] {
                let (byte, shift) = (bit / 8, 8 - bit % 8);
                let word = u16::from_be_bytes([bytes[byte], bytes[byte + 1]]);
                let gain = (word >> shift) & 0xff;
                let lowered = gain.saturating_sub(2);
                if lowered != gain {
                    let word = (word & !(0xff << shift)) | (lowered << shift);
                    bytes[byte..byte + 2].copy_from_slice(&word.to_be_bytes());
                    changed = true;
                }
            }
        }
        at += len.max(1);
    }
    changed
}

/// Whether the first MPEG frame (after any ID3v2 tag) carries a Xing, Info or VBRI header.
fn starts_with_info_frame(path: &Path) -> bool {
    use std::io::Read;
    let mut head = Vec::new();
    if File::open(path).and_then(|f| f.take(1 << 16).read_to_end(&mut head)).is_err() {
        return false;
    }
    let mut at = 0;
    if head.starts_with(b"ID3") && head.len() >= 10 {
        let size = head[6..10].iter().fold(0usize, |n, &b| (n << 7) | (b & 0x7f) as usize);
        at = 10 + size + if head[5] & 0x10 != 0 { 10 } else { 0 };
    }
    let Some(sync) = head.get(at..).and_then(|h| h.windows(2).position(|w| w[0] == 0xff && w[1] & 0xe0 == 0xe0)) else {
        return false;
    };
    let frame = &head[at + sync..];
    let tag_at = |offset: usize, tag: &[u8]| frame.get(offset..offset + 4) == Some(tag);
    (4..=40).any(|o| tag_at(o, b"Xing") || tag_at(o, b"Info")) || tag_at(36, b"VBRI")
}

/// Sample-rate conversion with REAPER's render default, the 192-point sinc.
pub fn resample(data: &AudioData, target_rate: u32) -> Result<AudioData, MediaError> {
    resample_with(data, target_rate, ResampleMode::default())
}

/// Sample-rate conversion the way REAPER's resample `mode` does it, measured from its renders
/// of impulses. Output frame `j` reads the input at `j * from / to`, so converted media stays
/// aligned with the original timeline.
pub fn resample_with(data: &AudioData, target_rate: u32, mode: ResampleMode) -> Result<AudioData, MediaError> {
    if data.sample_rate == target_rate || data.frames() == 0 {
        return Ok(AudioData { sample_rate: target_rate, channels: data.channels.clone() });
    }
    resample_rate(data, data.sample_rate as f64, target_rate, mode)
}

/// Converts `data` as if it ran at `from` Hz to `target_rate`: a take's rate change without
/// pitch preservation is a conversion from its file's rate times the play rate, with the same
/// resampler (REAPER's renders at play rates 0.5, 0.9 and 1.5 null against this).
pub fn resample_rate(
    data: &AudioData,
    from: f64,
    target_rate: u32,
    mode: ResampleMode,
) -> Result<AudioData, MediaError> {
    if from.is_nan() || from <= 0.0 || target_rate == 0 {
        return Err(MediaError::Resample("zero sample rate".into()));
    }
    // Rates that are not whole numbers become a ratio of scaled integers, exact to well under a
    // sample over hours.
    let scale = if (from - from.round()).abs() < 1e-6 { 1 } else { 100_000 };
    let r8_from = if scale == 1 { from.round() } else { from };
    let (from, to) = ((from * scale as f64).round() as u64, target_rate as u64 * scale);
    let unit = gcd(from, to);
    let (from, to) = (from / unit, to / unit);
    if from == to || data.frames() == 0 {
        return Ok(AudioData { sample_rate: target_rate, channels: data.channels.clone() });
    }
    let out_frames = (data.frames() as f64 * to as f64 / from as f64).round() as usize;
    let step = from as f64 / to as f64;
    let channels = match mode {
        ResampleMode::Sinc { half, tabled } => {
            let kernel = SincKernel::new(step, half as usize, tabled);
            let phases = PhaseTable::new(&kernel, from, to);
            data.channels.iter().map(|ch| phases.convert(&kernel, ch, from, to, out_frames)).collect()
        }
        ResampleMode::R8brain => data
            .channels
            .iter()
            .map(|ch| crate::r8brain::convert(ch, r8_from, target_rate as f64, out_frames))
            .collect(),
        // The frame before the read position, so a position landing exactly on a frame reads the
        // one before it.
        ResampleMode::Point => data
            .channels
            .iter()
            .map(|ch| {
                (0..out_frames as u64)
                    .map(|j| ch.get((j * from).div_ceil(to).saturating_sub(1) as usize).copied().unwrap_or(0.0))
                    .collect()
            })
            .collect(),
        ResampleMode::Linear => data.channels.iter().map(|ch| linear(ch, from, to, out_frames)).collect(),
        ResampleMode::LinearIir { stages } => {
            // A 2-pole low-pass at 0.693 (one stage) or 0.792 (two stages) of the lower Nyquist
            // frequency, run at the higher of the two rates.
            let fraction = if stages > 1 { 0.792 } else { 0.693 };
            let w = std::f64::consts::PI * fraction * from.min(to) as f64 / from.max(to) as f64;
            let filter = |x: &mut [f64]| {
                for _ in 0..stages.max(1) {
                    lowpass(x, w);
                }
            };
            data.channels
                .iter()
                .map(|ch| {
                    if from < to {
                        let mut y: Vec<f64> = linear(ch, from, to, out_frames).into_iter().map(f64::from).collect();
                        filter(&mut y);
                        y.into_iter().map(|v| v as f32).collect()
                    } else {
                        let mut x: Vec<f64> = ch.iter().map(|&v| v as f64).collect();
                        filter(&mut x);
                        let x: Vec<f32> = x.into_iter().map(|v| v as f32).collect();
                        linear(&x, from, to, out_frames)
                    }
                })
                .collect()
        }
    };
    Ok(AudioData { sample_rate: target_rate, channels })
}

fn linear(input: &[f32], from: u64, to: u64, frames: usize) -> Vec<f32> {
    let at = |i: u64| input.get(i as usize).copied().unwrap_or(0.0) as f64;
    (0..frames as u64)
        .map(|j| {
            let (i, rem) = (j * from / to, j * from % to);
            let f = rem as f64 / to as f64;
            (at(i) * (1.0 - f) + at(i + 1) * f) as f32
        })
        .collect()
}

/// RBJ low-pass biquad with a Q of 0.707, in place.
fn lowpass(x: &mut [f64], w: f64) {
    let (sin, cos) = w.sin_cos();
    let alpha = sin / (2.0 * 0.707);
    let a0 = 1.0 + alpha;
    let b0 = (1.0 - cos) / 2.0 / a0;
    let (b1, b2) = (2.0 * b0, b0);
    let (a1, a2) = (-2.0 * cos / a0, (1.0 - alpha) / a0);
    let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
    for v in x {
        let y = b0 * *v + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
        (x2, x1, y2, y1) = (x1, *v, y1, y);
        *v = y;
    }
}

/// REAPER's sinc resamplers: a Blackman-Harris window over `half` input frames each side, cut
/// off at the input's Nyquist frequency or 1/1.03 of the output's. The 16-, 64- and 192-point
/// kernels are read from a table of 32 steps per input frame with linear interpolation; the
/// longer ones are computed exactly.
struct SincKernel {
    half: usize,
    cutoff: f64,
    /// The kernel from its centre outwards, `OVERSAMPLE` steps per input frame.
    table: Option<Vec<f64>>,
}

impl SincKernel {
    const OVERSAMPLE: usize = 32;

    /// A kernel for reading the input `step` frames apart.
    fn new(step: f64, half: usize, tabled: bool) -> Self {
        let cutoff = if step > 1.0 { 1.0 / (step * 1.03) } else { 1.0 };
        let mut kernel = Self { half, cutoff, table: None };
        if tabled {
            let steps = half * Self::OVERSAMPLE;
            let table = (0..=steps + 1)
                .map(|q| if q >= steps { 0.0 } else { kernel.exact(q as f64 / Self::OVERSAMPLE as f64) });
            kernel.table = Some(table.collect());
        }
        kernel
    }

    fn exact(&self, x: f64) -> f64 {
        use std::f64::consts::PI;
        let u = x / self.half as f64;
        if u.abs() >= 1.0 {
            return 0.0;
        }
        let window =
            0.35875 + 0.48829 * (PI * u).cos() + 0.14128 * (2.0 * PI * u).cos() + 0.01168 * (3.0 * PI * u).cos();
        let sinc = if x == 0.0 { 1.0 } else { (PI * self.cutoff * x).sin() / (PI * self.cutoff * x) };
        self.cutoff * sinc * window
    }

    fn tap(&self, distance: f64) -> f64 {
        let Some(table) = &self.table else { return self.exact(distance) };
        let t = distance.abs() * Self::OVERSAMPLE as f64;
        let lo = t as usize;
        if lo + 1 >= table.len() {
            return 0.0;
        }
        let f = t - lo as f64;
        table[lo] * (1.0 - f) + table[lo + 1] * f
    }

    /// The input frames read for position `base + frac` and their weights.
    fn taps(&self, frac: f64, weights: &mut Vec<f64>) {
        weights.clear();
        weights.extend((1 - self.half as i64..=self.half as i64).map(|k| self.tap(frac - k as f64)));
    }

    fn sample(&self, input: &[f32], pos: f64) -> f32 {
        let base = pos.floor() as i64;
        let first = (base - self.half as i64 + 1).max(0);
        let last = (base + self.half as i64).min(input.len() as i64 - 1);
        let mut acc = 0.0f64;
        for idx in first..=last {
            acc += input[idx as usize] as f64 * self.tap(pos - idx as f64);
        }
        acc as f32
    }
}

/// Kernel weights for every fractional position a conversion from `from` to `to` Hz visits,
/// when there are few enough of them to keep.
struct PhaseTable {
    /// `from / gcd`-spaced phases, `2 * half` weights each; empty when computed per sample.
    weights: Vec<f64>,
    unit: u64,
}

impl PhaseTable {
    const MAX_WEIGHTS: u64 = 1 << 22;

    fn new(kernel: &SincKernel, from: u64, to: u64) -> Self {
        let unit = gcd(from, to);
        let phases = to / unit;
        if phases * 2 * kernel.half as u64 > Self::MAX_WEIGHTS {
            return Self { weights: Vec::new(), unit };
        }
        let mut weights = Vec::with_capacity((phases as usize) * 2 * kernel.half);
        let mut row = Vec::new();
        for p in 0..phases {
            kernel.taps((p * unit) as f64 / to as f64, &mut row);
            weights.extend_from_slice(&row);
        }
        Self { weights, unit }
    }

    fn convert(&self, kernel: &SincKernel, input: &[f32], from: u64, to: u64, frames: usize) -> Vec<f32> {
        let width = 2 * kernel.half;
        (0..frames as u64)
            .map(|j| {
                let (base, rem) = ((j * from / to) as i64, j * from % to);
                if self.weights.is_empty() {
                    return kernel.sample(input, base as f64 + rem as f64 / to as f64);
                }
                let row = &self.weights[(rem / self.unit) as usize * width..][..width];
                let first = base - kernel.half as i64 + 1;
                let lo = (-first).max(0) as usize;
                let hi = (input.len() as i64 - first).clamp(0, width as i64) as usize;
                let mut acc = 0.0f64;
                for k in lo..hi {
                    acc += input[(first + k as i64) as usize] as f64 * row[k];
                }
                acc as f32
            })
            .collect()
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Converts a stream from `from` to `to` Hz block by block with the 192-point sinc. Output
/// frame `m` reads the input at `m * from / to` and is produced once the input reaches
/// 96 frames past that position.
pub(crate) struct StreamResampler {
    kernel: SincKernel,
    from: u64,
    to: u64,
    /// Input frames from absolute frame `start` on.
    history: Vec<f32>,
    start: u64,
    next: u64,
}

impl StreamResampler {
    pub(crate) fn new(from: u32, to: u32) -> Self {
        let kernel = SincKernel::new(from as f64 / to as f64, 96, true);
        Self { kernel, from: from as u64, to: to as u64, history: Vec::new(), start: 0, next: 0 }
    }

    /// Input frames that must follow a read position before its output frame is ready.
    pub(crate) fn lookahead(&self) -> usize {
        self.kernel.half
    }

    pub(crate) fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.history.extend_from_slice(input);
        let half = self.kernel.half as u64;
        let end = self.start + self.history.len() as u64;
        loop {
            let (base, rem) = (self.next * self.from / self.to, self.next * self.from % self.to);
            if base + half >= end {
                break;
            }
            let frac = rem as f64 / self.to as f64;
            let mut acc = 0.0f64;
            for idx in (base + 1).saturating_sub(half).max(self.start)..=base + half {
                let x = self.history[(idx - self.start) as usize] as f64;
                acc += x * self.kernel.tap(base as f64 + frac - idx as f64);
            }
            out.push(acc as f32);
            self.next += 1;
        }
        let keep_from = (self.next * self.from / self.to + 1).saturating_sub(half).max(self.start);
        self.history.drain(..(keep_from - self.start) as usize);
        self.start = keep_from;
    }

    pub(crate) fn reset(&mut self) {
        self.history.clear();
        self.start = 0;
        self.next = 0;
    }
}

/// Reads `frames` output frames from `input`, `step` input frames apart (band-limited).
pub(crate) fn resample_by_step(input: &[f32], step: f64, frames: usize) -> Vec<f32> {
    let kernel = SincKernel::new(step, 96, true);
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
    /// Stretched, pitch-shifted or rate-changed copies, keyed by file and `Processing::key`.
    processed: Mutex<HashMap<ProcessedKey, Arc<AudioData>>>,
    /// How files at another rate are converted to the engine's, and how takes play at another
    /// rate without keeping their pitch.
    resample: ResampleMode,
}

type ProcessedKey = (PathBuf, (u64, u64, u64, i32));

impl MediaPool {
    /// A pool converting media with `mode`: the project's render mode for renders, the playback
    /// mode for playback.
    pub fn with_resample(mode: ResampleMode) -> Self {
        Self { resample: mode, ..Self::default() }
    }

    pub fn resample_mode(&self) -> ResampleMode {
        self.resample
    }

    /// The copy of `path` that went through `processing`, once prepared. It runs on its own time
    /// axis, `processing.length_ratio()` times the source's, at the engine's rate.
    pub fn processed(&self, path: &Path, processing: &Processing) -> Option<Arc<AudioData>> {
        self.processed.lock().unwrap().get(&(path.to_owned(), processing.key())).cloned()
    }

    /// Computes the processed copy (slow; for worker threads). `None` until the file is loaded.
    pub fn prepare_processed(&self, path: &Path, processing: &Processing) -> Option<Arc<AudioData>> {
        if let Some(done) = self.processed(path, processing) {
            return Some(done);
        }
        let base = self.ready(path)?;
        let rate = base.audio.sample_rate;
        let audio = if processing.shifts() {
            let shifted = crate::stretch::apply(&base.audio, processing);
            if processing.rate == 1.0 {
                shifted
            } else {
                resample_rate(&shifted, rate as f64 * processing.rate, rate, self.resample).ok()?
            }
        } else if base.source_rate == rate {
            resample_rate(&base.audio, rate as f64 * processing.rate, rate, self.resample).ok()?
        } else {
            // One conversion from the file's own rate, as REAPER does, rather than two.
            let raw = decode_file(path).ok()?;
            resample_rate(&raw, raw.sample_rate as f64 * processing.rate, rate, self.resample).ok()?
        };
        let audio = Arc::new(audio);
        self.processed.lock().unwrap().insert((path.to_owned(), processing.key()), audio.clone());
        Some(audio)
    }

    /// Prepares every processed copy the project's takes need; their files must be loaded.
    pub fn prepare_project(&self, project: &scythe_core::Project) {
        for take in project.tracks.iter().flat_map(|t| &t.items).flat_map(|i| &i.takes) {
            if let (Some(file), Some(processing)) = (take.source.file(), take.processing(project.default_pitch_mode)) {
                self.prepare_processed(&scythe_core::rpp::resolve_media_path(project, file), &processing);
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
            let audio = resample_with(&raw, engine_rate, self.resample)?;
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
        self.processed.lock().unwrap().clear();
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
    fn every_mode_keeps_level_and_pitch() {
        let data = AudioData { sample_rate: 44100, channels: vec![sine(44100, 1000.0, 1.0)] };
        for mode in 0..=10 {
            let out = resample_with(&data, 48000, ResampleMode::from_reaper(mode)).unwrap();
            let ch = &out.channels[0][4800..43200];
            let crossings = ch.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
            assert!((crossings as i64 - 800).abs() <= 1, "mode {mode}: crossings {crossings}");
            let peak = ch.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!((peak - 0.5).abs() < 0.02, "mode {mode}: peak {peak}");
        }
    }

    #[test]
    fn play_rates_resample_in_every_mode() {
        let data = AudioData { sample_rate: 48000, channels: vec![sine(48000, 1000.0, 1.0)] };
        for mode in 0..=10 {
            // 0.8734 is not a whole number of hertz at 48 kHz.
            for rate in [0.5, 0.8734, 1.5] {
                let out = resample_rate(&data, 48000.0 * rate, 48000, ResampleMode::from_reaper(mode)).unwrap();
                assert_eq!(out.frames(), (48000.0 / rate).round() as usize);
                let ch = &out.channels[0][4800..out.frames() - 4800];
                let crossings = ch.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count() as f64;
                let expected = 1000.0 * rate * ch.len() as f64 / 48000.0;
                assert!((crossings - expected).abs() <= 1.5, "mode {mode} rate {rate}: {crossings} vs {expected}");
            }
        }
    }

    #[test]
    fn point_sampling_reads_the_frame_before_an_exact_hit() {
        let data = AudioData { sample_rate: 2, channels: vec![vec![1.0, 2.0, 3.0, 4.0]] };
        let out = resample_with(&data, 3, ResampleMode::Point).unwrap();
        assert_eq!(out.channels[0], [1.0, 1.0, 2.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn streaming_matches_whole_file_conversion() {
        let input: Vec<f32> = (0..5000).map(|i| ((i * 7919 % 1000) as f32 / 500.0) - 1.0).collect();
        let whole = resample(&AudioData { sample_rate: 48000, channels: vec![input.clone()] }, 44100).unwrap();
        let mut stream = StreamResampler::new(48000, 44100);
        let mut out = Vec::new();
        for block in input.chunks(333) {
            stream.push(block, &mut out);
        }
        assert!(out.len() + 200 > whole.frames());
        for (a, b) in out.iter().zip(&whole.channels[0]) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn lowers_only_lsf_mid_side_granules() {
        let gain = |bytes: &[u8], bit: usize| {
            (u16::from_be_bytes([bytes[bit / 8], bytes[bit / 8 + 1]]) >> (8 - bit % 8)) & 0xff
        };
        let set = |bytes: &mut [u8], bit: usize, v: u16| {
            let word = u16::from_be_bytes([bytes[bit / 8], bytes[bit / 8 + 1]]) & !(0xff << (8 - bit % 8));
            bytes[bit / 8..bit / 8 + 2].copy_from_slice(&(word | (v << (8 - bit % 8))).to_be_bytes());
        };
        // MPEG-2 Layer III, 64 kbps, 22.05 kHz, joint stereo with mid/side: 208-byte frames.
        let frame = |mode_byte: u8| {
            let mut f = vec![0u8; 208];
            f[..4].copy_from_slice(&[0xff, 0xf3, 0x80, mode_byte]);
            set(&mut f, 32 + 31, 150);
            set(&mut f, 32 + 94, 1);
            f
        };
        let mut bytes = [frame(0x60), frame(0x40), frame(0x60)].concat();
        assert!(lower_lsf_mid_side_gain(&mut bytes));
        assert_eq!((gain(&bytes, 32 + 31), gain(&bytes, 32 + 94)), (148, 0));
        assert_eq!(gain(&bytes, (208 + 4) * 8 + 31), 150, "left/right frame");
        assert_eq!(gain(&bytes, (416 + 4) * 8 + 31), 148);

        let mut mpeg1 = frame(0x60);
        mpeg1[1] = 0xfb;
        let mut mpeg1 = [mpeg1.clone(), mpeg1].concat();
        assert!(!lower_lsf_mid_side_gain(&mut mpeg1));
    }

    #[test]
    fn finds_the_mp3_info_frame() {
        let dir = std::env::temp_dir().join(format!("scythe-info-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.mp3");
        let mut bytes = b"ID3\x04\x00\x00\x00\x00\x00\x02\x00\x00".to_vec();
        bytes.extend([0xff, 0xfb, 0x90, 0x64]);
        bytes.extend([0; 32]);
        bytes.extend(b"Info");
        std::fs::write(&path, &bytes).unwrap();
        assert!(starts_with_info_frame(&path));
        bytes[48..52].copy_from_slice(b"Junk");
        std::fs::write(&path, &bytes).unwrap();
        assert!(!starts_with_info_frame(&path));
        std::fs::remove_dir_all(&dir).ok();
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
