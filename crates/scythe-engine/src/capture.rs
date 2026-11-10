//! The device's one capture stream. Monitoring and recording share it, as an ALSA `hw:` device
//! opens for capture only once: the callback feeds the monitoring ring the audio thread reads
//! and, while a recording runs, the ring its writer empties. A channel map (REAPER's input
//! remapping) decides which hardware channel each input slot carries.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

use crate::DeviceRequest;

/// Where a recording takes the input from: the ring the capture callback fills.
pub struct RecordFeed {
    pub producer: rtrb::Producer<f32>,
    /// Samples dropped because the writer fell behind.
    pub overruns: Arc<AtomicU64>,
}

/// The slot a recording's feed goes into; empty while nothing records.
pub type RecordSlot = Arc<Mutex<Option<RecordFeed>>>;

/// What a recording needs from the capture stream.
pub struct InputTap {
    pub consumer: rtrb::Consumer<f32>,
    /// Interleaved input slots per frame.
    pub channels: usize,
    pub sample_rate: u32,
    pub device: String,
    pub overruns: Arc<AtomicU64>,
    /// Seconds from the converter to the callback, as f64 bits.
    pub latency: Arc<AtomicU64>,
    /// Emptied when the recording ends, which stops the feed.
    pub slot: RecordSlot,
}

pub struct Capture {
    _stream: cpal::Stream,
    /// Input slots: the map's length, else the hardware channels opened.
    pub channels: usize,
    pub sample_rate: u32,
    pub device: String,
    record: RecordSlot,
    latency: Arc<AtomicU64>,
}

impl Capture {
    /// Opens the input `devices` names at `rate`, and the ring monitoring reads. `map` gives each
    /// input slot its hardware channel (-1 for none).
    pub fn open(
        devices: &DeviceRequest,
        rate: u32,
        map: Option<&[i32]>,
    ) -> Result<(Self, rtrb::Consumer<f32>), String> {
        let device = crate::devices::input_device(devices)?;
        let name = crate::devices::device_name(&device);
        let (config, format) = crate::devices::input_config(&device, devices, rate)?;
        let hardware = config.channels as usize;
        let map: Vec<i32> = match map {
            Some(m) => m.to_vec(),
            None => (0..hardware as i32).collect(),
        };
        let channels = map.len();
        let (monitor, heard) = rtrb::RingBuffer::<f32>::new((rate as usize * channels.max(1) / 2).max(1024));
        let record: RecordSlot = Arc::default();
        let latency = Arc::new(AtomicU64::new(0));
        let feed = Feed { map, hardware, monitor, record: record.clone(), latency: latency.clone() };
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, &config, feed),
            SampleFormat::I16 => build::<i16>(&device, &config, feed),
            SampleFormat::I24 => build::<cpal::I24>(&device, &config, feed),
            SampleFormat::I32 => build::<i32>(&device, &config, feed),
            SampleFormat::U16 => build::<u16>(&device, &config, feed),
            SampleFormat::F64 => build::<f64>(&device, &config, feed),
            other => Err(format!("unsupported input sample format {other}")),
        }
        .map_err(|e| format!("input at {rate} Hz: {e}"))?;
        stream.play().map_err(|e| e.to_string())?;
        if devices.jack_no_connect {
            crate::devices::jack_disconnect(&format!("cpal_client_{}_in", std::process::id()));
        }
        Ok((Self { _stream: stream, channels, sample_rate: rate, device: name, record, latency }, heard))
    }

    /// Starts feeding a recording; `None` when one already records.
    pub fn tap(&self) -> Option<InputTap> {
        let mut slot = self.record.lock().ok()?;
        if slot.is_some() {
            return None;
        }
        let (producer, consumer) = rtrb::RingBuffer::<f32>::new(self.sample_rate as usize * self.channels.max(1) * 4);
        let overruns = Arc::new(AtomicU64::new(0));
        *slot = Some(RecordFeed { producer, overruns: overruns.clone() });
        Some(InputTap {
            consumer,
            channels: self.channels,
            sample_rate: self.sample_rate,
            device: self.device.clone(),
            overruns,
            latency: self.latency.clone(),
            slot: self.record.clone(),
        })
    }

    /// Seconds from the converter to the last callback, as the device reports it.
    pub fn latency(&self) -> f64 {
        f64::from_bits(self.latency.load(Ordering::Relaxed))
    }

    /// Whether a recording takes the input now.
    pub fn recording(&self) -> bool {
        self.record.lock().map(|s| s.is_some()).unwrap_or(false)
    }
}

struct Feed {
    map: Vec<i32>,
    hardware: usize,
    monitor: rtrb::Producer<f32>,
    record: RecordSlot,
    latency: Arc<AtomicU64>,
}

impl Feed {
    fn push(&mut self, data: &[f32]) {
        // Only contended while a recording starts or ends.
        let mut record = self.record.try_lock().ok();
        let mut feed = record.as_mut().and_then(|g| g.as_mut());
        for frame in data.chunks(self.hardware.max(1)) {
            for &hw in &self.map {
                let s = usize::try_from(hw).ok().and_then(|c| frame.get(c)).copied().unwrap_or(0.0);
                let _ = self.monitor.push(s);
                if let Some(f) = feed.as_mut()
                    && f.producer.push(s).is_err()
                {
                    f.overruns.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
}

fn build<T>(device: &cpal::Device, config: &cpal::StreamConfig, mut feed: Feed) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let mut scratch: Vec<f32> = Vec::with_capacity(16_384);
    device
        .build_input_stream(
            *config,
            move |data: &[T], info: &cpal::InputCallbackInfo| {
                let t = info.timestamp();
                let latency = t.callback.duration_since(t.capture);
                feed.latency.store(latency.as_secs_f64().to_bits(), Ordering::Relaxed);
                scratch.clear();
                scratch.extend(data.iter().map(|s| s.to_sample::<f32>()));
                feed.push(&scratch);
            },
            |e| eprintln!("input stream error: {e}"),
            None,
        )
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_map_picks_each_slots_hardware_channel_for_monitoring_and_recording() {
        let (monitor, mut heard) = rtrb::RingBuffer::<f32>::new(64);
        let record: RecordSlot = Arc::default();
        let (producer, mut recorded) = rtrb::RingBuffer::<f32>::new(64);
        let overruns = Arc::new(AtomicU64::new(0));
        let mut feed =
            Feed { map: vec![2, 1, -1], hardware: 3, monitor, record: record.clone(), latency: Arc::default() };
        feed.push(&[0.1, 0.2, 0.3]);
        *record.lock().unwrap() = Some(RecordFeed { producer, overruns });
        feed.push(&[0.4, 0.5, 0.6]);
        let heard: Vec<f32> = std::iter::from_fn(|| heard.pop().ok()).collect();
        assert_eq!(heard, [0.3, 0.2, 0.0, 0.6, 0.5, 0.0]);
        let recorded: Vec<f32> = std::iter::from_fn(|| recorded.pop().ok()).collect();
        assert_eq!(recorded, [0.6, 0.5, 0.0], "the recording hears from when it starts");
    }
}
