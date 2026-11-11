//! Anticipative FX, as REAPER's "Anticipative FX processing": the tracks that hear nothing
//! live render ahead of the playhead on worker threads, a render-ahead (200 ms by default)
//! before they are due, so heavy chains no longer have to fit in one device block. The audio
//! callback renders only the live tracks (armed, monitoring, hardware MIDI out and whatever
//! they feed, see [`AheadPlan`]) and the master, adding what the tracks rendered ahead left in
//! their sinks.
//!
//! The worker hands the callback chunks through a pair of lock-free queues; chunk buffers are
//! allocated once, by the worker. The callback steers the worker by epochs: it starts one at a
//! frame when playback starts or jumps, and pauses the worker whenever it needs every track to
//! itself (stopped, counting in, after a miss). A chunk counts only in its epoch, so whatever
//! the worker rendered for an old one is thrown away.
//!
//! An edit or a parameter change makes the callback keep only a short horizon of what was
//! rendered and start a new epoch after it, and caps the render-ahead at that horizon for a
//! second, so a knob being turned is heard within a few blocks, as REAPER renders less ahead
//! while effects are tweaked.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;

use crate::render::{AheadPlan, LiveInput, MAX_SINK_CHANNELS, RenderGraph, RenderScratch};

/// Chunks in flight: enough for a long render-ahead at small chunks.
const CHUNKS: usize = 512;
/// Most frames one chunk holds.
pub const MAX_CHUNK: usize = RenderScratch::MAX_BLOCK;
/// How long the render-ahead stays at the tweak horizon after an edit or parameter change.
const HOLD: Duration = Duration::from_secs(1);

/// REAPER's Preferences > Audio > Buffering, as far as Scythe follows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Buffering {
    /// "Anticipative FX processing" (`workrender` &1).
    pub anticipative: bool,
    /// "Render-ahead" in milliseconds (`renderaheadlen`, default 200).
    pub render_ahead_ms: u32,
    /// Threads that render ahead (`workthreads`); 0 for one per fast core. Takes effect at the
    /// next start.
    pub threads: usize,
    /// "Allow live FX multiprocessing": the tracks the callback renders spread over the
    /// real-time track threads instead of all running on the audio thread.
    pub live_multiprocessing: bool,
}

impl Default for Buffering {
    fn default() -> Self {
        Self { anticipative: true, render_ahead_ms: 200, threads: 0, live_multiprocessing: true }
    }
}

/// One chunk of sink audio: what the tracks rendered ahead feed the live ones over `len`
/// frames from project frame `pos`.
struct Chunk {
    epoch: u64,
    /// Counts every chunk rendered, for the meters (see `Meter::record_ahead`).
    serial: i64,
    pos: i64,
    len: usize,
    /// The plan's key, so a chunk rendered for another set of live tracks is not used.
    layout: u64,
    channels: Vec<Vec<f32>>,
}

/// The chunk buffers. A chunk belongs to whichever side last took its index from a queue.
struct Slots(Box<[UnsafeCell<Chunk>]>);

// SAFETY: a chunk is reached only by the side holding its index, which the queues hand over
// with the ordering a release/acquire pair gives.
unsafe impl Sync for Slots {}

impl Slots {
    /// # Safety
    /// The caller must hold index `i` (taken from a queue and not yet given back).
    #[allow(clippy::mut_from_ref)]
    unsafe fn get(&self, i: u16) -> &mut Chunk {
        unsafe { &mut *self.0[i as usize].get() }
    }
}

/// What the callback, the worker and the main thread share.
pub struct AheadShared {
    slots: Slots,
    /// The epoch the worker renders for, shifted left once, with bit 0 set while it may run.
    command: AtomicU64,
    /// The frame the current epoch starts at.
    start: AtomicI64,
    /// The worker is rendering a chunk (set before it reads `command`).
    busy: AtomicBool,
    parked: AtomicBool,
    /// Frames the callback has taken in the current epoch, and old-epoch frames it still takes.
    consumed: AtomicU64,
    old_left: AtomicU64,
    /// Frames of the first chunks after a start, and of chunks once the worker is ahead.
    first: AtomicUsize,
    block: AtomicUsize,
    /// Render-ahead in frames, and while tweaking.
    target: AtomicU64,
    horizon: AtomicU64,
    /// Bumped by the main thread on every parameter change.
    tweaks: AtomicU64,
    /// Until when (nanoseconds from `origin`) the render-ahead stays at the horizon.
    hold_until: AtomicU64,
    origin: Instant,
    enabled: AtomicBool,
    live_multiprocessing: AtomicBool,
    /// Chunk frames asked for instead of [`block_frames`]'s, 0 for none.
    chunk: AtomicUsize,
    /// How long the callback waits for a late chunk, in nanoseconds.
    patience: AtomicU64,
    /// Settings as last set, for reading back.
    settings: std::sync::Mutex<Buffering>,
    stop: AtomicBool,
    thread: OnceLock<std::thread::Thread>,
    /// Counters for the performance meter.
    misses: AtomicU64,
    lead: AtomicU64,
    live_tracks: AtomicU32,
    running: AtomicBool,
}

impl AheadShared {
    fn new() -> Self {
        Self {
            slots: Slots(
                (0..CHUNKS)
                    .map(|_| {
                        UnsafeCell::new(Chunk { epoch: 0, serial: 0, pos: 0, len: 0, layout: 0, channels: Vec::new() })
                    })
                    .collect(),
            ),
            command: AtomicU64::new(0),
            start: AtomicI64::new(0),
            busy: AtomicBool::new(false),
            parked: AtomicBool::new(false),
            consumed: AtomicU64::new(0),
            old_left: AtomicU64::new(0),
            first: AtomicUsize::new(128),
            block: AtomicUsize::new(512),
            target: AtomicU64::new(0),
            horizon: AtomicU64::new(512),
            tweaks: AtomicU64::new(0),
            hold_until: AtomicU64::new(0),
            origin: Instant::now(),
            enabled: AtomicBool::new(false),
            live_multiprocessing: AtomicBool::new(true),
            patience: AtomicU64::new(0),
            chunk: AtomicUsize::new(0),
            settings: std::sync::Mutex::new(Buffering { anticipative: false, ..Buffering::default() }),
            stop: AtomicBool::new(false),
            thread: OnceLock::new(),
            misses: AtomicU64::new(0),
            lead: AtomicU64::new(0),
            live_tracks: AtomicU32::new(0),
            running: AtomicBool::new(false),
        }
    }

    fn now(&self) -> u64 {
        self.origin.elapsed().as_nanos() as u64
    }

    fn wake(&self) {
        if self.parked.load(Ordering::Acquire)
            && let Some(t) = self.thread.get()
        {
            t.unpark();
        }
    }

    /// Applies `settings` at `sample_rate`.
    pub fn configure(&self, settings: Buffering, sample_rate: u32) {
        crate::render::set_ahead_threads(settings.threads);
        let frames = u64::from(settings.render_ahead_ms.clamp(10, 2000)) * u64::from(sample_rate) / 1000;
        self.target.store(frames, Ordering::Relaxed);
        self.live_multiprocessing.store(settings.live_multiprocessing, Ordering::Relaxed);
        self.enabled.store(settings.anticipative, Ordering::Release);
        if let Ok(mut s) = self.settings.lock() {
            *s = settings;
        }
        self.wake();
    }

    pub fn settings(&self) -> Buffering {
        self.settings.lock().map(|s| *s).unwrap_or_default()
    }

    /// An effect parameter or the project changed: what is rendered ahead is rendered again
    /// from a short horizon on, and the render-ahead stays short for a while.
    pub fn tweak(&self) {
        self.tweaks.fetch_add(1, Ordering::Relaxed);
        self.hold_until.store(self.now() + HOLD.as_nanos() as u64, Ordering::Relaxed);
        self.wake();
    }

    /// How long the callback waits for a chunk that is late; tests wait as long as it takes.
    pub fn set_patience(&self, patience: Duration) {
        self.patience.store(patience.as_nanos() as u64, Ordering::Relaxed);
    }

    /// Renders chunks of `frames` (whole device blocks) once ahead, instead of about 512.
    pub fn set_chunk(&self, frames: usize) {
        self.chunk.store(frames, Ordering::Relaxed);
    }

    pub fn stats(&self) -> AheadStats {
        AheadStats {
            running: self.running.load(Ordering::Relaxed),
            lead_frames: self.lead.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            live_tracks: self.live_tracks.load(Ordering::Relaxed),
        }
    }
}

/// How anticipative FX is doing, for the performance meter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AheadStats {
    /// Tracks render ahead now.
    pub running: bool,
    /// Frames rendered ahead of the playhead.
    pub lead_frames: u64,
    /// Times the callback found no chunk ready and rendered the block itself (or without the
    /// tracks rendered ahead), since the start.
    pub misses: u64,
    /// Tracks the callback renders.
    pub live_tracks: u32,
}

/// The worker's ends and its thread; stops the thread when dropped.
pub struct AheadWorker {
    shared: Arc<AheadShared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for AheadWorker {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.command.store(0, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
            let _ = t.join();
        }
    }
}

/// Starts the worker for the graph the callback plays; returns its handle and the callback's
/// side.
pub fn start(
    graph: Arc<ArcSwap<RenderGraph>>,
    recording: Arc<dyn Fn() -> bool + Send + Sync>,
    loop_range: Arc<dyn Fn() -> Option<(i64, i64)> + Send + Sync>,
) -> (AheadWorker, AheadClient) {
    let shared = Arc::new(AheadShared::new());
    let (filled_tx, filled_rx) = rtrb::RingBuffer::<u16>::new(CHUNKS);
    let (mut free_tx, free_rx) = rtrb::RingBuffer::<u16>::new(CHUNKS);
    for i in 0..CHUNKS as u16 {
        let _ = free_tx.push(i);
    }
    let s = shared.clone();
    let lr = loop_range.clone();
    let thread = std::thread::Builder::new()
        .name("scythe-ahead".into())
        .spawn(move || {
            crate::realtime::request_worker();
            let worker = Worker {
                shared: s,
                graph,
                recording,
                loop_range: lr,
                filled: filled_tx,
                free: free_rx,
                scratch: RenderScratch::new(crate::engine::MAX_TRACKS),
            };
            worker.run();
        })
        .expect("spawn anticipative render thread");
    let _ = shared.thread.set(thread.thread().clone());
    let client = AheadClient {
        shared: shared.clone(),
        filled: filled_rx,
        free: free_tx,
        loop_range,
        epoch: 0,
        running: false,
        old_left: 0,
        cur: None,
        sinks: (0..MAX_SINK_CHANNELS).map(|_| vec![0.0; MAX_CHUNK]).collect(),
        released: 0,
        seen_graph: 0,
        seen_tweaks: 0,
        seen_layout: 0,
        device: false,
    };
    (AheadWorker { shared, thread: Some(thread) }, client)
}

/// The frame after `pos + len` along the transport's path: a loop end wraps to its start.
fn next_frame(pos: i64, len: usize, loop_range: Option<(i64, i64)>) -> i64 {
    let end = pos + len as i64;
    match loop_range {
        Some((ls, le)) if le > ls && pos < le && end == le => ls,
        _ => end,
    }
}

struct Worker {
    shared: Arc<AheadShared>,
    graph: Arc<ArcSwap<RenderGraph>>,
    recording: Arc<dyn Fn() -> bool + Send + Sync>,
    loop_range: Arc<dyn Fn() -> Option<(i64, i64)> + Send + Sync>,
    filled: rtrb::Producer<u16>,
    free: rtrb::Consumer<u16>,
    scratch: RenderScratch,
}

impl Worker {
    fn run(mut self) {
        let s = self.shared.clone();
        let mut epoch = u64::MAX;
        let (mut pos, mut produced, mut next_len) = (0i64, 0u64, 0usize);
        let mut serial = 0i64;
        let idle = |s: &AheadShared, wait: Duration| {
            s.busy.store(false, Ordering::SeqCst);
            s.parked.store(true, Ordering::Release);
            std::thread::park_timeout(wait);
            s.parked.store(false, Ordering::Release);
        };
        while !s.stop.load(Ordering::Acquire) {
            s.busy.store(true, Ordering::SeqCst);
            let command = s.command.load(Ordering::SeqCst);
            if command & 1 == 0 {
                idle(&s, Duration::from_millis(20));
                continue;
            }
            let first = s.first.load(Ordering::Relaxed).clamp(1, MAX_CHUNK);
            if command >> 1 != epoch {
                epoch = command >> 1;
                pos = s.start.load(Ordering::Acquire);
                produced = 0;
                next_len = first;
            }
            let holding = s.now() < s.hold_until.load(Ordering::Relaxed);
            let horizon = s.horizon.load(Ordering::Relaxed);
            let target = if holding { horizon } else { s.target.load(Ordering::Relaxed).max(horizon) };
            let lead =
                (s.old_left.load(Ordering::Relaxed) + produced).saturating_sub(s.consumed.load(Ordering::Relaxed));
            s.lead.store(lead, Ordering::Relaxed);
            if lead >= target {
                let period = first as f64 / self.graph.load().sample_rate.max(1) as f64;
                idle(&s, Duration::from_secs_f64(period.clamp(0.0005, 0.02)));
                continue;
            }
            let graph = self.graph.load();
            let Some(plan) = graph.ahead.as_deref() else {
                idle(&s, Duration::from_millis(5));
                continue;
            };
            let Ok(id) = self.free.pop() else {
                idle(&s, Duration::from_millis(2));
                continue;
            };
            let block = s.block.load(Ordering::Relaxed).clamp(first, MAX_CHUNK);
            let mut len = if holding { first } else { next_len.min(block) };
            let loop_range = (self.loop_range)();
            if let Some((ls, le)) = loop_range
                && le > ls
                && pos < le
            {
                len = len.min((le - pos) as usize);
            }
            // SAFETY: `id` came from the free queue, so this side holds it.
            let chunk = unsafe { self.shared.slots.get(id) };
            if chunk.channels.len() < plan.channels {
                chunk.channels.resize_with(plan.channels, || vec![0.0; MAX_CHUNK]);
            }
            serial += 1;
            let input = LiveInput { recording: (self.recording)(), ahead: Some(serial), ..LiveInput::default() };
            graph.render_ahead(
                pos,
                len,
                &mut self.scratch,
                &input,
                &mut chunk.channels,
                crate::render::ahead_threads(),
            );
            (chunk.epoch, chunk.serial, chunk.pos, chunk.len, chunk.layout) = (epoch, serial, pos, len, plan.key);
            let _ = self.filled.push(id);
            produced += len as u64;
            pos = next_frame(pos, len, loop_range);
            next_len = (len * 2).min(block);
            s.busy.store(false, Ordering::SeqCst);
        }
        s.busy.store(false, Ordering::SeqCst);
    }
}

/// What gathering a block's sinks came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gather {
    /// The sinks hold the block.
    Ready,
    /// No chunk for these frames came in time.
    Late,
    /// The chunks do not continue at these frames (a jump, another plan).
    Mismatch,
}

/// The audio callback's side.
pub struct AheadClient {
    shared: Arc<AheadShared>,
    filled: rtrb::Consumer<u16>,
    free: rtrb::Producer<u16>,
    loop_range: Arc<dyn Fn() -> Option<(i64, i64)> + Send + Sync>,
    epoch: u64,
    running: bool,
    /// Frames of older epochs still taken, after an edit kept a horizon of them.
    old_left: u64,
    /// The chunk being read and how far.
    cur: Option<(u16, usize)>,
    /// The sinks of the current block, gathered from the chunks.
    sinks: Vec<Vec<f32>>,
    /// The last chunk played or dropped, up to which the meters show.
    released: i64,
    seen_graph: usize,
    seen_tweaks: u64,
    seen_layout: u64,
    /// A device plays: on the silent clock every track renders in the callback.
    device: bool,
}

impl AheadClient {
    pub fn shared(&self) -> &Arc<AheadShared> {
        &self.shared
    }

    /// Whether a device plays through this callback.
    pub fn set_device(&mut self, device: bool) {
        self.device = device;
    }

    pub fn running(&self) -> bool {
        self.running
    }

    pub fn live_multiprocessing(&self) -> bool {
        self.shared.live_multiprocessing.load(Ordering::Relaxed)
    }

    /// The sinks the last [`AheadClient::take`] filled.
    pub fn sinks(&self) -> &[Vec<f32>] {
        &self.sinks
    }

    /// Whether tracks may render ahead with this graph.
    pub fn eligible(&self, graph: &RenderGraph) -> bool {
        self.device && self.shared.enabled.load(Ordering::Acquire) && graph.ahead.is_some()
    }

    pub fn miss(&self) {
        self.shared.misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Called at the start of every callback of `frames` frames, before anything renders.
    pub fn begin(&mut self, graph: &Arc<RenderGraph>, frames: usize) {
        let s = &self.shared;
        let frames = frames.clamp(1, MAX_CHUNK);
        s.first.store(frames, Ordering::Relaxed);
        let rate = graph.sample_rate as usize;
        let block = match s.chunk.load(Ordering::Relaxed) {
            0 => block_frames(frames, rate),
            n => (n.div_ceil(frames) * frames).min(MAX_CHUNK / frames * frames),
        };
        s.block.store(block, Ordering::Relaxed);
        s.horizon.store((4 * frames).max(rate / 100) as u64, Ordering::Relaxed);
        let live = graph.ahead.as_deref().map_or(graph.tracks.len(), AheadPlan::live_tracks);
        s.live_tracks.store(live as u32, Ordering::Relaxed);
        if !self.running {
            return;
        }
        let ptr = Arc::as_ptr(graph) as usize;
        let tweaks = s.tweaks.load(Ordering::Relaxed);
        let layout = graph.ahead.as_deref().map_or(0, |p| p.key);
        if layout != self.seen_layout || !self.eligible(graph) {
            // Other live tracks: the worker stops before the callback takes them over.
            self.pause();
        } else if ptr != self.seen_graph || tweaks != self.seen_tweaks {
            self.seen_graph = ptr;
            self.seen_tweaks = tweaks;
            match self.horizon_end(s.horizon.load(Ordering::Relaxed)) {
                Some((pos, kept)) => self.restart(pos, kept),
                None => self.pause(),
            }
        }
    }

    /// Stops the worker after its current chunk and drops what it rendered.
    pub fn pause(&mut self) {
        if self.running {
            self.running = false;
            self.shared.running.store(false, Ordering::Relaxed);
            self.epoch += 1;
            self.shared.command.store(self.epoch << 1, Ordering::SeqCst);
        }
        self.drain();
    }

    /// Whether the callback has every track to itself: the worker is paused and not inside a
    /// chunk. Waits for it until `deadline`, or the patience when that is longer.
    pub fn exclusive(&mut self, deadline: Instant) -> bool {
        self.pause();
        let patience = Duration::from_nanos(self.shared.patience.load(Ordering::Relaxed));
        let until = deadline.max(Instant::now() + patience);
        loop {
            if !self.shared.busy.load(Ordering::SeqCst) {
                return true;
            }
            if Instant::now() >= until {
                return false;
            }
            std::hint::spin_loop();
        }
    }

    /// Starts rendering ahead from `pos`, the frame the next callback plays.
    pub fn start(&mut self, graph: &Arc<RenderGraph>, pos: i64) {
        self.drain();
        self.seen_graph = Arc::as_ptr(graph) as usize;
        self.seen_tweaks = self.shared.tweaks.load(Ordering::Relaxed);
        self.seen_layout = graph.ahead.as_deref().map_or(0, |p| p.key);
        self.restart(pos, 0);
    }

    fn restart(&mut self, pos: i64, old_left: u64) {
        let s = &self.shared;
        self.epoch += 1;
        self.old_left = old_left;
        s.start.store(pos, Ordering::Release);
        s.consumed.store(0, Ordering::Relaxed);
        s.old_left.store(old_left, Ordering::Relaxed);
        s.command.store(self.epoch << 1 | 1, Ordering::SeqCst);
        self.running = true;
        s.running.store(true, Ordering::Relaxed);
        s.wake();
    }

    /// Gives chunk `id` back to the worker.
    fn recycle(&mut self, id: u16) {
        // SAFETY: the callback holds the chunk until it pushes it back here.
        let serial = unsafe { self.shared.slots.get(id) }.serial;
        self.released = self.released.max(serial);
        let _ = self.free.push(id);
    }

    /// Lets the meters of the tracks rendered ahead show the chunks played so far.
    pub fn release_meters(&self, graph: &RenderGraph) {
        let Some(plan) = graph.ahead.as_deref() else { return };
        for (t, _) in graph.tracks.iter().zip(&plan.live).filter(|(_, live)| !**live) {
            t.meter.release(self.released);
        }
    }

    /// Gives back every chunk held or queued.
    fn drain(&mut self) {
        if let Some((id, _)) = self.cur.take() {
            self.recycle(id);
        }
        while let Ok(id) = self.filled.pop() {
            self.recycle(id);
        }
        self.old_left = 0;
    }

    /// Whether the callback reads chunk `c`, with `budget` frames of older epochs left.
    fn valid(&self, c: &Chunk, budget: u64) -> bool {
        c.epoch == self.epoch || (c.epoch < self.epoch && budget > 0)
    }

    /// Where the frames `horizon` past the next one to play lie, along the chunks the callback
    /// would read, and how many of those frames are rendered: the start of a new epoch that
    /// keeps them. `None` when nothing is rendered.
    fn horizon_end(&mut self, horizon: u64) -> Option<(i64, u64)> {
        let mut walk = Walk {
            epoch: self.epoch,
            budget: self.old_left,
            horizon,
            kept: 0,
            at: None,
            loop_range: (self.loop_range)(),
        };
        let shared = self.shared.clone();
        if let Some((id, off)) = self.cur {
            // SAFETY: the callback holds the chunk it reads.
            walk.step(unsafe { shared.slots.get(id) }, off);
        }
        if walk.kept < horizon
            && let Ok(queued) = self.filled.read_chunk(self.filled.slots())
        {
            let (a, b) = queued.as_slices();
            for &id in a.iter().chain(b) {
                // SAFETY: a queued chunk is no longer the worker's; the callback only reads it.
                let c = unsafe { shared.slots.get(id) };
                if !walk.valid(c) {
                    continue;
                }
                if !walk.step(c, 0) || walk.kept >= horizon {
                    break;
                }
            }
        }
        let end = walk.at?;
        (walk.kept > 0).then_some((end, walk.kept))
    }

    /// Fills the sinks with `len` frames from project frame `pos`, waiting for a late chunk
    /// until `deadline`.
    pub fn take(&mut self, plan: &AheadPlan, pos: i64, len: usize, deadline: Instant) -> Gather {
        let patience = Duration::from_nanos(self.shared.patience.load(Ordering::Relaxed));
        let deadline = deadline.max(Instant::now() + patience);
        let mut done = 0;
        while done < len {
            if self.cur.is_none() {
                match self.filled.pop() {
                    Ok(id) => {
                        // SAFETY: popped from the filled queue, the chunk is the callback's.
                        let c = unsafe { self.shared.slots.get(id) };
                        if self.valid(c, self.old_left) {
                            self.cur = Some((id, 0));
                        } else {
                            self.recycle(id);
                        }
                        continue;
                    }
                    Err(_) if Instant::now() < deadline => {
                        self.shared.wake();
                        std::hint::spin_loop();
                        continue;
                    }
                    Err(_) => return Gather::Late,
                }
            }
            let Some((id, off)) = self.cur else { continue };
            // SAFETY: the callback holds the chunk it reads.
            let c = unsafe { self.shared.slots.get(id) };
            if c.layout != plan.key || c.pos + off as i64 != pos + done as i64 {
                return Gather::Mismatch;
            }
            let old = c.epoch < self.epoch;
            let mut avail = c.len - off;
            if old {
                avail = avail.min(self.old_left as usize);
            }
            let t = avail.min(len - done);
            for (dst, src) in self.sinks.iter_mut().zip(&c.channels).take(plan.channels) {
                dst[done..done + t].copy_from_slice(&src[off..off + t]);
            }
            done += t;
            let off = off + t;
            if old {
                self.old_left -= t as u64;
                self.shared.old_left.store(self.old_left, Ordering::Relaxed);
            } else {
                self.shared.consumed.fetch_add(t as u64, Ordering::Relaxed);
            }
            if off == c.len || (old && self.old_left == 0) {
                self.cur = None;
                self.recycle(id);
            } else {
                self.cur = Some((id, off));
            }
        }
        if self.shared.parked.load(Ordering::Relaxed) {
            self.shared.wake();
        }
        Gather::Ready
    }
}

/// Frames of the chunks the worker renders once it is ahead: whole device blocks of about
/// 512 frames at 44.1 or 48 kHz (`SCYTHE_AHEAD_BLOCK` overrides).
fn block_frames(frames: usize, rate: usize) -> usize {
    static WANTED: OnceLock<Option<usize>> = OnceLock::new();
    let wanted = WANTED
        .get_or_init(|| std::env::var("SCYTHE_AHEAD_BLOCK").ok().and_then(|v| v.parse().ok()))
        .unwrap_or(512 * rate / 48_000);
    (wanted.max(frames).div_ceil(frames) * frames).min(MAX_CHUNK / frames * frames).max(frames)
}

/// Follows the chunks the callback would read, for [`AheadClient::horizon_end`].
struct Walk {
    epoch: u64,
    budget: u64,
    horizon: u64,
    kept: u64,
    at: Option<i64>,
    loop_range: Option<(i64, i64)>,
}

impl Walk {
    fn valid(&self, c: &Chunk) -> bool {
        c.epoch == self.epoch || (c.epoch < self.epoch && self.budget > 0)
    }

    /// Takes chunk `c` from `off`, up to the horizon; false when it does not continue the
    /// frames walked so far.
    fn step(&mut self, c: &Chunk, off: usize) -> bool {
        if !self.valid(c) || self.at.is_some_and(|p| p != c.pos + off as i64) {
            return false;
        }
        let mut avail = (c.len - off) as u64;
        if c.epoch < self.epoch {
            avail = avail.min(self.budget);
        }
        let t = avail.min(self.horizon - self.kept);
        if c.epoch < self.epoch {
            self.budget -= t;
        }
        self.kept += t;
        self.at = Some(if off as u64 + t == c.len as u64 {
            next_frame(c.pos, c.len, self.loop_range)
        } else {
            c.pos + off as i64 + t as i64
        });
        true
    }
}
