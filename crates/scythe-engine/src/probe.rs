//! What a running effect shows its editor: levels and gain reduction as atomics the audio
//! thread raises, and the audio around the effect copied into two rings the editor turns into
//! spectra on its own thread. Nothing here locks or allocates on the audio thread, and an
//! effect nobody watches only pays for one atomic load per block.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use realfft::num_complex::Complex32;
use realfft::{RealFftPlanner, RealToComplex};

/// Samples each ring keeps (a power of two).
pub const RING: usize = 16384;
const MASK: usize = RING - 1;

/// Peak input level (linear) since the last read.
pub const INPUT: usize = 0;
/// Peak output level (linear) since the last read.
pub const OUTPUT: usize = 1;
/// Deepest gain reduction in dB since the last read.
pub const REDUCTION: usize = 2;
/// Loudest detector level (linear) since the last read.
pub const DETECTOR: usize = 3;
/// How far a gate stood open (0..1) since the last read, at most.
pub const GATE: usize = 4;
/// Gain reduction in dB of multiband band `k` at `BAND_REDUCTION + k`.
pub const BAND_REDUCTION: usize = 8;
/// Bands a multiband effect can report.
pub const BANDS: usize = 32;
const SLOTS: usize = BAND_REDUCTION + BANDS;

/// Which side of the effect a ring holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Pre,
    Post,
}

/// A single-producer ring of mono samples: the audio thread writes, any reader copies the
/// newest samples out. A reader racing a lap of the writer sees a few mixed samples, which a
/// display can take.
struct Ring {
    buf: Box<[AtomicU32]>,
    /// Samples written so far.
    head: AtomicUsize,
}

impl Ring {
    fn new() -> Self {
        Self { buf: (0..RING).map(|_| AtomicU32::new(0)).collect(), head: AtomicUsize::new(0) }
    }

    fn push(&self, left: &[f32], right: &[f32]) {
        let mut h = self.head.load(Ordering::Relaxed);
        for (l, r) in left.iter().zip(right) {
            self.buf[h & MASK].store(((l + r) * 0.5).to_bits(), Ordering::Relaxed);
            h = h.wrapping_add(1);
        }
        self.head.store(h, Ordering::Release);
    }

    /// Copies the newest `out.len()` samples into `out`; returns the write count they end at.
    fn latest(&self, out: &mut [f32]) -> usize {
        let h = self.head.load(Ordering::Acquire);
        let n = out.len().min(RING);
        for (i, o) in out.iter_mut().enumerate().take(n) {
            *o = f32::from_bits(self.buf[h.wrapping_sub(n - i) & MASK].load(Ordering::Relaxed));
        }
        h
    }
}

struct Rings {
    pre: Ring,
    post: Ring,
}

/// One effect instance's probe. Effects publish into it only while [`FxProbe::active`].
pub struct FxProbe {
    watchers: AtomicU32,
    /// Blocks the probe stays on for a one-off reader (the control API).
    lease: AtomicU32,
    sample_rate: AtomicU32,
    values: [AtomicU32; SLOTS],
    rings: OnceLock<Rings>,
}

impl Default for FxProbe {
    fn default() -> Self {
        Self {
            watchers: AtomicU32::new(0),
            lease: AtomicU32::new(0),
            sample_rate: AtomicU32::new(0),
            values: std::array::from_fn(|_| AtomicU32::new(0)),
            rings: OnceLock::new(),
        }
    }
}

impl std::fmt::Debug for FxProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FxProbe").field("watchers", &self.watchers.load(Ordering::Relaxed)).finish()
    }
}

/// Keeps a probe on while it lives (an open editor).
pub struct Watch(Arc<FxProbe>);

impl Drop for Watch {
    fn drop(&mut self) {
        self.0.watchers.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Watch {
    pub fn probe(&self) -> &Arc<FxProbe> {
        &self.0
    }
}

impl FxProbe {
    /// Whether anyone reads the probe; effects skip their publishing otherwise.
    #[inline]
    pub fn active(&self) -> bool {
        self.watchers.load(Ordering::Relaxed) > 0 || self.lease.load(Ordering::Relaxed) > 0
    }

    /// Turns the probe on until the returned guard drops. The rings are made here, on the
    /// watcher's thread, the first time.
    pub fn watch(self: &Arc<Self>) -> Watch {
        self.rings.get_or_init(|| Rings { pre: Ring::new(), post: Ring::new() });
        self.watchers.fetch_add(1, Ordering::Relaxed);
        Watch(self.clone())
    }

    /// Turns the probe on for the next `blocks` audio blocks, for a reader that only looks once.
    pub fn lease(&self, blocks: u32) {
        self.lease.fetch_max(blocks, Ordering::Relaxed);
    }

    /// Raises `slot` to `value` if it is higher (values are never negative).
    #[inline]
    pub fn publish_max(&self, slot: usize, value: f32) {
        let v = if value > 0.0 { value } else { 0.0 };
        if let Some(s) = self.values.get(slot) {
            s.fetch_max(v.to_bits(), Ordering::Relaxed);
        }
    }

    /// The highest value published to `slot` since the last take, resetting it.
    pub fn take(&self, slot: usize) -> f32 {
        self.values.get(slot).map_or(0.0, |s| f32::from_bits(s.swap(0, Ordering::Relaxed)))
    }

    /// The highest value published to `slot` since the last take, leaving it.
    pub fn peek(&self, slot: usize) -> f32 {
        self.values.get(slot).map_or(0.0, |s| f32::from_bits(s.load(Ordering::Relaxed)))
    }

    /// The rate of the audio last tapped (0 before any).
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate.load(Ordering::Relaxed)
    }

    /// Copies a block of the audio on `side` of the effect and raises the matching level.
    /// Called by the effect's host around each block while the probe is active.
    pub fn tap(&self, side: Side, left: &[f32], right: &[f32], sample_rate: u32) {
        let peak = left.iter().chain(right).fold(0.0f32, |m, s| m.max(s.abs()));
        match side {
            Side::Pre => {
                self.publish_max(INPUT, peak);
                self.sample_rate.store(sample_rate, Ordering::Relaxed);
            }
            Side::Post => {
                self.publish_max(OUTPUT, peak);
                let lease = self.lease.load(Ordering::Relaxed);
                if lease > 0 {
                    self.lease.store(lease - 1, Ordering::Relaxed);
                }
            }
        }
        if let Some(rings) = self.rings.get() {
            match side {
                Side::Pre => rings.pre.push(left, right),
                Side::Post => rings.post.push(left, right),
            }
        }
    }

    /// The newest samples tapped on `side`; returns the write count they end at (0 when the
    /// probe never ran).
    pub fn latest(&self, side: Side, out: &mut [f32]) -> usize {
        match self.rings.get() {
            Some(r) => match side {
                Side::Pre => r.pre.latest(out),
                Side::Post => r.post.latest(out),
            },
            None => 0,
        }
    }
}

/// The floor spectra and levels fall to, in dB.
pub const SPECTRUM_FLOOR: f32 = -120.0;

/// A running spectrum of one side of a probe, made on the reader's thread: a Hann-windowed
/// FFT of the newest samples, rising at once and falling at `fall` dB per second.
pub struct Spectrum {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    input: Vec<f32>,
    scratch: Vec<Complex32>,
    /// Level per bin in dB (0 dB for a full-scale sine).
    pub db: Vec<f32>,
    pub sample_rate: u32,
    head: usize,
    pub fall: f32,
}

impl Spectrum {
    pub fn new(size: usize) -> Self {
        let size = size.next_power_of_two().clamp(256, RING / 2);
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(size);
        let window: Vec<f32> =
            (0..size).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / size as f32).cos()).collect();
        let scratch = fft.make_output_vec();
        Self {
            db: vec![SPECTRUM_FLOOR; scratch.len()],
            fft,
            window,
            input: vec![0.0; size],
            scratch,
            sample_rate: 48000,
            head: 0,
            fall: 40.0,
        }
    }

    pub fn size(&self) -> usize {
        self.window.len()
    }

    /// Takes the newest audio from `probe`; `dt` is the time since the last update.
    /// Returns whether new audio arrived.
    pub fn update(&mut self, probe: &FxProbe, side: Side, dt: f32) -> bool {
        let head = probe.latest(side, &mut self.input);
        let fresh = head != self.head;
        self.head = head;
        if probe.sample_rate() > 0 {
            self.sample_rate = probe.sample_rate();
        }
        let fall = self.fall * dt.max(0.0);
        if !fresh {
            self.db.iter_mut().for_each(|d| *d = (*d - fall).max(SPECTRUM_FLOOR));
            return false;
        }
        self.analyse(fall);
        true
    }

    /// Analyses `samples` (the newest `size` of them) as if they had just arrived.
    pub fn feed(&mut self, samples: &[f32]) {
        let n = self.input.len();
        let from = samples.len().saturating_sub(n);
        self.input.fill(0.0);
        let take = &samples[from..];
        self.input[n - take.len()..].copy_from_slice(take);
        self.analyse(f32::INFINITY);
    }

    fn analyse(&mut self, fall: f32) {
        let norm = 2.0 / self.window.iter().sum::<f32>();
        for (x, w) in self.input.iter_mut().zip(&self.window) {
            *x *= w;
        }
        if self.fft.process(&mut self.input, &mut self.scratch).is_err() {
            return;
        }
        for (d, c) in self.db.iter_mut().zip(&self.scratch) {
            let level = 20.0 * (c.norm() * norm).max(1e-9).log10();
            *d = level.max(*d - fall).max(SPECTRUM_FLOOR);
        }
    }

    /// The level over the band from `lo` to `hi` Hz: the loudest bin in it, or the level
    /// interpolated at its middle when it holds none.
    pub fn level(&self, lo: f64, hi: f64) -> f32 {
        let hz_per_bin = self.sample_rate as f64 / self.size() as f64;
        let last = self.db.len() - 1;
        let (a, b) = (lo / hz_per_bin, hi / hz_per_bin);
        let (first, end) = (a.ceil().max(0.0) as usize, (b.floor() as usize).min(last));
        if first <= end {
            return self.db[first..=end].iter().copied().fold(SPECTRUM_FLOOR, f32::max);
        }
        let x = ((a + b) / 2.0).clamp(0.0, last as f64);
        let (i, t) = (x.floor() as usize, (x - x.floor()) as f32);
        let j = (i + 1).min(last);
        self.db[i] + (self.db[j] - self.db[i]) * t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_hold_their_peak_until_taken() {
        let p = FxProbe::default();
        p.publish_max(REDUCTION, 3.0);
        p.publish_max(REDUCTION, 1.5);
        p.publish_max(REDUCTION, -4.0);
        assert_eq!(p.peek(REDUCTION), 3.0);
        assert_eq!(p.take(REDUCTION), 3.0);
        assert_eq!(p.take(REDUCTION), 0.0);
    }

    #[test]
    fn watching_turns_the_probe_on_while_held() {
        let p = Arc::new(FxProbe::default());
        assert!(!p.active());
        let w = p.watch();
        assert!(p.active());
        drop(w);
        assert!(!p.active());
        p.lease(2);
        let block = [0.25f32; 8];
        p.tap(Side::Post, &block, &block, 48000);
        assert!(p.active());
        p.tap(Side::Post, &block, &block, 48000);
        assert!(!p.active());
    }

    #[test]
    fn rings_keep_the_newest_samples() {
        let p = Arc::new(FxProbe::default());
        let _w = p.watch();
        let l: Vec<f32> = (0..RING + 100).map(|i| i as f32).collect();
        p.tap(Side::Pre, &l, &l, 44100);
        let mut out = [0.0f32; 4];
        let head = p.latest(Side::Pre, &mut out);
        assert_eq!(head, RING + 100);
        assert_eq!(out, [(RING + 96) as f32, (RING + 97) as f32, (RING + 98) as f32, (RING + 99) as f32]);
        assert_eq!(p.sample_rate(), 44100);
        assert_eq!(p.take(INPUT), (RING + 99) as f32);
    }

    #[test]
    fn a_full_scale_sine_reads_zero_db_in_its_band() {
        let mut s = Spectrum::new(4096);
        s.sample_rate = 48000;
        let hz = 1500.0;
        let sine: Vec<f32> = (0..4096).map(|i| (std::f32::consts::TAU * hz * i as f32 / 48000.0).sin()).collect();
        s.feed(&sine);
        assert!(s.level(1400.0, 1600.0).abs() < 0.5, "{}", s.level(1400.0, 1600.0));
        assert!(s.level(4000.0, 6000.0) < -40.0);
    }
}
