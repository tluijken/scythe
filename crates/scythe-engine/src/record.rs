use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use scythe_core::TrackId;

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
    pub sample_rate: u32,
    pub device: String,
}

impl Recording {
    /// Opens the default input and starts writing one WAV file per target. `preferred_rate` is
    /// used when the device supports it, so recordings need no resampling.
    pub fn start(targets: Vec<RecordTarget>, preferred_rate: u32) -> Result<Self, RecordError> {
        let host = cpal::default_host();
        let device = host.default_input_device().ok_or(RecordError::NoDevice)?;
        let name = device.description().map(|d| d.name().to_owned()).unwrap_or_else(|_| "input".into());
        let default = device.default_input_config().map_err(|e| RecordError::Device(e.to_string()))?;
        let format = default.sample_format();
        let mut config = default.config();
        let supports_preferred = device.supported_input_configs().is_ok_and(|mut cfgs| {
            cfgs.any(|c| {
                c.min_sample_rate() <= preferred_rate
                    && c.max_sample_rate() >= preferred_rate
                    && c.channels() == config.channels
            })
        });
        if supports_preferred {
            config.sample_rate = preferred_rate;
        }
        let in_channels = config.channels as usize;
        let sample_rate = config.sample_rate;

        let (producer, mut consumer) = rtrb::RingBuffer::<f32>::new(sample_rate as usize * in_channels * 4);
        let stop = Arc::new(AtomicBool::new(false));
        let levels: Arc<Vec<AtomicU32>> = Arc::new(targets.iter().map(|_| AtomicU32::new(0)).collect());
        let overruns = Arc::new(AtomicU64::new(0));

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
                out.push(RecordedTake { track: t.track, path: t.path, frames, sample_rate });
            }
            Ok(out)
        })?;

        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, &config, producer, overruns.clone()),
            SampleFormat::I16 => build::<i16>(&device, &config, producer, overruns.clone()),
            SampleFormat::I32 => build::<i32>(&device, &config, producer, overruns.clone()),
            SampleFormat::U16 => build::<u16>(&device, &config, producer, overruns.clone()),
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
        Ok(Self { _stream: stream, stop, writer: Some(writer), levels, overruns, sample_rate, device: name })
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
        match self.writer.take().map(|w| w.join()) {
            Some(Ok(result)) => result,
            _ => Err(RecordError::Device("writer thread panicked".into())),
        }
    }
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut producer: rtrb::Producer<f32>,
    overruns: Arc<AtomicU64>,
) -> Result<cpal::Stream, RecordError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    device
        .build_input_stream(
            *config,
            move |data: &[T], _| {
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
