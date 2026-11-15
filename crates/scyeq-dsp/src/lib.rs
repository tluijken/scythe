//! ScyEQ-Pro: Scythe's own parametric and dynamic equaliser. Up to 24 bands of ten shapes,
//! zero latency (matched IIR), natural phase or linear phase processing, per-band stereo
//! placement, dynamic and spectral dynamic bands with internal or external sidechain, a
//! character stage, auto gain and an output section.
//!
//! The DSP on its own, without a host: Scythe runs it as one of its effects, a plugin build
//! wraps it for other hosts.

pub mod dynamics;
pub mod filters;
pub mod fir;
pub mod matching;
pub mod params;
pub mod probe;
pub mod reference;
pub mod spectral;

pub use realfft;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use probe::FxProbe;

use dynamics::{Ballistics, Detector};
use filters::{BRICKWALL, C64, Cascade, CascadeState, Design, Prototype, Shape};
use fir::{FirEngine, Layout, Timing};
use params::{BAND_STRIDE, BANDS, GLOBAL, PhaseMode, Placement, band, global};
use spectral::{Spectral, SpectralBand};

#[derive(Debug, Clone, PartialEq)]
pub struct ParamInfo {
    pub id: u32,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    /// Edit on a logarithmic scale (frequencies, times).
    pub log: bool,
}

/// Frames between parameter updates of the zero-latency filters and the detectors. The cells
/// lie on multiples of the sample position, so the output does not depend on the block sizes.
const SUB: usize = 32;
const SMOOTH_SECONDS: f64 = 0.015;
const FADE_SECONDS: f64 = 0.01;

/// What the editor reads from a running instance.
pub struct Meters {
    /// Current dynamic gain change per band in dB (f32 bits).
    pub dynamic_db: [AtomicU32; BANDS],
    /// The threshold auto threshold uses per band (f32 bits).
    pub auto_threshold: [AtomicU32; BANDS],
    /// Detector level per band in dB (f32 bits).
    pub detector_db: [AtomicU32; BANDS],
    /// The auto gain applied, in dB.
    pub auto_gain_db: AtomicU32,
    /// The external sidechain, mixed to mono, for the analyzer.
    pub sidechain: Arc<FxProbe>,
}

impl Default for Meters {
    fn default() -> Self {
        let zero = || std::array::from_fn(|_| AtomicU32::new(0));
        Self {
            dynamic_db: zero(),
            auto_threshold: zero(),
            detector_db: zero(),
            auto_gain_db: AtomicU32::new(0),
            sidechain: Arc::new(FxProbe::default()),
        }
    }
}

impl Meters {
    pub fn dynamic(&self, b: usize) -> f32 {
        f32::from_bits(self.dynamic_db[b].load(Ordering::Relaxed))
    }

    pub fn threshold(&self, b: usize) -> f32 {
        f32::from_bits(self.auto_threshold[b].load(Ordering::Relaxed))
    }

    pub fn detector(&self, b: usize) -> f32 {
        f32::from_bits(self.detector_db[b].load(Ordering::Relaxed))
    }
}

/// A band's settings read from the parameter table.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandSettings {
    pub used: bool,
    pub enabled: bool,
    pub design: Design,
    pub placement: Placement,
    /// 0 static, 1 dynamic, 2 spectral.
    pub dynamic: u8,
    pub range: f64,
    pub threshold: f64,
    pub auto_threshold: bool,
    pub attack: f64,
    pub release: f64,
    pub external: bool,
    pub solo: bool,
    pub audition: bool,
}

impl BandSettings {
    pub fn read(values: &[f64], b: usize) -> Self {
        let v = |o: u32| values[(b as u32 * BAND_STRIDE + o) as usize];
        let scale = values[(GLOBAL + global::GAIN_SCALE) as usize] / 100.0;
        let shape = Shape::from_index(v(band::SHAPE) as usize);
        let zero_latency = values[(GLOBAL + global::PHASE) as usize] < 0.5;
        let dynamic = v(band::DYNAMIC) as u8;
        Self {
            used: v(band::USED) >= 0.5,
            enabled: v(band::ENABLED) >= 0.5,
            design: Design {
                shape,
                freq: v(band::FREQ),
                gain_db: v(band::GAIN) * scale,
                q: v(band::Q),
                slope: v(band::SLOPE) as usize,
                gain_q: values[(GLOBAL + global::GAIN_Q) as usize] >= 0.5,
            },
            placement: Placement::from_index(v(band::STEREO) as usize),
            dynamic: if shape.has_gain() || (dynamic == 1 && zero_latency && shape.dynamic_in_zero_latency()) {
                dynamic
            } else {
                0
            },
            range: v(band::RANGE) * scale,
            threshold: v(band::THRESHOLD),
            auto_threshold: v(band::AUTO_THRESHOLD) >= 0.5,
            attack: v(band::ATTACK),
            release: v(band::RELEASE),
            external: v(band::SIDECHAIN) >= 0.5,
            solo: v(band::SOLO) >= 0.5,
            audition: v(band::AUDITION) >= 0.5,
        }
    }

    pub fn active(&self) -> bool {
        self.used && self.enabled
    }

    /// The design at rest: a dynamic notch is transparent until it moves.
    pub fn rest(&self) -> Design {
        if self.dynamic == 1 { self.design.moved(0.0) } else { self.design }
    }

    fn spectral(&self) -> Option<SpectralBand> {
        (self.active() && self.dynamic == 2).then_some(SpectralBand {
            design: self.design,
            placement: self.placement,
            range: self.range,
            threshold: self.threshold,
            auto: self.auto_threshold,
            attack_ms: self.attack,
            release_ms: self.release,
        })
    }

    fn structure(&self) -> (Shape, usize, bool) {
        let shape = if self.dynamic == 1 { self.design.moved(0.0).shape } else { self.design.shape };
        (shape, self.design.slope, self.design.gain_q)
    }
}

/// A band in the zero-latency path.
#[derive(Default)]
struct Band {
    /// Smoothed frequency (log), gain and Q (log) in use.
    log_freq: f64,
    gain: f64,
    log_q: f64,
    structure: Option<(Shape, usize, bool)>,
    last: Option<Design>,
    cascade: Cascade,
    state: [CascadeState; 2],
    mix: f64,
    /// The mix at the start of the current grid cell.
    mix_from: f64,
    detector: Detector,
    /// The detector ran in the last block; a band that starts detecting starts afresh. Judged per
    /// block, not per change, so values set one by one or all at once between two blocks agree.
    detecting: bool,
    dynamic_db: f64,
    solo: Cascade,
    solo_state: [CascadeState; 2],
    solo_design: Option<Design>,
}

/// The response caches of the linear and natural phase paths.
struct FirPath {
    engine: FirEngine,
    /// Each band's static response on the design grid, and the settings it was made from.
    cache: Vec<Vec<C64>>,
    cached: Vec<Option<BandSettings>>,
    resp: [Vec<C64>; 4],
    /// Dynamic amounts per band, delayed by the latency in SUB-frame steps.
    delay: Vec<Vec<f32>>,
    delay_at: usize,
    amounts: Vec<f32>,
    /// The external sidechain delayed to line up with the output, for spectral bands.
    aux: [Vec<f32>; 2],
    aux_at: usize,
    dirty: bool,
    /// Frames until the kernels may be rebuilt again.
    hold: usize,
}

pub struct ScytheEq {
    fs: f64,
    values: Vec<f64>,
    /// Stored values from a newer version, written back unchanged.
    extra: Vec<f64>,
    version: u32,
    /// Known values stored outside the range, written back while left alone.
    unclamped: Vec<(u32, f64)>,
    settings: Vec<BandSettings>,
    bands: Vec<Band>,
    mode: PhaseMode,
    resolution: usize,
    fir: Option<Box<FirPath>>,
    spectral: Option<Box<Spectral>>,
    /// Stages for `spectral` and the outgoing processing to take, so spectral bands turning on
    /// or off do not allocate.
    spare_spectral: [Option<Box<Spectral>>; 2],
    output_db: f64,
    auto_db: f64,
    auto_target_db: f64,
    auto_dirty: bool,
    dc: [(f64, f64); 2],
    meters: Arc<Meters>,
    inp: [Vec<f64>; 2],
    buf: [Vec<f64>; 2],
    det: Vec<f64>,
    /// Whether audio went through, so a structural change crossfades.
    running: bool,
    outgoing: Box<Outgoing>,
    /// The sample position of the next frame, which places the grid cells.
    clock: i64,
    /// A grid cell has started since the last reset.
    primed: bool,
}

/// The processing a structural change (phase mode, linear phase resolution, spectral bands on
/// or off) replaced: it plays on until the new processing has filled its latency, then
/// crossfades out. Each keeps its own timing, as the chain compensates the new latency only
/// once the graph is rebuilt. It is fading while `at < len`.
struct Outgoing {
    /// The old FIR path, when the old processing had one.
    fir: Option<Box<FirPath>>,
    /// The old processing was the zero-latency bands (still in `ScytheEq::bands`).
    zero: bool,
    /// Only the spectral stage changed: the bands are shared.
    shared: bool,
    spectral: Option<Box<Spectral>>,
    out: [Vec<f64>; 2],
    /// Frames before the crossfade starts, then frames into it.
    wait: usize,
    at: usize,
    len: usize,
}

const SWITCH_FADE_SECONDS: f64 = 0.03;

impl ScytheEq {
    pub fn new(sample_rate: u32) -> Self {
        Self::with_values(params::defaults(), sample_rate)
    }

    pub fn from_state(state: &[u8], sample_rate: u32) -> Option<Self> {
        let decoded = params::decode(state)?;
        let mut eq = Self::with_values(decoded.values, sample_rate);
        eq.extra = decoded.extra;
        eq.version = decoded.version;
        eq.unclamped = decoded.unclamped;
        Some(eq)
    }

    pub fn with_values(values: Vec<f64>, sample_rate: u32) -> Self {
        let fs = sample_rate.max(8000) as f64;
        let mut eq = Self {
            fs,
            settings: (0..BANDS).map(|b| BandSettings::read(&values, b)).collect(),
            values,
            extra: Vec::new(),
            version: params::VERSION,
            unclamped: Vec::new(),
            bands: (0..BANDS).map(|_| Band::default()).collect(),
            mode: PhaseMode::ZeroLatency,
            resolution: 1,
            fir: None,
            spectral: None,
            spare_spectral: std::array::from_fn(|_| Some(Box::new(Spectral::new(fs, BANDS)))),
            output_db: 0.0,
            auto_db: 0.0,
            auto_target_db: 0.0,
            auto_dirty: true,
            dc: [(0.0, 0.0); 2],
            meters: Arc::new(Meters::default()),
            inp: [vec![0.0; SUB], vec![0.0; SUB]],
            buf: [vec![0.0; SUB], vec![0.0; SUB]],
            det: vec![0.0; SUB],
            running: false,
            outgoing: Box::new(Outgoing {
                fir: None,
                zero: false,
                shared: false,
                spectral: None,
                out: [vec![0.0; SUB], vec![0.0; SUB]],
                wait: 0,
                at: 0,
                len: 0,
            }),
            clock: 0,
            primed: false,
        };
        eq.reconfigure();
        eq.output_db = eq.value(GLOBAL + global::OUTPUT);
        eq.auto_db = eq.auto_target();
        eq.auto_target_db = eq.auto_db;
        eq.auto_dirty = false;
        for b in 0..BANDS {
            let s = eq.settings[b];
            let band = &mut eq.bands[b];
            band.log_freq = s.design.freq.ln();
            band.gain = s.design.gain_db;
            band.log_q = s.design.q.ln();
            band.structure = Some(s.structure());
            band.mix = if s.active() { 1.0 } else { 0.0 };
        }
        eq
    }

    pub fn values(&self) -> &[f64] {
        &self.values
    }

    pub fn value(&self, id: u32) -> f64 {
        self.values.get(id as usize).copied().unwrap_or(0.0)
    }

    pub fn sample_rate(&self) -> f64 {
        self.fs
    }

    pub fn meters(&self) -> &Arc<Meters> {
        &self.meters
    }

    pub fn band(&self, b: usize) -> BandSettings {
        self.settings[b]
    }

    pub fn phase_mode(&self) -> PhaseMode {
        self.mode
    }

    /// Sets parameter `id` to `v` in its own units.
    pub fn set(&mut self, id: u32, v: f64) {
        let Some(slot) = self.values.get_mut(id as usize) else { return };
        let v = params::clamp(id, v);
        if *slot == v {
            return;
        }
        *slot = v;
        self.reconfigure();
    }

    /// Replaces every value (a recalled state).
    pub fn set_all(&mut self, values: &[f64]) {
        for (id, v) in values.iter().enumerate().take(params::TOTAL) {
            self.values[id] = params::clamp(id as u32, *v);
        }
        self.reconfigure();
    }

    /// Reads the parameter table and makes the processing structures match it. It allocates
    /// only for a new FIR length: a change of phase mode or linear phase resolution.
    fn reconfigure(&mut self) {
        for b in 0..BANDS {
            self.settings[b] = BandSettings::read(&self.values, b);
        }
        self.auto_dirty = true;
        let mode = PhaseMode::from_index(self.value(GLOBAL + global::PHASE) as usize);
        let resolution = self.value(GLOBAL + global::RESOLUTION) as usize;
        let core_change = mode != self.mode
            || (mode == PhaseMode::Linear && resolution != self.resolution)
            || self.fir.is_some() == (mode == PhaseMode::ZeroLatency);
        let was_zero = self.fir.is_none();
        let mut old_fir = None;
        if core_change {
            self.mode = mode;
            self.resolution = resolution;
            let fir = match mode {
                PhaseMode::ZeroLatency => None,
                _ => Some(Box::new(self.fir_path())),
            };
            old_fir = std::mem::replace(&mut self.fir, fir);
            for b in &mut self.bands {
                b.detector.reset();
                if mode == PhaseMode::ZeroLatency {
                    b.state.iter_mut().for_each(CascadeState::reset);
                }
            }
        }
        if let Some(fir) = &mut self.fir {
            for (b, s) in self.settings.iter().enumerate().take(BANDS) {
                fir.engine.use_dynamic(b, s.active() && s.dynamic == 1);
            }
            fir.dirty = true;
        }
        let wants_spectral = self.settings.iter().any(|s| s.spectral().is_some());
        let spectral_change = wants_spectral != self.spectral.is_some();
        let fade = self.running && (core_change || spectral_change);
        if fade && let Some(stage) = self.outgoing.spectral.take() {
            self.keep_spectral(stage);
        }
        let old_spectral = match (&self.spectral, wants_spectral) {
            (Some(_), false) => self.spectral.take(),
            (Some(_), true) if fade => {
                let mut copy = self.spare_spectral();
                copy.copy_from(self.spectral.as_deref().unwrap());
                Some(copy)
            }
            _ => None,
        };
        if wants_spectral {
            if self.spectral.is_none() {
                self.spectral = Some(self.spare_spectral());
            }
            let stage = self.spectral.as_deref_mut().unwrap();
            for (b, s) in self.settings.iter().enumerate() {
                stage.configure(b, s.spectral());
            }
        }
        if fade {
            let latency =
                self.fir.as_ref().map_or(0, |f| f.engine.latency()) + self.spectral.as_ref().map_or(0, |s| s.latency());
            let o = &mut *self.outgoing;
            // A spectral switch keeps an earlier mode switch's FIR path rather than free it on
            // the audio thread; only a mode switch, which builds a path there anyway, lets it go.
            if core_change {
                o.fir = old_fir;
            }
            o.zero = core_change && was_zero;
            o.shared = !core_change;
            o.spectral = old_spectral;
            o.out.iter_mut().for_each(|out| out.fill(0.0));
            o.wait = latency + 2 * SUB;
            o.at = 0;
            o.len = (SWITCH_FADE_SECONDS * self.fs) as usize;
        } else if let Some(stage) = old_spectral {
            self.keep_spectral(stage);
        }
    }

    /// A spectral stage as `Spectral::new` makes it, from the spares when there is one.
    fn spare_spectral(&mut self) -> Box<Spectral> {
        match self.spare_spectral.iter_mut().find_map(Option::take) {
            Some(mut stage) => {
                stage.clear();
                stage
            }
            None => {
                debug_assert!(false, "no spare spectral stage");
                Box::new(Spectral::new(self.fs, BANDS))
            }
        }
    }

    fn keep_spectral(&mut self, stage: Box<Spectral>) {
        if let Some(slot) = self.spare_spectral.iter_mut().find(|s| s.is_none()) {
            *slot = Some(stage);
        } else {
            debug_assert!(false, "no room for a spare spectral stage");
        }
    }

    fn fir_path(&self) -> FirPath {
        let scale = (self.fs / 48000.0).log2().round().clamp(0.0, 3.0) as u32;
        let p = 256usize << scale;
        let (n, timing) = match self.mode {
            PhaseMode::Natural => (8192usize << scale, Timing::Natural),
            _ => (2048usize << (self.resolution.min(4) as u32 + scale), Timing::Linear),
        };
        let engine = FirEngine::new(n, p, timing, BANDS);
        let bins = n / 2 + 1;
        let latency = engine.latency();
        FirPath {
            engine,
            cache: (0..BANDS).map(|_| vec![C64::ONE; bins]).collect(),
            cached: vec![None; BANDS],
            resp: std::array::from_fn(|_| vec![C64::default(); bins]),
            delay: (0..BANDS).map(|_| vec![0.0; latency.div_ceil(SUB)]).collect(),
            delay_at: 0,
            amounts: vec![0.0; BANDS],
            aux: [vec![0.0; latency], vec![0.0; latency]],
            aux_at: 0,
            dirty: true,
            hold: 0,
        }
    }

    /// The EQ's static response at `hz` for a placement's channel, as the editor draws it
    /// (gain changes of dynamic bands left out).
    pub fn response(&self, hz: f64) -> f64 {
        self.settings.iter().filter(|s| s.active()).map(|s| Prototype::design(&s.rest()).magnitude(hz)).product()
    }

    fn auto_target(&self) -> f64 {
        if self.value(GLOBAL + global::AUTO_GAIN) < 0.5 {
            return 0.0;
        }
        let mut protos = [(Prototype::default(), 0.0); BANDS];
        let mut count = 0;
        for s in self.settings.iter().filter(|s| s.active()) {
            let w = match s.placement {
                Placement::Stereo | Placement::Mid => 1.0,
                _ => 0.5,
            };
            protos[count] = (Prototype::design(&s.rest()), w);
            count += 1;
        }
        let protos = &protos[..count];
        auto_gain_db(|hz| protos.iter().map(|(p, w)| p.magnitude(hz).powf(2.0 * w)).product())
    }

    fn rebuild_fir(&mut self) {
        let Some(fir) = self.fir.as_deref_mut() else { return };
        let n = fir.engine.grid();
        let natural = fir.engine.timing() == Timing::Natural;
        let fs = self.fs;
        for b in 0..BANDS {
            let s = self.settings[b];
            let key = s.active().then_some(s);
            let same = match (&fir.cached[b], &key) {
                (Some(a), Some(k)) => a.design == k.design && a.placement == k.placement,
                (None, None) => true,
                _ => false,
            };
            if same {
                continue;
            }
            band_response(&mut fir.cache[b], key.as_ref().map(|s| &s.design), n, fs, natural);
            fir.cached[b] = key;
        }
        let layout = layout_of(self.settings.iter().filter(|s| s.active()).map(|s| s.placement));
        let FirPath { engine, cache, resp, dirty, .. } = fir;
        let settings = &self.settings;
        let groups = |k: usize, swap: Option<(usize, C64)>| -> [C64; 5] {
            let mut g = [C64::ONE; 5];
            for (b, s) in settings.iter().enumerate() {
                if !s.active() {
                    continue;
                }
                let h = cache[b][k];
                let i = s.placement as usize;
                g[i] = match swap {
                    Some((sb, d)) if sb == b => g[i].times(d),
                    _ => g[i].times(h),
                };
            }
            g
        };
        let bins = n / 2 + 1;
        for k in 0..bins {
            let m = matrix(groups(k, None));
            for (r, v) in resp.iter_mut().zip(m) {
                r[k] = v;
            }
        }
        engine.set_main(layout, resp);
        for b in 0..BANDS {
            if !engine.has_dynamic(b) {
                continue;
            }
            let s = settings[b];
            let full = Prototype::design(&Design { gain_db: s.design.gain_db + s.range, ..s.design });
            let i = s.placement as usize;
            for k in 0..bins {
                let hz = k as f64 * fs / n as f64;
                let hs = cache[b][k];
                let hf = if natural { full.response(hz) } else { C64::new(full.magnitude(hz), 0.0) };
                // The band's factor swapped for (full - static); everything else as is.
                let mut g = groups(k, Some((b, hf.minus(hs))));
                if i >= 3 {
                    // A mid band leaves the side untouched and the other way round.
                    g[7 - i] = C64::default();
                }
                let m = matrix_delta(g, i);
                for (r, v) in resp.iter_mut().zip(m) {
                    r[k] = v;
                }
            }
            engine.set_dynamic(b, layout_of(std::iter::once(s.placement)).max_with(layout), resp);
        }
        *dirty = false;
    }

    /// Feeds the detectors `frames` frames; at the `end` of a grid cell they set the bands'
    /// dynamic gain, which the next cell uses.
    fn run_detectors(&mut self, frames: usize, aux: Option<(&[f32], &[f32])>, at: usize, end: bool) {
        let fs = self.fs;
        for b in 0..BANDS {
            let s = self.settings[b];
            let band = &mut self.bands[b];
            if !(s.active() && s.dynamic == 1) {
                band.dynamic_db = 0.0;
                band.detecting = false;
                continue;
            }
            if !std::mem::replace(&mut band.detecting, true) {
                band.detector.reset();
            }
            // A dynamic cut listens to what it removes.
            let listen = match s.design.shape {
                Shape::LowCut => Design { shape: Shape::LowShelf, ..s.design },
                Shape::HighCut => Design { shape: Shape::HighShelf, ..s.design },
                _ => s.design,
            };
            band.detector.retune(&listen, fs);
            for i in 0..frames {
                let (l, r) = if s.external {
                    aux.map_or((0.0, 0.0), |(al, ar)| {
                        (al.get(at + i).copied().unwrap_or(0.0) as f64, ar.get(at + i).copied().unwrap_or(0.0) as f64)
                    })
                } else {
                    (self.inp[0][i], self.inp[1][i])
                };
                self.det[i] = match s.placement {
                    Placement::Stereo => {
                        if l.abs() > r.abs() {
                            l
                        } else {
                            r
                        }
                    }
                    Placement::Left => l,
                    Placement::Right => r,
                    Placement::Mid => 0.5 * (l + r),
                    Placement::Side => 0.5 * (l - r),
                };
            }
            band.detector.feed(&self.det[..frames], Ballistics::new(s.attack, s.release, fs), fs);
            if end {
                band.dynamic_db = band.detector.settle(s.threshold, s.auto_threshold, s.range);
            }
        }
    }

    /// Moves the zero-latency bands on by a grid cell: their smoothing, fades and designs.
    fn step_bands(&mut self) {
        let fs = self.fs;
        let k = 1.0 - (-(SUB as f64) / (SMOOTH_SECONDS * fs)).exp();
        let fade_step = SUB as f64 / (FADE_SECONDS * fs);
        for b in 0..BANDS {
            let s = self.settings[b];
            let band = &mut self.bands[b];
            band.mix_from = band.mix;
            let want = if s.active() { 1.0 } else { 0.0 };
            if band.mix == 0.0 && want == 0.0 {
                continue;
            }
            let restructure = band.structure != Some(s.structure());
            let target_mix = if restructure { 0.0 } else { want };
            let from = band.mix;
            band.mix =
                if target_mix > from { (from + fade_step).min(target_mix) } else { (from - fade_step).max(target_mix) };
            if restructure && band.mix == 0.0 {
                band.structure = Some(s.structure());
                band.log_freq = s.design.freq.ln();
                band.gain = s.design.gain_db;
                band.log_q = s.design.q.ln();
                band.state.iter_mut().for_each(CascadeState::reset);
                band.last = None;
                band.mix_from = 0.0;
                continue;
            }
            let snap = |cur: &mut f64, target: f64| {
                *cur += (target - *cur) * k;
                if (target - *cur).abs() < 1e-6 {
                    *cur = target;
                }
            };
            if !restructure {
                snap(&mut band.log_freq, s.design.freq.ln());
                snap(&mut band.gain, s.design.gain_db);
                snap(&mut band.log_q, s.design.q.ln());
            }
            let rest = Design { freq: band.log_freq.exp(), gain_db: band.gain, q: band.log_q.exp(), ..s.design };
            let design = if s.dynamic == 1 || s.design.shape.has_gain() { rest.moved(band.dynamic_db) } else { rest };
            let design = Design { shape: band.structure.map_or(design.shape, |st| st.0), ..design };
            if band.last != Some(design) {
                band.cascade = Cascade::matched(&Prototype::design(&design), fs);
                band.last = Some(design);
            }
        }
    }

    /// Runs the zero-latency bands over `buf[..frames]`, `phase` frames into the grid cell.
    fn run_bands(&mut self, phase: usize, frames: usize, end: bool) {
        for b in 0..BANDS {
            let s = self.settings[b];
            let band = &mut self.bands[b];
            let (m0, m1) = (band.mix_from, band.mix);
            if m0 == 0.0 && m1 == 0.0 {
                continue;
            }
            let [bl, br] = &mut self.buf;
            for i in 0..frames {
                let m = m0 + (m1 - m0) * (phase + i + 1) as f64 / SUB as f64;
                let (l, r) = (bl[i], br[i]);
                let (nl, nr) = match s.placement {
                    Placement::Stereo => (band.state[0].run(&band.cascade, l), band.state[1].run(&band.cascade, r)),
                    Placement::Left => (band.state[0].run(&band.cascade, l), r),
                    Placement::Right => (l, band.state[1].run(&band.cascade, r)),
                    Placement::Mid => {
                        let y = band.state[0].run(&band.cascade, 0.5 * (l + r));
                        let side = 0.5 * (l - r);
                        (y + side, y - side)
                    }
                    Placement::Side => {
                        let y = band.state[1].run(&band.cascade, 0.5 * (l - r));
                        let mid = 0.5 * (l + r);
                        (mid + y, mid - y)
                    }
                };
                bl[i] = l + m * (nl - l);
                br[i] = r + m * (nr - r);
            }
            if end {
                band.state.iter_mut().for_each(CascadeState::flush);
            }
        }
    }

    /// The signal of a soloed band or an auditioned detector over `buf`, when any.
    fn run_solo(&mut self, frames: usize, aux: Option<(&[f32], &[f32])>, at: usize, end: bool) -> bool {
        let Some(b) = self.settings.iter().position(|s| s.used && (s.solo || s.audition)) else { return false };
        let s = self.settings[b];
        let band = &mut self.bands[b];
        let ad = filters::audition_design(&s.design);
        if band.solo_design != Some(ad) {
            band.solo = Cascade::matched(&Prototype::design(&ad), self.fs);
            band.solo_design = Some(ad);
        }
        for i in 0..frames {
            let (l, r) = if s.audition && s.external {
                aux.map_or((0.0, 0.0), |(al, ar)| {
                    (al.get(at + i).copied().unwrap_or(0.0) as f64, ar.get(at + i).copied().unwrap_or(0.0) as f64)
                })
            } else {
                (self.inp[0][i], self.inp[1][i])
            };
            self.buf[0][i] = band.solo_state[0].run(&band.solo, l);
            self.buf[1][i] = band.solo_state[1].run(&band.solo, r);
        }
        if end {
            band.solo_state.iter_mut().for_each(CascadeState::flush);
        }
        true
    }

    fn publish(&self) {
        for (b, band) in self.bands.iter().enumerate() {
            let s = &self.settings[b];
            let db = match s.dynamic {
                1 => band.dynamic_db,
                2 => self.spectral.as_ref().map_or(0.0, |sp| sp.band_gain(b)),
                _ => 0.0,
            };
            self.meters.dynamic_db[b].store((db as f32).to_bits(), Ordering::Relaxed);
            self.meters.auto_threshold[b].store((band.detector.auto_threshold() as f32).to_bits(), Ordering::Relaxed);
            self.meters.detector_db[b].store((band.detector.level_db() as f32).to_bits(), Ordering::Relaxed);
        }
        self.meters.auto_gain_db.store((self.auto_db as f32).to_bits(), Ordering::Relaxed);
    }
}

trait MaxWith {
    fn max_with(self, other: Layout) -> Layout;
}

impl MaxWith for Layout {
    fn max_with(self, other: Layout) -> Layout {
        let rank = |l: Layout| match l {
            Layout::Scalar => 0,
            Layout::Diagonal => 1,
            Layout::Full => 2,
        };
        if rank(self) >= rank(other) { self } else { other }
    }
}

fn layout_of(placements: impl Iterator<Item = Placement>) -> Layout {
    let mut layout = Layout::Scalar;
    for p in placements {
        layout = layout.max_with(match p {
            Placement::Stereo => Layout::Scalar,
            Placement::Left | Placement::Right => Layout::Diagonal,
            Placement::Mid | Placement::Side => Layout::Full,
        });
    }
    layout
}

/// The 2×2 matrix (LL, LR, RL, RR) of group products `[stereo, left, right, mid, side]`:
/// mid/side bands first, then the single-channel ones, all scaled by the stereo ones.
fn matrix(g: [C64; 5]) -> [C64; 4] {
    let [st, l, r, m, s] = g;
    let (sum, diff) = (m.plus(s).scale(0.5), m.minus(s).scale(0.5));
    let (a, b) = (st.times(l), st.times(r));
    [a.times(sum), a.times(diff), b.times(diff), b.times(sum)]
}

/// The change of `matrix` when group `i` changes by `g[i]` (the others as given; the pair
/// partner of a mid, side, left or right group is zeroed by the caller).
fn matrix_delta(g: [C64; 5], i: usize) -> [C64; 4] {
    match i {
        1 => {
            let [st, l, _, m, s] = g;
            let (sum, diff) = (m.plus(s).scale(0.5), m.minus(s).scale(0.5));
            let a = st.times(l);
            [a.times(sum), a.times(diff), C64::default(), C64::default()]
        }
        2 => {
            let [st, _, r, m, s] = g;
            let (sum, diff) = (m.plus(s).scale(0.5), m.minus(s).scale(0.5));
            let b = st.times(r);
            [C64::default(), C64::default(), b.times(diff), b.times(sum)]
        }
        _ => matrix(g),
    }
}

/// A band's response on an `n`-point grid: the analog magnitude (linear phase) or complex
/// response (natural phase); the brickwall is ideal in linear phase.
fn band_response(out: &mut [C64], design: Option<&Design>, n: usize, fs: f64, natural: bool) {
    let Some(d) = design else {
        out.fill(C64::ONE);
        return;
    };
    let p = Prototype::design(d);
    let brickwall = !natural && d.slope == BRICKWALL && matches!(d.shape, Shape::LowCut | Shape::HighCut);
    for (k, o) in out.iter_mut().enumerate() {
        let hz = k as f64 * fs / n as f64;
        *o = if brickwall {
            let pass = if d.shape == Shape::LowCut { hz >= d.freq } else { hz <= d.freq };
            C64::new(if pass { 1.0 } else { 0.0 }, 0.0)
        } else if natural {
            p.response(hz)
        } else {
            C64::new(p.magnitude(hz), 0.0)
        };
    }
}

/// The gain that keeps pink noise through `power(hz)` (|H|²) at the same A-weighted level:
/// −10·log10 of the A-weighted mean of the power over a log grid from 20 Hz to 20 kHz.
pub fn auto_gain_db(power: impl Fn(f64) -> f64) -> f64 {
    let (mut num, mut den) = (0.0, 0.0);
    for i in 0..96 {
        let hz = 20.0 * 1000f64.powf(i as f64 / 95.0);
        let w = a_weight(hz).powi(2);
        num += w * power(hz);
        den += w;
    }
    (-10.0 * (num / den).max(1e-12).log10()).clamp(-36.0, 36.0)
}

/// The A-weighting curve (IEC 61672) as an amplitude.
fn a_weight(f: f64) -> f64 {
    let f2 = f * f;
    let num = 12194f64.powi(2) * f2 * f2;
    let den =
        (f2 + 20.6f64.powi(2)) * ((f2 + 107.7f64.powi(2)) * (f2 + 737.9f64.powi(2))).sqrt() * (f2 + 12194f64.powi(2));
    num / den * 10f64.powf(2.0 / 20.0)
}

/// The character stage. Subtle: `2·tanh(x/2)`, odd harmonics only, under 0.1 % at −12 dBFS.
/// Warm: `(tanh(x + 0.15) − tanh(0.15)) / (1 − tanh²(0.15))`, whose bias adds even harmonics;
/// the DC it leaves is removed by a 5 Hz high pass. Both have unity small-signal gain.
fn character(kind: usize, x: f64, dc: &mut (f64, f64), fs: f64) -> f64 {
    match kind {
        1 => 2.0 * (x * 0.5).tanh(),
        2 => {
            const BIAS: f64 = 0.15;
            let t = BIAS.tanh();
            let y = ((x + BIAS).tanh() - t) / (1.0 - t * t);
            let r = 1.0 - 2.0 * std::f64::consts::PI * 5.0 / fs;
            let out = y - dc.0 + r * dc.1;
            *dc = (y, out);
            out
        }
        _ => x,
    }
}

impl ScytheEq {
    /// Processes a block whose first frame is at sample `position`; without one (a stopped
    /// transport) the grid carries on from the previous block.
    pub fn process_block(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        aux: Option<(&[f32], &[f32])>,
        position: Option<i64>,
    ) {
        let frames = left.len().min(right.len());
        self.running = true;
        if let Some(p) = position {
            self.clock = p;
        }
        if self.auto_dirty {
            self.auto_target_db = self.auto_target();
            self.auto_dirty = false;
        }
        let output_target = self.value(GLOBAL + global::OUTPUT);
        let char_kind = self.value(GLOBAL + global::CHARACTER) as usize;
        let width = self.value(GLOBAL + global::WIDTH) / 100.0;
        let pan = self.value(GLOBAL + global::PAN) / 100.0;
        let (pan_l, pan_r) = ((1.0 - pan).min(1.0), (1.0 + pan).min(1.0));
        let fs = self.fs;
        let smooth = 1.0 - (-1.0 / (SMOOTH_SECONDS * fs)).exp();
        let mut at = 0;
        while at < frames {
            let phase = self.clock.rem_euclid(SUB as i64) as usize;
            let n = (frames - at).min(SUB - phase);
            // A cell entered part way (the first after a reset) starts where it is entered.
            let start = phase == 0 || !self.primed;
            let end = phase + n == SUB;
            self.primed = true;
            for i in 0..n {
                self.inp[0][i] = left[at + i] as f64;
                self.inp[1][i] = right[at + i] as f64;
            }
            self.buf[0][..n].copy_from_slice(&self.inp[0][..n]);
            self.buf[1][..n].copy_from_slice(&self.inp[1][..n]);
            let solo = self.run_solo(n, aux, at, end);
            let fading = !solo && self.outgoing.at < self.outgoing.len;
            let old_bands = fading && self.outgoing.zero;
            if start && !solo && (old_bands || self.fir.is_none()) {
                self.step_bands();
            }
            self.run_detectors(n, aux, at, end);
            if old_bands {
                self.run_bands(phase, n, end);
                let o = &mut *self.outgoing;
                for c in 0..2 {
                    o.out[c][..n].copy_from_slice(&self.buf[c][..n]);
                    self.buf[c][..n].copy_from_slice(&self.inp[c][..n]);
                }
            } else if fading && !self.outgoing.shared {
                let o = &mut *self.outgoing;
                if let Some(fir) = o.fir.as_deref_mut() {
                    for i in 0..n {
                        let (l, r) = fir.engine.push(self.inp[0][i] as f32, self.inp[1][i] as f32, &fir.amounts);
                        o.out[0][i] = l as f64;
                        o.out[1][i] = r as f64;
                    }
                }
            }
            if let Some(fir) = self.fir.as_deref() {
                if fir.dirty && fir.hold == 0 {
                    self.rebuild_fir();
                    let fir = self.fir.as_deref_mut().unwrap();
                    fir.hold = fir.engine.latency().min(4096);
                }
                let fir = self.fir.as_deref_mut().unwrap();
                fir.hold = fir.hold.saturating_sub(n);
                if start {
                    for (amount, ring) in fir.amounts.iter_mut().zip(&fir.delay) {
                        *amount = ring[fir.delay_at];
                    }
                }
                if !solo {
                    for i in 0..n {
                        let (l, r) = fir.engine.push(self.buf[0][i] as f32, self.buf[1][i] as f32, &fir.amounts);
                        self.buf[0][i] = l as f64;
                        self.buf[1][i] = r as f64;
                    }
                }
                if end {
                    for (b, s) in self.settings.iter().enumerate() {
                        fir.delay[b][fir.delay_at] = if s.active() && s.dynamic == 1 {
                            let gs = 10f64.powf(s.design.gain_db / 20.0);
                            let gf = 10f64.powf((s.design.gain_db + s.range) / 20.0);
                            let gt = 10f64.powf((s.design.gain_db + self.bands[b].dynamic_db) / 20.0);
                            if (gf - gs).abs() > 1e-12 { ((gt - gs) / (gf - gs)) as f32 } else { 0.0 }
                        } else {
                            0.0
                        };
                    }
                    fir.delay_at = (fir.delay_at + 1) % fir.delay[0].len();
                }
            } else if !solo {
                self.run_bands(phase, n, end);
            }
            if fading {
                let o = &mut *self.outgoing;
                if o.shared {
                    for c in 0..2 {
                        o.out[c][..n].copy_from_slice(&self.buf[c][..n]);
                    }
                }
                if let Some(sp) = o.spectral.as_deref_mut() {
                    for i in 0..n {
                        let x = (o.out[0][i] as f32, o.out[1][i] as f32);
                        let (l, r) = sp.push(x, x);
                        o.out[0][i] = l as f64;
                        o.out[1][i] = r as f64;
                    }
                }
            }
            if let Some(sp) = self.spectral.as_deref_mut()
                && !solo
            {
                let externals = self.settings.iter().any(|s| s.active() && s.dynamic == 2 && s.external);
                for i in 0..n {
                    let x = (self.buf[0][i] as f32, self.buf[1][i] as f32);
                    let mut sc = x;
                    if externals && let Some((al, ar)) = aux {
                        let raw = (al.get(at + i).copied().unwrap_or(0.0), ar.get(at + i).copied().unwrap_or(0.0));
                        sc = match self.fir.as_deref_mut() {
                            Some(fir) if !fir.aux[0].is_empty() => {
                                let j = fir.aux_at;
                                let old = (fir.aux[0][j], fir.aux[1][j]);
                                fir.aux[0][j] = raw.0;
                                fir.aux[1][j] = raw.1;
                                fir.aux_at = (j + 1) % fir.aux[0].len();
                                old
                            }
                            _ => raw,
                        };
                    }
                    let (l, r) = sp.push(x, sc);
                    self.buf[0][i] = l as f64;
                    self.buf[1][i] = r as f64;
                }
            }
            if fading {
                let o = &mut *self.outgoing;
                for i in 0..n {
                    let (new, old) = if o.wait > 0 {
                        o.wait -= 1;
                        (0.0, 1.0)
                    } else if o.at < o.len {
                        let t = o.at as f64 / o.len as f64 * std::f64::consts::FRAC_PI_2;
                        o.at += 1;
                        (t.sin(), t.cos())
                    } else {
                        (1.0, 0.0)
                    };
                    for c in 0..2 {
                        self.buf[c][i] = new * self.buf[c][i] + old * o.out[c][i];
                    }
                }
            }
            for i in 0..n {
                self.output_db += (output_target - self.output_db) * smooth;
                self.auto_db += (self.auto_target_db - self.auto_db) * smooth;
                let (mut l, mut r) = (self.buf[0][i], self.buf[1][i]);
                if !solo {
                    let g = 10f64.powf((self.output_db + self.auto_db) / 20.0);
                    l = character(char_kind, l * g, &mut self.dc[0], fs);
                    r = character(char_kind, r * g, &mut self.dc[1], fs);
                    if width != 1.0 {
                        let (m, s) = (0.5 * (l + r), 0.5 * (l - r) * width);
                        l = m + s;
                        r = m - s;
                    }
                    l *= pan_l;
                    r *= pan_r;
                }
                left[at + i] = l as f32;
                right[at + i] = r as f32;
            }
            at += n;
            self.clock += n as i64;
        }
        self.publish();
    }
}

impl ScytheEq {
    /// Clears what the processing remembers of earlier audio, as after a seek.
    pub fn reset(&mut self) {
        for b in &mut self.bands {
            b.state.iter_mut().for_each(CascadeState::reset);
            b.solo_state.iter_mut().for_each(CascadeState::reset);
            b.detector.reset();
            b.dynamic_db = 0.0;
        }
        if let Some(fir) = &mut self.fir {
            fir.engine.reset();
            fir.delay.iter_mut().for_each(|d| d.fill(0.0));
            fir.aux.iter_mut().for_each(|d| d.fill(0.0));
        }
        if let Some(sp) = &mut self.spectral {
            sp.reset();
        }
        self.outgoing.at = self.outgoing.len;
        self.dc = [(0.0, 0.0); 2];
        self.clock = 0;
        self.primed = false;
    }

    /// Frames the output lags the input.
    pub fn latency(&self) -> usize {
        self.fir.as_ref().map_or(0, |f| f.engine.latency()) + self.spectral.as_ref().map_or(0, |s| s.latency())
    }

    /// The stored state, with any values from a newer version written back unchanged.
    pub fn state(&self) -> Vec<u8> {
        params::encode(&params::unclamp(&self.values, &self.unclamped), &self.extra, self.version)
    }

    /// Recalls a stored state; false (and nothing changed) when `chunk` is not one.
    pub fn load_state(&mut self, chunk: &[u8]) -> bool {
        let Some(decoded) = params::decode(chunk) else { return false };
        self.set_all(&decoded.values);
        self.extra = decoded.extra;
        self.version = decoded.version;
        self.unclamped = decoded.unclamped;
        true
    }
}

#[cfg(test)]
mod tests;
