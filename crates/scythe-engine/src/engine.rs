use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use scythe_core::marker_action::TransportAction;
use scythe_core::{Project, TimeRange};

use crate::cues::{CueList, Hit, MarkerAction};
use crate::devices::DeviceRequest;
use crate::fx::FxRegistry;
use crate::media::MediaPool;
use crate::midi_io::{InputRings, LiveMidi, MidiInputs, MidiSender, OutMessage, SyncInput};
use crate::midi_sync::{Chase, ChaseAction, ClockOut, SyncConfig};
use crate::render::{Clicks, CountInClicks, LiveInput, MeterRegistry, MidiOutClock, RenderGraph, RenderScratch};

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
    /// Whether crossing a `!` marker runs its actions.
    marker_actions: AtomicBool,
    cues: ArcSwap<CueList>,
    sample_rate: u32,
    /// Frames of count-in left before the playhead moves; the others describe its clicks.
    count_in_left: AtomicI64,
    count_in_frames: AtomicI64,
    count_in_beat: AtomicU64,
    count_in_first: AtomicU64,
    count_in_first_in_bar: AtomicU32,
    count_in_bar: AtomicU32,
    count_in_audible: AtomicI64,
    /// Recording is on, for the metronome's "run during recording".
    recording: AtomicBool,
    /// Buffer underruns and overruns the device reported.
    xruns: AtomicU64,
    /// Callbacks that took longer to render than the audio they produced.
    overloads: AtomicU64,
    /// Highest `load` since the last health report, as f32 bits.
    load_peak: AtomicU32,
    /// Output callbacks so far, so a stream that stopped calling back shows.
    callbacks: AtomicU64,
    /// The device reported it is gone.
    lost: AtomicBool,
    /// Frames the device asked for in its last callback.
    block_frames: AtomicU32,
    /// The project play rate as `f64` bits: the graph runs on the sped-up time axis and
    /// positions in project seconds are that many times its frames.
    play_rate: AtomicU64,
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
            marker_actions: AtomicBool::new(true),
            cues: ArcSwap::from_pointee(CueList::default()),
            sample_rate,
            count_in_left: AtomicI64::new(0),
            count_in_frames: AtomicI64::new(0),
            count_in_beat: AtomicU64::new(0),
            count_in_first: AtomicU64::new(0),
            count_in_first_in_bar: AtomicU32::new(0),
            count_in_bar: AtomicU32::new(1),
            count_in_audible: AtomicI64::new(0),
            recording: AtomicBool::new(false),
            xruns: AtomicU64::new(0),
            overloads: AtomicU64::new(0),
            load_peak: AtomicU32::new(0),
            callbacks: AtomicU64::new(0),
            lost: AtomicBool::new(false),
            block_frames: AtomicU32::new(0),
            play_rate: AtomicU64::new(1f64.to_bits()),
        }
    }

    fn play_rate(&self) -> f64 {
        f64::from_bits(self.play_rate.load(Ordering::Relaxed))
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
        frames as f64 / self.sample_rate as f64 * self.play_rate()
    }

    pub fn load(&self) -> f32 {
        f32::from_bits(self.load.load(Ordering::Relaxed))
    }

    /// While counting in: how far before the start playback is, as negative seconds, and the
    /// beat sounding (from 1) of how many.
    pub fn count_in(&self) -> Option<(f64, u32, u32)> {
        let left = self.count_in_left.load(Ordering::Relaxed);
        if !self.is_playing() || left <= 0 {
            return None;
        }
        let clicks = self.count_in_clicks();
        let beats = clicks.beats();
        let elapsed = clicks.frames - left;
        let current = (0..beats).take_while(|&k| clicks.click_at(k) <= elapsed).count() as u32;
        Some((-(left as f64) / self.sample_rate as f64, current.max(1).min(beats), beats))
    }

    fn start_count_in(&self, c: CountInClicks) {
        self.count_in_frames.store(c.frames, Ordering::Relaxed);
        self.count_in_beat.store(c.beat.to_bits(), Ordering::Relaxed);
        self.count_in_first.store(c.first.to_bits(), Ordering::Relaxed);
        self.count_in_first_in_bar.store(c.first_in_bar, Ordering::Relaxed);
        self.count_in_bar.store(c.beats_per_bar, Ordering::Relaxed);
        self.count_in_audible.store(c.audible, Ordering::Relaxed);
        self.count_in_left.store(c.frames, Ordering::Release);
    }

    fn count_in_clicks(&self) -> CountInClicks {
        CountInClicks {
            frames: self.count_in_frames.load(Ordering::Relaxed),
            beat: f64::from_bits(self.count_in_beat.load(Ordering::Relaxed)),
            first: f64::from_bits(self.count_in_first.load(Ordering::Relaxed)),
            first_in_bar: self.count_in_first_in_bar.load(Ordering::Relaxed),
            beats_per_bar: self.count_in_bar.load(Ordering::Relaxed).max(1),
            audible: self.count_in_audible.load(Ordering::Relaxed),
        }
    }

    /// Takes up to `frames` of a running count-in: its clicks, how far into it the block starts
    /// and how many frames it fills.
    fn count_in_step(&self, frames: usize) -> Option<(CountInClicks, i64, usize)> {
        if !self.is_playing() {
            return None;
        }
        let left = self.count_in_left.load(Ordering::Acquire);
        if left <= 0 {
            return None;
        }
        let len = (left as usize).min(frames);
        // Stop and play clear the count-in from the main thread meanwhile; theirs wins.
        self.count_in_left.compare_exchange(left, left - len as i64, Ordering::AcqRel, Ordering::Relaxed).ok()?;
        let clicks = self.count_in_clicks();
        Some((clicks, clicks.frames - left, len))
    }

    /// Advances the playhead over one block, honouring seeks, the loop range and marker
    /// actions. Returns the segments `(project_frame, offset_in_block, len)` to render.
    ///
    /// A marker action runs when playback crosses its marker: the marker lies after the frame
    /// the segment starts at and no later than where it ends. One the playhead starts on (play,
    /// seek or loop wrap onto the marker) does not run, so a song started at the stop marker of
    /// the song before it plays.
    fn advance(&self, frames: usize, segments: &mut [(i64, usize, usize); 8], hit: &mut impl FnMut(Hit)) -> usize {
        let seek = self.seek.swap(NO_SEEK, Ordering::Relaxed);
        if seek != NO_SEEK {
            self.position.store(seek, Ordering::Relaxed);
        }
        if !self.is_playing() {
            return 0;
        }
        let cues = self.cues.load();
        let cues = self.marker_actions.load(Ordering::Relaxed).then_some(&**cues);
        let mut pos = self.position.load(Ordering::Relaxed);
        let mut done = 0;
        let mut count = 0;
        while done < frames && count < segments.len() {
            let looping = self.loop_enabled.load(Ordering::Relaxed);
            let (ls, le) = (self.loop_start.load(Ordering::Relaxed), self.loop_end.load(Ordering::Relaxed));
            // Like REAPER, only crossing the loop end wraps; a playhead already past it plays on.
            let will_wrap = looping && le > ls && pos < le;
            let mut len = frames - done;
            if will_wrap {
                len = len.min((le - pos) as usize);
            }
            let cue = cues.and_then(|c| c.first_in(pos, pos + len as i64));
            if let Some(c) = cue {
                len = (c.frame - pos) as usize;
            }
            segments[count] = (pos, done, len);
            count += 1;
            done += len;
            pos += len as i64;
            if let (Some(c), Some(list)) = (cue, cues) {
                let before = pos;
                for &command in &c.commands {
                    let action = CueList::transport_action(command);
                    match action {
                        Some(TransportAction::Stop | TransportAction::Pause) => {
                            self.playing.store(false, Ordering::Relaxed)
                        }
                        Some(TransportAction::GoToStart) => pos = 0,
                        Some(TransportAction::GoToEnd) => pos = list.end_frame,
                        Some(TransportAction::GoToLoopStart) => pos = ls,
                        Some(TransportAction::ToggleRepeat) => {
                            self.loop_enabled.fetch_xor(true, Ordering::Relaxed);
                        }
                        Some(TransportAction::Play) | None => {}
                    }
                    hit(Hit { frame: c.frame, command, applied: action.is_some(), generation: list.generation });
                }
                if !self.is_playing() {
                    break;
                }
                if pos != before {
                    continue;
                }
            }
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
/// The rings of the open MIDI inputs.
type MidiFeed = Vec<InputRings>;
/// MIDI that arrived while playing, with the project frame of the block it was played in.
type RecordedMidi = (i64, LiveMidi);

/// The audio callback's ends of the channels to the main thread.
struct Feeds {
    input_rx: Receiver<Option<InputFeed>>,
    midi_rx: Receiver<MidiFeed>,
    recorded: rtrb::Producer<RecordedMidi>,
    midi_out_rx: Receiver<Option<MidiOutQueue>>,
    hits: rtrb::Producer<Hit>,
    keyboard: rtrb::Consumer<LiveMidi>,
    sync_rx: Receiver<SyncConfig>,
    control: rtrb::Producer<LiveMidi>,
}

/// The main thread's ends.
struct FeedSenders {
    input_tx: Sender<Option<InputFeed>>,
    midi_tx: Sender<MidiFeed>,
    recorded: rtrb::Consumer<RecordedMidi>,
    midi_out_tx: Sender<Option<MidiOutQueue>>,
    hits: rtrb::Consumer<Hit>,
    keyboard: rtrb::Producer<LiveMidi>,
    sync_tx: Sender<SyncConfig>,
    control: rtrb::Consumer<LiveMidi>,
}

fn feeds() -> (FeedSenders, Feeds) {
    let (input_tx, input_rx) = channel();
    let (midi_tx, midi_rx) = channel();
    let (midi_out_tx, midi_out_rx) = channel();
    let (producer, consumer) = rtrb::RingBuffer::new(16_384);
    let (hits_tx, hits_rx) = rtrb::RingBuffer::new(256);
    let (keys_tx, keys_rx) = rtrb::RingBuffer::new(512);
    let (sync_tx, sync_rx) = channel();
    let (control_tx, control_rx) = rtrb::RingBuffer::new(1024);
    (
        FeedSenders {
            input_tx,
            midi_tx,
            recorded: consumer,
            midi_out_tx,
            hits: hits_rx,
            keyboard: keys_tx,
            sync_tx,
            control: control_rx,
        },
        Feeds {
            input_rx,
            midi_rx,
            recorded: producer,
            midi_out_rx,
            hits: hits_tx,
            keyboard: keys_rx,
            sync_rx,
            control: control_tx,
        },
    )
}

/// Most live MIDI messages one block takes; the rest wait for the next block.
const MAX_LIVE_MIDI: usize = 512;

struct Processor {
    graph: Arc<ArcSwap<RenderGraph>>,
    transport: Arc<Transport>,
    scratch: RenderScratch,
    left: Vec<f32>,
    right: Vec<f32>,
    /// Master channels 3 and up for the device outputs past the first pair.
    extra: Vec<Vec<f32>>,
    input_rx: Receiver<Option<InputFeed>>,
    input: Option<InputFeed>,
    /// De-interleaved input for the current block.
    in_bufs: Vec<Vec<f32>>,
    midi_rx: Receiver<MidiFeed>,
    midi_in: MidiFeed,
    /// Live MIDI for the current block.
    live: Vec<LiveMidi>,
    recorded: rtrb::Producer<RecordedMidi>,
    midi_out_rx: Receiver<Option<MidiOutQueue>>,
    midi_out: Option<MidiOutQueue>,
    was_playing: bool,
    /// After a count-in: its clicks and the frame into it where the next block starts, while
    /// its last clicks still ring.
    count_in_tail: Option<(CountInClicks, i64)>,
    hits: rtrb::Producer<Hit>,
    /// Notes from the virtual MIDI keyboard.
    keyboard: rtrb::Consumer<LiveMidi>,
    sync_rx: Receiver<SyncConfig>,
    clock: ClockOut,
    chase: Option<Chase>,
    /// Synchronization messages of the current block.
    sync_in: Vec<SyncInput>,
    /// The virtual keyboard has played, so monitoring keeps running while stopped as it does
    /// for open inputs.
    keyboard_used: bool,
    /// Every message from the MIDI inputs, for learned controls and actions.
    control: rtrb::Producer<LiveMidi>,
    /// The device as hardware inserts see it.
    hardware: Arc<crate::hardware_io::HardwareIo>,
    /// The media explorer's preview, mixed in after the master.
    preview: Arc<crate::preview::Preview>,
}

/// The queue hardware MIDI outputs are fed through.
type MidiOutQueue = Arc<std::sync::Mutex<rtrb::Producer<OutMessage>>>;

impl Processor {
    fn new(graph: Arc<ArcSwap<RenderGraph>>, transport: Arc<Transport>, feeds: Feeds) -> Self {
        Self {
            graph,
            transport,
            scratch: RenderScratch::new(MAX_TRACKS),
            left: vec![0.0; RenderScratch::MAX_BLOCK],
            right: vec![0.0; RenderScratch::MAX_BLOCK],
            extra: Vec::new(),
            input_rx: feeds.input_rx,
            input: None,
            in_bufs: (0..LiveInput::MAX_CHANNELS).map(|_| vec![0.0; RenderScratch::MAX_BLOCK]).collect(),
            midi_rx: feeds.midi_rx,
            midi_in: Vec::new(),
            live: Vec::with_capacity(MAX_LIVE_MIDI),
            recorded: feeds.recorded,
            midi_out_rx: feeds.midi_out_rx,
            midi_out: None,
            was_playing: false,
            count_in_tail: None,
            hits: feeds.hits,
            keyboard: feeds.keyboard,
            sync_rx: feeds.sync_rx,
            clock: ClockOut::default(),
            chase: None,
            sync_in: Vec::with_capacity(MAX_LIVE_MIDI),
            keyboard_used: false,
            control: feeds.control,
            hardware: Arc::default(),
            preview: Arc::default(),
        }
    }

    /// Sizes the buffers for a device with `channels` outputs. Not for the audio thread.
    fn set_outputs(&mut self, channels: usize) {
        self.extra.resize_with(channels.saturating_sub(2), || vec![0.0; RenderScratch::MAX_BLOCK]);
    }

    /// Collects the MIDI that arrived since the last block. Returns whether there is live MIDI
    /// to monitor.
    fn read_midi(&mut self) -> bool {
        while let Ok(config) = self.sync_rx.try_recv() {
            self.clock.ports = config.clock;
            let rate = self.transport.sample_rate;
            // Rebuilding the chaser allocates; it happens when the settings change, not per block.
            self.chase = config.chase.map(|c| match self.chase.take() {
                Some(mut old) if old.config.source == c.source => {
                    old.config = c;
                    old
                }
                _ => Chase::new(c, rate),
            });
        }
        while let Ok(out) = self.midi_out_rx.try_recv() {
            self.midi_out = out;
        }
        while let Ok(feed) = self.midi_rx.try_recv() {
            // Dropping the old rings here is rare (inputs reopened) and frees little.
            self.midi_in = feed;
        }
        self.live.clear();
        self.sync_in.clear();
        for rx in &mut self.midi_in {
            while self.live.len() < MAX_LIVE_MIDI {
                let Ok(m) = rx.midi.pop() else { break };
                self.live.push(m);
                let _ = self.control.push(m);
            }
            while self.sync_in.len() < MAX_LIVE_MIDI {
                let Ok(m) = rx.sync.pop() else { break };
                self.sync_in.push(m);
            }
        }
        while self.live.len() < MAX_LIVE_MIDI {
            let Ok(m) = self.keyboard.pop() else { break };
            self.live.push(m);
            self.keyboard_used = true;
        }
        !self.midi_in.is_empty() || self.keyboard_used
    }

    /// Lets chased timecode start, move or stop the transport before the block plays.
    fn chase(&mut self, has_input: bool, frames: usize) {
        let Some(chase) = &mut self.chase else { return };
        for m in &self.sync_in {
            chase.feed(m);
        }
        let now = Instant::now();
        if let crate::midi_sync::ChaseSource::Ltc { channel, .. } = chase.config.source
            && has_input
            && let Some(buf) = self.in_bufs.get(channel)
        {
            chase.feed_audio(&buf[..frames], now);
        }
        let t = &self.transport;
        let sr = t.sample_rate as f64;
        let graph = self.graph.load();
        let position = t.position.load(Ordering::Relaxed) as f64 / sr;
        let action = chase.decide(now, position, t.is_playing(), |qn| graph.timeline.time_at_qn(qn));
        match action {
            Some(ChaseAction::Play(at)) => {
                t.seek.store((at * sr).round() as i64, Ordering::Relaxed);
                t.playing.store(true, Ordering::Relaxed);
            }
            Some(ChaseAction::Seek(at)) => t.seek.store((at * sr).round() as i64, Ordering::Relaxed),
            Some(ChaseAction::Stop) => t.playing.store(false, Ordering::Relaxed),
            None => {}
        }
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
        for b in &mut self.extra {
            b[..frames].fill(0.0);
        }
        let mut segments = [(0i64, 0usize, 0usize); 8];
        let has_input = self.read_input(frames);
        self.hardware.begin(frames, has_input.then_some(self.in_bufs.as_slice()));
        let has_midi = self.read_midi();
        self.chase(has_input, frames);
        let count_in = self.transport.count_in_step(frames);
        let counted = count_in.map_or(0, |(_, _, len)| len);
        let hits = &mut self.hits;
        let n = self.transport.advance(frames - counted, &mut segments, &mut |h| {
            let _ = hits.push(h);
        });
        for segment in &mut segments[..n] {
            segment.1 += counted;
        }
        let graph = self.graph.load();
        if counted == 0
            && let Some(&(pos, _, _)) = segments[..n].first()
        {
            for &m in &self.live {
                let _ = self.recorded.push((pos, m));
            }
        }
        let recording = self.transport.recording.load(Ordering::Relaxed);
        let metronome = &graph.metronome;
        let beats = if recording { metronome.on_record } else { metronome.on_play };
        let song_clicks = if beats { Clicks::Beats } else { Clicks::Off };
        let (l, r) = (&mut self.left[..frames], &mut self.right[..frames]);
        let channels = |from: usize, len: usize| -> [Option<&[f32]>; LiveInput::MAX_CHANNELS] {
            std::array::from_fn(|c| has_input.then(|| &self.in_bufs[c][from..from + len]))
        };
        // Stopped: items stay silent, but effects run while the input is monitored and for a
        // few seconds after stopping or a parameter change, so tails ring out and changes apply
        // without heavy chains burning CPU while idle.
        let awake = self.transport.wake_frames.fetch_sub(frames as i64, Ordering::Relaxed) > 0;
        // What leaves the speakers now was rendered one output latency ago.
        let heard = Instant::now() + Duration::from_secs_f64(self.transport.output_latency());
        let sr = self.transport.sample_rate as f64;
        let clock = |frame: i64, off: usize| {
            self.midi_out.as_deref().map(|queue| MidiOutClock {
                queue,
                at: heard + Duration::from_secs_f64(off as f64 / sr),
                frame,
            })
        };
        if self.was_playing
            && n == 0
            && let Some(queue) = &self.midi_out
            && let Ok(mut q) = queue.try_lock()
        {
            let _ = q.push(OutMessage::all_notes_off(heard));
        }
        if let Some(queue) = &self.midi_out
            && let Ok(mut q) = queue.try_lock()
        {
            let pos = self.transport.position.load(Ordering::Relaxed);
            let played = if counted > 0 { &segments[..0] } else { &segments[..n] };
            self.clock.block(played, pos, &graph.timeline, self.transport.sample_rate, &mut |off, port, m| {
                let _ = q.push(OutMessage::new(heard + Duration::from_secs_f64(off as f64 / sr), port, m));
            });
        }
        self.was_playing = n > 0;
        if let Some((clicks, at, len)) = count_in {
            // Items wait for the count-in; live input and effects carry on so the band hears itself.
            let pos = self.transport.position.load(Ordering::Relaxed);
            let live = LiveInput {
                channels: channels(0, len),
                midi: &self.live,
                play_items: false,
                recording,
                midi_out: clock(pos, 0),
                clicks: Clicks::CountIn { clicks, at },
            };
            self.hardware.segment(0);
            graph.render_with(pos, &mut l[..len], &mut r[..len], &mut self.scratch, &live);
            graph.master_extra_at(0, len, &mut self.extra);
            graph.device_out_at(0, len, l, r, &mut self.extra);
            graph.monitor_fx_at(pos, 0, len, l, r, &mut self.extra);
        } else if n == 0 && (((has_input || has_midi) && graph.monitoring()) || awake) {
            let pos = self.transport.position.load(Ordering::Relaxed);
            let live = LiveInput {
                channels: channels(0, frames),
                midi: &self.live,
                play_items: false,
                recording,
                midi_out: clock(pos, 0),
                clicks: Clicks::Off,
            };
            self.hardware.segment(0);
            graph.render_with(pos, l, r, &mut self.scratch, &live);
            graph.master_extra_at(0, frames, &mut self.extra);
            graph.device_out_at(0, frames, l, r, &mut self.extra);
            graph.monitor_fx_at(pos, 0, frames, l, r, &mut self.extra);
        }
        let tail = match count_in {
            Some((clicks, at, len)) if at + len as i64 >= clicks.frames => Some((clicks, at)),
            Some(_) => None,
            None => self.count_in_tail.filter(|_| n > 0),
        };
        for (i, &(pos, off, len)) in segments[..n].iter().enumerate() {
            let midi: &[LiveMidi] = if i == 0 && counted == 0 { &self.live } else { &[] };
            let clicks = match tail {
                Some((clicks, at)) => Clicks::CountInTail { clicks, at: at + off as i64, beats },
                None => song_clicks,
            };
            let live = LiveInput {
                channels: channels(off, len),
                midi,
                play_items: true,
                recording,
                midi_out: clock(pos, off),
                clicks,
            };
            self.hardware.segment(off);
            graph.render_with(pos, &mut l[off..off + len], &mut r[off..off + len], &mut self.scratch, &live);
            graph.master_extra_at(off, len, &mut self.extra);
            graph.device_out_at(off, len, l, r, &mut self.extra);
            graph.monitor_fx_at(pos, off, len, l, r, &mut self.extra);
        }
        self.count_in_tail = tail
            .map(|(clicks, at)| (clicks, at + frames as i64))
            .filter(|(clicks, at)| *at < clicks.frames + graph.click_len());
        self.hardware.mix_into(&mut self.left[..frames], &mut self.right[..frames], &mut self.extra);
        self.preview.mix_into(&mut self.left[..frames], &mut self.right[..frames]);
        let budget = frames as f32 / self.transport.sample_rate as f32;
        let load = started.elapsed().as_secs_f32() / budget;
        let t = &self.transport;
        t.load.store(load.to_bits(), Ordering::Relaxed);
        if load > f32::from_bits(t.load_peak.load(Ordering::Relaxed)) {
            t.load_peak.store(load.to_bits(), Ordering::Relaxed);
        }
        if load > 1.0 {
            t.overloads.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn write_interleaved<T: SizedSample + FromSample<f32>>(&mut self, out: &mut [T], channels: usize) {
        for chunk in out.chunks_mut(channels * RenderScratch::MAX_BLOCK) {
            let frames = chunk.len() / channels;
            self.process(frames);
            interleave(chunk, channels, &self.left, &self.right, &self.extra);
        }
    }
}

/// Writes the master's channels to the device's in order; outputs past the master's last
/// channel stay silent.
fn interleave<T: SizedSample + FromSample<f32>>(
    out: &mut [T],
    channels: usize,
    left: &[f32],
    right: &[f32],
    extra: &[Vec<f32>],
) {
    for (i, frame) in out.chunks_mut(channels).enumerate() {
        for (c, s) in frame.iter_mut().enumerate() {
            let v = match c {
                0 => left[i],
                1 => right[i],
                _ => extra.get(c - 2).map_or(0.0, |b| b[i]),
            };
            *s = T::from_sample(v);
        }
    }
}

enum Output {
    Device(DeviceOutput),
    /// A thread that renders in real time and throws the audio away.
    Null {
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    },
}

impl Output {
    const NULL_BLOCK: usize = 512;
    const NULL_MAX_LAG: Duration = Duration::from_millis(250);

    fn null(proc: Arc<Mutex<Processor>>, sample_rate: u32, block: usize) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        let block = block.clamp(16, RenderScratch::MAX_BLOCK);
        let period = Duration::from_secs_f64(block as f64 / sample_rate as f64);
        let thread = std::thread::Builder::new()
            .name("scythe-null-audio".into())
            .spawn(move || {
                crate::realtime::request();
                let mut next = Instant::now();
                while !stop_t.load(Ordering::Relaxed) {
                    if let Ok(mut proc) = proc.lock() {
                        proc.process(block);
                    }
                    next += period;
                    let now = Instant::now();
                    match next.checked_duration_since(now) {
                        Some(wait) => std::thread::sleep(wait),
                        // Late blocks follow at once, so the clock keeps time through a stall;
                        // a backlog the rendering cannot work off is given up.
                        None if now.duration_since(next) > Self::NULL_MAX_LAG => next = now,
                        None => {}
                    }
                }
            })
            .expect("spawn null audio thread");
        Output::Null { stop, thread: Some(thread) }
    }
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

/// An open output device. The stream is reopened with more channels when the master has more
/// than the device's default configuration.
struct DeviceOutput {
    stream: Option<cpal::Stream>,
    device: cpal::Device,
    config: StreamConfig,
    format: SampleFormat,
    default_channels: u16,
    /// Channel counts the device offers at this rate and format, queried on first need.
    supported: Option<Vec<u16>>,
    proc: Arc<Mutex<Processor>>,
}

impl DeviceOutput {
    fn match_master(&mut self, master_channels: usize, transport: &Arc<Transport>) {
        let want = if master_channels <= self.default_channels as usize {
            self.default_channels
        } else {
            let (device, format, rate) = (&self.device, self.format, self.config.sample_rate);
            let supported = self.supported.get_or_insert_with(|| supported_channels(device, format, rate));
            output_channels(supported, self.default_channels, master_channels)
        };
        let previous = self.config.channels;
        if want == previous {
            return;
        }
        // Many devices allow only one open stream, so the old one goes first.
        self.stream = None;
        for channels in [want, previous] {
            self.config.channels = channels;
            if let Ok(mut proc) = self.proc.lock() {
                proc.set_outputs(channels as usize);
            }
            match open_stream(&self.device, &self.config, self.format, self.proc.clone(), transport.clone()) {
                Ok(stream) => {
                    self.stream = Some(stream);
                    return;
                }
                Err(e) => eprintln!("audio output with {channels} channels: {e}"),
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
    /// The audio system, as `devices::hosts` names it; empty without a device.
    pub host: String,
    /// The output device's id (`hw:CARD=USB,DEV=0`).
    pub device_id: String,
    /// The input device's name and id, when there is one.
    pub input: Option<(String, String)>,
    /// The input channels the device offers, named as REAPER names them (`Input 1`).
    pub inputs: Vec<String>,
    /// The output channels the device plays (`Output 1`).
    pub outputs: Vec<String>,
}

/// How the audio is doing, for a status display and the control API's `health` event.
#[derive(Debug, Clone, PartialEq)]
pub struct Health {
    pub device: String,
    pub sample_rate: u32,
    /// Frames per callback: what was asked for, else what the device delivers.
    pub block_size: Option<u32>,
    /// Seconds from rendering a block until it is heard, as the device reports it.
    pub output_latency: f64,
    /// DSP time over buffer time in the last callback; above 1 the output drops out.
    pub load: f32,
    /// Highest `load` since [`Engine::take_load_peak`] last ran.
    pub load_peak: f32,
    /// Underruns and overruns the device reported.
    pub xruns: u64,
    /// Callbacks whose rendering took longer than the audio they made.
    pub overloads: u64,
    /// No device is playing: the engine runs on a silent clock, for `reason`.
    pub silent: bool,
    pub reason: Option<String>,
    /// Times the device came back after being lost or missing.
    pub reconnects: u64,
    /// How the thread rendering the audio is scheduled, once it has run.
    pub realtime: Option<crate::realtime::Realtime>,
}

/// A change in the audio device, from [`Engine::maintain`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceEvent {
    /// The device went away; the engine plays on silently and keeps trying to reopen it.
    Lost(String),
    /// The device (named) is playing again.
    Restored(String),
}

/// How often a missing device is looked for again.
const RETRY_INTERVAL: Duration = Duration::from_secs(5);
/// A device that has not called back for this long has stopped.
const STALL_TIMEOUT: Duration = Duration::from_secs(2);

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
    input_tx: Sender<Option<InputFeed>>,
    /// Open while a track monitors its input.
    input_stream: Option<cpal::Stream>,
    midi_tx: Sender<MidiFeed>,
    midi_inputs: Option<MidiInputs>,
    recorded_midi: rtrb::Consumer<RecordedMidi>,
    midi_out_tx: Sender<Option<MidiOutQueue>>,
    midi_sender: Option<MidiSender>,
    keyboard: rtrb::Producer<LiveMidi>,
    sync_tx: Sender<SyncConfig>,
    sync: SyncConfig,
    control_midi: rtrb::Consumer<LiveMidi>,
    marker_hits: rtrb::Consumer<Hit>,
    /// The cue lists handed to the audio thread lately, to name the commands it reports.
    recent_cues: VecDeque<Arc<CueList>>,
    proc: Arc<Mutex<Processor>>,
    output: Output,
    devices: DeviceRequest,
    /// When to look for the device again, while it is missing.
    retry_at: Option<Instant>,
    reconnects: u64,
    /// Device output channels the graph last asked for.
    device_channels: usize,
    /// The callback count last seen and when it changed.
    callbacks_seen: (u64, Instant),
    /// Where the metronome plays when the project names no output (`metronome_defout`).
    click_output: i64,
    preview: Arc<crate::preview::Preview>,
    /// Frames per block of the silent clock.
    null_block: usize,
}

impl Engine {
    /// Opens the default output device, falling back to a silent real-time clock when there
    /// is none so the transport still behaves.
    pub fn start(pool: Arc<MediaPool>) -> Self {
        Self::start_with(pool, &DeviceRequest::default())
    }

    /// Opens the devices `request` names, falling back like [`Engine::start`] and looking for
    /// the device again every few seconds (see [`Engine::maintain`]).
    pub fn start_with(pool: Arc<MediaPool>, request: &DeviceRequest) -> Self {
        let rate = request.sample_rate.unwrap_or(48_000);
        if request.is_dummy() {
            return Self::start_dummy(pool, request);
        }
        let mut engine = match open_device(request) {
            Ok((device, config, format, name)) => {
                let mut engine = Self::start_null(pool, config.sample_rate, None);
                engine.devices = request.clone();
                if let Err(e) = engine.attach(&device, config, format, name.clone()) {
                    engine.info.fallback_reason = Some(format!("{name}: {e}"));
                }
                engine
            }
            Err(reason) => Self::start_null(pool, rate, Some(reason)),
        };
        if engine.info.fallback_reason.is_some() {
            engine.retry_at = Some(Instant::now() + RETRY_INTERVAL);
        }
        engine.devices = request.clone();
        engine
    }

    /// Opens exactly the devices `request` names, or says why it cannot: for switching
    /// devices, where a failure keeps the old one rather than falling back to silence.
    pub fn open(pool: Arc<MediaPool>, request: &DeviceRequest) -> Result<Self, String> {
        if request.is_dummy() {
            return Ok(Self::start_dummy(pool, request));
        }
        let (device, config, format, name) = open_device(request)?;
        let mut engine = Self::start_null(pool, config.sample_rate, None);
        engine.devices = request.clone();
        engine.attach(&device, config, format, name.clone()).map_err(|e| format!("{name}: {e}"))?;
        Ok(engine)
    }

    /// REAPER's "Dummy Audio": a silent clock at the requested rate and block size, with the
    /// requested outputs (a pair by default) and no inputs.
    fn start_dummy(pool: Arc<MediaPool>, request: &DeviceRequest) -> Self {
        let rate = request.sample_rate.unwrap_or(48_000);
        let mut engine = Self::start_null(pool, rate, None);
        engine.devices = request.clone();
        engine.null_block = request.buffer_size.map_or(Output::NULL_BLOCK, |b| b as usize);
        engine.go_silent();
        let outputs = request.output_channels.unwrap_or(2) as usize;
        engine.info.name = "Dummy Audio".into();
        engine.info.host = crate::devices::DUMMY_HOST.into();
        engine.info.outputs = crate::devices::channel_names(crate::devices::DUMMY_HOST, false, outputs);
        if let Ok(mut proc) = engine.proc.lock() {
            proc.set_outputs(outputs);
        }
        engine
    }

    /// Closes the device and plays on with a silent clock, for opening another device that
    /// may be the same hardware.
    pub fn release_device(&mut self) {
        self.go_silent();
        self.input_stream = None;
        let _ = self.input_tx.send(None);
        self.retry_at = None;
    }

    /// Opens the devices this engine was asked for again, after [`Engine::release_device`].
    pub fn reopen_device(&mut self) -> Result<(), String> {
        if self.devices.is_dummy() {
            return Ok(());
        }
        let (device, config, format, name) = open_device(&self.devices)?;
        self.attach(&device, config, format, name.clone()).map_err(|e| format!("{name}: {e}"))
    }

    /// Replaces the silent clock with `device`.
    fn attach(
        &mut self,
        device: &cpal::Device,
        mut config: StreamConfig,
        format: SampleFormat,
        name: String,
    ) -> Result<(), String> {
        config.sample_rate = self.info.sample_rate;
        self.output = Output::Device(DeviceOutput {
            stream: None,
            device: device.clone(),
            config,
            format,
            default_channels: config.channels,
            supported: None,
            proc: self.proc.clone(),
        });
        if let Ok(mut proc) = self.proc.lock() {
            proc.set_outputs(config.channels as usize);
        }
        match open_stream(device, &config, format, self.proc.clone(), self.transport.clone()) {
            Ok(stream) => {
                if let Output::Device(out) = &mut self.output {
                    out.stream = Some(stream);
                    out.match_master(self.device_channels, &self.transport);
                }
                let block_size = match config.buffer_size {
                    cpal::BufferSize::Fixed(n) => Some(n),
                    cpal::BufferSize::Default => None,
                };
                let host = crate::devices::host_name(&self.devices);
                let input = crate::devices::input_device(&self.devices).ok().and_then(|d| {
                    let (config, _) =
                        crate::devices::input_config(&d, self.devices.input_channels, self.info.sample_rate).ok()?;
                    Some((crate::devices::device_name(&d), crate::devices::device_id(&d), config.channels as usize))
                });
                let inputs = input.as_ref().map_or(0, |i| i.2);
                self.info = DeviceInfo {
                    device_id: crate::devices::device_id(device),
                    name,
                    sample_rate: self.info.sample_rate,
                    block_size,
                    fallback_reason: None,
                    outputs: crate::devices::channel_names(&host, false, self.output_channels()),
                    inputs: crate::devices::channel_names(&host, true, inputs),
                    input: input.map(|(name, id, _)| (name, id)),
                    host,
                };
                self.callbacks_seen = (self.transport.callbacks.load(Ordering::Relaxed), Instant::now());
                self.transport.lost.store(false, Ordering::Relaxed);
                Ok(())
            }
            Err(e) => {
                self.go_silent();
                Err(e)
            }
        }
    }

    /// Output channels the device plays now.
    fn output_channels(&self) -> usize {
        match &self.output {
            Output::Device(out) => out.config.channels as usize,
            Output::Null { .. } => self.info.outputs.len(),
        }
    }

    fn go_silent(&mut self) {
        self.output = Output::null(self.proc.clone(), self.info.sample_rate, self.null_block);
        self.info.block_size = Some(self.null_block as u32);
    }

    /// Watches the device: when it disappears (unplugged, or it stopped calling back) the
    /// engine carries on with a silent clock, so the transport and the control API keep going,
    /// and the device is looked for again every few seconds. Call it regularly.
    pub fn maintain(&mut self) -> Option<DeviceEvent> {
        let now = Instant::now();
        if let Output::Device(_) = &self.output {
            let calls = self.transport.callbacks.load(Ordering::Relaxed);
            if calls != self.callbacks_seen.0 {
                self.callbacks_seen = (calls, now);
            }
            let stalled = now.duration_since(self.callbacks_seen.1) > STALL_TIMEOUT;
            if !self.transport.lost.swap(false, Ordering::Relaxed) && !stalled {
                return None;
            }
            let what = if stalled { "stopped responding" } else { "was disconnected" };
            let reason = format!("{} {what}", self.info.name);
            self.go_silent();
            self.input_stream = None;
            let _ = self.input_tx.send(None);
            self.info.fallback_reason = Some(reason.clone());
            self.retry_at = Some(now + RETRY_INTERVAL);
            return Some(DeviceEvent::Lost(reason));
        }
        let due = self.retry_at.is_some_and(|at| now >= at);
        if !due {
            return None;
        }
        self.retry_at = Some(now + RETRY_INTERVAL);
        let (device, config, format, name) = open_device(&self.devices).ok()?;
        self.attach(&device, config, format, name.clone()).ok()?;
        self.retry_at = None;
        self.reconnects += 1;
        // The input went with the device; the next sync opens it again.
        self.input_stream = None;
        let _ = self.input_tx.send(None);
        Some(DeviceEvent::Restored(name))
    }

    /// The device and how the audio is doing.
    pub fn health(&self) -> Health {
        let t = &self.transport;
        let delivered = t.block_frames.load(Ordering::Relaxed);
        Health {
            device: self.info.name.clone(),
            sample_rate: self.info.sample_rate,
            block_size: self.info.block_size.or((delivered > 0).then_some(delivered)),
            output_latency: t.output_latency(),
            load: t.load(),
            load_peak: f32::from_bits(t.load_peak.load(Ordering::Relaxed)),
            xruns: t.xruns.load(Ordering::Relaxed),
            overloads: t.overloads.load(Ordering::Relaxed),
            silent: matches!(self.output, Output::Null { .. }),
            reason: self.info.fallback_reason.clone(),
            reconnects: self.reconnects,
            realtime: crate::realtime::status(),
        }
    }

    /// The highest load since the last call, starting a new window.
    pub fn take_load_peak(&self) -> f32 {
        f32::from_bits(self.transport.load_peak.swap(0, Ordering::Relaxed))
    }

    /// An engine on a silent real-time clock: no output device. `reason` says why, when it
    /// should have had one.
    pub fn start_null(pool: Arc<MediaPool>, sample_rate: u32, reason: Option<String>) -> Self {
        let graph = Arc::new(ArcSwap::from_pointee(RenderGraph::empty(sample_rate)));
        let transport = Arc::new(Transport::new(sample_rate));
        let (senders, feeds) = feeds();
        let proc = Arc::new(Mutex::new(Processor::new(graph.clone(), transport.clone(), feeds)));
        let hardware = proc.lock().map(|p| p.hardware.clone()).ok();
        let preview = proc.lock().map(|p| p.preview.clone()).unwrap_or_default();
        let output = Output::null(proc.clone(), sample_rate, Output::NULL_BLOCK);
        Self {
            graph,
            transport,
            pool,
            meters: MeterRegistry::default(),
            fx: {
                let mut fx = FxRegistry::new(sample_rate);
                fx.hardware = hardware;
                fx
            },
            retired: VecDeque::new(),
            input_tx: senders.input_tx,
            input_stream: None,
            midi_tx: senders.midi_tx,
            midi_inputs: None,
            recorded_midi: senders.recorded,
            marker_hits: senders.hits,
            recent_cues: VecDeque::new(),
            midi_out_tx: senders.midi_out_tx,
            midi_sender: None,
            keyboard: senders.keyboard,
            sync_tx: senders.sync_tx,
            sync: SyncConfig::default(),
            control_midi: senders.control,
            info: DeviceInfo {
                name: "No audio device".into(),
                sample_rate,
                block_size: Some(Output::NULL_BLOCK as u32),
                fallback_reason: reason,
                host: String::new(),
                device_id: String::new(),
                input: None,
                inputs: Vec::new(),
                outputs: crate::devices::channel_names("", false, 2),
            },
            proc,
            output,
            devices: DeviceRequest::default(),
            retry_at: None,
            reconnects: 0,
            device_channels: 2,
            callbacks_seen: (0, Instant::now()),
            click_output: -1,
            preview,
            null_block: Output::NULL_BLOCK,
        }
    }

    /// A silent engine that never opens a device, not even an input.
    pub fn start_offline(pool: Arc<MediaPool>, sample_rate: u32) -> Self {
        let mut engine = Self::start_null(pool, sample_rate, None);
        engine.devices.no_input = true;
        engine
    }

    /// The devices this engine was asked to use.
    pub fn devices(&self) -> &DeviceRequest {
        &self.devices
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

    /// The media explorer's preview player.
    pub fn preview(&self) -> &Arc<crate::preview::Preview> {
        &self.preview
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

    /// Where the metronome plays when a project names no output (a `METRONOMEOUT` value, -1
    /// for the master); takes effect at the next [`Engine::sync`].
    pub fn set_default_click_output(&mut self, output: i64) {
        self.click_output = output;
    }

    /// Sets the monitoring effects, which play on the device output only; takes effect at the
    /// next `sync`.
    pub fn set_monitor_fx(&mut self, slots: Vec<scythe_core::FxSlot>) {
        self.fx.monitor_fx = slots;
    }

    /// Rebuilds the render graph from `project` and hands it to the audio thread.
    pub fn sync(&mut self, project: &Project) {
        let played = project.at_playrate();
        let rate = project.playrate.rate;
        let rate = if project.playrate.is_normal() { 1.0 } else { rate };
        let old_rate = self.transport.play_rate();
        if rate != old_rate {
            let at = self.transport.position.load(Ordering::Relaxed) as f64 * old_rate / rate;
            self.transport.position.store(at.round() as i64, Ordering::Relaxed);
            self.transport.play_rate.store(rate.to_bits(), Ordering::Relaxed);
        }
        let project = played.as_ref();
        self.meters.retain(project);
        self.fx.retain(project);
        self.fx.project_dir = project.path.as_deref().and_then(std::path::Path::parent).map(Into::into);
        let rate = self.info.sample_rate;
        let graph =
            RenderGraph::build_with_click(project, &self.pool, &mut self.meters, &mut self.fx, rate, self.click_output);
        self.device_channels = graph.device_channels;
        if let Output::Device(out) = &mut self.output {
            out.match_master(graph.device_channels, &self.transport);
        }
        let playing = self.output_channels();
        if playing != self.info.outputs.len() && matches!(self.output, Output::Device(_)) {
            self.info.outputs = crate::devices::channel_names(&self.info.host, false, playing);
        }
        let old = self.graph.swap(Arc::new(graph));
        let now = Instant::now();
        self.retired.push_back((now, old));
        while self.retired.front().is_some_and(|(t, _)| now.duration_since(*t) > Duration::from_secs(2)) {
            self.retired.pop_front();
        }
        self.set_loop(project.loop_enabled, project.time_selection);
        self.sync_cues(project);
    }

    fn sync_cues(&mut self, project: &Project) {
        let mut cues = CueList::build(project, self.info.sample_rate);
        let current = self.transport.cues.load_full();
        if (&cues.cues, &cues.names, cues.end_frame) == (&current.cues, &current.names, current.end_frame) {
            return;
        }
        cues.generation = current.generation + 1;
        let cues = Arc::new(cues);
        self.transport.cues.store(cues.clone());
        self.recent_cues.push_back(cues);
        while self.recent_cues.len() > 4 {
            self.recent_cues.pop_front();
        }
    }

    /// Turns running the actions of `!` markers during playback on or off.
    pub fn set_marker_actions(&self, on: bool) {
        self.transport.marker_actions.store(on, Ordering::Relaxed);
    }

    pub fn marker_actions(&self) -> bool {
        self.transport.marker_actions.load(Ordering::Relaxed)
    }

    /// The marker actions playback ran into since the last call, in order. Transport commands
    /// (`applied`) have already taken effect; the host runs the others and follows up on stops,
    /// e.g. returning the play cursor to the edit cursor as REAPER's Stop does.
    pub fn take_marker_actions(&mut self) -> Vec<MarkerAction> {
        let mut out = Vec::new();
        while let Ok(hit) = self.marker_hits.pop() {
            let Some(list) = self.recent_cues.iter().find(|c| c.generation == hit.generation) else { continue };
            out.push(MarkerAction {
                position: hit.frame as f64 / self.info.sample_rate as f64,
                command: list.command(hit.command),
                applied: if hit.applied { CueList::transport_action(hit.command) } else { None },
            });
        }
        out
    }

    /// Seeks to `seconds` and plays.
    pub fn play_from(&self, seconds: f64) {
        self.seek(seconds);
        self.play();
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
        let device = crate::devices::input_device(&self.devices)?;
        let (config, _) = crate::devices::input_config(&device, self.devices.input_channels, self.info.sample_rate)?;
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
        if !on {
            return self.set_midi_input_devices(&[]);
        }
        if self.midi_inputs.is_some() {
            return Ok(());
        }
        let map = crate::midi_io::MidiDevices::scan("");
        let devices: Vec<(u8, String)> = map.inputs.iter().map(|p| (p.number, p.name.clone())).collect();
        self.set_midi_input_devices(&devices)
    }

    /// Opens exactly these MIDI inputs (device number and port name), closing the others.
    pub fn set_midi_input_devices(&mut self, devices: &[(u8, String)]) -> Result<(), String> {
        let open: Vec<(u8, String)> = self
            .midi_inputs
            .as_ref()
            .map(|m| m.open.iter().map(|i| (i.number, i.name.clone())).collect())
            .unwrap_or_default();
        if open == devices && (self.midi_inputs.is_some() || devices.is_empty()) {
            return Ok(());
        }
        if devices.is_empty() {
            self.midi_inputs = None;
            let _ = self.midi_tx.send(Vec::new());
            return Ok(());
        }
        self.midi_inputs = None;
        let (inputs, rings) = MidiInputs::open(devices);
        if rings.is_empty() {
            let _ = self.midi_tx.send(Vec::new());
            return Err("no MIDI input".into());
        }
        let _ = self.midi_tx.send(rings);
        self.midi_inputs = Some(inputs);
        Ok(())
    }

    /// Opens every hardware MIDI output (numbered as a fresh device map numbers them) for
    /// tracks that send to one, or closes them.
    pub fn set_midi_outputs(&mut self, on: bool) -> Result<(), String> {
        if !on {
            return self.set_midi_output_devices(&[]);
        }
        if self.midi_sender.is_some() {
            return Ok(());
        }
        let map = crate::midi_io::MidiDevices::scan("");
        let devices: Vec<(u8, String)> = map.outputs.iter().map(|p| (p.number, p.name.clone())).collect();
        self.set_midi_output_devices(&devices)
    }

    /// Opens exactly these MIDI outputs (device number and port name), closing the others.
    pub fn set_midi_output_devices(&mut self, devices: &[(u8, String)]) -> Result<(), String> {
        if self.midi_sender.as_ref().map_or(devices.is_empty(), |s| s.ports == devices) {
            return Ok(());
        }
        let _ = self.midi_out_tx.send(None);
        self.midi_sender = None;
        if devices.is_empty() {
            return Ok(());
        }
        let (sender, queue) = MidiSender::open(devices)?;
        let _ = self.midi_out_tx.send(Some(Arc::new(std::sync::Mutex::new(queue))));
        self.midi_sender = Some(sender);
        Ok(())
    }

    /// Names of the open MIDI inputs.
    pub fn midi_input_names(&self) -> &[String] {
        self.midi_inputs.as_ref().map_or(&[], |m| &m.names)
    }

    /// Device numbers of the open MIDI outputs.
    pub fn midi_output_ports(&self) -> Vec<u8> {
        self.midi_sender.as_ref().map(|s| s.ports.iter().map(|p| p.0).collect()).unwrap_or_default()
    }

    /// Plays a message from the virtual MIDI keyboard (device 62) into the tracks listening
    /// to it; also recorded like any input.
    pub fn virtual_keyboard(&mut self, bytes: [u8; 3]) {
        let len = crate::midi_io::message_len(bytes[0]) as u8;
        let _ = self.keyboard.push(LiveMidi::new(bytes, len).from_device(crate::midi_io::VIRTUAL_KEYBOARD));
        self.wake_effects();
    }

    /// Sets which outputs get MIDI clock and what the transport chases.
    pub fn set_sync(&mut self, config: SyncConfig) {
        if config != self.sync {
            self.sync = config.clone();
            let _ = self.sync_tx.send(config);
        }
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

    /// Messages from the MIDI inputs since the last call, whether or not the transport runs.
    pub fn take_control_midi(&mut self) -> Vec<LiveMidi> {
        let mut out = Vec::new();
        while let Ok(m) = self.control_midi.pop() {
            out.push(m);
        }
        out
    }

    /// Starts recording the signal of each target track; the taps take effect with the next
    /// [`Engine::sync`], and the graph writes them while the transport plays and recording is
    /// on ([`Engine::set_recording`]).
    pub fn start_output_recording(
        &mut self,
        targets: Vec<crate::record::OutputTarget>,
    ) -> Result<crate::record::OutputRecording, crate::record::RecordError> {
        let rec = crate::record::OutputRecording::start(targets, self.info.sample_rate)?;
        for (id, tap) in rec.taps() {
            self.meters.set_record_tap(*id, Some(tap.clone()));
        }
        Ok(rec)
    }

    /// Starts gathering the MIDI the tracks' effects put out (effective with the next
    /// [`Engine::sync`]), for "Output (MIDI)" recording.
    pub fn start_midi_output_recording(&mut self, tracks: &[scythe_core::TrackId]) {
        for &id in tracks {
            let tap = Arc::new(crate::render::MidiRecordTap::new(Vec::with_capacity(1 << 16)));
            self.meters.set_midi_record_tap(id, Some(tap));
        }
    }

    /// What a track put out since [`Engine::start_midi_output_recording`]; with `finish` the
    /// gathering ends (effective with the next [`Engine::sync`]).
    pub fn take_midi_output(&mut self, id: scythe_core::TrackId, finish: bool) -> Vec<(i64, [u8; 3])> {
        let events = self
            .meters
            .midi_record_tap(id)
            .and_then(|t| t.lock().ok().map(|mut e| std::mem::take(&mut *e)))
            .unwrap_or_default();
        if finish {
            self.meters.set_midi_record_tap(id, None);
        }
        events
    }

    /// Takes the taps out (effective with the next [`Engine::sync`]) and closes the files.
    pub fn finish_output_recording(
        &mut self,
        rec: crate::record::OutputRecording,
    ) -> Result<Vec<crate::record::RecordedOutput>, crate::record::RecordError> {
        for (id, _) in rec.taps() {
            self.meters.set_record_tap(*id, None);
        }
        rec.finish()
    }

    /// Feeds `channels` interleaved input channels from `rx` instead of an input device, as
    /// monitored tracks hear it: for tests and generated input.
    pub fn set_input_feed(&mut self, rx: rtrb::Consumer<f32>, channels: usize) {
        self.input_stream = None;
        let _ = self.input_tx.send(Some((rx, channels)));
    }

    /// Tells the metronome whether a recording runs ("run metronome during recording").
    pub fn set_recording(&self, on: bool) {
        self.transport.recording.store(on, Ordering::Relaxed);
    }

    pub fn play(&self) {
        self.transport.count_in_left.store(0, Ordering::Relaxed);
        self.transport.playing.store(true, Ordering::Relaxed);
    }

    /// Plays after `count_in`: the playhead waits where it is while the count-in clicks.
    pub fn play_counting_in(&self, count_in: &scythe_core::metronome::CountIn) {
        self.transport.start_count_in(CountInClicks::from_seconds(count_in, self.info.sample_rate));
        self.transport.playing.store(true, Ordering::Relaxed);
    }

    pub fn stop(&self) {
        self.transport.playing.store(false, Ordering::Relaxed);
        self.transport.count_in_left.store(0, Ordering::Relaxed);
        self.wake_effects();
    }

    /// Keeps effects running for a few seconds while stopped.
    pub fn wake_effects(&self) {
        let frames = self.info.sample_rate as i64 * 3;
        self.transport.wake_frames.store(frames, Ordering::Relaxed);
    }

    pub fn seek(&self, seconds: f64) {
        let frames = (seconds.max(0.0) * self.info.sample_rate as f64 / self.transport.play_rate()).round() as i64;
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

fn open_device(request: &DeviceRequest) -> Result<(cpal::Device, StreamConfig, SampleFormat, String), String> {
    let device = crate::devices::output_device(request)?;
    let name = crate::devices::device_name(&device);
    let supported = device.default_output_config().map_err(|e| e.to_string())?;
    let format = supported.sample_format();
    let mut config: StreamConfig = supported.into();
    // A JACK server runs at its own rate and block size, as REAPER's JACK page has none to ask.
    let server = crate::devices::host_name(request).eq_ignore_ascii_case("JACK");
    if let Some(rate) = request.sample_rate.filter(|_| !server) {
        config.sample_rate = rate;
    }
    if let Some(frames) = request.buffer_size.filter(|_| !server) {
        config.buffer_size = cpal::BufferSize::Fixed(frames);
    }
    let supported = supported_channels(&device, format, config.sample_rate);
    config.channels = crate::devices::stream_channels(&supported, config.channels, request.output_channels);
    Ok((device, config, format, name))
}

/// Channel counts `device` can play at `rate` in `format`, ascending.
fn supported_channels(device: &cpal::Device, format: SampleFormat, rate: u32) -> Vec<u16> {
    let mut channels: Vec<u16> = device
        .supported_output_configs()
        .map(|configs| {
            configs
                .filter(|c| c.sample_format() == format && (c.min_sample_rate()..=c.max_sample_rate()).contains(&rate))
                .map(|c| c.channels())
                .collect()
        })
        .unwrap_or_default();
    channels.sort_unstable();
    channels.dedup();
    channels
}

/// The output channel count for a master of `master_channels`: the fewest the device offers
/// that carry every master channel, else as many as it has, never fewer than its default.
fn output_channels(supported: &[u16], default: u16, master_channels: usize) -> u16 {
    let want = u16::try_from(master_channels).unwrap_or(u16::MAX).max(default);
    let fitting = supported.iter().copied().filter(|&c| c >= want).min();
    fitting.or_else(|| supported.iter().copied().max()).map_or(default, |c| c.max(default))
}

fn open_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    format: SampleFormat,
    proc: Arc<Mutex<Processor>>,
    transport: Arc<Transport>,
) -> Result<cpal::Stream, String> {
    let stream = match format {
        SampleFormat::F32 => build::<f32>(device, config, proc, transport),
        SampleFormat::I16 => build::<i16>(device, config, proc, transport),
        SampleFormat::I32 => build::<i32>(device, config, proc, transport),
        SampleFormat::U16 => build::<u16>(device, config, proc, transport),
        SampleFormat::F64 => build::<f64>(device, config, proc, transport),
        other => return Err(format!("unsupported sample format {other}")),
    }?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}

fn build<T: SizedSample + FromSample<f32> + Send + 'static>(
    device: &cpal::Device,
    config: &StreamConfig,
    proc: Arc<Mutex<Processor>>,
    transport: Arc<Transport>,
) -> Result<cpal::Stream, String> {
    let channels = config.channels as usize;
    let errors = transport.clone();
    let mut asked_realtime = false;
    device
        .build_output_stream::<T, _, _>(
            *config,
            move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
                if !asked_realtime {
                    crate::realtime::request();
                    asked_realtime = true;
                }
                let t = info.timestamp();
                let latency = t.playback.duration_since(t.callback);
                transport.output_latency.store(latency.as_secs_f64().to_bits(), Ordering::Relaxed);
                transport.callbacks.fetch_add(1, Ordering::Relaxed);
                transport.block_frames.store((data.len() / channels.max(1)) as u32, Ordering::Relaxed);
                // Only contended while the main thread swaps the stream.
                match proc.try_lock() {
                    Ok(mut proc) => proc.write_interleaved(data, channels),
                    Err(_) => data.fill(T::EQUILIBRIUM),
                }
            },
            move |err| match err.kind() {
                cpal::ErrorKind::Xrun => {
                    errors.xruns.fetch_add(1, Ordering::Relaxed);
                }
                cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::StreamInvalidated => {
                    errors.lost.store(true, Ordering::Relaxed);
                }
                _ => eprintln!("audio stream error: {err}"),
            },
            None,
        )
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segs(t: &Transport, frames: usize) -> Vec<(i64, usize, usize)> {
        segs_hits(t, frames).0
    }

    /// Rendered segments, and the hits as (frame, command id or 0 when named, applied).
    type Advanced = (Vec<(i64, usize, usize)>, Vec<(i64, u32, bool)>);

    fn segs_hits(t: &Transport, frames: usize) -> Advanced {
        let mut s = [(0, 0, 0); 8];
        let mut hits = Vec::new();
        let n = t.advance(frames, &mut s, &mut |h: Hit| {
            let id = match h.command {
                crate::cues::CueCommand::Id(id) => id,
                crate::cues::CueCommand::Named(_) => 0,
            };
            hits.push((h.frame, id, h.applied));
        });
        (s[..n].to_vec(), hits)
    }

    /// A transport at 100 Hz playing from `from` with these markers.
    fn with_markers(markers: &[(f64, &str)], from: i64) -> Transport {
        let mut p = Project::default();
        for &(at, name) in markers {
            p.add_marker(at, name);
        }
        let t = Transport::new(100);
        t.cues.store(Arc::new(CueList::build(&p, 100)));
        t.position.store(from, Ordering::Relaxed);
        t.playing.store(true, Ordering::Relaxed);
        t
    }

    #[test]
    fn stop_marker_stops_on_its_frame() {
        let t = with_markers(&[(0.25, "!1016"), (0.27, "!40042")], 0);
        assert_eq!(segs_hits(&t, 10), (vec![(0, 0, 10)], vec![]));
        assert_eq!(segs_hits(&t, 10), (vec![(10, 0, 10)], vec![]));
        assert_eq!(segs_hits(&t, 10), (vec![(20, 0, 5)], vec![(25, 1016, true)]));
        assert!(!t.is_playing());
        assert_eq!(t.position.load(Ordering::Relaxed), 25);
        assert_eq!(segs(&t, 10), []);
    }

    #[test]
    fn marker_on_a_block_edge_fires_once() {
        let t = with_markers(&[(0.2, "!1016")], 0);
        assert_eq!(segs_hits(&t, 20), (vec![(0, 0, 20)], vec![(20, 1016, true)]));
        assert_eq!(t.position.load(Ordering::Relaxed), 20);
        assert!(!t.is_playing());
    }

    #[test]
    fn starting_or_seeking_onto_a_marker_does_not_run_it() {
        let t = with_markers(&[(0.2, "!1016"), (0.5, "!1016")], 20);
        assert_eq!(segs_hits(&t, 10), (vec![(20, 0, 10)], vec![]));
        t.seek.store(60, Ordering::Relaxed);
        assert_eq!(segs_hits(&t, 10), (vec![(60, 0, 10)], vec![]));
        assert!(t.is_playing());
    }

    #[test]
    fn disabled_marker_actions_play_through() {
        let t = with_markers(&[(0.05, "!1016")], 0);
        t.marker_actions.store(false, Ordering::Relaxed);
        assert_eq!(segs_hits(&t, 10), (vec![(0, 0, 10)], vec![]));
        assert!(t.is_playing());
    }

    #[test]
    fn several_commands_run_in_order() {
        let t = with_markers(&[(0.05, "!1068 _SWS_X 40043"), (0.15, "!1016"), (0.3, "!40042 1016")], 0);
        let (s, h) = segs_hits(&t, 10);
        assert_eq!(s, [(0, 0, 5), (30, 5, 5)]);
        assert_eq!(h, [(5, 1068, true), (5, 0, false), (5, 40043, true)]);
        assert!(t.loop_enabled.load(Ordering::Relaxed));
        t.seek.store(25, Ordering::Relaxed);
        assert_eq!(segs_hits(&t, 10), (vec![(25, 0, 5)], vec![(30, 40042, true), (30, 1016, true)]));
        assert_eq!(t.position.load(Ordering::Relaxed), 0);
        assert!(!t.is_playing());
    }

    #[test]
    fn markers_before_the_loop_end_fire_and_the_loop_start_does_not() {
        let t = with_markers(&[(0.1, "!1007"), (0.18, "!1007")], 12);
        t.loop_enabled.store(true, Ordering::Relaxed);
        t.loop_start.store(10, Ordering::Relaxed);
        t.loop_end.store(20, Ordering::Relaxed);
        let (s, h) = segs_hits(&t, 12);
        assert_eq!(s, [(12, 0, 6), (18, 6, 2), (10, 8, 4)]);
        assert_eq!(h, [(18, 1007, true)]);
    }

    #[test]
    fn engine_names_commands_and_reports_stops() {
        let mut e = Engine::start_null(Arc::default(), 48_000, None);
        let mut p = Project::default();
        p.add_marker(0.05, "!_SWS_X 1016");
        e.sync(&p);
        e.play();
        std::thread::sleep(Duration::from_millis(200));
        assert!(!e.transport().is_playing());
        assert_eq!(e.transport().position_seconds(), 0.05);
        let actions = e.take_marker_actions();
        assert_eq!(
            actions,
            [
                MarkerAction {
                    position: 0.05,
                    command: scythe_core::marker_action::Command::Named("_SWS_X".into()),
                    applied: None
                },
                MarkerAction {
                    position: 0.05,
                    command: scythe_core::marker_action::Command::Id(1016),
                    applied: Some(TransportAction::Stop)
                },
            ]
        );
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
    fn output_channels_follow_the_master_within_the_device() {
        assert_eq!(output_channels(&[1, 2, 4, 6, 8], 2, 2), 2);
        assert_eq!(output_channels(&[1, 2, 4, 6, 8], 2, 4), 4);
        assert_eq!(output_channels(&[2, 6, 8], 2, 4), 6);
        assert_eq!(output_channels(&[2, 6, 8], 2, 16), 8);
        assert_eq!(output_channels(&[2], 2, 6), 2);
        assert_eq!(output_channels(&[], 2, 6), 2);
        assert_eq!(output_channels(&[2, 4, 8], 8, 4), 8);
    }

    #[test]
    fn interleave_maps_master_channels_to_outputs() {
        let (l, r) = ([1.0, 2.0], [3.0, 4.0]);
        let extra = [vec![5.0, 6.0]];
        let mut out = [9.0f32; 8];
        interleave(&mut out, 4, &l, &r, &extra);
        assert_eq!(out, [1.0, 3.0, 5.0, 0.0, 2.0, 4.0, 6.0, 0.0]);
        let mut stereo = [9.0f32; 4];
        interleave(&mut stereo, 2, &l, &r, &extra);
        assert_eq!(stereo, [1.0, 3.0, 2.0, 4.0]);
    }

    #[test]
    fn processor_sends_every_master_channel_to_the_device() {
        use scythe_core::{Item, Take};
        let dir = std::env::temp_dir().join(format!("scythe-engine-mc-{}", std::process::id()));
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
        let t = p.add_track("t");
        p.track_mut(t).unwrap().channels = 4;
        let mut item = Item::new(0.0, 1.0, Take::from_file(src.clone()));
        (item.fade_in, item.fade_out) = (scythe_core::Fade::default(), scythe_core::Fade::default());
        p.track_mut(t).unwrap().items.push(item);
        let pool = MediaPool::default();
        pool.load(&src, 8000).unwrap();
        let graph = RenderGraph::build(&p, &pool, &mut MeterRegistry::default(), &mut FxRegistry::new(8000), 8000);
        let transport = Arc::new(Transport::new(8000));
        transport.playing.store(true, Ordering::Relaxed);
        let (_senders, feeds) = feeds();
        let mut proc = Processor::new(Arc::new(ArcSwap::from_pointee(graph)), transport, feeds);
        proc.set_outputs(6);
        let mut out = vec![0.0f32; 6 * 1000];
        proc.write_interleaved(&mut out, 6);
        let frame = &out[6 * 500..6 * 501];
        let expected = [0.1, 0.2, 0.3, 0.4, 0.0, 0.0];
        assert!(frame.iter().zip(expected).all(|(a, b)| (a - b).abs() < 1e-6), "{frame:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A processor at 8 kHz playing `p`, whose first track holds one item of ones from `item_at`.
    fn ones_project(item_at: f64) -> (Project, MediaPool, std::path::PathBuf) {
        use scythe_core::{Item, Take};
        let dir = std::env::temp_dir().join(format!("scythe-engine-ci-{}-{item_at}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("ones.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 8000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(&src, spec).unwrap();
        for _ in 0..80_000 {
            w.write_sample(1.0f32).unwrap();
        }
        w.finalize().unwrap();
        let mut p = Project { tempo: scythe_core::TempoMap::constant(120.0, 4, 4), ..Project::default() };
        let t = p.add_track("t");
        let mut item = Item::new(item_at, 10.0, Take::from_file(src.clone()));
        (item.fade_in, item.fade_out) = (scythe_core::Fade::default(), scythe_core::Fade::default());
        p.track_mut(t).unwrap().items.push(item);
        let pool = MediaPool::default();
        pool.load(&src, 8000).unwrap();
        (p, pool, dir)
    }

    fn processor(p: &Project, pool: &MediaPool, transport: &Arc<Transport>) -> Processor {
        let graph = RenderGraph::build(p, pool, &mut MeterRegistry::default(), &mut FxRegistry::new(8000), 8000);
        let (_senders, feeds) = feeds();
        let mut proc = Processor::new(Arc::new(ArcSwap::from_pointee(graph)), transport.clone(), feeds);
        proc.set_outputs(2);
        proc
    }

    /// Renders `frames` in uneven blocks so block edges fall everywhere; the left channel.
    fn run(proc: &mut Processor, frames: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames);
        let mut block = 97;
        while out.len() < frames {
            let n = block.min(frames - out.len());
            let mut buf = vec![0.0f32; n * 2];
            proc.write_interleaved(&mut buf, 2);
            out.extend(buf.chunks(2).map(|f| f[0]));
            block = if block > 1500 { 97 } else { block * 2 + 1 };
        }
        out
    }

    /// A hardware insert on the null engine with a 300-frame loopback cable on outputs and
    /// inputs 3/4: with its delay compensation at 300 frames the return lines up with a
    /// second track playing the same item.
    #[test]
    fn a_hardware_insert_lines_up_through_a_loopback() {
        use scythe_core::{Item, Take};
        let dir = std::env::temp_dir().join(format!("scythe-engine-insert-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("tone.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 8000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let tone = |i: usize| 0.25 * (i as f32 * 0.037).sin() + 0.1 * (i as f32 * 0.0051).cos();
        let mut w = hound::WavWriter::create(&src, spec).unwrap();
        for i in 0..16_000 {
            w.write_sample(tone(i)).unwrap();
        }
        w.finalize().unwrap();
        let mut state = crate::builtin::Builtin::HardwareInsert.default_state();
        state[32..40].copy_from_slice(&(300.0 / crate::stock::reainsert::MAX_DELAY).to_le_bytes());
        let mut p = Project::default();
        for with_insert in [true, false] {
            let t = p.add_track("t");
            let track = p.track_mut(t).unwrap();
            let mut item = Item::new(0.0, 2.0, Take::from_file(src.clone()));
            (item.fade_in, item.fade_out) = (scythe_core::Fade::default(), scythe_core::Fade::default());
            track.items.push(item);
            if with_insert {
                track.fx.push(crate::builtin::Builtin::HardwareInsert.slot_with(&state));
            }
        }
        let pool = MediaPool::default();
        pool.load(&src, 8000).unwrap();
        let transport = Arc::new(Transport::new(8000));
        transport.playing.store(true, Ordering::Relaxed);
        let (_senders, feeds) = feeds();
        let hardware = Arc::new(crate::hardware_io::HardwareIo::default());
        hardware.set_loopback(Some(300));
        let mut fx = FxRegistry::new(8000);
        fx.hardware = Some(hardware.clone());
        let graph = RenderGraph::build(&p, &pool, &mut MeterRegistry::default(), &mut fx, 8000);
        assert_eq!(graph.device_channels, 4);
        let mut proc = Processor::new(Arc::new(ArcSwap::from_pointee(graph)), transport, feeds);
        proc.hardware = hardware;
        proc.set_outputs(4);
        let mut out = Vec::new();
        for _ in 0..60 {
            let mut buf = vec![0.0f32; 4 * 128];
            proc.write_interleaved(&mut buf, 4);
            out.extend(buf.chunks(4).map(|f| f[0]));
        }
        // Items on the insert's track are read 300 frames ahead, so its return arrives in time;
        // before the cable has carried anything only the other track plays.
        for (i, v) in out.iter().enumerate() {
            let want = if i < 300 { tone(i) } else { 2.0 * tone(i) };
            assert!((v - want).abs() < 1e-5, "frame {i}: {v} vs {want}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn count_in_clicks_on_the_beat_and_starts_on_the_frame() {
        let (p, pool, dir) = ones_project(2.0);
        let transport = Arc::new(Transport::new(8000));
        let mut proc = processor(&p, &pool, &transport);
        // One 4/4 bar at 120 bpm: four beats of 4000 frames, playback from 2 s (frame 16000).
        transport.position.store(16_000, Ordering::Relaxed);
        transport.start_count_in(CountInClicks {
            frames: 16_000,
            beat: 4000.0,
            first: 0.0,
            first_in_bar: 0,
            beats_per_bar: 4,
            audible: 16_000,
        });
        transport.playing.store(true, Ordering::Relaxed);
        assert_eq!(transport.count_in(), Some((-2.0, 1, 4)));

        let out = run(&mut proc, 20_000);
        let click = |at: usize| out[at..at + 200].iter().any(|&s| s != 0.0);
        for k in 0..4 {
            let at = k * 4000;
            assert!(out[at] == 0.0 && click(at), "beat {k} clicks from frame {at}");
            assert!(at == 0 || out[at - 1] == 0.0, "nothing sounds just before beat {k}");
            assert!(out[at + 400..at + 4000].iter().all(|&s| s == 0.0), "only the click sounds in beat {k}");
        }
        let accent = out[..240].iter().fold(0f32, |m, s| m.max(s.abs()));
        let second = out[4000..4240].iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(accent > second, "the bar's first beat is accented");
        assert_eq!(out[15_999], 0.0);
        assert!(out[16_000..].iter().all(|&s| (s - 1.0).abs() < 1e-6), "the song starts on the count-in's last frame");
        assert_eq!(transport.position.load(Ordering::Relaxed), 16_000 + 4000);
        assert_eq!(transport.count_in(), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_lead_in_clicks_on_the_grid_and_only_while_audible() {
        let (p, pool, dir) = ones_project(2.0);
        let transport = Arc::new(Transport::new(8000));
        let mut proc = processor(&p, &pool, &transport);
        transport.position.store(16_000, Ordering::Relaxed);
        // 9000 frames before the playhead moves; clicks a quarter beat in, on beat 4 of a bar,
        // and silent after frame 4000 (a pre-roll before the project start, metronome off).
        transport.start_count_in(CountInClicks {
            frames: 9000,
            beat: 4000.0,
            first: 1000.0,
            first_in_bar: 3,
            beats_per_bar: 4,
            audible: 4000,
        });
        transport.playing.store(true, Ordering::Relaxed);
        assert_eq!(transport.count_in(), Some((-9000.0 / 8000.0, 1, 2)));
        let out = run(&mut proc, 10_000);
        assert!(out[..1000].iter().all(|&s| s == 0.0));
        assert!(out[1000..1240].iter().any(|&s| s != 0.0), "the first click");
        assert!(out[1300..9000].iter().all(|&s| s == 0.0), "the second click is past the audible part");
        assert!(out[9000..].iter().all(|&s| (s - 1.0).abs() < 1e-6), "the song starts on frame 9000");
        assert_eq!(transport.position.load(Ordering::Relaxed), 17_000);

        let mut accent = |first_in_bar| {
            transport.position.store(16_000, Ordering::Relaxed);
            transport.start_count_in(CountInClicks {
                frames: 4000,
                beat: 4000.0,
                first: 0.0,
                first_in_bar,
                beats_per_bar: 4,
                audible: 4000,
            });
            let out = run(&mut proc, 4000);
            out[..240].iter().fold(0f32, |m, s| m.max(s.abs()))
        };
        let (first, fourth) = (accent(0), accent(3));
        assert!(first > fourth, "the bar's first beat is accented");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_last_count_in_click_rings_past_the_start() {
        let (p, pool, dir) = ones_project(2.0);
        let transport = Arc::new(Transport::new(8000));
        let mut proc = processor(&p, &pool, &transport);
        transport.position.store(16_000, Ordering::Relaxed);
        // The only click 100 frames before the playhead moves, played across block edges.
        transport.start_count_in(CountInClicks {
            frames: 4100,
            beat: 4000.0,
            first: 4000.0,
            first_in_bar: 0,
            beats_per_bar: 4,
            audible: 4100,
        });
        transport.playing.store(true, Ordering::Relaxed);
        let out = run(&mut proc, 9000);
        assert!(out[..4000].iter().all(|&s| s == 0.0));
        assert!(out[4000..4100].iter().any(|&s| s != 0.0), "the click");
        assert!(out[4100..4200].iter().any(|&s| (s - 1.0).abs() > 1e-3), "it rings on over the song");
        assert!(out[8000..].iter().all(|&s| (s - 1.0).abs() < 1e-6), "and then the song alone");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stopping_ends_the_count_in() {
        let (p, pool, dir) = ones_project(0.0);
        let mut e = Engine::start_offline(Arc::new(pool), 8000);
        e.sync(&p);
        let count_in = scythe_core::metronome::CountIn {
            length: 4.0,
            beat: 0.5,
            first: 0.0,
            first_in_bar: 0,
            beats_per_bar: 4,
            audible: 4.0,
        };
        e.play_counting_in(&count_in);
        let (at, beat, beats) = e.transport().count_in().unwrap();
        assert!((-4.0..=0.0).contains(&at) && beat >= 1 && beats == 8, "{at} {beat}");
        e.stop();
        assert_eq!(e.transport().count_in(), None);
        e.play();
        assert_eq!(e.transport().count_in(), None, "plain play does not resume a count-in");
        e.stop();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_metronome_follows_its_outputs_and_mix_clicks() {
        use scythe_core::monitor::{CueBusOptions, EXT_CLICK, MonitorOutput};
        let (mut p, pool, dir) = ones_project(100.0);
        p.metronome.set(scythe_core::metronome::ENABLED, true);
        p.metronome.outputs = vec![2];
        let ids: Vec<_> = p.tracks.iter().map(|t| t.id).collect();
        let opts = CueBusOptions { output: Some(MonitorOutput::Stereo(4)), ..CueBusOptions::default() };
        let mix = p.create_monitor_mix("Drums", &ids, &[], &opts);
        p.track_mut(mix).unwrap().fx.clear();
        p.track_mut(mix).unwrap().set_ext(EXT_CLICK, Some("2"));
        let transport = Arc::new(Transport::new(8000));
        let graph = RenderGraph::build(&p, &pool, &mut MeterRegistry::default(), &mut FxRegistry::new(8000), 8000);
        assert_eq!(graph.device_channels, 6);
        let (_senders, feeds) = feeds();
        let mut proc = Processor::new(Arc::new(ArcSwap::from_pointee(graph)), transport.clone(), feeds);
        proc.set_outputs(6);
        transport.playing.store(true, Ordering::Relaxed);
        let mut out = vec![0.0f32; 6 * 400];
        proc.write_interleaved(&mut out, 6);
        let peak = |c: usize| out.chunks(6).fold(0f32, |m, f| m.max(f[c].abs()));
        assert_eq!((peak(0), peak(1)), (0.0, 0.0), "the audience hears no click");
        assert!(peak(2) > 0.0 && peak(3) > 0.0, "the metronome output does");
        assert!(peak(4) > 0.0 && (peak(4) / peak(2) - 2.0).abs() < 1e-3, "the mix has it at its own level");

        // Off, the metronome leaves the outputs and the mix alone.
        p.metronome.set(scythe_core::metronome::ENABLED, false);
        let graph = RenderGraph::build(&p, &pool, &mut MeterRegistry::default(), &mut FxRegistry::new(8000), 8000);
        proc.graph.store(Arc::new(graph));
        transport.seek.store(0, Ordering::Relaxed);
        proc.write_interleaved(&mut out, 6);
        assert!(out.iter().all(|&s| s == 0.0));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A band's interface with 20 outputs: the audience on 1/2, an in-ear mix on 9/10 and the
    /// click alone on 11, each output carrying only its own signal.
    #[test]
    fn a_twenty_output_device_plays_an_in_ear_mix_on_9_10_and_the_click_on_11() {
        use scythe_core::monitor::{CueBusOptions, MonitorOutput};
        let (mut p, pool, dir) = ones_project(100.0);
        p.metronome.set(scythe_core::metronome::ENABLED, true);
        p.metronome.outputs = vec![10 | scythe_core::HwOut::MONO];
        let ids: Vec<_> = p.tracks.iter().map(|t| t.id).collect();
        let opts = CueBusOptions { output: Some(MonitorOutput::Stereo(8)), ..CueBusOptions::default() };
        let mix = p.create_monitor_mix("Bass", &ids, &[], &opts);
        p.track_mut(mix).unwrap().fx.clear();
        for item in &mut p.tracks[0].items {
            item.position = 0.0;
        }
        let graph = RenderGraph::build(&p, &pool, &mut MeterRegistry::default(), &mut FxRegistry::new(8000), 8000);
        assert_eq!(graph.device_channels, 11);
        let transport = Arc::new(Transport::new(8000));
        let (_senders, feeds) = feeds();
        let mut proc = Processor::new(Arc::new(ArcSwap::from_pointee(graph)), transport.clone(), feeds);
        proc.set_outputs(20);
        transport.playing.store(true, Ordering::Relaxed);
        let mut out = vec![0.0f32; 20 * 800];
        proc.write_interleaved(&mut out, 20);
        let peak = |c: usize| out.chunks(20).fold(0f32, |m, f| m.max(f[c].abs()));
        let peaks: Vec<f32> = (0..20).map(peak).collect();
        assert!(peaks[0] > 0.5 && peaks[1] > 0.5, "the audience hears the track: {peaks:?}");
        assert!(peaks[8] > 0.5 && peaks[9] > 0.5, "the in-ear mix plays on 9/10: {peaks:?}");
        assert!(peaks[10] > 0.0, "the click plays on 11: {peaks:?}");
        let silent = [2, 3, 4, 5, 6, 7, 11, 12, 13, 14, 15, 16, 17, 18, 19];
        assert!(silent.iter().all(|&c| peaks[c] == 0.0), "nothing else is heard: {peaks:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An 18-input interface: a track monitoring input 17 hears that channel, not the first.
    #[test]
    fn monitoring_reaches_every_input_of_a_large_interface() {
        let mut p = Project::default();
        let t = p.add_track("Vocal");
        let track = p.track_mut(t).unwrap();
        (track.armed, track.monitor, track.rec_input) = (true, true, 16);
        let pool = MediaPool::default();
        let graph = RenderGraph::build(&p, &pool, &mut MeterRegistry::default(), &mut FxRegistry::new(8000), 8000);
        let transport = Arc::new(Transport::new(8000));
        let (senders, feeds) = feeds();
        let mut proc = Processor::new(Arc::new(ArcSwap::from_pointee(graph)), transport, feeds);
        proc.set_outputs(2);
        let (mut tx, rx) = rtrb::RingBuffer::<f32>::new(18 * 4096);
        for _ in 0..1024 {
            for c in 0..18 {
                let _ = tx.push(if c == 16 { 0.5 } else { 0.0 });
            }
        }
        senders.input_tx.send(Some((rx, 18))).unwrap();
        let mut out = vec![0.0f32; 2 * 512];
        proc.write_interleaved(&mut out, 2);
        let peak = out.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.2, "input 17 reaches the track: {peak}");
    }

    /// Records a track's output (post-fader and pre-FX) on the null engine, the track hearing
    /// a generated input.
    #[test]
    fn null_engine_records_track_output() {
        use scythe_core::recording::{OutputFormat, RecordMode, RecordTap};
        let dir = std::env::temp_dir().join(format!("scythe-outrec-{}", std::process::id()));
        let mut e = Engine::start_null(Arc::default(), 48_000, None);
        let (mut tx, rx) = rtrb::RingBuffer::<f32>::new(48_000);
        e.set_input_feed(rx, 1);
        let mut p = Project::default();
        let mut ids = Vec::new();
        for tap in [RecordTap::PostFader, RecordTap::PreFx] {
            let id = p.add_track("out");
            let t = p.track_mut(id).unwrap();
            (t.armed, t.monitor, t.rec_input, t.volume) = (true, true, 0, 0.5);
            t.rec_mode = RecordMode::Output { format: OutputFormat::Stereo, compensated: true };
            t.rec_tap = tap;
            ids.push(id);
        }
        let targets = ids
            .iter()
            .enumerate()
            .map(|(i, &track)| crate::record::OutputTarget {
                track,
                point: p.track(track).unwrap().rec_tap,
                compensated: true,
                channels: 2,
                path: dir.join(format!("{i}.wav")),
            })
            .collect();
        let rec = e.start_output_recording(targets).unwrap();
        e.sync(&p);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_feed = stop.clone();
        let feeder = std::thread::spawn(move || {
            while !stop_feed.load(Ordering::Relaxed) {
                while tx.slots() > 24_000 {
                    let _ = tx.push(0.4);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        e.seek(1.0);
        e.set_recording(true);
        e.play();
        std::thread::sleep(Duration::from_millis(300));
        e.stop();
        e.set_recording(false);
        stop.store(true, Ordering::Relaxed);
        feeder.join().unwrap();
        let out = e.finish_output_recording(rec).unwrap();
        e.sync(&p);
        assert_eq!(out.len(), 2);
        for (o, want) in out.iter().zip([0.2f32, 0.4]) {
            assert!(o.frames > 4_800, "{} frames", o.frames);
            assert!(o.start.is_some_and(|s| (1.0..1.05).contains(&s)), "start {:?}", o.start);
            let mut reader = hound::WavReader::open(&o.path).unwrap();
            assert_eq!(reader.spec().channels, 2);
            let samples: Vec<f32> = reader.samples::<i32>().map(|s| s.unwrap() as f32 / 8_388_607.0).collect();
            // The input ring starts filling with the first block; past that it is steady.
            let steady = &samples[samples.len() / 2..];
            assert!(steady.iter().all(|s| (s - want).abs() < 1e-4), "want {want}, got {:?}", &steady[..4]);
        }
        std::fs::remove_dir_all(&dir).ok();
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

    #[test]
    fn null_engine_keeps_time_while_rayon_is_busy() {
        let mut e = Engine::start_null(Arc::default(), 48_000, None);
        let mut p = Project::default();
        p.add_track("a");
        p.add_track("b");
        e.sync(&p);
        // Work queued on the global pool, as converting media while a set loads does.
        let release = Arc::new(AtomicBool::new(false));
        for _ in 0..rayon::current_num_threads() * 2 {
            let release = release.clone();
            rayon::spawn(move || {
                let until = Instant::now() + Duration::from_secs(5);
                while !release.load(Ordering::Relaxed) && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(5));
                }
            });
        }
        e.play();
        let started = Instant::now();
        std::thread::sleep(Duration::from_secs(2));
        let (played, wall) = (e.transport().position_seconds(), started.elapsed().as_secs_f64());
        release.store(true, Ordering::Relaxed);
        e.stop();
        assert!((played / wall - 1.0).abs() < 0.05, "played {played:.3} s in {wall:.3} s");
    }
}
