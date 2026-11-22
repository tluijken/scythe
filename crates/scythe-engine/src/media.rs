use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use scythe_core::Processing;
use scythe_core::render_settings::ResampleMode;

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
    decode_audio(path, false)
}

fn decode_audio(path: &Path, sampler: bool) -> Result<AudioData, MediaError> {
    match extension(path).as_deref() {
        Some("opus") => decode_opus(path),
        Some("wv") => decode_with_ffmpeg(path),
        Some("rpp") => decode_subproject(path),
        _ => {
            decode(path, sampler).or_else(|e| if scythe_decode::is_ogg_opus(path) { decode_opus(path) } else { Err(e) })
        }
    }
}

/// The proxy a subproject plays from: REAPER renders the project's master mix next to it as
/// `<file>-PROX`, a 32-bit float WAV at the subproject's sample rate.
pub fn subproject_proxy(path: &Path) -> PathBuf {
    let mut proxy = path.as_os_str().to_owned();
    proxy.push("-PROX");
    PathBuf::from(proxy)
}

/// A subproject (`RPP_PROJECT` source) plays its proxy, rendered first when it is missing, as
/// REAPER does on load. A proxy that exists is played as it is, even if the project changed
/// since: REAPER renders it again when the subproject is saved.
fn decode_subproject(path: &Path) -> Result<AudioData, MediaError> {
    let proxy = subproject_proxy(path);
    if !proxy.exists() {
        render_subproject(path)?;
    }
    decode(&proxy, false)
}

/// Renders the subproject at `path` to its proxy: the master mix from 0 to the project's end.
pub fn render_subproject(path: &Path) -> Result<(), MediaError> {
    static RENDERING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    let decode_err = |message: String| MediaError::Decode { path: path.to_owned(), message };
    let project = crate::upgrade::read_project(path).map_err(|e| decode_err(e.to_string()))?;
    let rate = project.sample_rate.unwrap_or(48_000);
    {
        let mut rendering = RENDERING.lock().unwrap();
        if rendering.iter().any(|p| p == path) {
            return Err(decode_err("the subproject contains itself".into()));
        }
        rendering.push(path.to_owned());
    }
    let pool = MediaPool::with_resample(scythe_core::render_settings::RenderSettings::from_project(&project).resample);
    for take in project.tracks.iter().flat_map(|t| &t.items).flat_map(|i| &i.takes) {
        if let Some(file) = take.source.file() {
            let _ = pool.load(&scythe_core::rpp::resolve_media_path(&project, file), rate);
        }
    }
    pool.prepare_project(&project);
    let range = scythe_core::TimeRange::new(0.0, project.length());
    let encoding = crate::bounce::Encoding::wav(crate::bounce::BitDepth::Float32);
    let result = crate::bounce::bounce(&project, &pool, range, rate, 2, encoding, &subproject_proxy(path), |_| {});
    RENDERING.lock().unwrap().retain(|p| p != path);
    result.map_err(|e| decode_err(format!("rendering the subproject: {e}")))
}

fn extension(path: &Path) -> Option<String> {
    path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase)
}

/// An Ogg Opus file decoded at 48 kHz the way REAPER plays it (`scythe_decode::decode_opus`).
fn decode_opus(path: &Path) -> Result<AudioData, MediaError> {
    from_decoded(path, scythe_decode::decode_opus(path))
}

/// The first audio stream of a video or WavPack file, decoded by the `ffmpeg` command-line tool.
/// REAPER reads video files with FFmpeg, which applies their edit lists (an AAC encoder's priming
/// samples left out); Symphonia does not. FFmpeg's WavPack decoder is lossless and matches
/// REAPER bit for bit, but it ignores a hybrid file's `.wvc` correction file, which REAPER applies.
fn decode_with_ffmpeg(path: &Path) -> Result<AudioData, MediaError> {
    let decode_err = |message: String| MediaError::Decode { path: path.to_owned(), message };
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let wav = std::env::temp_dir().join(format!("scythe-video-audio-{}-{n}.wav", std::process::id()));
    let output = crate::process::command("ffmpeg")
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

/// Decodes a file the way ReaSamplOmatic5000 loads a sample: held to 16-bit range. MP3 plays
/// gapless, as in a media item.
pub fn decode_sample(path: &Path) -> Result<AudioData, MediaError> {
    decode_audio(path, true)
}

fn decode(path: &Path, sampler: bool) -> Result<AudioData, MediaError> {
    from_decoded(path, scythe_decode::decode(path, sampler))
}

fn from_decoded(
    path: &Path,
    decoded: Result<scythe_decode::Audio, scythe_decode::Error>,
) -> Result<AudioData, MediaError> {
    match decoded {
        Ok(audio) => Ok(AudioData { sample_rate: audio.sample_rate, channels: audio.channels }),
        Err(scythe_decode::Error::Open(source)) => Err(MediaError::Open { path: path.to_owned(), source }),
        Err(scythe_decode::Error::Decode(message)) => Err(MediaError::Decode { path: path.to_owned(), message }),
    }
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
pub(crate) struct SincKernel {
    half: usize,
    cutoff: f64,
    /// The kernel from its centre outwards, `OVERSAMPLE` steps per input frame.
    table: Option<Vec<f64>>,
}

impl SincKernel {
    const OVERSAMPLE: usize = 32;

    /// A kernel for reading the input `step` frames apart.
    pub(crate) fn new(step: f64, half: usize, tabled: bool) -> Self {
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

    pub(crate) fn sample(&self, input: &[f32], pos: f64) -> f32 {
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
        use rayon::prelude::*;
        const CHUNK: usize = 1 << 16;
        let mut out = vec![0.0; frames];
        out.par_chunks_mut(CHUNK).enumerate().for_each(|(i, chunk)| {
            self.convert_into(kernel, input, from, to, (i * CHUNK) as u64, chunk);
        });
        out
    }

    /// Output frames `start..` into `out`. Away from the input's ends, frames go four at a
    /// time with an accumulator each: every sum still adds its taps in order, so the samples
    /// match converting one frame at a time, without each addition waiting on the last.
    fn convert_into(&self, kernel: &SincKernel, input: &[f32], from: u64, to: u64, start: u64, out: &mut [f32]) {
        let width = 2 * kernel.half;
        let place = |j: u64| ((j * from / to) as i64 - kernel.half as i64 + 1, j * from % to);
        let row = |rem: u64| &self.weights[(rem / self.unit) as usize * width..][..width];
        let one = |j: u64| {
            let (first, rem) = place(j);
            if self.weights.is_empty() {
                return kernel.sample(input, (first + kernel.half as i64 - 1) as f64 + rem as f64 / to as f64);
            }
            let row = row(rem);
            let lo = (-first).max(0) as usize;
            let hi = (input.len() as i64 - first).clamp(0, width as i64) as usize;
            let mut acc = 0.0f64;
            for k in lo..hi {
                acc += input[(first + k as i64) as usize] as f64 * row[k];
            }
            acc as f32
        };
        let inside = |(first, _): (i64, u64)| first >= 0 && first as usize + width <= input.len();
        // Each input frame is read by many output frames; widening it once is cheaper.
        let span = place(start).0.clamp(0, input.len() as i64) as usize
            ..(place(start + out.len() as u64).0 + width as i64).clamp(0, input.len() as i64) as usize;
        let wide: Vec<f64> = input[span.clone()].iter().map(|&x| x as f64).collect();
        let mut j = start;
        let (quads, rest) = out.as_chunks_mut::<4>();
        for quad in quads {
            let places: [(i64, u64); 4] = std::array::from_fn(|l| place(j + l as u64));
            if self.weights.is_empty() || !places.into_iter().all(inside) {
                for (l, s) in quad.iter_mut().enumerate() {
                    *s = one(j + l as u64);
                }
            } else {
                let [r0, r1, r2, r3] = places.map(|(_, rem)| row(rem));
                let [x0, x1, x2, x3] = places.map(|(first, _)| &wide[first as usize - span.start..][..width]);
                let (mut a0, mut a1, mut a2, mut a3) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
                for k in 0..width {
                    a0 += x0[k] * r0[k];
                    a1 += x1[k] * r1[k];
                    a2 += x2[k] * r2[k];
                    a3 += x3[k] * r3[k];
                }
                *quad = [a0 as f32, a1 as f32, a2 as f32, a3 as f32];
            }
            j += 4;
        }
        for s in rest {
            *s = one(j);
            j += 1;
        }
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
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
    /// `min` and `max` over 2, 4, 8 … peaks, so a range spanning many peaks (a zoomed-out
    /// waveform reads thousands per pixel column) takes a few reads rather than all of them.
    coarser: Vec<(Vec<f32>, Vec<f32>)>,
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
        let mut coarser: Vec<(Vec<f32>, Vec<f32>)> = Vec::new();
        loop {
            let (lo, hi) = coarser.last().map_or((&min, &max), |(l, h)| (l, h));
            if lo.len() <= 1 {
                break;
            }
            let halve =
                |v: &[f32], f: fn(f32, f32) -> f32| v.chunks(2).map(|p| p.iter().copied().fold(p[0], f)).collect();
            let level = (halve(lo, f32::min), halve(hi, f32::max));
            coarser.push(level);
        }
        Self { frames_per_peak, min, max, coarser }
    }

    /// Min/max over the frame range `[from, to)` of the source.
    pub fn range(&self, from: f64, to: f64) -> (f32, f32) {
        let a = (from / self.frames_per_peak as f64).floor().max(0.0) as usize;
        let b = ((to / self.frames_per_peak as f64).ceil() as usize).min(self.min.len());
        if a >= b {
            return (0.0, 0.0);
        }
        // Peaks at the ends of the range one by one, the whole pairs between them from the
        // next level up.
        let (mut a, mut b) = (a, b);
        let (mut lo, mut hi) = (0f32, 0f32);
        let mut level = 0;
        while a < b {
            let (mins, maxs) = if level == 0 {
                (&self.min, &self.max)
            } else {
                (&self.coarser[level - 1].0, &self.coarser[level - 1].1)
            };
            if a % 2 == 1 {
                lo = lo.min(mins[a]);
                hi = hi.max(maxs[a]);
                a += 1;
            }
            if b % 2 == 1 && a < b {
                b -= 1;
                lo = lo.min(mins[b]);
                hi = hi.max(maxs[b]);
            }
            if a >= b {
                break;
            }
            if level == self.coarser.len() {
                lo = mins[a..b].iter().copied().fold(lo, f32::min);
                hi = maxs[a..b].iter().copied().fold(hi, f32::max);
                break;
            }
            a /= 2;
            b /= 2;
            level += 1;
        }
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

/// The threads media decodes and converts on: one core is left free and they run at a lower
/// priority, so loading a set while it plays leaves the audio and the UI their time.
fn media_threads() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
        rayon::ThreadPoolBuilder::new()
            .num_threads(cores.saturating_sub(1).max(1))
            .thread_name(|i| format!("scythe-media-{i}"))
            // Per thread on Linux only; elsewhere it would lower the whole process.
            .start_handler(|_| {
                #[cfg(target_os = "linux")]
                unsafe {
                    libc::setpriority(libc::PRIO_PROCESS, 0, 10);
                }
            })
            .build()
            .expect("spawn media threads")
    })
}

/// Decoded media keyed by path. Decoding is slow, so `load` is meant for worker threads; the
/// audio thread only ever sees `Arc<AudioData>` handed over in a render graph.
#[derive(Debug, Default)]
pub struct MediaPool {
    entries: Mutex<HashMap<PathBuf, MediaState>>,
    /// Each entry's file as it was when read, to notice it written anew under the same name.
    stamps: Mutex<HashMap<PathBuf, Stamp>>,
    /// Stretched, pitch-shifted or rate-changed copies, keyed by file and `Processing::key`.
    processed: Mutex<HashMap<ProcessedKey, Arc<AudioData>>>,
    /// Takes with stretch markers or a pitch envelope, rendered for their item, keyed by file
    /// and `Warp::key`.
    warped: Mutex<HashMap<(PathBuf, u64), Arc<AudioData>>>,
    /// How files at another rate are converted to the engine's, and how takes play at another
    /// rate without keeping their pitch.
    resample: ResampleMode,
}

type ProcessedKey = (PathBuf, (u64, u64, u64, i32));

/// A file's modification time and size; `None` while it does not exist.
type Stamp = Option<(std::time::SystemTime, u64)>;

fn stamp(path: &Path) -> Stamp {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().unwrap_or(std::time::UNIX_EPOCH), meta.len()))
}

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
        let audio = media_threads().install(|| {
            if processing.shifts() {
                let shifted = crate::stretch::apply(&base.audio, processing);
                if processing.rate == 1.0 {
                    Some(shifted)
                } else {
                    resample_rate(&shifted, rate as f64 * processing.rate, rate, self.resample).ok()
                }
            } else if base.source_rate == rate {
                resample_rate(&base.audio, rate as f64 * processing.rate, rate, self.resample).ok()
            } else {
                // One conversion from the file's own rate, as REAPER does, rather than two.
                let raw = decode_file(path).ok()?;
                resample_rate(&raw, raw.sample_rate as f64 * processing.rate, rate, self.resample).ok()
            }
        })?;
        let audio = Arc::new(audio);
        self.processed.lock().unwrap().insert((path.to_owned(), processing.key()), audio.clone());
        Some(audio)
    }

    /// The take rendered as `warp` says, once prepared.
    pub fn warped(&self, path: &Path, warp: &crate::warp::Warp) -> Option<Arc<AudioData>> {
        self.warped.lock().unwrap().get(&(path.to_owned(), warp.key())).cloned()
    }

    /// Renders a warped take (slow; for worker threads). `None` until the file is loaded.
    pub fn prepare_warped(&self, path: &Path, warp: &crate::warp::Warp) -> Option<Arc<AudioData>> {
        if let Some(done) = self.warped(path, warp) {
            return Some(done);
        }
        let base = self.ready(path)?;
        let audio = Arc::new(media_threads().install(|| crate::warp::render(&base.audio, warp, self.resample)));
        self.warped.lock().unwrap().insert((path.to_owned(), warp.key()), audio.clone());
        Some(audio)
    }

    /// Prepares every processed copy the project's takes need; their files must be loaded.
    pub fn prepare_project(&self, project: &scythe_core::Project) {
        // Renders play the project at its play rate, which changes how takes are processed.
        let played = project.at_playrate();
        for project in [project, played.as_ref()] {
            for item in project.tracks.iter().flat_map(|t| &t.items) {
                for take in &item.takes {
                    let Some(file) = take.source.file() else { continue };
                    let path = scythe_core::rpp::resolve_media_path(project, file);
                    if let Some(processing) = take.processing(project.default_pitch_mode) {
                        self.prepare_processed(&path, &processing);
                    }
                    if let Some(warp) = crate::warp::Warp::of(item, take, project.default_pitch_mode) {
                        self.prepare_warped(&path, &warp);
                    }
                }
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

    /// Decodes `path` (slow; for worker threads), or answers the audio read before when the file
    /// has not changed since.
    pub fn load(&self, path: &Path, engine_rate: u32) -> Result<Arc<Media>, MediaError> {
        self.refresh(path);
        if let Some(m) = self.ready(path) {
            return Ok(m);
        }
        let stamp = stamp(path);
        let result = media_threads().install(|| -> Result<_, MediaError> {
            let raw = decode_file(path)?;
            let audio = resample_with(&raw, engine_rate, self.resample)?;
            let peaks = Peaks::compute(&audio, Peaks::DEFAULT_RESOLUTION);
            Ok(Arc::new(Media { path: path.to_owned(), source_rate: raw.sample_rate, audio: Arc::new(audio), peaks }))
        });
        let state = match &result {
            Ok(m) => MediaState::Ready(m.clone()),
            Err(e) => MediaState::Failed(e.to_string()),
        };
        self.entries.lock().unwrap().insert(path.to_owned(), state);
        self.stamps.lock().unwrap().insert(path.to_owned(), stamp);
        result
    }

    /// Forgets a file whose modification time or size changed since it was read (written anew
    /// under the same name, or appeared or vanished), so the next load reads it again. Answers
    /// whether it did.
    pub fn refresh(&self, path: &Path) -> bool {
        let Some(read) = self.stamps.lock().unwrap().get(path).copied() else { return false };
        if read == stamp(path) {
            return false;
        }
        self.forget(path);
        true
    }

    /// Drops a file's decoded audio and its processed copies, so the next load reads it again
    /// (a subproject whose proxy was rendered anew).
    pub fn forget(&self, path: &Path) {
        self.entries.lock().unwrap().remove(path);
        self.stamps.lock().unwrap().remove(path);
        self.processed.lock().unwrap().retain(|k, _| k.0 != path);
        self.warped.lock().unwrap().retain(|k, _| k.0 != path);
    }

    pub fn clear(&self) {
        self.entries.lock().unwrap().clear();
        self.stamps.lock().unwrap().clear();
        self.processed.lock().unwrap().clear();
        self.warped.lock().unwrap().clear();
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
    fn decodes_ogg_opus_without_its_pre_skip() {
        use opus_pure::{Application, MAX_PACKET_BYTES, OggOpusWriter, OpusEncoder, OpusHead};
        let dir = std::env::temp_dir().join(format!("scythe-opus-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone.opus");
        let tone = sine(48000, 440.0, 0.4);
        let mut encoder = OpusEncoder::new(48000, 1, Application::Audio).unwrap();
        let mut head = OpusHead::for_encoder(&encoder, 48000);
        head.output_gain_q8 = -1536;
        let pre_skip = usize::from(head.pre_skip);
        let mut writer = OggOpusWriter::new(std::fs::File::create(&path).unwrap(), head).unwrap();
        let mut packet = vec![0u8; MAX_PACKET_BYTES];
        for block in tone.as_chunks::<960>().0 {
            let n = encoder.encode(block, 960, &mut packet).unwrap();
            writer.write_packet(&packet[..n]).unwrap();
        }
        writer.finish().unwrap();

        let data = decode_file(&path).unwrap();
        assert_eq!(data.sample_rate, 48000);
        assert_eq!(data.frames(), tone.len() - pre_skip);
        let half = 10f32.powf(-6.0 / 20.0);
        let err = data.channels[0][4800..14400]
            .iter()
            .zip(&tone[4800..])
            .map(|(a, b)| (a - b * half).abs())
            .fold(0.0, f32::max);
        assert!(err < 0.05, "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_subproject_plays_its_proxy_and_renders_a_missing_one() {
        let dir = std::env::temp_dir().join(format!("scythe-subproject-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tone = sine(44100, 440.0, 0.5);
        write_wav(&dir.join("tone.wav"), 44100, &[tone.clone(), tone.clone()]);
        let sub = dir.join("sub.RPP");
        let text = "<REAPER_PROJECT 0.1 \"7.73\" 0\n  SAMPLERATE 44100 1 0\n  MASTER_VOLUME 0.5 0 -1 -1 1\n  <TRACK\n    <ITEM\n      POSITION 0.25\n      LENGTH 0.5\n      FADEIN 1 0 0 1 0 0 0\n      FADEOUT 1 0 0 1 0 0 0\n      <SOURCE WAVE\n        FILE \"tone.wav\"\n      >\n    >\n  >\n>\n";
        std::fs::write(&sub, text).unwrap();

        let data = decode_file(&sub).unwrap();
        assert!(subproject_proxy(&sub).exists());
        assert_eq!((data.sample_rate, data.frames()), (44100, 33075));
        assert_eq!(data.channels[0][..11025].iter().fold(0.0f32, |m, s| m.max(s.abs())), 0.0);
        assert!((data.channels[0][11025 + 100] - tone[100] * 0.5).abs() < 1e-6);

        write_wav(&subproject_proxy(&sub), 44100, &[vec![0.25; 10], vec![0.25; 10]]);
        assert_eq!(decode_file(&sub).unwrap().frames(), 10);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_is_an_error() {
        assert!(matches!(decode_file(Path::new("/nonexistent/x.wav")), Err(MediaError::Open { .. })));
    }

    #[test]
    fn sinc_conversion_matches_one_frame_at_a_time() {
        let input: Vec<f32> =
            (0..150_001u32).map(|i| ((i.wrapping_mul(2_654_435_761) >> 8) as f32 / 8e6) - 1.0).collect();
        for (half, from, to) in [(32, 441, 480), (96, 480, 441), (8, 3, 7)] {
            let kernel = SincKernel::new(from as f64 / to as f64, half, true);
            let phases = PhaseTable::new(&kernel, from, to);
            let frames = input.len() * to as usize / from as usize;
            let whole = phases.convert(&kernel, &input, from, to, frames);
            for j in (0..frames).step_by(7).chain(frames - 300..frames) {
                let mut one = [0.0];
                phases.convert_into(&kernel, &input, from, to, j as u64, &mut one);
                assert_eq!(whole[j].to_bits(), one[0].to_bits(), "frame {j} of {from} to {to}");
            }
        }
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
    fn peaks_track_envelope() {
        let data = AudioData { sample_rate: 1000, channels: vec![vec![0.0, 0.5, -0.25, 1.0, 0.0, 0.0]] };
        let p = Peaks::compute(&data, 2);
        assert_eq!(p.max, [0.5, 1.0, 0.0]);
        assert_eq!(p.min, [0.0, -0.25, 0.0]);
        assert_eq!(p.range(0.0, 4.0), (-0.25, 1.0));
    }

    #[test]
    fn peak_ranges_read_from_coarser_levels_match_every_peak() {
        let samples: Vec<f32> = (0..9973).map(|i| ((i * 7919 % 2003) as f32 / 1001.5) - 1.0).collect();
        let data = AudioData { sample_rate: 1000, channels: vec![samples] };
        let p = Peaks::compute(&data, 3);
        let n = p.min.len();
        for (a, b) in [(0, n), (1, n), (0, n - 1), (5, 6), (7, 300), (1000, 3001), (n - 2, n), (4, 4), (17, 1024)] {
            let lo = p.min[a..b].iter().copied().fold(0.0, f32::min);
            let hi = p.max[a..b].iter().copied().fold(0.0, f32::max);
            let got = p.range((a * 3) as f64, (b * 3) as f64);
            let want = if a < b { (lo, hi) } else { (0.0, 0.0) };
            assert_eq!(got, want, "peaks {a}..{b}");
        }
    }
}
