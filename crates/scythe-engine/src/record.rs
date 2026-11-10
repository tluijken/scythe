use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use scythe_core::TrackId;
use scythe_core::recording::RecordTap;

/// What one armed track records: which input channels (one for mono, two for stereo) and where
/// the file goes.
#[derive(Debug, Clone)]
pub struct RecordTarget {
    pub track: TrackId,
    pub channels: Vec<usize>,
    pub path: PathBuf,
}

impl RecordTarget {
    /// Input channels for REAPER's `I_RECINPUT` encoding: the low bits pick the first hardware
    /// channel and 1024 makes it a stereo pair. Other input kinds fall back to input 1.
    pub fn channels_for_input(rec_input: i64) -> Vec<usize> {
        if rec_input < 0 || rec_input & (4096 | 2048 | 512) != 0 {
            return vec![0];
        }
        let first = (rec_input & 511) as usize;
        if rec_input & 1024 != 0 { vec![first, first + 1] } else { vec![first] }
    }
}

#[derive(Debug, Clone)]
pub struct RecordedTake {
    pub track: TrackId,
    pub path: PathBuf,
    pub frames: u64,
    pub sample_rate: u32,
    /// How long the input took from the converter to the callback, as the device reports it.
    pub input_latency: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("no audio input device")]
    NoDevice,
    #[error("input device: {0}")]
    Device(String),
    #[error(transparent)]
    Wav(#[from] hound::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// A running recording. Input samples travel from the device callback through a lock-free ring
/// buffer to a writer thread, so the callback never touches the disk.
pub struct Recording {
    _stream: cpal::Stream,
    stop: Arc<AtomicBool>,
    writer: Option<JoinHandle<Result<Vec<RecordedTake>, RecordError>>>,
    /// Peak per target since the last `take_levels`, as f32 bits.
    levels: Arc<Vec<AtomicU32>>,
    overruns: Arc<AtomicU64>,
    /// Input latency in seconds, as f64 bits.
    input_latency: Arc<AtomicU64>,
    pub sample_rate: u32,
    pub device: String,
}

impl Recording {
    /// Opens the default input and starts writing one WAV file per target. `preferred_rate` is
    /// used when the device supports it, so recordings need no resampling.
    pub fn start(targets: Vec<RecordTarget>, preferred_rate: u32) -> Result<Self, RecordError> {
        Self::start_on(&crate::DeviceRequest::default(), targets, preferred_rate)
    }

    /// Like [`Recording::start`], from the input `devices` names.
    pub fn start_on(
        devices: &crate::DeviceRequest,
        targets: Vec<RecordTarget>,
        preferred_rate: u32,
    ) -> Result<Self, RecordError> {
        let device = crate::devices::input_device(devices).map_err(RecordError::Device)?;
        let name = device.description().map(|d| d.name().to_owned()).unwrap_or_else(|_| "input".into());
        let (config, format) = match crate::devices::input_config(&device, devices.input_channels, preferred_rate) {
            Ok(found) => found,
            Err(_) => {
                let default = device.default_input_config().map_err(|e| RecordError::Device(e.to_string()))?;
                (default.config(), default.sample_format())
            }
        };
        let in_channels = config.channels as usize;
        let sample_rate = config.sample_rate;

        let (producer, mut consumer) = rtrb::RingBuffer::<f32>::new(sample_rate as usize * in_channels * 4);
        let stop = Arc::new(AtomicBool::new(false));
        let levels: Arc<Vec<AtomicU32>> = Arc::new(targets.iter().map(|_| AtomicU32::new(0)).collect());
        let overruns = Arc::new(AtomicU64::new(0));
        let input_latency = Arc::new(AtomicU64::new(0));

        let mut writers = Vec::new();
        for t in &targets {
            if let Some(dir) = t.path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let spec = hound::WavSpec {
                channels: t.channels.len() as u16,
                sample_rate,
                bits_per_sample: 24,
                sample_format: hound::SampleFormat::Int,
            };
            writers.push(hound::WavWriter::create(&t.path, spec)?);
        }

        let stop_w = stop.clone();
        let levels_w = levels.clone();
        let writer = std::thread::Builder::new().name("scythe-record".into()).spawn(move || {
            let mut frames = 0u64;
            let mut frame = vec![0.0f32; in_channels];
            loop {
                let stopping = stop_w.load(Ordering::Acquire);
                while consumer.slots() >= in_channels {
                    for s in frame.iter_mut() {
                        *s = consumer.pop().unwrap_or(0.0);
                    }
                    for (i, (t, w)) in targets.iter().zip(&mut writers).enumerate() {
                        let mut peak = 0.0f32;
                        for &c in &t.channels {
                            let s = frame.get(c).copied().unwrap_or(0.0);
                            peak = peak.max(s.abs());
                            w.write_sample((s.clamp(-1.0, 1.0) * 8_388_607.0) as i32)?;
                        }
                        let old = f32::from_bits(levels_w[i].load(Ordering::Relaxed));
                        if peak > old {
                            levels_w[i].store(peak.to_bits(), Ordering::Relaxed);
                        }
                    }
                    frames += 1;
                }
                if stopping {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let mut out = Vec::new();
            for (t, w) in targets.into_iter().zip(writers) {
                w.finalize()?;
                out.push(RecordedTake { track: t.track, path: t.path, frames, sample_rate, input_latency: 0.0 });
            }
            Ok(out)
        })?;

        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, &config, producer, overruns.clone(), input_latency.clone()),
            SampleFormat::I16 => build::<i16>(&device, &config, producer, overruns.clone(), input_latency.clone()),
            SampleFormat::I32 => build::<i32>(&device, &config, producer, overruns.clone(), input_latency.clone()),
            SampleFormat::U16 => build::<u16>(&device, &config, producer, overruns.clone(), input_latency.clone()),
            other => Err(RecordError::Device(format!("unsupported sample format {other}"))),
        };
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                stop.store(true, Ordering::Release);
                return Err(e);
            }
        };
        stream.play().map_err(|e| RecordError::Device(e.to_string()))?;
        Ok(Self {
            _stream: stream,
            stop,
            writer: Some(writer),
            levels,
            overruns,
            input_latency,
            sample_rate,
            device: name,
        })
    }

    /// Peak level per target since the previous call.
    pub fn take_levels(&self) -> Vec<f32> {
        self.levels.iter().map(|l| f32::from_bits(l.swap(0, Ordering::Relaxed))).collect()
    }

    /// Samples dropped because the writer fell behind.
    pub fn overruns(&self) -> u64 {
        self.overruns.load(Ordering::Relaxed)
    }

    /// Stops the input, flushes the files and returns what was recorded.
    pub fn finish(mut self) -> Result<Vec<RecordedTake>, RecordError> {
        drop(self._stream);
        self.stop.store(true, Ordering::Release);
        let latency = f64::from_bits(self.input_latency.load(Ordering::Relaxed));
        match self.writer.take().map(|w| w.join()) {
            Some(Ok(result)) => {
                result.map(|takes| takes.into_iter().map(|t| RecordedTake { input_latency: latency, ..t }).collect())
            }
            _ => Err(RecordError::Device("writer thread panicked".into())),
        }
    }
}

/// Where an output recording taps a track, and the ring its samples go through to the writer.
/// The render graph writes into it from the audio thread while a recording runs.
pub struct OutputTap {
    pub point: RecordTap,
    /// Stamp the audio with the project time it belongs to after the track's effects (latency
    /// compensated), rather than the time it entered them.
    pub compensated: bool,
    /// 1 writes the average of left and right, 2 the pair, more every channel of the track.
    channels: usize,
    ring: Mutex<rtrb::Producer<f32>>,
    /// Project frame of the first sample written, `i64::MIN` until then.
    first_frame: AtomicI64,
    dropped: AtomicU64,
}

impl std::fmt::Debug for OutputTap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputTap").field("point", &self.point).field("channels", &self.channels).finish()
    }
}

impl OutputTap {
    /// Writes one block whose first frame belongs at project frame `frame`.
    pub(crate) fn write(&self, frame: i64, l: &[f32], r: &[f32], aux: Option<&[Vec<f32>]>) {
        let n = l.len().min(r.len());
        let Ok(mut ring) = self.ring.try_lock() else {
            self.dropped.fetch_add(n as u64, Ordering::Relaxed);
            return;
        };
        if ring.slots() < n * self.channels {
            self.dropped.fetch_add(n as u64, Ordering::Relaxed);
            return;
        }
        let _ = self.first_frame.compare_exchange(i64::MIN, frame, Ordering::Relaxed, Ordering::Relaxed);
        for i in 0..n {
            match self.channels {
                1 => {
                    let _ = ring.push((l[i] + r[i]) * 0.5);
                }
                c => {
                    let _ = ring.push(l[i]);
                    let _ = ring.push(r[i]);
                    for k in 0..c.saturating_sub(2) {
                        let _ = ring.push(aux.and_then(|a| a.get(k)).and_then(|b| b.get(i)).copied().unwrap_or(0.0));
                    }
                }
            }
        }
    }
}

/// One track whose own signal is recorded.
#[derive(Debug, Clone)]
pub struct OutputTarget {
    pub track: TrackId,
    pub point: RecordTap,
    pub compensated: bool,
    /// 1 for mono, 2 for stereo, more for multichannel.
    pub channels: usize,
    pub path: PathBuf,
}

/// A recording of track outputs: the render graph fills the taps, a writer thread empties them
/// into one WAV file per track.
pub struct OutputRecording {
    taps: Vec<(TrackId, Arc<OutputTap>)>,
    stop: Arc<AtomicBool>,
    writer: Option<JoinHandle<Result<Vec<Written>, RecordError>>>,
    sample_rate: u32,
}

/// A closed output file: its track, path and frames.
type Written = (TrackId, PathBuf, u64);

/// A finished output recording of one track.
#[derive(Debug, Clone)]
pub struct RecordedOutput {
    pub track: TrackId,
    pub path: PathBuf,
    pub frames: u64,
    pub sample_rate: u32,
    /// Project time of the first frame, `None` when nothing was written.
    pub start: Option<f64>,
    /// Frames lost because the writer fell behind.
    pub dropped: u64,
}

impl OutputRecording {
    /// Creates the taps and files and starts the writer. The taps take effect once the engine
    /// installs them in its next graph (`Engine::start_output_recording`).
    pub fn start(targets: Vec<OutputTarget>, sample_rate: u32) -> Result<Self, RecordError> {
        let mut taps = Vec::new();
        let mut sinks = Vec::new();
        for t in targets {
            if let Some(dir) = t.path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let channels = t.channels.max(1);
            let spec = hound::WavSpec {
                channels: channels as u16,
                sample_rate,
                bits_per_sample: 24,
                sample_format: hound::SampleFormat::Int,
            };
            let writer = hound::WavWriter::create(&t.path, spec)?;
            let (producer, consumer) = rtrb::RingBuffer::<f32>::new(sample_rate as usize * channels * 4);
            let tap = Arc::new(OutputTap {
                point: t.point,
                compensated: t.compensated,
                channels,
                ring: Mutex::new(producer),
                first_frame: AtomicI64::new(i64::MIN),
                dropped: AtomicU64::new(0),
            });
            taps.push((t.track, tap));
            sinks.push((t.track, t.path, consumer, writer, 0u64, channels));
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stop_w = stop.clone();
        let writer = std::thread::Builder::new().name("scythe-record-out".into()).spawn(move || {
            loop {
                let stopping = stop_w.load(Ordering::Acquire);
                for (_, _, consumer, writer, frames, channels) in &mut sinks {
                    while consumer.slots() >= *channels {
                        for _ in 0..*channels {
                            let s = consumer.pop().unwrap_or(0.0);
                            writer.write_sample((s.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32)?;
                        }
                        *frames += 1;
                    }
                }
                if stopping {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let mut out = Vec::new();
            for (track, path, _, writer, frames, _) in sinks {
                writer.finalize()?;
                out.push((track, path, frames));
            }
            Ok(out)
        })?;
        Ok(Self { taps, stop, writer: Some(writer), sample_rate })
    }

    pub fn taps(&self) -> &[(TrackId, Arc<OutputTap>)] {
        &self.taps
    }

    /// Flushes and closes the files. Call once the taps are out of the graph.
    pub fn finish(mut self) -> Result<Vec<RecordedOutput>, RecordError> {
        self.stop.store(true, Ordering::Release);
        let written = match self.writer.take().map(|w| w.join()) {
            Some(Ok(result)) => result?,
            _ => return Err(RecordError::Device("output writer thread panicked".into())),
        };
        let sr = self.sample_rate;
        Ok(written
            .into_iter()
            .map(|(track, path, frames)| {
                let tap = self.taps.iter().find(|(t, _)| *t == track).map(|(_, tap)| tap);
                let first = tap.map_or(i64::MIN, |t| t.first_frame.load(Ordering::Relaxed));
                RecordedOutput {
                    track,
                    path,
                    frames,
                    sample_rate: sr,
                    start: (first != i64::MIN).then(|| first as f64 / sr as f64),
                    dropped: tap.map_or(0, |t| t.dropped.load(Ordering::Relaxed)),
                }
            })
            .collect())
    }
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut producer: rtrb::Producer<f32>,
    overruns: Arc<AtomicU64>,
    input_latency: Arc<AtomicU64>,
) -> Result<cpal::Stream, RecordError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    device
        .build_input_stream(
            *config,
            move |data: &[T], info: &cpal::InputCallbackInfo| {
                let t = info.timestamp();
                let latency = t.callback.duration_since(t.capture);
                input_latency.store(latency.as_secs_f64().to_bits(), Ordering::Relaxed);
                for &s in data {
                    if producer.push(s.to_sample::<f32>()).is_err() {
                        overruns.fetch_add(1, Ordering::Relaxed);
                    }
                }
            },
            |e| eprintln!("input stream error: {e}"),
            None,
        )
        .map_err(|e| RecordError::Device(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_encoding() {
        assert_eq!(RecordTarget::channels_for_input(0), [0]);
        assert_eq!(RecordTarget::channels_for_input(3), [3]);
        assert_eq!(RecordTarget::channels_for_input(1024), [0, 1]);
        assert_eq!(RecordTarget::channels_for_input(1026), [2, 3]);
        assert_eq!(RecordTarget::channels_for_input(4096 + 63), [0]);
        assert_eq!(RecordTarget::channels_for_input(-1), [0]);
    }
}
