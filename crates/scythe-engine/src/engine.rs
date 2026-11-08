use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use scythe_core::{Project, TimeRange};

use crate::fx::FxRegistry;
use crate::media::MediaPool;
use crate::midi_io::{LiveMidi, MidiInputs};
use crate::render::{LiveInput, MeterRegistry, RenderGraph, RenderScratch};

const NO_SEEK: i64 = i64::MIN;
/// Upper bound on tracks the audio callback preallocates scratch buffers for.
pub const MAX_TRACKS: usize = 512;

/// State shared with the audio thread. Everything here is lock-free.
#[derive(Debug)]
pub struct Transport {
    playing: AtomicBool,
    position: AtomicI64,
    seek: AtomicI64,
    loop_enabled: AtomicBool,
    loop_start: AtomicI64,
    loop_end: AtomicI64,
    /// Fraction of the callback period spent rendering, as f32 bits.
    load: AtomicU32,
    /// Frames during which effects keep running while stopped (tails, parameter changes).
    wake_frames: AtomicI64,
    /// Time from the output callback until its first frame is heard, as f64 bits (seconds).
    output_latency: AtomicU64,
    sample_rate: u32,
}

impl Transport {
    fn new(sample_rate: u32) -> Self {
        Self {
            playing: AtomicBool::new(false),
            position: AtomicI64::new(0),
            seek: AtomicI64::new(NO_SEEK),
            loop_enabled: AtomicBool::new(false),
            loop_start: AtomicI64::new(0),
            loop_end: AtomicI64::new(0),
            load: AtomicU32::new(0),
            wake_frames: AtomicI64::new(0),
            output_latency: AtomicU64::new(0),
            sample_rate,
        }
    }

    /// How long rendered audio takes to reach the speakers, as the device reports it.
    pub fn output_latency(&self) -> f64 {
        f64::from_bits(self.output_latency.load(Ordering::Relaxed))
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    pub fn position_seconds(&self) -> f64 {
        let pending = self.seek.load(Ordering::Relaxed);
        let frames = if pending != NO_SEEK { pending } else { self.position.load(Ordering::Relaxed) };
        frames as f64 / self.sample_rate as f64
    }

    pub fn load(&self) -> f32 {
        f32::from_bits(self.load.load(Ordering::Relaxed))
    }

    /// Advances the playhead over one block, honouring seeks and the loop range. Returns the
    /// segments `(project_frame, offset_in_block, len)` to render.
    fn advance(&self, frames: usize, segments: &mut [(i64, usize, usize); 8]) -> usize {
        let seek = self.seek.swap(NO_SEEK, Ordering::Relaxed);
        if seek != NO_SEEK {
            self.position.store(seek, Ordering::Relaxed);
        }
        if !self.is_playing() {
            return 0;
        }
        let mut pos = self.position.load(Ordering::Relaxed);
        let looping = self.loop_enabled.load(Ordering::Relaxed);
        let (ls, le) = (self.loop_start.load(Ordering::Relaxed), self.loop_end.load(Ordering::Relaxed));
        // Like REAPER, only crossing the loop end wraps; a playhead already past it plays on.
        let wraps = |p: i64| looping && le > ls && p < le;
        let mut done = 0;
        let mut count = 0;
        while done < frames && count < segments.len() {
            let mut len = frames - done;
            let will_wrap = wraps(pos);
            if will_wrap {
                len = len.min((le - pos) as usize);
            }
            segments[count] = (pos, done, len);
            count += 1;
            done += len;
            pos += len as i64;
            if will_wrap && pos == le {
                pos = ls;
            }
        }
        self.position.store(pos, Ordering::Relaxed);
        count
    }
}

/// Renders one output buffer. Shared by the device callback and the null output thread.
/// Live input delivered to the audio callback: interleaved samples and their channel count.
type InputFeed = (rtrb::Consumer<f32>, usize);
/// One ring per open MIDI input.
type MidiFeed = Vec<rtrb::Consumer<LiveMidi>>;
/// MIDI that arrived while playing, with the project frame of the block it was played in.
type RecordedMidi = (i64, LiveMidi);

/// The audio callback's ends of the channels to the main thread.
struct Feeds {
    input_rx: Receiver<Option<InputFeed>>,
    midi_rx: Receiver<MidiFeed>,
    recorded: rtrb::Producer<RecordedMidi>,
}

/// The main thread's ends.
struct FeedSenders {
    input_tx: Sender<Option<InputFeed>>,
    midi_tx: Sender<MidiFeed>,
    recorded: rtrb::Consumer<RecordedMidi>,
}

fn feeds() -> (FeedSenders, Feeds) {
    let (input_tx, input_rx) = channel();
    let (midi_tx, midi_rx) = channel();
    let (producer, consumer) = rtrb::RingBuffer::new(16_384);
    (FeedSenders { input_tx, midi_tx, recorded: consumer }, Feeds { input_rx, midi_rx, recorded: producer })
}

/// Most live MIDI messages one block takes; the rest wait for the next block.
const MAX_LIVE_MIDI: usize = 512;

struct Processor {
    graph: Arc<ArcSwap<RenderGraph>>,
    transport: Arc<Transport>,
    scratch: RenderScratch,
    left: Vec<f32>,
    right: Vec<f32>,
    input_rx: Receiver<Option<InputFeed>>,
    input: Option<InputFeed>,
    /// De-interleaved input for the current block.
    in_bufs: Vec<Vec<f32>>,
    midi_rx: Receiver<MidiFeed>,
    midi_in: MidiFeed,
    /// Live MIDI for the current block.
    live: Vec<LiveMidi>,
    recorded: rtrb::Producer<RecordedMidi>,
}

impl Processor {
    fn new(graph: Arc<ArcSwap<RenderGraph>>, transport: Arc<Transport>, feeds: Feeds) -> Self {
        Self {
            graph,
            transport,
            scratch: RenderScratch::new(MAX_TRACKS),
            left: vec![0.0; RenderScratch::MAX_BLOCK],
            right: vec![0.0; RenderScratch::MAX_BLOCK],
            input_rx: feeds.input_rx,
            input: None,
            in_bufs: (0..LiveInput::MAX_CHANNELS).map(|_| vec![0.0; RenderScratch::MAX_BLOCK]).collect(),
            midi_rx: feeds.midi_rx,
            midi_in: Vec::new(),
            live: Vec::with_capacity(MAX_LIVE_MIDI),
            recorded: feeds.recorded,
        }
    }

    /// Collects the MIDI that arrived since the last block. Returns whether MIDI inputs are open.
    fn read_midi(&mut self) -> bool {
        while let Ok(feed) = self.midi_rx.try_recv() {
            // Dropping the old rings here is rare (inputs reopened) and frees little.
            self.midi_in = feed;
        }
        self.live.clear();
        for rx in &mut self.midi_in {
            while self.live.len() < MAX_LIVE_MIDI {
                let Ok(m) = rx.pop() else { break };
                self.live.push(m);
            }
        }
        !self.midi_in.is_empty()
    }

    /// Pulls `frames` frames of live input, keeping at most a little more than a block queued
    /// so monitoring latency cannot creep up. Returns whether input is available.
    fn read_input(&mut self, frames: usize) -> bool {
        while let Ok(feed) = self.input_rx.try_recv() {
            self.input = feed;
        }
        let Some((rx, channels)) = &mut self.input else { return false };
        let ch = (*channels).max(1);
        let queued = rx.slots() / ch;
        let excess = queued.saturating_sub(frames * 3);
        for _ in 0..excess * ch {
            let _ = rx.pop();
        }
        for buf in &mut self.in_bufs {
            buf[..frames].fill(0.0);
        }
        let available = (rx.slots() / ch).min(frames);
        for f in 0..available {
            for c in 0..ch {
                let s = rx.pop().unwrap_or(0.0);
                if let Some(buf) = self.in_bufs.get_mut(c) {
                    buf[f] = s;
                }
            }
        }
        true
    }

    /// Fills `frames` frames into the internal stereo buffers.
    fn process(&mut self, frames: usize) {
        let started = Instant::now();
        let frames = frames.min(self.left.len());
        let (l, r) = (&mut self.left[..frames], &mut self.right[..frames]);
        l.fill(0.0);
        r.fill(0.0);
        let mut segments = [(0i64, 0usize, 0usize); 8];
        let n = self.transport.advance(frames, &mut segments);
        let graph = self.graph.load();
        let has_input = self.read_input(frames);
        let has_midi = self.read_midi();
        if let Some(&(pos, _, _)) = segments[..n].first() {
            for &m in &self.live {
                let _ = self.recorded.push((pos, m));
            }
        }
        let (l, r) = (&mut self.left[..frames], &mut self.right[..frames]);
        let channels = |from: usize, len: usize| -> [Option<&[f32]>; LiveInput::MAX_CHANNELS] {
            std::array::from_fn(|c| has_input.then(|| &self.in_bufs[c][from..from + len]))
        };
        // Stopped: items stay silent, but effects run while the input is monitored and for a
        // few seconds after stopping or a parameter change, so tails ring out and changes apply
        // without heavy chains burning CPU while idle.
        let awake = self.transport.wake_frames.fetch_sub(frames as i64, Ordering::Relaxed) > 0;
        if n == 0 && (((has_input || has_midi) && graph.monitoring()) || awake) {
            let pos = self.transport.position.load(Ordering::Relaxed);
            let live = LiveInput { channels: channels(0, frames), midi: &self.live, play_items: false };
            graph.render_with(pos, l, r, &mut self.scratch, &live);
        }
        for (i, &(pos, off, len)) in segments[..n].iter().enumerate() {
            let midi: &[LiveMidi] = if i == 0 { &self.live } else { &[] };
            let live = LiveInput { channels: channels(off, len), midi, play_items: true };
            graph.render_with(pos, &mut l[off..off + len], &mut r[off..off + len], &mut self.scratch, &live);
        }
        let budget = frames as f32 / self.transport.sample_rate as f32;
        let load = started.elapsed().as_secs_f32() / budget;
        self.transport.load.store(load.to_bits(), Ordering::Relaxed);
    }

    fn write_interleaved<T: SizedSample + FromSample<f32>>(&mut self, out: &mut [T], channels: usize) {
        for chunk in out.chunks_mut(channels * RenderScratch::MAX_BLOCK) {
            let frames = chunk.len() / channels;
            self.process(frames);
            for (i, frame) in chunk.chunks_mut(channels).enumerate() {
                for (c, s) in frame.iter_mut().enumerate() {
                    let v = match c {
                        0 => self.left[i],
                        1 => self.right[i],
                        _ => 0.0,
                    };
                    *s = T::from_sample(v);
                }
            }
        }
    }
}

enum Output {
    Device { _stream: cpal::Stream },
    Null { stop: Arc<AtomicBool>, thread: Option<JoinHandle<()>> },
}

impl Drop for Output {
    fn drop(&mut self) {
        if let Output::Null { stop, thread } = self {
            stop.store(true, Ordering::Relaxed);
            if let Some(t) = thread.take() {
                let _ = t.join();
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub name: String,
    pub sample_rate: u32,
    pub block_size: Option<u32>,
    /// Set when no device could be opened and playback is simulated.
    pub fallback_reason: Option<String>,
}

/// The playback engine: owns the output, the media pool and the current render graph.
pub struct Engine {
    graph: Arc<ArcSwap<RenderGraph>>,
    transport: Arc<Transport>,
    pool: Arc<MediaPool>,
    meters: MeterRegistry,
    fx: FxRegistry,
    /// Graphs swapped out recently. The audio thread may still hold one for the rest of its
    /// callback; keeping them alive here ensures the final drop never happens on that thread.
    retired: VecDeque<(Instant, Arc<RenderGraph>)>,
    info: DeviceInfo,
    metronome: f32,
    input_tx: Sender<Option<InputFeed>>,
    /// Open while a track monitors its input.
    input_stream: Option<cpal::Stream>,
    midi_tx: Sender<MidiFeed>,
    midi_inputs: Option<MidiInputs>,
    recorded_midi: rtrb::Consumer<RecordedMidi>,
    _output: Output,
}

impl Engine {
    /// Opens the default output device, falling back to a silent real-time clock when there
    /// is none so the transport still behaves.
    pub fn start(pool: Arc<MediaPool>) -> Self {
        match open_device() {
            Ok((device, config, format, name)) => {
                match Self::with_device(pool.clone(), &device, config, format, name.clone()) {
                    Ok(engine) => engine,
                    Err(e) => Self::start_null(pool, 48_000, Some(format!("{name}: {e}"))),
                }
            }
            Err(reason) => Self::start_null(pool, 48_000, Some(reason)),
        }
    }

    fn with_device(
        pool: Arc<MediaPool>,
        device: &cpal::Device,
        config: StreamConfig,
        format: SampleFormat,
        name: String,
    ) -> Result<Self, String> {
        let sample_rate = config.sample_rate;
        let graph = Arc::new(ArcSwap::from_pointee(RenderGraph::empty(sample_rate)));
        let transport = Arc::new(Transport::new(sample_rate));
        let (senders, feeds) = feeds();
        let proc = Processor::new(graph.clone(), transport.clone(), feeds);
        let stream = match format {
            SampleFormat::F32 => build::<f32>(device, &config, proc, transport.clone()),
            SampleFormat::I16 => build::<i16>(device, &config, proc, transport.clone()),
            SampleFormat::I32 => build::<i32>(device, &config, proc, transport.clone()),
            SampleFormat::U16 => build::<u16>(device, &config, proc, transport.clone()),
            SampleFormat::F64 => build::<f64>(device, &config, proc, transport.clone()),
            other => return Err(format!("unsupported sample format {other}")),
        }?;
        stream.play().map_err(|e| e.to_string())?;
        let block_size = match config.buffer_size {
            cpal::BufferSize::Fixed(n) => Some(n),
            cpal::BufferSize::Default => None,
        };
        Ok(Self {
            graph,
            transport,
            pool,
            meters: MeterRegistry::default(),
            fx: FxRegistry::new(sample_rate),
            retired: VecDeque::new(),
            metronome: 0.0,
            input_tx: senders.input_tx,
            input_stream: None,
            midi_tx: senders.midi_tx,
            midi_inputs: None,
            recorded_midi: senders.recorded,
            info: DeviceInfo { name, sample_rate, block_size, fallback_reason: None },
            _output: Output::Device { _stream: stream },
        })
    }

    pub fn start_null(pool: Arc<MediaPool>, sample_rate: u32, reason: Option<String>) -> Self {
        const BLOCK: usize = 512;
        let graph = Arc::new(ArcSwap::from_pointee(RenderGraph::empty(sample_rate)));
        let transport = Arc::new(Transport::new(sample_rate));
        let (senders, feeds) = feeds();
        let mut proc = Processor::new(graph.clone(), transport.clone(), feeds);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        let period = Duration::from_secs_f64(BLOCK as f64 / sample_rate as f64);
        let thread = std::thread::Builder::new()
            .name("scythe-null-audio".into())
            .spawn(move || {
                let mut next = Instant::now();
                while !stop_t.load(Ordering::Relaxed) {
                    proc.process(BLOCK);
                    next += period;
                    if let Some(wait) = next.checked_duration_since(Instant::now()) {
                        std::thread::sleep(wait);
                    } else {
                        next = Instant::now();
                    }
                }
            })
            .expect("spawn null audio thread");
        Self {
            graph,
            transport,
            pool,
            meters: MeterRegistry::default(),
            fx: FxRegistry::new(sample_rate),
            retired: VecDeque::new(),
            metronome: 0.0,
            input_tx: senders.input_tx,
            input_stream: None,
            midi_tx: senders.midi_tx,
            midi_inputs: None,
            recorded_midi: senders.recorded,
            info: DeviceInfo {
                name: "No audio device".into(),
                sample_rate,
                block_size: Some(BLOCK as u32),
                fallback_reason: reason,
            },
            _output: Output::Null { stop, thread: Some(thread) },
        }
    }

    pub fn info(&self) -> &DeviceInfo {
        &self.info
    }

    pub fn sample_rate(&self) -> u32 {
        self.info.sample_rate
    }

    pub fn pool(&self) -> &Arc<MediaPool> {
        &self.pool
    }

    pub fn transport(&self) -> &Arc<Transport> {
        &self.transport
    }

    pub fn fx(&self) -> &FxRegistry {
        &self.fx
    }

    /// Writes hosted plugins' current state into the project before a save.
    pub fn store_plugin_states(&mut self, project: &mut Project) {
        self.fx.store_states(project);
    }

    pub fn meters(&self) -> &MeterRegistry {
        &self.meters
    }

    /// Rebuilds the render graph from `project` and hands it to the audio thread.
    pub fn sync(&mut self, project: &Project) {
        self.meters.retain(project);
        self.fx.retain(project);
        self.fx.project_dir = project.path.as_deref().and_then(std::path::Path::parent).map(Into::into);
        let mut graph = RenderGraph::build(project, &self.pool, &mut self.meters, &mut self.fx, self.info.sample_rate);
        graph.metronome = self.metronome;
        let old = self.graph.swap(Arc::new(graph));
        let now = Instant::now();
        self.retired.push_back((now, old));
        while self.retired.front().is_some_and(|(t, _)| now.duration_since(*t) > Duration::from_secs(2)) {
            self.retired.pop_front();
        }
        self.set_loop(project.loop_enabled, project.time_selection);
    }

    /// Opens or closes the live input for monitoring. Returns the reason when it cannot open.
    pub fn set_monitoring(&mut self, on: bool) -> Result<(), String> {
        if on == self.input_stream.is_some() {
            return Ok(());
        }
        if !on {
            self.input_stream = None;
            let _ = self.input_tx.send(None);
            return Ok(());
        }
        let host = cpal::default_host();
        let device = host.default_input_device().ok_or("no audio input device")?;
        let default = device.default_input_config().map_err(|e| e.to_string())?;
        let mut config = default.config();
        config.sample_rate = self.info.sample_rate;
        let channels = config.channels as usize;
        let (mut tx, rx) = rtrb::RingBuffer::<f32>::new(self.info.sample_rate as usize * channels / 2);
        let stream = device
            .build_input_stream(
                config,
                move |data: &[f32], _| {
                    for &s in data {
                        let _ = tx.push(s);
                    }
                },
                |e| eprintln!("input stream error: {e}"),
                None,
            )
            .map_err(|e| format!("input at {} Hz: {e}", self.info.sample_rate))?;
        stream.play().map_err(|e| e.to_string())?;
        let _ = self.input_tx.send(Some((rx, channels)));
        self.input_stream = Some(stream);
        Ok(())
    }

    /// Opens every MIDI input for live play and recording, or closes them.
    pub fn set_midi_input(&mut self, on: bool) -> Result<(), String> {
        if on == self.midi_inputs.is_some() {
            return Ok(());
        }
        if !on {
            self.midi_inputs = None;
            let _ = self.midi_tx.send(Vec::new());
            return Ok(());
        }
        let (inputs, rings) = MidiInputs::open_all()?;
        if rings.is_empty() {
            return Err("no MIDI input".into());
        }
        let _ = self.midi_tx.send(rings);
        self.midi_inputs = Some(inputs);
        Ok(())
    }

    /// Names of the open MIDI inputs.
    pub fn midi_input_names(&self) -> &[String] {
        self.midi_inputs.as_ref().map_or(&[], |m| &m.names)
    }

    /// MIDI played since the last call while the transport ran, with the project frame of the
    /// block it arrived in.
    pub fn take_recorded_midi(&mut self) -> Vec<(i64, LiveMidi)> {
        let mut out = Vec::with_capacity(self.recorded_midi.slots());
        while let Ok(e) = self.recorded_midi.pop() {
            out.push(e);
        }
        out
    }

    /// Metronome gain while playing; takes effect on the next `sync`.
    pub fn set_metronome(&mut self, gain: f32) {
        self.metronome = gain;
    }

    pub fn play(&self) {
        self.transport.playing.store(true, Ordering::Relaxed);
    }

    pub fn stop(&self) {
        self.transport.playing.store(false, Ordering::Relaxed);
        self.wake_effects();
    }

    /// Keeps effects running for a few seconds while stopped.
    pub fn wake_effects(&self) {
        let frames = self.info.sample_rate as i64 * 3;
        self.transport.wake_frames.store(frames, Ordering::Relaxed);
    }

    pub fn seek(&self, seconds: f64) {
        let frames = (seconds.max(0.0) * self.info.sample_rate as f64).round() as i64;
        self.transport.seek.store(frames, Ordering::Relaxed);
    }

    pub fn set_loop(&self, enabled: bool, range: Option<TimeRange>) {
        let sr = self.info.sample_rate as f64;
        let range = range.filter(|r| !r.is_empty());
        let t = &self.transport;
        if let Some(r) = range {
            t.loop_start.store((r.start * sr).round() as i64, Ordering::Relaxed);
            t.loop_end.store((r.end * sr).round() as i64, Ordering::Relaxed);
        }
        t.loop_enabled.store(enabled && range.is_some(), Ordering::Relaxed);
    }
}

fn open_device() -> Result<(cpal::Device, StreamConfig, SampleFormat, String), String> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or("no default output device")?;
    let name = device.description().map(|d| d.name().to_owned()).unwrap_or_else(|_| "Default output".into());
    let supported = device.default_output_config().map_err(|e| e.to_string())?;
    let format = supported.sample_format();
    Ok((device, supported.into(), format, name))
}

fn build<T: SizedSample + FromSample<f32> + Send + 'static>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut proc: Processor,
    transport: Arc<Transport>,
) -> Result<cpal::Stream, String> {
    let channels = config.channels as usize;
    device
        .build_output_stream::<T, _, _>(
            *config,
            move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
                let t = info.timestamp();
                let latency = t.playback.duration_since(t.callback);
                transport.output_latency.store(latency.as_secs_f64().to_bits(), Ordering::Relaxed);
                proc.write_interleaved(data, channels)
            },
            |err| eprintln!("audio stream error: {err}"),
            None,
        )
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segs(t: &Transport, frames: usize) -> Vec<(i64, usize, usize)> {
        let mut s = [(0, 0, 0); 8];
        let n = t.advance(frames, &mut s);
        s[..n].to_vec()
    }

    #[test]
    fn stopped_transport_does_not_move() {
        let t = Transport::new(100);
        assert!(segs(&t, 10).is_empty());
        assert_eq!(t.position.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn playing_advances_and_applies_seek() {
        let t = Transport::new(100);
        t.playing.store(true, Ordering::Relaxed);
        assert_eq!(segs(&t, 10), [(0, 0, 10)]);
        t.seek.store(50, Ordering::Relaxed);
        assert_eq!(segs(&t, 10), [(50, 0, 10)]);
        assert_eq!(t.position_seconds(), 0.6);
    }

    #[test]
    fn loop_wraps_inside_block() {
        let t = Transport::new(100);
        t.playing.store(true, Ordering::Relaxed);
        t.loop_enabled.store(true, Ordering::Relaxed);
        t.loop_start.store(10, Ordering::Relaxed);
        t.loop_end.store(20, Ordering::Relaxed);
        t.seek.store(15, Ordering::Relaxed);
        assert_eq!(segs(&t, 12), [(15, 0, 5), (10, 5, 7)]);
        assert_eq!(t.position.load(Ordering::Relaxed), 17);
    }

    #[test]
    fn playhead_before_loop_runs_into_it() {
        let t = Transport::new(100);
        t.playing.store(true, Ordering::Relaxed);
        t.loop_enabled.store(true, Ordering::Relaxed);
        t.loop_start.store(10, Ordering::Relaxed);
        t.loop_end.store(20, Ordering::Relaxed);
        assert_eq!(segs(&t, 8), [(0, 0, 8)]);
        assert_eq!(segs(&t, 8), [(8, 0, 8)]);
        assert_eq!(segs(&t, 8), [(16, 0, 4), (10, 4, 4)]);
    }

    #[test]
    fn null_engine_plays_in_real_time() {
        let mut e = Engine::start_null(Arc::default(), 48_000, None);
        e.sync(&Project::default());
        e.play();
        std::thread::sleep(Duration::from_millis(120));
        e.stop();
        let pos = e.transport().position_seconds();
        assert!(pos > 0.05 && pos < 0.5, "position {pos}");
    }
}
