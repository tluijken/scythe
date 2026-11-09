//! REAPER's stock effects (the Cockos `rea*.vst.so` plugins), emulated from the state they
//! store. `PLUGINS` maps each one's VST identity to a constructor; an emulation lives in its own
//! file and only adds a row here.

pub(crate) mod params;
pub mod reacomp;
pub mod reagate;
pub mod realimit;
pub mod reasynth;
pub mod reaverbate;
pub mod reaxcomp;

use std::path::Path;

use crate::clap_host::ParamInfo;
use crate::fx::{Effect, ProcessContext};

/// What a constructor gets besides the plugin's state.
pub struct StockContext<'a> {
    pub sample_rate: u32,
    /// For plugins that load files relative to the project (samples, impulse responses).
    pub project_dir: Option<&'a Path>,
}

pub type Constructor = fn(&[u8], &StockContext) -> Result<Box<dyn Effect>, String>;

/// One stock plugin as REAPER names and stores it.
pub struct StockPlugin {
    /// The name in the FX browser, e.g. `ReaComp`.
    pub name: &'static str,
    /// Library file REAPER loads it from.
    pub file: &'static str,
    /// The VST 2 unique id and the class guid REAPER writes after it.
    pub vst_id: u32,
    pub guid: &'static str,
    /// Shown as the slot's status.
    pub status: &'static str,
    pub create: Constructor,
}

impl StockPlugin {
    /// The slot title REAPER writes: `VST: ReaComp (Cockos)`, `VSTi: ReaSynth (Cockos)`.
    pub fn display(&self) -> String {
        let kind = if INSTRUMENTS.contains(&self.name) { "VSTi" } else { "VST" };
        format!("{kind}: {} (Cockos)", self.name)
    }

    /// Whether a slot titled `display` (`VST: …`, `VSTi: …`) holds this plugin.
    pub fn matches(&self, display: &str) -> bool {
        let title = format!("{} (Cockos)", self.name);
        display.starts_with(&title) || display.contains(&format!(" {title}"))
    }
}

/// Stock plugins REAPER lists as instruments.
const INSTRUMENTS: &[&str] = &["ReaSamplOmatic5000", "ReaSynth", "ReaSynDr"];

macro_rules! stock {
    ($name:literal, $file:literal, $id:literal, $guid:literal, $status:literal, $create:expr) => {
        StockPlugin { name: $name, file: $file, vst_id: $id, guid: $guid, status: $status, create: $create }
    };
}

fn boxed<E: Effect + 'static>(e: Option<E>, what: &str) -> Result<Box<dyn Effect>, String> {
    e.map(|e| Box::new(e) as Box<dyn Effect>).ok_or_else(|| format!("unreadable {what} settings"))
}

pub static PLUGINS: &[StockPlugin] = &[
    stock!(
        "ReaEQ",
        "reaeq.vst.so",
        1919247729,
        "56535472656571726561657100000000",
        "ReaEQ-compatible EQ",
        |chunk, cx| boxed(crate::reaeq::ReaEqCompat::from_chunk(chunk, cx.sample_rate), "ReaEQ")
    ),
    stock!(
        "ReaSamplOmatic5000",
        "reasamplomatic.vst.so",
        1920167789,
        "56535472736F6D72656173616D706C6F",
        "ReaSamplOmatic5000-compatible sampler",
        |chunk, cx| {
            use crate::sampler::{Rs5kCompat, Rs5kSettings};
            let settings = Rs5kSettings::parse(chunk).ok_or("unreadable ReaSamplOmatic5000 settings")?;
            Ok(Box::new(Rs5kCompat::new(settings, cx.sample_rate, cx.project_dir)?))
        }
    ),
    stock!(
        "ReaDelay",
        "readelay.vst.so",
        1919247468,
        "5653547265646C72656164656C617900",
        "ReaDelay-compatible delay",
        |chunk, cx| {
            use crate::delay::{ReaDelayCompat, ReaDelaySettings};
            let settings = ReaDelaySettings::parse(chunk).ok_or("unreadable ReaDelay settings")?;
            let mut delay = ReaDelayCompat::new(settings, cx.sample_rate);
            delay.chunk = chunk.to_vec();
            Ok(Box::new(delay))
        }
    ),
    stock!(
        "ReaLimit",
        "realimit.vst.so",
        1919708532,
        "565354726C6D747265616C696D697400",
        "ReaLimit-compatible limiter",
        |chunk, cx| boxed(realimit::Limiter::from_realimit(chunk, cx.sample_rate), "ReaLimit")
    ),
    stock!(
        "ReaGate",
        "reagate.vst.so",
        1919248244,
        "56535472656774726561676174650000",
        "ReaGate-compatible gate",
        |chunk, cx| boxed(reagate::Gate::from_reagate(chunk, cx.sample_rate), "ReaGate")
    ),
    stock!(
        "ReaComp",
        "reacomp.vst.so",
        1919247213,
        "5653547265636D726561636F6D700000",
        "ReaComp-compatible compressor",
        |chunk, cx| boxed(reacomp::Compressor::from_reacomp(chunk, cx.sample_rate), "ReaComp")
    ),
    stock!(
        "ReaXcomp",
        "reaxcomp.vst.so",
        1919252579,
        "5653547265786372656178636F6D7000",
        "ReaXcomp-compatible multiband compressor",
        |chunk, cx| boxed(reaxcomp::MultibandCompressor::from_reaxcomp(chunk, cx.sample_rate), "ReaXcomp")
    ),
    stock!(
        "ReaVerb",
        "reaverb.vst.so",
        1919252066,
        "56535472657662726561766572620000",
        "ReaVerb-compatible convolution",
        |chunk, cx| {
            use crate::reaverb::{ReaVerbCompat, ReaVerbSettings};
            let settings = ReaVerbSettings::parse(chunk).ok_or("unreadable ReaVerb settings")?;
            let mut verb = ReaVerbCompat::new(settings, cx.sample_rate, cx.project_dir)?;
            verb.chunk = chunk.to_vec();
            Ok(Box::new(verb))
        }
    ),
    stock!(
        "ReaVerbate",
        "reaverbate.vst.so",
        1920361016,
        "56535472766238726561766572626174",
        "ReaVerbate-compatible reverb",
        |chunk, cx| boxed(reaverbate::Reverb::from_reaverbate(chunk, cx.sample_rate), "ReaVerbate")
    ),
    stock!(
        "ReaSynth",
        "reasynth.vst.so",
        1919251321,
        "5653547265737972656173796E746800",
        "ReaSynth-compatible synth",
        |chunk, cx| boxed(reasynth::Synth::from_reasynth(chunk, cx.sample_rate), "ReaSynth")
    ),
];

/// The stock plugin a `VST` slot titled `display` holds.
pub fn find(display: &str) -> Option<&'static StockPlugin> {
    PLUGINS.iter().find(|p| p.matches(display))
}

/// Whether a `VST` slot with these arguments holds ReaGate or ReaComp, whose states REAPER
/// only reads with its sidechain header (see `builtin::sidechain_vst_lines`).
pub fn takes_sidechain_header(args: &[String]) -> bool {
    args.first().and_then(|d| find(d)).is_some_and(|p| matches!(p.name, "ReaGate" | "ReaComp"))
}

/// The stock plugin called `name` (`ReaComp`).
pub fn by_name(name: &str) -> Option<&'static StockPlugin> {
    PLUGINS.iter().find(|p| p.name == name)
}

/// ReaGate and ReaComp chunks: two 0xdeadbeef/0xdeadf00d marker words, then every parameter
/// as an f32 in parameter order.
pub(crate) fn marked_params(chunk: &[u8]) -> Option<Vec<f32>> {
    let word = |i: usize| chunk.get(i * 4..i * 4 + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    if word(0)? != 0xdead_beef || word(1)? != 0xdead_f00d {
        return None;
    }
    Some(chunk[8..].as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect())
}

pub(crate) fn marked_chunk(params: &[f32]) -> Vec<u8> {
    let mut d = Vec::with_capacity(8 + params.len() * 4);
    d.extend(0xdead_beef_u32.to_le_bytes());
    d.extend(0xdead_f00d_u32.to_le_bytes());
    for p in params {
        d.extend(p.to_le_bytes());
    }
    d
}

pub(crate) fn param(id: u32, name: &str, min: f64, max: f64, default: f64, log: bool) -> ParamInfo {
    ParamInfo { id, name: name.into(), min, max, default, log }
}

pub(crate) fn db_value(gain: f64) -> (f64, String) {
    let db = 20.0 * gain.max(1e-9).log10();
    (db.max(-150.0), if db <= -149.0 { "-inf dB".into() } else { format!("{db:+.1} dB") })
}

pub(crate) fn ms_value(ms: f64) -> (f64, String) {
    (ms, if ms >= 1000.0 { format!("{:.2} s", ms / 1000.0) } else { format!("{ms:.1} ms") })
}

/// A delay of the stereo signal by a fixed number of frames (lookahead).
pub(crate) struct Lookahead {
    pub(crate) l: Vec<f32>,
    pub(crate) r: Vec<f32>,
    at: usize,
}

impl Lookahead {
    pub(crate) fn new(frames: usize) -> Self {
        Self { l: vec![0.0; frames], r: vec![0.0; frames], at: 0 }
    }

    /// Pushes the newest frame and returns the one `frames` ago.
    pub(crate) fn swap(&mut self, l: f32, r: f32) -> (f32, f32) {
        if self.l.is_empty() {
            return (l, r);
        }
        let out = (self.l[self.at], self.r[self.at]);
        self.l[self.at] = l;
        self.r[self.at] = r;
        self.at = (self.at + 1) % self.l.len();
        out
    }
}

/// The two channels a detector listens to. `SignIn` stores the first of them as a fraction of
/// 1084 (0: the main pair; 1: right and aux left; 2 and up: the aux pair, channels 3/4),
/// silent where the track has no such channels.
pub(crate) struct DetectorInput {
    first: usize,
}

impl DetectorInput {
    pub fn new(sign_in: f32) -> Self {
        Self { first: ((sign_in as f64 * 1084.0).round().max(0.0) as usize).min(2) }
    }

    /// Frame `i` of the detector's channels, given the main pair's.
    pub fn frame(&self, i: usize, l: f32, r: f32, ctx: &ProcessContext) -> (f32, f32) {
        let aux = |c: usize| ctx.aux.map_or(0.0, |a| if c == 0 { a.0[i] } else { a.1[i] });
        match self.first {
            0 => (l, r),
            1 => (r, aux(0)),
            _ => (aux(0), aux(1)),
        }
    }
}

/// A biquad from the RBJ cookbook, run in transposed direct form II.
#[derive(Clone, Copy, Default)]
pub(crate) struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    pub fn lowpass(freq: f64, q: f64, sample_rate: u32) -> Self {
        let (w, alpha) = Self::omega(freq, q, sample_rate);
        let c = w.cos();
        Self::normalized([(1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0], [1.0 + alpha, -2.0 * c, 1.0 - alpha])
    }

    pub fn highpass(freq: f64, q: f64, sample_rate: u32) -> Self {
        let (w, alpha) = Self::omega(freq, q, sample_rate);
        let c = w.cos();
        Self::normalized([(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0], [1.0 + alpha, -2.0 * c, 1.0 - alpha])
    }

    fn omega(freq: f64, q: f64, sample_rate: u32) -> (f64, f64) {
        let w = 2.0 * std::f64::consts::PI * freq / sample_rate as f64;
        (w, w.sin() / (2.0 * q))
    }

    fn normalized(b: [f64; 3], a: [f64; 3]) -> Self {
        Self { b: b.map(|v| v / a[0]), a: [a[1] / a[0], a[2] / a[0]], z: [0.0; 2] }
    }

    pub fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }

    pub fn reset(&mut self) {
        self.z = [0.0; 2];
    }

    /// Takes `other`'s coefficients, keeping the filter's memory.
    pub fn retune(&mut self, other: &Biquad) {
        self.b = other.b;
        self.a = other.a;
    }
}

/// The detector's low- and high-pass filters (ReaComp and ReaGate), one pair per channel;
/// a filter at its end of the range (20 kHz, 0 Hz) is off.
#[derive(Clone)]
pub(crate) struct DetectorFilter {
    stages: Vec<[Biquad; 2]>,
}

impl DetectorFilter {
    pub fn new(lowpass: f32, hipass: f32, sample_rate: u32) -> Self {
        let (lp, hp) = (lowpass as f64 * 20000.0, hipass as f64 * 20000.0);
        let mut stages = Vec::new();
        if lp > 0.0 && lp < 20000.0 {
            stages.push([Biquad::lowpass(lp, 1.0, sample_rate); 2]);
        }
        if hp > 0.0 {
            stages.push([Biquad::highpass(hp, 1.0, sample_rate); 2]);
        }
        Self { stages }
    }

    pub fn run(&mut self, l: f64, r: f64) -> (f64, f64) {
        self.stages.iter_mut().fold((l, r), |(l, r), [fl, fr]| (fl.run(l), fr.run(r)))
    }

    pub fn reset(&mut self) {
        self.stages.iter_mut().flatten().for_each(Biquad::reset);
    }
}

/// Per-channel RMS over a window of frames (the peak of each channel for a window of one),
/// louder channel wins.
pub(crate) struct Rms {
    squares: Vec<[f64; 2]>,
    sum: [f64; 2],
    at: usize,
}

impl Rms {
    pub fn new(frames: usize) -> Self {
        Self { squares: vec![[0.0; 2]; frames.max(1)], sum: [0.0; 2], at: 0 }
    }

    pub fn window(&self) -> usize {
        self.squares.len()
    }

    pub fn run(&mut self, l: f64, r: f64) -> f64 {
        let n = self.squares.len();
        if n == 1 {
            return l.abs().max(r.abs());
        }
        let sq = [l * l, r * r];
        let old = self.squares[self.at];
        for ((sum, new), old) in self.sum.iter_mut().zip(sq).zip(old) {
            *sum += new - old;
        }
        self.squares[self.at] = sq;
        self.at = (self.at + 1) % n;
        if self.at == 0 {
            // Resum now and then so rounding cannot drift.
            self.sum = self.squares.iter().fold([0.0; 2], |s, v| [s[0] + v[0], s[1] + v[1]]);
        }
        (self.sum[0].max(0.0) / n as f64).sqrt().max((self.sum[1].max(0.0) / n as f64).sqrt())
    }

    pub fn reset(&mut self) {
        self.squares.fill([0.0; 2]);
        self.sum = [0.0; 2];
        self.at = 0;
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use crate::fx::{Effect, ProcessContext};

    pub fn chunk(params: &[f32]) -> Vec<u8> {
        super::marked_chunk(params)
    }

    pub fn context() -> ProcessContext<'static> {
        ProcessContext {
            midi: None,
            midi_from: 0,
            sample_rate: 48000,
            position: 0,
            playing: true,
            notes: None,
            live: &[],
            live_channel: None,
            aux: None,
            bpm: 120.0,
        }
    }

    /// DC at 0.1, then 0.5 from 0.3 s to 0.7 s, then 0.1: REAPER's test signal.
    pub fn steps() -> Vec<f32> {
        (0..57600).map(|i| if (14400..33600).contains(&i) { 0.5 } else { 0.1 }).collect()
    }

    /// The gain `fx` applies to `steps()`.
    pub fn run(fx: &mut dyn Effect) -> Vec<f32> {
        let input = steps();
        let (mut l, mut r) = (input.clone(), input.clone());
        fx.process(&mut l, &mut r, &context());
        l.iter().zip(&input).map(|(o, i)| o / i).collect()
    }

    /// Gains REAPER 7.73 rendered at these frames.
    pub fn check(gain: &[f32], expect: &[(usize, f32)]) {
        for &(i, want) in expect {
            assert!((gain[i] - want).abs() < 2e-4, "frame {i}: {} vs {want}", gain[i]);
        }
    }

    #[test]
    fn every_stock_plugin_is_found_by_its_title() {
        for p in super::PLUGINS {
            assert_eq!(super::find(&p.display()).map(|f| f.name), Some(p.name));
        }
        assert!(super::find("VST: ReaCompressor (Cockos)").is_none());
        assert!(super::find("VSTi: ReaSamplOmatic5000 (Cockos)").is_some());
    }
}
