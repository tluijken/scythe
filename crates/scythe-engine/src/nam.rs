//! Neural Amp Modeler: loads `.nam` WaveNet models and runs them, so tracks that used the NAM
//! plugin play through the same amp models. The network follows the open NAM architecture:
//! stacked layer arrays of dilated convolutions with a residual stream, a conditioning input
//! (the dry signal) mixed into every layer and a summed head.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use serde::Deserialize;

use crate::fx::{Effect, ProcessContext};
use crate::media::StreamResampler;

#[derive(Debug, Deserialize)]
struct ModelFile {
    architecture: String,
    config: Config,
    weights: Vec<f32>,
    #[serde(default)]
    sample_rate: Option<f64>,
    #[serde(default)]
    metadata: Option<Metadata>,
}

#[derive(Debug, Deserialize)]
struct Metadata {
    #[serde(default)]
    loudness: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct Config {
    layers: Vec<ArrayConfig>,
    #[serde(default)]
    head_scale: Option<f32>,
}

#[derive(Debug, Clone, Deserialize)]
struct ArrayConfig {
    input_size: usize,
    condition_size: usize,
    head_size: usize,
    channels: usize,
    kernel_size: usize,
    dilations: Vec<usize>,
    activation: String,
    gated: bool,
    head_bias: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum NamError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("model file: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0} models are not supported (only WaveNet)")]
    Architecture(String),
    #[error("model has {found} weights, its configuration needs {expected}")]
    WeightCount { expected: usize, found: usize },
    #[error("activation {0} is not supported")]
    Activation(String),
}

#[derive(Debug, Clone, Copy)]
enum Activation {
    Tanh,
    Relu,
    Sigmoid,
    Hardtanh,
    LeakyRelu,
}

impl Activation {
    fn parse(name: &str) -> Result<Self, NamError> {
        Ok(match name {
            "Tanh" | "Fasttanh" => Self::Tanh,
            "ReLU" => Self::Relu,
            "Sigmoid" => Self::Sigmoid,
            "Hardtanh" => Self::Hardtanh,
            "LeakyReLU" => Self::LeakyRelu,
            other => return Err(NamError::Activation(other.into())),
        })
    }

    fn apply(self, x: f32) -> f32 {
        match self {
            Self::Tanh => x.tanh(),
            Self::Relu => x.max(0.0),
            Self::Sigmoid => 1.0 / (1.0 + (-x).exp()),
            Self::Hardtanh => x.clamp(-1.0, 1.0),
            Self::LeakyRelu => {
                if x > 0.0 {
                    x
                } else {
                    0.01 * x
                }
            }
        }
    }
}

/// A dense `out × in` matrix, row major, with an optional bias.
#[derive(Debug, Clone)]
struct Dense {
    rows: usize,
    cols: usize,
    w: Vec<f32>,
    bias: Option<Vec<f32>>,
}

impl Dense {
    fn read(rows: usize, cols: usize, bias: bool, weights: &mut impl Iterator<Item = f32>) -> Self {
        let w = weights.by_ref().take(rows * cols).collect();
        let bias = bias.then(|| weights.by_ref().take(rows).collect());
        Self { rows, cols, w, bias }
    }
}

struct Layer {
    dilation: usize,
    /// One matrix per kernel tap; tap `k` reads the input `dilation * (kernel - 1 - k)` samples back.
    conv: Vec<Dense>,
    conv_bias: Vec<f32>,
    mixin: Dense,
    one_by_one: Dense,
}

struct LayerArray {
    config: ArrayConfig,
    activation: Activation,
    rechannel: Dense,
    layers: Vec<Layer>,
    head: Dense,
    /// Per layer: its input, channel major, as `backs[i]` samples of history followed by
    /// the current block.
    inbufs: Vec<Vec<f32>>,
    backs: Vec<usize>,
}

impl LayerArray {
    fn read(config: ArrayConfig, weights: &mut impl Iterator<Item = f32>) -> Result<Self, NamError> {
        let ch = config.channels;
        let conv_out = if config.gated { 2 * ch } else { ch };
        let rechannel = Dense::read(ch, config.input_size, false, weights);
        let mut layers = Vec::new();
        for &dilation in &config.dilations {
            // Stored as out, in, tap; split into one matrix per tap.
            let raw: Vec<f32> = weights.by_ref().take(conv_out * ch * config.kernel_size).collect();
            let conv = (0..config.kernel_size)
                .map(|k| Dense {
                    rows: conv_out,
                    cols: ch,
                    w: (0..conv_out * ch)
                        .map(|rc| raw.get(rc * config.kernel_size + k).copied().unwrap_or(0.0))
                        .collect(),
                    bias: None,
                })
                .collect();
            let conv_bias = weights.by_ref().take(conv_out).collect();
            let mixin = Dense::read(conv_out, config.condition_size, false, weights);
            let one_by_one = Dense::read(ch, ch, true, weights);
            layers.push(Layer { dilation, conv, conv_bias, mixin, one_by_one });
        }
        let head = Dense::read(config.head_size, ch, config.head_bias, weights);
        let backs: Vec<usize> = config.dilations.iter().map(|d| d * (config.kernel_size - 1)).collect();
        let inbufs = backs.iter().map(|b| vec![0.0; ch * (b + BLOCK)]).collect();
        Ok(Self { activation: Activation::parse(&config.activation)?, config, rechannel, layers, head, inbufs, backs })
    }

    fn weight_count(c: &ArrayConfig) -> usize {
        let ch = c.channels;
        let conv_out = if c.gated { 2 * ch } else { ch };
        let per_layer = conv_out * ch * c.kernel_size + conv_out + conv_out * c.condition_size + ch * ch + ch;
        ch * c.input_size + c.dilations.len() * per_layer + c.head_size * ch + if c.head_bias { c.head_size } else { 0 }
    }
}

/// Samples per internal block; longer host blocks are split.
const BLOCK: usize = 512;

impl Dense {
    /// `out[r][t] += Σ_c W[r][c] · x[c][x_off + t] (+ b[r])` for `n` columns of channel-major
    /// buffers with the given row strides.
    fn add_block(&self, x: &[f32], x_stride: usize, x_off: usize, out: &mut [f32], out_stride: usize, n: usize) {
        for r in 0..self.rows {
            let o = &mut out[r * out_stride..r * out_stride + n];
            if let Some(b) = &self.bias {
                o.iter_mut().for_each(|v| *v += b[r]);
            }
            for c in 0..self.cols {
                let w = self.w[r * self.cols + c];
                let xs = &x[c * x_stride + x_off..c * x_stride + x_off + n];
                for (ov, xv) in o.iter_mut().zip(xs) {
                    *ov += w * xv;
                }
            }
        }
    }
}

pub struct NamModel {
    arrays: Vec<LayerArray>,
    head_scale: f32,
    /// Channel-major scratch, `BLOCK` columns per row.
    input: Vec<f32>,
    z: Vec<f32>,
    head_acc: Vec<f32>,
    head_out: Vec<f32>,
    pub sample_rate: Option<f64>,
    pub loudness: Option<f64>,
}

impl NamModel {
    pub fn load(path: &Path) -> Result<Self, NamError> {
        Self::from_json(&std::fs::read(path)?)
    }

    pub fn from_json(json: &[u8]) -> Result<Self, NamError> {
        let file: ModelFile = serde_json::from_slice(json)?;
        if file.architecture != "WaveNet" {
            return Err(NamError::Architecture(file.architecture));
        }
        let expected = file.config.layers.iter().map(LayerArray::weight_count).sum::<usize>() + 1;
        if file.weights.len() != expected {
            return Err(NamError::WeightCount { expected, found: file.weights.len() });
        }
        let mut weights = file.weights.iter().copied();
        let arrays = file
            .config
            .layers
            .iter()
            .cloned()
            .map(|c| LayerArray::read(c, &mut weights))
            .collect::<Result<Vec<_>, _>>()?;
        let head_scale = weights.next().or(file.config.head_scale).unwrap_or(1.0);
        let widest = arrays
            .iter()
            .map(|a| a.config.channels.max(a.config.head_size).max(a.config.input_size))
            .max()
            .unwrap_or(1);
        Ok(Self {
            arrays,
            head_scale,
            input: vec![0.0; widest * BLOCK],
            z: vec![0.0; 2 * widest * BLOCK],
            head_acc: vec![0.0; widest * BLOCK],
            head_out: vec![0.0; widest * BLOCK],
            sample_rate: file.sample_rate,
            loudness: file.metadata.and_then(|m| m.loudness),
        })
    }

    pub fn reset(&mut self) {
        for a in &mut self.arrays {
            for b in &mut a.inbufs {
                b.fill(0.0);
            }
        }
    }

    /// Runs the model over `buf` in place (mono).
    pub fn process(&mut self, buf: &mut [f32]) {
        for chunk in buf.chunks_mut(BLOCK) {
            self.process_block(chunk);
        }
    }

    fn process_block(&mut self, buf: &mut [f32]) {
        let n = buf.len();
        self.input[..n].copy_from_slice(buf);
        let cond: &[f32] = buf;
        for (ai, array) in self.arrays.iter_mut().enumerate() {
            let ch = array.config.channels;
            let conv_out = if array.config.gated { 2 * ch } else { ch };
            // The head accumulator starts from the previous array's head output.
            if ai == 0 {
                self.head_acc[..ch * BLOCK].fill(0.0);
            } else {
                self.head_acc[..ch * BLOCK].copy_from_slice(&self.head_out[..ch * BLOCK]);
            }
            let k = array.config.kernel_size;
            let layers = array.layers.len();
            for li in 0..layers {
                let back = array.backs[li];
                let stride = back + BLOCK;
                if li == 0 {
                    let first = &mut array.inbufs[0];
                    for c in 0..ch {
                        first[c * stride + back..c * stride + back + n].fill(0.0);
                    }
                    array.rechannel.add_block(&self.input, BLOCK, 0, &mut first[back..], stride, n);
                }
                let layer = &array.layers[li];
                let z = &mut self.z[..conv_out * BLOCK];
                for r in 0..conv_out {
                    z[r * BLOCK..r * BLOCK + n].fill(layer.conv_bias[r]);
                }
                let ib = &array.inbufs[li];
                for (tap, m) in layer.conv.iter().enumerate() {
                    let off = back - layer.dilation * (k - 1 - tap);
                    m.add_block(ib, stride, off, z, BLOCK, n);
                }
                layer.mixin.add_block(cond, n, 0, z, BLOCK, n);
                let act = array.activation;
                for c in 0..ch {
                    let (top, rest) = z.split_at_mut((c + 1) * BLOCK);
                    let top = &mut top[c * BLOCK..c * BLOCK + n];
                    if array.config.gated {
                        let gate = &rest[(ch - 1) * BLOCK..(ch - 1) * BLOCK + n];
                        for (t, g) in top.iter_mut().zip(gate) {
                            *t = act.apply(*t) * Activation::Sigmoid.apply(*g);
                        }
                    } else {
                        top.iter_mut().for_each(|t| *t = act.apply(*t));
                    }
                    let h = &mut self.head_acc[c * BLOCK..c * BLOCK + n];
                    for (hv, t) in h.iter_mut().zip(top.iter()) {
                        *hv += t;
                    }
                }
                // Residual: next input = this input + 1x1(z), written to the next layer's buffer
                // or, after the last layer, to the array output.
                let (cur, next) = array.inbufs.split_at_mut(li + 1);
                let cur = &cur[li];
                let (dst, dst_stride, dst_off): (&mut [f32], usize, usize) = if li + 1 < layers {
                    let nb = array.backs[li + 1];
                    (&mut next[0][..], nb + BLOCK, nb)
                } else {
                    (&mut self.input[..], BLOCK, 0)
                };
                for c in 0..ch {
                    let src = &cur[c * stride + back..c * stride + back + n];
                    dst[c * dst_stride + dst_off..c * dst_stride + dst_off + n].copy_from_slice(src);
                }
                layer.one_by_one.add_block(&self.z, BLOCK, 0, &mut dst[dst_off..], dst_stride, n);
                // Keep the newest `back` samples as history for the next block.
                let cur = &mut array.inbufs[li];
                for c in 0..ch {
                    cur.copy_within(c * stride + n..c * stride + n + back, c * stride);
                }
            }
            let hs = array.config.head_size;
            self.head_out[..hs * BLOCK].fill(0.0);
            array.head.add_block(&self.head_acc, BLOCK, 0, &mut self.head_out, BLOCK, n);
        }
        for (o, h) in buf.iter_mut().zip(&self.head_out[..n]) {
            *o = h * self.head_scale;
        }
    }
}

impl NamModel {
    /// Samples of past input the output depends on.
    pub fn receptive_field(&self) -> usize {
        self.arrays.iter().map(|a| a.backs.iter().sum::<usize>()).sum::<usize>() + 1
    }
}

/// What a `.nam` file says about itself: its architecture, rate and the metadata the trainer
/// wrote (name, who modelled it, the gear), read without building the network.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NamInfo {
    pub version: Option<String>,
    pub architecture: String,
    pub sample_rate: Option<f64>,
    pub name: Option<String>,
    pub modeled_by: Option<String>,
    pub gear_type: Option<String>,
    pub gear_make: Option<String>,
    pub gear_model: Option<String>,
    pub tone_type: Option<String>,
    pub loudness: Option<f64>,
    /// The trainer's size preset (standard, lite, feather, nano), from the WaveNet's widths.
    pub size: Option<String>,
}

#[derive(Deserialize)]
struct InfoFile {
    #[serde(default)]
    version: Option<String>,
    architecture: String,
    #[serde(default)]
    sample_rate: Option<f64>,
    #[serde(default)]
    config: Option<serde_json::Value>,
    #[serde(default)]
    metadata: Option<serde_json::Map<String, serde_json::Value>>,
}

impl NamInfo {
    pub fn read(path: &Path) -> Result<Self, NamError> {
        Self::from_json(&std::fs::read(path)?)
    }

    pub fn from_json(json: &[u8]) -> Result<Self, NamError> {
        let file: InfoFile = serde_json::from_slice(json)?;
        let meta = file.metadata.unwrap_or_default();
        let text = |key: &str| match meta.get(key) {
            Some(serde_json::Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_owned()),
            _ => None,
        };
        let channels: Vec<u64> = file
            .config
            .as_ref()
            .and_then(|c| c.get("layers"))
            .and_then(|l| l.as_array())
            .map(|layers| layers.iter().filter_map(|l| l.get("channels")?.as_u64()).collect())
            .unwrap_or_default();
        let size = (file.architecture == "WaveNet")
            .then(|| match channels.as_slice() {
                [16, 8] => "standard",
                [12, 6] => "lite",
                [8, 4] => "feather",
                [4, 2] => "nano",
                _ => "custom",
            })
            .map(str::to_owned);
        Ok(Self {
            version: file.version,
            architecture: file.architecture,
            sample_rate: file.sample_rate,
            name: text("name"),
            modeled_by: text("modeled_by"),
            gear_type: text("gear_type"),
            gear_make: text("gear_make"),
            gear_model: text("gear_model"),
            tone_type: text("tone_type"),
            loudness: meta.get("loudness").and_then(serde_json::Value::as_f64),
            size,
        })
    }
}

/// A model running at the project's rate, through a rate bridge when the model has another.
pub struct NamVoice {
    model: NamModel,
    bridge: Option<RateBridge>,
}

impl NamVoice {
    pub fn new(model: NamModel, sample_rate: u32) -> Self {
        let model_rate = model.sample_rate.map(|r| r.round() as u32).filter(|&r| r > 0 && r != sample_rate);
        let bridge = model_rate.map(|r| RateBridge::new(sample_rate, r));
        Self { model, bridge }
    }

    fn process(&mut self, buf: &mut [f32]) {
        match &mut self.bridge {
            Some(bridge) => bridge.process(&mut self.model, buf),
            None => self.model.process(buf),
        }
    }

    fn latency(&self) -> usize {
        self.bridge.as_ref().map_or(0, |b| b.latency)
    }

    /// Frames of project-rate input after which the voice no longer hears the silence it
    /// started from.
    fn warmup(&self, sample_rate: u32) -> usize {
        let field = self.model.receptive_field() as f64;
        let scale = self.model.sample_rate.filter(|&r| r > 0.0).map_or(1.0, |r| sample_rate as f64 / r);
        (field * scale).ceil() as usize + self.latency()
    }

    fn reset(&mut self) {
        self.model.reset();
        if let Some(bridge) = &mut self.bridge {
            bridge.reset();
        }
    }
}

/// Reaches a running [`NamEffect`] from other threads without its lock: new models to switch
/// to and the input and output levels.
pub struct NamControl {
    incoming: Mutex<Option<Box<NamVoice>>>,
    /// Voices the audio thread is done with, dropped here rather than there.
    retired: Mutex<Vec<Box<NamVoice>>>,
    input_db: AtomicU32,
    output_db: AtomicU32,
}

const RETIRED_CAPACITY: usize = 8;

impl NamControl {
    fn new(input_db: f64, output_db: f64) -> Self {
        Self {
            incoming: Mutex::new(None),
            retired: Mutex::new(Vec::with_capacity(RETIRED_CAPACITY)),
            input_db: AtomicU32::new((input_db as f32).to_bits()),
            output_db: AtomicU32::new((output_db as f32).to_bits()),
        }
    }

    /// Switches to `voice`: it runs alongside the current model until it has heard enough
    /// input, then the two crossfade.
    pub fn load(&self, voice: NamVoice) {
        let replaced = self.incoming.lock().unwrap().replace(Box::new(voice));
        drop(replaced);
        self.collect();
    }

    pub fn set_levels(&self, input_db: f64, output_db: f64) {
        self.input_db.store((input_db as f32).to_bits(), Ordering::Relaxed);
        self.output_db.store((output_db as f32).to_bits(), Ordering::Relaxed);
    }

    pub fn levels(&self) -> (f64, f64) {
        let get = |a: &AtomicU32| f32::from_bits(a.load(Ordering::Relaxed)) as f64;
        (get(&self.input_db), get(&self.output_db))
    }

    /// Drops the voices the audio thread retired.
    pub fn collect(&self) {
        let old: Vec<_> = self.retired.lock().unwrap().drain(..).collect();
        drop(old);
    }

    fn gains(&self) -> (f32, f32) {
        let (i, o) = self.levels();
        (db_to_gain(i), db_to_gain(o))
    }
}

fn db_to_gain(db: f64) -> f32 {
    10f64.powf(db / 20.0) as f32
}

/// How long the old and new model overlap when switching.
const CROSSFADE_SECONDS: f64 = 0.012;

struct Fade {
    /// `None` fades in from the dry signal.
    old: Option<Box<NamVoice>>,
    at: usize,
}

struct Pending {
    voice: Box<NamVoice>,
    warmup: usize,
}

/// The NAM plugin as stored in a project: a model plus input and output levels in dB. A new
/// model swaps in without a click: it first runs silently beside the old one and then the two
/// crossfade.
pub struct NamEffect {
    voice: Option<Box<NamVoice>>,
    next: Option<Pending>,
    fade: Option<Fade>,
    control: Arc<NamControl>,
    input_gain: f32,
    output_gain: f32,
    sample_rate: u32,
    fade_len: usize,
    next_buf: Vec<f32>,
    old_buf: Vec<f32>,
    /// A retired voice the control could not take yet.
    limbo: Option<Box<NamVoice>>,
}

const CHUNK: usize = crate::render::RenderScratch::MAX_BLOCK;

impl NamEffect {
    pub fn new(model_path: &Path, input_db: f64, output_db: f64, sample_rate: u32) -> Result<Self, NamError> {
        let model = NamModel::load(model_path)?;
        Ok(Self::with_model(model, input_db, output_db, sample_rate))
    }

    pub fn with_model(model: NamModel, input_db: f64, output_db: f64, sample_rate: u32) -> Self {
        let mut fx = Self::empty(input_db, output_db, sample_rate);
        fx.voice = Some(Box::new(NamVoice::new(model, sample_rate)));
        fx
    }

    /// A slot without a model, which passes its input through until one is loaded.
    pub fn empty(input_db: f64, output_db: f64, sample_rate: u32) -> Self {
        let control = Arc::new(NamControl::new(input_db, output_db));
        let (input_gain, output_gain) = control.gains();
        Self {
            voice: None,
            next: None,
            fade: None,
            control,
            input_gain,
            output_gain,
            sample_rate,
            fade_len: ((sample_rate as f64 * CROSSFADE_SECONDS).round() as usize).max(1),
            next_buf: vec![0.0; CHUNK],
            old_buf: vec![0.0; CHUNK],
            limbo: None,
        }
    }

    pub fn control(&self) -> Arc<NamControl> {
        self.control.clone()
    }

    fn receive(&mut self) {
        let incoming = match self.control.incoming.try_lock() {
            Ok(mut slot) => slot.take(),
            Err(_) => None,
        };
        if let Some(voice) = incoming {
            let warmup = voice.warmup(self.sample_rate);
            if let Some(stale) = self.next.replace(Pending { voice, warmup }) {
                self.retire(stale.voice);
            }
        }
        if let Some(v) = self.limbo.take() {
            self.retire(v);
        }
    }

    fn retire(&mut self, voice: Box<NamVoice>) {
        match self.control.retired.try_lock() {
            Ok(mut list) if list.len() < list.capacity() => list.push(voice),
            _ if self.limbo.is_none() => self.limbo = Some(voice),
            _ => drop(voice),
        }
    }

    fn process_chunk(&mut self, left: &mut [f32]) {
        let n = left.len();
        let (input_target, output_target) = self.control.gains();
        ramp_gain(left, &mut self.input_gain, input_target);
        if let Some(next) = &mut self.next {
            self.next_buf[..n].copy_from_slice(left);
            next.voice.process(&mut self.next_buf[..n]);
            next.warmup = next.warmup.saturating_sub(n);
        }
        if let Some(fade) = &mut self.fade {
            self.old_buf[..n].copy_from_slice(left);
            if let Some(old) = &mut fade.old {
                old.process(&mut self.old_buf[..n]);
            }
        }
        if let Some(voice) = &mut self.voice {
            voice.process(left);
        }
        if let Some(fade) = &mut self.fade {
            let len = self.fade_len as f32;
            for (k, (s, o)) in left.iter_mut().zip(&self.old_buf[..n]).enumerate() {
                let t = ((fade.at + k) as f32 / len).min(1.0);
                let w = 0.5 - 0.5 * (std::f32::consts::PI * t).cos();
                *s = *o + (*s - *o) * w;
            }
            fade.at += n;
            if fade.at >= self.fade_len
                && let Some(done) = self.fade.take()
                && let Some(old) = done.old
            {
                self.retire(old);
            }
        }
        ramp_gain(left, &mut self.output_gain, output_target);
        if self.fade.is_none() && self.next.as_ref().is_some_and(|p| p.warmup == 0) {
            let next = self.next.take().unwrap();
            let old = self.voice.replace(next.voice);
            self.fade = Some(Fade { old, at: 0 });
        }
    }
}

/// Multiplies by a gain moving linearly from `current` to `target` over the block.
fn ramp_gain(buf: &mut [f32], current: &mut f32, target: f32) {
    if *current == target {
        buf.iter_mut().for_each(|s| *s *= target);
        return;
    }
    let step = (target - *current) / buf.len().max(1) as f32;
    for (k, s) in buf.iter_mut().enumerate() {
        *s *= *current + step * (k + 1) as f32;
    }
    *current = target;
}

/// Runs the model at its own sample rate inside a project at another, like the plugin does,
/// with a fixed delay that covers both converters' lookahead.
struct RateBridge {
    up: StreamResampler,
    down: StreamResampler,
    at_model_rate: Vec<f32>,
    converted: Vec<f32>,
    out: VecDeque<f32>,
    latency: usize,
}

impl RateBridge {
    fn new(project_rate: u32, model_rate: u32) -> Self {
        let up = StreamResampler::new(project_rate, model_rate);
        let down = StreamResampler::new(model_rate, project_rate);
        let ahead = up.lookahead() as f64 + down.lookahead() as f64 * project_rate as f64 / model_rate as f64;
        let latency = ahead.ceil() as usize + 3;
        let mut bridge =
            Self { up, down, at_model_rate: Vec::new(), converted: Vec::new(), out: VecDeque::new(), latency };
        bridge.reset();
        bridge
    }

    fn process(&mut self, model: &mut NamModel, buf: &mut [f32]) {
        self.at_model_rate.clear();
        self.up.push(buf, &mut self.at_model_rate);
        model.process(&mut self.at_model_rate);
        self.converted.clear();
        self.down.push(&self.at_model_rate, &mut self.converted);
        self.out.extend(self.converted.iter().copied());
        for s in buf.iter_mut() {
            *s = self.out.pop_front().unwrap_or(0.0);
        }
    }

    fn reset(&mut self) {
        self.up.reset();
        self.down.reset();
        self.out.clear();
        self.out.extend(std::iter::repeat_n(0.0, self.latency));
    }
}

impl Effect for NamEffect {
    fn process(&mut self, left: &mut [f32], right: &mut [f32], _ctx: &ProcessContext) {
        self.receive();
        // The plugin runs mono on the left input and feeds both outputs.
        for chunk in left.chunks_mut(CHUNK) {
            self.process_chunk(chunk);
        }
        for (l, r) in left.iter().zip(right.iter_mut()) {
            *r = *l;
        }
    }

    fn latency(&mut self) -> usize {
        self.voice.as_ref().map_or(0, |v| v.latency())
    }

    fn reset(&mut self) {
        self.receive();
        if let Some(fade) = self.fade.take()
            && let Some(old) = fade.old
        {
            self.retire(old);
        }
        if let Some(next) = self.next.take()
            && let Some(old) = self.voice.replace(next.voice)
        {
            self.retire(old);
        }
        if let Some(voice) = &mut self.voice {
            voice.reset();
        }
    }

    fn nam_control(&self) -> Option<Arc<NamControl>> {
        Some(self.control())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One layer array, one channel, kernel 2, dilation 2: every term of the forward pass can
    /// be checked by hand.
    #[test]
    fn forward_pass_by_hand() {
        let (a, w0, w1, b, m, u, c, h, hb, s) = (0.5f32, 0.25, 2.0, 0.1, 1.0, 3.0, 0.0, 2.0, 0.5, 0.5);
        let json = format!(
            r#"{{"version": "0.5.1", "architecture": "WaveNet",
               "config": {{"layers": [{{"input_size": 1, "condition_size": 1, "head_size": 1, "channels": 1,
                 "kernel_size": 2, "dilations": [2], "activation": "ReLU", "gated": false, "head_bias": true}}],
                 "head_scale": 1.0}},
               "weights": [{a}, {w0}, {w1}, {b}, {m}, {u}, {c}, {h}, {hb}, {s}]}}"#
        );
        let mut model = NamModel::from_json(json.as_bytes()).unwrap();
        let x = [1.0f32, -2.0, 0.5, 3.0, 0.0];
        let mut out = x;
        model.process(&mut out);
        for t in 0..x.len() {
            let r = |i: usize| a * x[i];
            let back = if t >= 2 { r(t - 2) } else { 0.0 };
            let z = (w0 * back + w1 * r(t) + b + m * x[t]).max(0.0);
            let expect = s * (h * z + hb);
            assert!((out[t] - expect).abs() < 1e-6, "t {t}: {} vs {expect}", out[t]);
        }
    }

    /// A 48 kHz model that passes its input through, in a 44.1 kHz project: the bridge to the
    /// model's rate keeps the signal's level and pitch, delayed by the reported latency.
    #[test]
    fn model_runs_at_its_own_rate() {
        let json = br#"{"architecture": "WaveNet", "sample_rate": 48000,
            "config": {"layers": [{"input_size": 1, "condition_size": 1, "head_size": 1, "channels": 1,
              "kernel_size": 2, "dilations": [1], "activation": "Hardtanh", "gated": false, "head_bias": true}],
              "head_scale": 1.0},
            "weights": [1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 0.0, 1.0]}"#;
        let model = NamModel::from_json(json).unwrap();
        let rate = 44_100;
        let mut fx = NamEffect::with_model(model, 0.0, 0.0, rate);
        let latency = fx.latency();
        assert!(latency > 0);
        let sine = |t: usize| 0.5 * (2.0 * std::f32::consts::PI * 1000.0 * t as f32 / rate as f32).sin();
        let ctx = ProcessContext {
            sample_rate: rate,
            position: 0,
            playing: true,
            notes: None,
            live: &[],
            live_channel: None,
            aux: None,
            bpm: 120.0,
            midi: None,
            midi_from: 0,
        };
        let mut out = Vec::new();
        for block in 0..86 {
            let mut left: Vec<f32> = (block * 512..(block + 1) * 512).map(sine).collect();
            let mut right = vec![0.0; 512];
            fx.process(&mut left, &mut right, &ctx);
            out.extend(left);
        }
        for (t, &y) in out.iter().enumerate().skip(latency + 200) {
            assert!((y - sine(t - latency)).abs() < 1e-3, "frame {t}: {y} vs {}", sine(t - latency));
        }
    }

    #[test]
    fn rejects_wrong_weight_counts() {
        let json = br#"{"architecture": "WaveNet", "config": {"layers": [{"input_size": 1, "condition_size": 1,
            "head_size": 1, "channels": 1, "kernel_size": 2, "dilations": [1], "activation": "Tanh", "gated": false,
            "head_bias": false}]}, "weights": [1.0, 2.0]}"#;
        assert!(matches!(NamModel::from_json(json), Err(NamError::WeightCount { .. })));
    }

    /// A model whose output is its input times `scale` (for inputs within ±1).
    fn scaled(scale: f32) -> NamModel {
        let json = format!(
            r#"{{"architecture": "WaveNet",
            "config": {{"layers": [{{"input_size": 1, "condition_size": 1, "head_size": 1, "channels": 1,
              "kernel_size": 2, "dilations": [4], "activation": "Hardtanh", "gated": false, "head_bias": true}}],
              "head_scale": 1.0}},
            "weights": [1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 0.0, {scale}]}}"#
        );
        NamModel::from_json(json.as_bytes()).unwrap()
    }

    fn context(sample_rate: u32) -> ProcessContext<'static> {
        ProcessContext {
            sample_rate,
            position: 0,
            playing: true,
            notes: None,
            live: &[],
            live_channel: None,
            aux: None,
            bpm: 120.0,
            midi: None,
            midi_from: 0,
        }
    }

    /// Switching from a model that passes the signal through to one that inverts and halves it
    /// changes the output smoothly: no step between neighbouring samples is larger than the
    /// signal's own slope plus the crossfade's, and the new model takes over completely. A
    /// hard switch would step by up to 0.75.
    #[test]
    fn model_switch_crossfades_without_a_step() {
        let rate = 48_000;
        let mut fx = NamEffect::with_model(scaled(1.0), 0.0, 0.0, rate);
        let control = fx.nam_control().unwrap();
        let sine = |t: usize| 0.5 * (2.0 * std::f32::consts::PI * 110.0 * t as f32 / rate as f32).sin();
        let ctx = context(rate);
        let mut out = Vec::new();
        let switch_block = 10;
        for block in 0..60 {
            if block == switch_block {
                control.load(NamVoice::new(scaled(-0.5), rate));
            }
            let mut left: Vec<f32> = (block * 256..(block + 1) * 256).map(sine).collect();
            let mut right = vec![0.0; 256];
            fx.process(&mut left, &mut right, &ctx);
            assert_eq!(left, right);
            out.extend(left);
        }
        let slope = 2.0 * std::f32::consts::PI * 110.0 * 0.5 / rate as f32;
        let fade = (rate as f64 * CROSSFADE_SECONDS) as f32;
        let bound = slope + 0.75 * std::f32::consts::FRAC_PI_2 / fade * 1.05;
        let worst = out.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        assert!(worst < bound, "largest step {worst}, allowed {bound}");
        for (t, &y) in out.iter().enumerate().take(switch_block * 256) {
            assert!((y - sine(t)).abs() < 1e-6, "before the switch, frame {t}");
        }
        for (t, &y) in out.iter().enumerate().skip(out.len() - 2048) {
            assert!((y + 0.5 * sine(t)).abs() < 1e-6, "after the switch, frame {t}");
        }
        assert!(fx.fade.is_none() && fx.next.is_none());
        control.collect();
        assert!(control.retired.lock().unwrap().is_empty());
    }

    #[test]
    fn levels_ramp_instead_of_jumping() {
        let rate = 48_000;
        let mut fx = NamEffect::with_model(scaled(1.0), 0.0, 0.0, rate);
        let control = fx.nam_control().unwrap();
        let ctx = context(rate);
        let mut left = vec![0.25f32; 512];
        let mut right = vec![0.0; 512];
        fx.process(&mut left, &mut right, &ctx);
        control.set_levels(0.0, -6.0);
        let mut left = vec![0.25f32; 512];
        fx.process(&mut left, &mut right, &ctx);
        assert!((left[0] - 0.25).abs() < 1e-3);
        let target = 0.25 * db_to_gain(-6.0);
        assert!((left[511] - target).abs() < 1e-6);
        assert!(left.windows(2).all(|w| w[1] <= w[0]));
        assert_eq!(control.levels(), (0.0, -6.0));
    }

    #[test]
    fn info_reads_trainer_metadata() {
        let json = br#"{"version": "0.5.4", "architecture": "WaveNet", "sample_rate": 48000,
            "config": {"layers": [{"channels": 16}, {"channels": 8}], "head_scale": 0.02},
            "weights": [0.1, 0.2],
            "metadata": {"date": {"year": 2024}, "loudness": -23.5, "gain": 0.0016, "name": "HB-800B",
              "modeled_by": "Patrick Sogl", "gear_type": "amp", "gear_make": "Harley Benton",
              "gear_model": "HB-800B", "tone_type": "clean", "input_level_dbu": null}}"#;
        let info = NamInfo::from_json(json).unwrap();
        assert_eq!(info.name.as_deref(), Some("HB-800B"));
        assert_eq!(info.modeled_by.as_deref(), Some("Patrick Sogl"));
        assert_eq!(info.gear_make.as_deref(), Some("Harley Benton"));
        assert_eq!(info.gear_type.as_deref(), Some("amp"));
        assert_eq!(info.tone_type.as_deref(), Some("clean"));
        assert_eq!(info.sample_rate, Some(48000.0));
        assert_eq!(info.loudness, Some(-23.5));
        assert_eq!(info.size.as_deref(), Some("standard"));
        assert_eq!(info.version.as_deref(), Some("0.5.4"));
        let bare = NamInfo::from_json(br#"{"architecture": "LSTM", "config": {}, "weights": []}"#).unwrap();
        assert_eq!((bare.architecture.as_str(), bare.name, bare.size), ("LSTM", None, None));
    }
}
