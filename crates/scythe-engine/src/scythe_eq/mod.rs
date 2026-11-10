//! Scythe EQ: Scythe's own parametric and dynamic equaliser. Up to 24 bands of ten shapes,
//! zero latency (matched IIR), natural phase or linear phase processing, per-band stereo
//! placement, dynamic and spectral dynamic bands with internal or external sidechain, a
//! character stage, auto gain and an output section.
//!
//! It is stored as a CLAP slot (`<CLAP "CLAP: Scythe EQ (Scythe)" scythe.eq "">` with its
//! state in `<STATE`), a block REAPER keeps byte for byte when it does not know the plugin.

pub mod dynamics;
pub mod filters;
pub mod fir;
pub mod params;
pub mod spectral;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::clap_host::ParamInfo;
use crate::fx::{Effect, ProcessContext};
use crate::probe::FxProbe;

use dynamics::{Ballistics, Detector};
use filters::{BRICKWALL, C64, Cascade, CascadeState, Design, Prototype, Shape};
use fir::{FirEngine, Layout, Timing};
use params::{BAND_STRIDE, BANDS, GLOBAL, PhaseMode, Placement, band, global};
use spectral::{Spectral, SpectralBand};

pub const NAME: &str = "Scythe EQ";
pub const CLAP_ID: &str = "scythe.eq";
pub const DISPLAY: &str = "CLAP: Scythe EQ (Scythe)";
pub const STATUS: &str = "Scythe EQ";

/// Whether `slot` holds a Scythe EQ.
pub fn is_slot(slot: &scythe_core::FxSlot) -> bool {
    slot.raw.iter().any(|n| {
        matches!(n, scythe_core::rpp::Node::Chunk(c)
            if c.name() == "CLAP" && c.args().get(1).is_some_and(|id| id == CLAP_ID))
    })
}

/// The values stored in a Scythe EQ slot.
pub fn slot_values(slot: &scythe_core::FxSlot) -> Option<Vec<f64>> {
    use base64::Engine as _;
    let chunk = slot.raw.iter().find_map(|n| match n {
        scythe_core::rpp::Node::Chunk(c) if c.name() == "CLAP" => Some(c),
        _ => None,
    })?;
    let state = chunk.chunks("STATE").next()?;
    let text: String = state
        .children
        .iter()
        .filter_map(|n| match n {
            scythe_core::rpp::Node::Line(l) => Some(l.trim()),
            scythe_core::rpp::Node::Chunk(_) => None,
        })
        .collect();
    params::decode(&base64::engine::general_purpose::STANDARD.decode(text).ok()?)
}

/// A new slot holding a Scythe EQ with `state` (the default when `None`).
pub fn slot(state: Option<&[u8]>) -> scythe_core::FxSlot {
    use base64::Engine as _;
    use scythe_core::rpp::{Chunk, Node};
    let state = state.map_or_else(|| params::encode(&params::defaults()), <[u8]>::to_vec);
    let mut chunk = Chunk::new("CLAP", &[DISPLAY, CLAP_ID, ""]);
    chunk.line(["CFG", "4", "0", "0", ""]);
    let mut st = Chunk::new("STATE", &[]);
    let text = base64::engine::general_purpose::STANDARD.encode(&state);
    for line in text.as_bytes().chunks(128) {
        st.line([String::from_utf8_lossy(line)]);
    }
    chunk.push_chunk(st);
    scythe_core::FxSlot {
        name: DISPLAY.into(),
        enabled: true,
        raw: vec![
            Node::Line("BYPASS 0 0 0".into()),
            Node::Chunk(chunk),
            Node::Line(format!("FXID {}", scythe_core::model::new_guid())),
            Node::Line("WAK 0 0".into()),
        ],
    }
}

/// Frames between parameter updates of the zero-latency filters and the detectors.
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
    pub sidechain: crate::probe::FxProbe,
}

impl Default for Meters {
    fn default() -> Self {
        let zero = || std::array::from_fn(|_| AtomicU32::new(0));
        Self {
            dynamic_db: zero(),
            auto_threshold: zero(),
            detector_db: zero(),
            auto_gain_db: AtomicU32::new(0),
            sidechain: FxProbe::default(),
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
            dynamic: if shape.has_gain() { v(band::DYNAMIC) as u8 } else { 0 },
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

    fn structure(&self) -> (Shape, usize, bool) {
        (self.design.shape, self.design.slope, self.design.gain_q)
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
    detector: Detector,
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
    settings: Vec<BandSettings>,
    bands: Vec<Band>,
    mode: PhaseMode,
    resolution: usize,
    fir: Option<Box<FirPath>>,
    spectral: Option<Box<Spectral>>,
    output_db: f64,
    auto_db: f64,
    auto_target_db: f64,
    auto_dirty: bool,
    dc: [(f64, f64); 2],
    meters: Arc<Meters>,
    inp: [Vec<f64>; 2],
    buf: [Vec<f64>; 2],
    det: Vec<f64>,
}

impl ScytheEq {
    pub fn new(sample_rate: u32) -> Self {
        Self::with_values(params::defaults(), sample_rate)
    }

    pub fn from_state(state: &[u8], sample_rate: u32) -> Option<Self> {
        Some(Self::with_values(params::decode(state)?, sample_rate))
    }

    pub fn with_values(values: Vec<f64>, sample_rate: u32) -> Self {
        let fs = sample_rate.max(8000) as f64;
        let mut eq = Self {
            fs,
            settings: (0..BANDS).map(|b| BandSettings::read(&values, b)).collect(),
            values,
            bands: (0..BANDS).map(|_| Band::default()).collect(),
            mode: PhaseMode::ZeroLatency,
            resolution: 1,
            fir: None,
            spectral: None,
            output_db: 0.0,
            auto_db: 0.0,
            auto_target_db: 0.0,
            auto_dirty: true,
            dc: [(0.0, 0.0); 2],
            meters: Arc::new(Meters::default()),
            inp: [vec![0.0; SUB], vec![0.0; SUB]],
            buf: [vec![0.0; SUB], vec![0.0; SUB]],
            det: vec![0.0; SUB],
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

    pub fn meters(&self) -> Arc<Meters> {
        self.meters.clone()
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

    /// Reads the parameter table and makes the processing structures match it; this is the
    /// only place that allocates.
    fn reconfigure(&mut self) {
        for b in 0..BANDS {
            self.settings[b] = BandSettings::read(&self.values, b);
        }
        self.auto_dirty = true;
        let mode = PhaseMode::from_index(self.value(GLOBAL + global::PHASE) as usize);
        let resolution = self.value(GLOBAL + global::RESOLUTION) as usize;
        if mode != self.mode || (mode == PhaseMode::Linear && resolution != self.resolution) || self.fir.is_none() {
            self.mode = mode;
            self.resolution = resolution;
            self.fir = match mode {
                PhaseMode::ZeroLatency => None,
                _ => Some(Box::new(self.fir_path())),
            };
            for b in &mut self.bands {
                b.detector.reset();
            }
        }
        if let Some(fir) = &mut self.fir {
            for b in 0..BANDS {
                let s = &self.settings[b];
                fir.engine.ensure_dynamic(b, s.active() && s.dynamic == 1);
            }
            fir.dirty = true;
        }
        let spectral: Vec<Option<SpectralBand>> = self
            .settings
            .iter()
            .map(|s| {
                (s.active() && s.dynamic == 2).then_some(SpectralBand {
                    design: s.design,
                    placement: s.placement,
                    range: s.range,
                    threshold: s.threshold,
                    auto: s.auto_threshold,
                    attack_ms: s.attack,
                    release_ms: s.release,
                })
            })
            .collect();
        if spectral.iter().any(Option::is_some) {
            let stage = self.spectral.get_or_insert_with(|| Box::new(Spectral::new(self.fs, BANDS)));
            for (b, c) in spectral.into_iter().enumerate() {
                stage.configure(b, c);
            }
        } else {
            self.spectral = None;
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
        self.settings.iter().filter(|s| s.active()).map(|s| Prototype::design(&s.design).magnitude(hz)).product()
    }

    fn auto_target(&self) -> f64 {
        if self.value(GLOBAL + global::AUTO_GAIN) < 0.5 {
            return 0.0;
        }
        let protos: Vec<(Prototype, f64)> = self
            .settings
            .iter()
            .filter(|s| s.active())
            .map(|s| {
                let w = match s.placement {
                    Placement::Stereo | Placement::Mid => 1.0,
                    _ => 0.5,
                };
                (Prototype::design(&s.design), w)
            })
            .collect();
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

    fn run_detectors(&mut self, frames: usize, ctx: &ProcessContext, at: usize) {
        let fs = self.fs;
        for b in 0..BANDS {
            let s = self.settings[b];
            let band = &mut self.bands[b];
            if !(s.active() && s.dynamic == 1) {
                band.dynamic_db = 0.0;
                continue;
            }
            band.detector.retune(&s.design, fs);
            for i in 0..frames {
                let (l, r) = if s.external {
                    ctx.aux.map_or((0.0, 0.0), |(al, ar)| {
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
            let ballistics = Ballistics::new(s.attack, s.release, fs);
            band.dynamic_db =
                band.detector.run(&self.det[..frames], ballistics, s.threshold, s.auto_threshold, s.range, fs);
        }
    }

    /// Runs the zero-latency bands over `buf`.
    fn run_bands(&mut self, frames: usize) {
        let fs = self.fs;
        let k = 1.0 - (-(frames as f64) / (SMOOTH_SECONDS * fs)).exp();
        let fade_step = frames as f64 / (FADE_SECONDS * fs);
        for b in 0..BANDS {
            let s = self.settings[b];
            let band = &mut self.bands[b];
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
            let design = Design {
                freq: band.log_freq.exp(),
                gain_db: band.gain + band.dynamic_db,
                q: band.log_q.exp(),
                ..s.design
            };
            let design = Design { shape: band.structure.map_or(design.shape, |st| st.0), ..design };
            if band.last != Some(design) {
                band.cascade = Cascade::matched(&Prototype::design(&design), fs);
                band.last = Some(design);
            }
            let (m0, m1) = (from, band.mix);
            let [bl, br] = &mut self.buf;
            for i in 0..frames {
                let m = m0 + (m1 - m0) * (i + 1) as f64 / frames as f64;
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
            band.state.iter_mut().for_each(CascadeState::flush);
        }
    }

    /// The signal of a soloed band or an auditioned detector over `buf`, when any.
    fn run_solo(&mut self, frames: usize, ctx: &ProcessContext, at: usize) -> bool {
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
                ctx.aux.map_or((0.0, 0.0), |(al, ar)| {
                    (al.get(at + i).copied().unwrap_or(0.0) as f64, ar.get(at + i).copied().unwrap_or(0.0) as f64)
                })
            } else {
                (self.inp[0][i], self.inp[1][i])
            };
            self.buf[0][i] = band.solo_state[0].run(&band.solo, l);
            self.buf[1][i] = band.solo_state[1].run(&band.solo, r);
        }
        band.solo_state.iter_mut().for_each(CascadeState::flush);
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

impl Effect for ScytheEq {
    fn process(&mut self, left: &mut [f32], right: &mut [f32], ctx: &ProcessContext) {
        let frames = left.len().min(right.len());
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
            let n = (frames - at).min(SUB);
            for i in 0..n {
                self.inp[0][i] = left[at + i] as f64;
                self.inp[1][i] = right[at + i] as f64;
            }
            self.buf[0][..n].copy_from_slice(&self.inp[0][..n]);
            self.buf[1][..n].copy_from_slice(&self.inp[1][..n]);
            self.run_detectors(n, ctx, at);
            let solo = self.run_solo(n, ctx, at);
            if let Some(fir) = self.fir.as_deref() {
                if fir.dirty && fir.hold == 0 {
                    self.rebuild_fir();
                    let fir = self.fir.as_deref_mut().unwrap();
                    fir.hold = fir.engine.latency().min(4096);
                }
                let fir = self.fir.as_deref_mut().unwrap();
                fir.hold = fir.hold.saturating_sub(n);
                let slots = fir.delay[0].len();
                for (b, s) in self.settings.iter().enumerate() {
                    let ring = &mut fir.delay[b];
                    let k = if s.active() && s.dynamic == 1 {
                        let gs = 10f64.powf(s.design.gain_db / 20.0);
                        let gf = 10f64.powf((s.design.gain_db + s.range) / 20.0);
                        let gt = 10f64.powf((s.design.gain_db + self.bands[b].dynamic_db) / 20.0);
                        if (gf - gs).abs() > 1e-12 { ((gt - gs) / (gf - gs)) as f32 } else { 0.0 }
                    } else {
                        0.0
                    };
                    fir.amounts[b] = ring[fir.delay_at];
                    ring[fir.delay_at] = k;
                }
                fir.delay_at = (fir.delay_at + 1) % slots;
                if !solo {
                    for i in 0..n {
                        let (l, r) = fir.engine.push(self.buf[0][i] as f32, self.buf[1][i] as f32, &fir.amounts);
                        self.buf[0][i] = l as f64;
                        self.buf[1][i] = r as f64;
                    }
                }
            } else if !solo {
                self.run_bands(n);
            }
            if let Some(sp) = self.spectral.as_deref_mut()
                && !solo
            {
                let externals = self.settings.iter().any(|s| s.active() && s.dynamic == 2 && s.external);
                for i in 0..n {
                    let x = (self.buf[0][i] as f32, self.buf[1][i] as f32);
                    let mut sc = x;
                    if externals && let Some((al, ar)) = ctx.aux {
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
        }
        if let Some(sc) = ctx.aux
            && self.meters.sidechain.active()
        {
            self.meters.sidechain.tap(crate::probe::Side::Pre, sc.0, sc.1, ctx.sample_rate);
        }
        self.publish();
    }

    fn reset(&mut self) {
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
        self.dc = [(0.0, 0.0); 2];
    }

    fn latency(&mut self) -> usize {
        self.fir.as_ref().map_or(0, |f| f.engine.latency()) + self.spectral.as_ref().map_or(0, |s| s.latency())
    }

    fn params(&mut self) -> Vec<ParamInfo> {
        params::automatable().filter_map(params::info).collect()
    }

    fn param(&mut self, id: u32) -> Option<(f64, String)> {
        params::spec_of(id)?;
        let v = self.value(id);
        Some((v, params::format(id, v)))
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.set(id, value);
    }

    fn set_normalized(&mut self, index: usize, value: f64) -> bool {
        let Some(id) = params::automatable().nth(index) else { return false };
        self.set(id, params::from_normalized(id, value));
        true
    }

    fn normalized(&mut self, index: usize) -> Option<f64> {
        let id = params::automatable().nth(index)?;
        Some(params::to_normalized(id, self.value(id)))
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        Some(params::encode(&self.values))
    }

    fn update_state(&mut self, chunk: &[u8]) -> bool {
        let Some(values) = params::decode(chunk) else { return false };
        self.set_all(&values);
        true
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests;
