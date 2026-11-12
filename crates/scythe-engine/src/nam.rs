//! Neural Amp Modeler: loads `.nam` WaveNet models and runs them, so tracks that used the NAM
//! plugin play through the same amp models. The network follows the open NAM architecture:
//! stacked layer arrays of dilated convolutions with a residual stream, a conditioning input
//! (the dry signal) mixed into every layer and a summed head.

use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use serde::Deserialize;

use crate::fx::{Effect, ProcessContext};

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

    fn apply_all(self, xs: &mut [f32]) {
        match self {
            Self::Tanh => tanh_all(xs),
            _ => xs.iter_mut().for_each(|x| *x = self.apply(*x)),
        }
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
    /// Per layer: its input, channel major, `backs[i] + ROOM` samples per channel: the
    /// current block starts at `heads[i]`, preceded by `backs[i]` samples of history.
    inbufs: Vec<Vec<f32>>,
    backs: Vec<usize>,
    heads: Vec<usize>,
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
        let inbufs = backs.iter().map(|b| vec![0.0; ch * (b + ROOM)]).collect();
        let heads = backs.clone();
        let activation = Activation::parse(&config.activation)?;
        Ok(Self { activation, config, rechannel, layers, head, inbufs, backs, heads })
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

/// Room per layer buffer for blocks to advance through before the history is moved back to
/// the start: moving it after every block would copy far more than the block itself.
const ROOM: usize = 4 * BLOCK;

impl Dense {
    /// `out[r][t] += Σ_c W[r][c] · x[c][x_off + t] (+ b[r])` for `n` columns of channel-major
    /// buffers with the given row strides.
    fn add_block(&self, x: &[f32], x_stride: usize, x_off: usize, out: &mut [f32], out_stride: usize, n: usize) {
        add_all(&[Source { m: self, x, x_stride, x_off }], out, out_stride, n);
    }
}

/// One product of [`add_all`]: matrix `m` times the columns of `x` from `x_off` on.
struct Source<'a> {
    m: &'a Dense,
    x: &'a [f32],
    x_stride: usize,
    x_off: usize,
}

/// Adds the products of every source (all with the same rows) to `out`, as consecutive
/// [`Dense::add_block`] calls would, the sums staying in registers across the sources.
fn add_all(sources: &[Source], out: &mut [f32], out_stride: usize, n: usize) {
    #[cfg(target_arch = "x86_64")]
    if fma_available() {
        // SAFETY: the CPU supports AVX2 and FMA.
        return unsafe { add_all_fma(sources, out, out_stride, n) };
    }
    add_all_with::<false>(sources, out, out_stride, n);
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn add_all_fma(sources: &[Source], out: &mut [f32], out_stride: usize, n: usize) {
    add_all_with::<true>(sources, out, out_stride, n);
}

#[inline(always)]
fn add_all_with<const FMA: bool>(sources: &[Source], out: &mut [f32], out_stride: usize, n: usize) {
    let Some(rows) = sources.first().map(|s| s.m.rows) else { return };
    let mut r = 0;
    while r < rows {
        r += match rows - r {
            8.. => rows_tile::<8, FMA>(sources, r, out, out_stride, n),
            4.. => rows_tile::<4, FMA>(sources, r, out, out_stride, n),
            2.. => rows_tile::<2, FMA>(sources, r, out, out_stride, n),
            _ => rows_tile::<1, FMA>(sources, r, out, out_stride, n),
        };
    }
}

/// Rows `r0..r0 + R`, eight columns at a time with the sums held in registers rather than
/// written back after every input channel. Returns `R`.
#[inline(always)]
fn rows_tile<const R: usize, const FMA: bool>(
    sources: &[Source],
    r0: usize,
    out: &mut [f32],
    out_stride: usize,
    n: usize,
) -> usize {
    const T: usize = 8;
    let madd = |w: f32, x: f32, a: f32| if FMA { w.mul_add(x, a) } else { a + w * x };
    let full = n / T * T;
    let mut t = 0;
    while t < full {
        let mut acc = [[0f32; T]; R];
        for (i, a) in acc.iter_mut().enumerate() {
            a.copy_from_slice(&out[(r0 + i) * out_stride + t..][..T]);
        }
        for s in sources {
            let cols = s.m.cols;
            let w = &s.m.w[r0 * cols..(r0 + R) * cols];
            if let Some(b) = &s.m.bias {
                for (i, a) in acc.iter_mut().enumerate() {
                    a.iter_mut().for_each(|v| *v += b[r0 + i]);
                }
            }
            for c in 0..cols {
                let xv: &[f32; T] = s.x[c * s.x_stride + s.x_off + t..][..T].try_into().unwrap();
                for (i, a) in acc.iter_mut().enumerate() {
                    let wv = w[i * cols + c];
                    for k in 0..T {
                        a[k] = madd(wv, xv[k], a[k]);
                    }
                }
            }
        }
        for (i, a) in acc.iter().enumerate() {
            out[(r0 + i) * out_stride + t..][..T].copy_from_slice(a);
        }
        t += T;
    }
    for i in 0..R {
        let r = r0 + i;
        let o = &mut out[r * out_stride + full..r * out_stride + n];
        for s in sources {
            if let Some(b) = &s.m.bias {
                o.iter_mut().for_each(|v| *v += b[r]);
            }
            for c in 0..s.m.cols {
                let wv = s.m.w[r * s.m.cols + c];
                let xs = &s.x[c * s.x_stride + s.x_off + full..c * s.x_stride + s.x_off + n];
                for (ov, xv) in o.iter_mut().zip(xs) {
                    *ov = madd(wv, *xv, *ov);
                }
            }
        }
    }
    R
}

#[cfg(target_arch = "x86_64")]
fn fma_available() -> bool {
    std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma")
}

/// `tanh` over a buffer, vectorised.
fn tanh_all(xs: &mut [f32]) {
    #[cfg(target_arch = "x86_64")]
    if fma_available() {
        // SAFETY: the CPU supports AVX2 and FMA.
        return unsafe { tanh_all_fma(xs) };
    }
    xs.iter_mut().for_each(|x| *x = tanh(*x));
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn tanh_all_fma(xs: &mut [f32]) {
    xs.iter_mut().for_each(|x| *x = tanh(*x));
}

/// `tanh` without branches or library calls, so a loop over it vectorises: `(1 - e) / (1 + e)`
/// with `e = exp(-2|x|)` from a range-reduced polynomial. Within 2e-7 of `f32::tanh`.
#[inline(always)]
fn tanh(x: f32) -> f32 {
    // Past 9, tanh is 1 to within half an f32 step.
    let a = x.abs().min(9.0);
    let y = -2.0 * a;
    // Round y / ln 2 to the nearest integer k; adding 1.5 * 2^23 leaves k in the low mantissa bits.
    const SHIFT: f32 = 12_582_912.0;
    let shifted = y * std::f32::consts::LOG2_E + SHIFT;
    let k = shifted - SHIFT;
    let k_bits = shifted.to_bits().wrapping_sub(SHIFT.to_bits());
    let r = (y - k * 0.693_359_4) + k * 2.121_944_4e-4;
    let p = 1.0
        + r * (1.0
            + r * (1.0 / 2.0
                + r * (1.0 / 6.0 + r * (1.0 / 24.0 + r * (1.0 / 120.0 + r * (1.0 / 720.0 + r * (1.0 / 5040.0)))))));
    let e = p * f32::from_bits(k_bits.wrapping_add(127) << 23);
    ((1.0 - e) / (1.0 + e)).copysign(x)
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
        let mut model = Self {
            arrays,
            head_scale,
            input: vec![0.0; widest * BLOCK],
            z: vec![0.0; 2 * widest * BLOCK],
            head_acc: vec![0.0; widest * BLOCK],
            head_out: vec![0.0; widest * BLOCK],
            sample_rate: file.sample_rate,
            loudness: file.metadata.and_then(|m| m.loudness),
        };
        model.reset();
        Ok(model)
    }

    /// Forgets the input heard so far, leaving the state a long silence leaves, as NAM's
    /// prewarm does: a model fed silence answers with its silent output from the first frame,
    /// where empty layer histories would give a burst as long as its receptive field.
    pub fn reset(&mut self) {
        for a in &mut self.arrays {
            for b in &mut a.inbufs {
                b.fill(0.0);
            }
            a.heads.clone_from(&a.backs);
        }
        // Silence keeps every layer's input constant, and a layer settles one frame after the
        // one before it has: a frame per layer, each history then filled with its layer's
        // newest input, gets there without running a receptive field of silence.
        let layers = self.arrays.iter().map(|a| a.layers.len()).sum::<usize>();
        for _ in 0..layers {
            self.process_block(&mut [0.0]);
            for a in &mut self.arrays {
                let ch = a.config.channels;
                for (li, buf) in a.inbufs.iter_mut().enumerate() {
                    let (back, newest) = (a.backs[li], a.heads[li] - 1);
                    let stride = back + ROOM;
                    for c in 0..ch {
                        let settled = buf[c * stride + newest];
                        buf[c * stride..c * stride + back].fill(settled);
                    }
                    a.heads[li] = back;
                }
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
            for c in 0..ch {
                let row = c * BLOCK..c * BLOCK + n;
                if ai == 0 {
                    self.head_acc[row].fill(0.0);
                } else {
                    self.head_acc[row.clone()].copy_from_slice(&self.head_out[row]);
                }
            }
            let k = array.config.kernel_size;
            let layers = array.layers.len();
            for li in 0..layers {
                let back = array.backs[li];
                let stride = back + ROOM;
                let head = array.heads[li];
                if li == 0 {
                    let first = &mut array.inbufs[0];
                    for c in 0..ch {
                        first[c * stride + head..c * stride + head + n].fill(0.0);
                    }
                    array.rechannel.add_block(&self.input, BLOCK, 0, &mut first[head..], stride, n);
                }
                let layer = &array.layers[li];
                let z = &mut self.z[..conv_out * BLOCK];
                for r in 0..conv_out {
                    z[r * BLOCK..r * BLOCK + n].fill(layer.conv_bias[r]);
                }
                let ib = &array.inbufs[li];
                let source = |i: usize| match i.min(k) {
                    tap if tap < k => Source {
                        m: &layer.conv[tap],
                        x: ib,
                        x_stride: stride,
                        x_off: head - layer.dilation * (k - 1 - tap),
                    },
                    _ => Source { m: &layer.mixin, x: cond, x_stride: n, x_off: 0 },
                };
                if k < 8 {
                    let sources: [Source; 8] = std::array::from_fn(source);
                    add_all(&sources[..=k], z, BLOCK, n);
                } else {
                    for i in 0..=k {
                        add_all(&[source(i)], z, BLOCK, n);
                    }
                }
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
                        act.apply_all(top);
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
                    (&mut next[0][..], nb + ROOM, array.heads[li + 1])
                } else {
                    (&mut self.input[..], BLOCK, 0)
                };
                for c in 0..ch {
                    let src = &cur[c * stride + head..c * stride + head + n];
                    dst[c * dst_stride + dst_off..c * dst_stride + dst_off + n].copy_from_slice(src);
                }
                layer.one_by_one.add_block(&self.z, BLOCK, 0, &mut dst[dst_off..], dst_stride, n);
                array.heads[li] = head + n;
                if head + n + BLOCK > stride {
                    // Out of room: the newest `back` samples become the history at the start.
                    let cur = &mut array.inbufs[li];
                    for c in 0..ch {
                        cur.copy_within(c * stride + head + n - back..c * stride + head + n, c * stride);
                    }
                    array.heads[li] = back;
                }
            }
            let hs = array.config.head_size;
            for r in 0..hs {
                self.head_out[r * BLOCK..r * BLOCK + n].fill(0.0);
            }
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
            .then_some(match channels.as_slice() {
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

/// A model running at the project's rate whatever rate it was trained at, as the Neural Amp
/// Modeler LV2 plugin does in REAPER: no rate conversion, so no delay. A 48 kHz model in a
/// 44.1 kHz project shapes the signal as if it were 9 % slower, as there (REAPER 7.73 with the
/// plugin's 0.1.4 at 44.1 kHz renders what running the model straight at 44.1 kHz gives, and
/// the plugin adds no latency). Its output is brought to the plugin's level: a model that
/// states its loudness is turned up or down to [`NORMALIZED_LOUDNESS`].
pub struct NamVoice {
    model: NamModel,
    gain: f32,
}

/// The loudness, in dB, the LV2 plugin brings every model's output to that states one in its
/// metadata (REAPER 7.73 with the plugin 0.1.4: levels within 0.09 dB of REAPER's on models from
/// -8.9 to -39.6 dB); a model without it plays as it is.
pub const NORMALIZED_LOUDNESS: f64 = -18.0;

impl NamVoice {
    pub fn new(model: NamModel) -> Self {
        let gain = model.loudness.map_or(1.0, |l| 10f64.powf((NORMALIZED_LOUDNESS - l) / 20.0) as f32);
        Self { model, gain }
    }

    fn process(&mut self, buf: &mut [f32]) {
        self.model.process(buf);
        if self.gain != 1.0 {
            buf.iter_mut().for_each(|s| *s *= self.gain);
        }
    }

    /// Frames of input after which the voice no longer hears the silence it started from.
    fn warmup(&self) -> usize {
        self.model.receptive_field()
    }

    fn reset(&mut self) {
        self.model.reset();
    }
}

/// Reaches a running [`NamEffect`] from other threads without its lock: new models to switch
/// to and the input and output levels.
pub struct NamControl {
    incoming: Mutex<Option<Box<NamVoice>>>,
    /// Voices the audio thread is done with, dropped here rather than there. Boxed so that
    /// handing one over moves a pointer and frees nothing on the audio thread.
    #[allow(clippy::vec_box)]
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
        fx.voice = Some(Box::new(NamVoice::new(model)));
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
            let warmup = voice.warmup();
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
pub(crate) mod tests {
    use super::*;

    #[test]
    fn fast_tanh_follows_the_library() {
        let mut worst = 0.0f32;
        for i in -2_000_000..=2_000_000 {
            let x = i as f32 * 1e-5;
            let mut v = [x];
            tanh_all(&mut v);
            worst = worst.max((v[0] - x.tanh()).abs());
            assert_eq!(v[0].is_sign_negative(), x.is_sign_negative(), "sign at {x}");
        }
        assert!(worst < 2e-7, "worst error {worst}");
        let mut big = [1e30f32, -1e30, 0.0];
        tanh_all(&mut big);
        assert_eq!(big, [1.0, -1.0, 0.0]);
    }

    #[test]
    fn tiled_dense_matches_the_plain_sum() {
        let (rows, cols, n) = (13, 5, 37);
        let w: Vec<f32> = (0..rows * cols).map(|i| ((i * 37 % 11) as f32 - 5.0) * 0.1).collect();
        let bias: Vec<f32> = (0..rows).map(|i| i as f32 * 0.01).collect();
        let d = Dense { rows, cols, w: w.clone(), bias: Some(bias.clone()) };
        let x: Vec<f32> = (0..cols * 50).map(|i| ((i * 13 % 7) as f32 - 3.0) * 0.2).collect();
        let mut out = vec![0.5f32; rows * 40];
        d.add_block(&x, 50, 3, &mut out, 40, n);
        for r in 0..rows {
            for t in 0..40 {
                let want = if t < n {
                    0.5 + bias[r] + (0..cols).map(|c| w[r * cols + c] * x[c * 50 + 3 + t]).sum::<f32>()
                } else {
                    0.5
                };
                assert!((out[r * 40 + t] - want).abs() < 1e-5, "row {r} col {t}");
            }
        }
    }

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

    /// Two arrays with long dilations and Tanh, run over many blocks: splitting the input
    /// differently (so the history moves back to the start at other moments) changes nothing.
    #[test]
    fn block_size_does_not_change_the_output() {
        let array = |input: usize, ch: usize, head: usize| {
            format!(
                r#"{{"input_size": {input}, "condition_size": 1, "head_size": {head}, "channels": {ch},
                  "kernel_size": 3, "dilations": [1, 64, 700], "activation": "Tanh", "gated": false,
                  "head_bias": true}}"#
            )
        };
        let configs = [(1, 3, 2), (3, 2, 1)];
        let count: usize = configs
            .iter()
            .map(|&(input, ch, head)| {
                let per_layer = ch * ch * 3 + ch + ch + ch * ch + ch;
                ch * input + 3 * per_layer + head * ch + head
            })
            .sum::<usize>()
            + 1;
        let weights: Vec<String> = (0..count).map(|i| format!("{}", ((i * 7919 % 97) as f32 - 48.0) / 97.0)).collect();
        let layers: Vec<String> = configs.iter().map(|&(i, c, h)| array(i, c, h)).collect();
        let json = format!(
            r#"{{"version": "0.5.1", "architecture": "WaveNet", "config": {{"layers": [{}], "head_scale": 0.5}},
               "weights": [{}]}}"#,
            layers.join(","),
            weights.join(",")
        );
        let input: Vec<f32> = (0..9000).map(|i| ((i as f32) * 0.013).sin() * 0.8).collect();
        let mut whole = NamModel::from_json(json.as_bytes()).unwrap();
        let mut one = input.clone();
        whole.process(&mut one);
        let mut split = NamModel::from_json(json.as_bytes()).unwrap();
        let mut parts = input.clone();
        for chunk in parts.chunks_mut(37) {
            split.process(chunk);
        }
        assert_eq!(one, parts);
        assert!(one.iter().any(|v| v.abs() > 1e-3));
    }

    /// A 48 kHz model that passes its input through, in a 44.1 kHz project: it runs at the
    /// A model that passes its input through, stating its loudness or not: one at -24 dB comes
    /// out 6 dB louder, as the LV2 plugin brings it to -18 dB; one without plays as it is.
    #[test]
    fn a_stated_loudness_is_brought_to_minus_18_db() {
        for (loudness, gain) in [(Some(-24.0), 1.995_262f32), (None, 1.0)] {
            let metadata = loudness.map_or(String::new(), |l| format!(r#""metadata": {{"loudness": {l}}},"#));
            let json = format!(
                r#"{{"architecture": "WaveNet", "sample_rate": 48000, {metadata}
                "config": {{"layers": [{{"input_size": 1, "condition_size": 1, "head_size": 1, "channels": 1,
                  "kernel_size": 2, "dilations": [1], "activation": "Hardtanh", "gated": false, "head_bias": true}}],
                  "head_scale": 1.0}},
                "weights": [1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 0.0, 1.0]}}"#
            );
            let mut voice = NamVoice::new(NamModel::from_json(json.as_bytes()).unwrap());
            let mut buf = vec![0.25f32; 64];
            voice.process(&mut buf);
            assert!((buf[63] - 0.25 * gain).abs() < 1e-5, "{loudness:?}: {}", buf[63]);
        }
    }

    /// project's rate, as the LV2 plugin does in REAPER, so the input comes out as it went in,
    /// on the same frame.
    #[test]
    fn a_model_of_another_rate_runs_at_the_projects_without_delay() {
        let json = br#"{"architecture": "WaveNet", "sample_rate": 48000,
            "config": {"layers": [{"input_size": 1, "condition_size": 1, "head_size": 1, "channels": 1,
              "kernel_size": 2, "dilations": [1], "activation": "Hardtanh", "gated": false, "head_bias": true}],
              "head_scale": 1.0},
            "weights": [1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 0.0, 1.0]}"#;
        let model = NamModel::from_json(json).unwrap();
        let rate = 44_100;
        let mut fx = NamEffect::with_model(model, 0.0, 0.0, rate);
        assert_eq!(fx.latency(), 0);
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
        for (t, &y) in out.iter().enumerate().skip(200) {
            assert!((y - sine(t)).abs() < 1e-6, "frame {t}: {y} vs {}", sine(t));
        }
    }

    /// A model that answers silence with silence once its layers have heard it, but whose
    /// second layer starts from an empty history that differs from the silent state: unwarmed,
    /// it gives -0.5 for the first 2000 frames of silence.
    pub(crate) const SETTLING_MODEL: &str = r#"{"architecture": "WaveNet",
        "config": {"layers": [{"input_size": 1, "condition_size": 1, "head_size": 1, "channels": 1,
          "kernel_size": 2, "dilations": [1, 2000], "activation": "Hardtanh", "gated": false, "head_bias": false}],
          "head_scale": 1.0},
        "weights": [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.5, 1.0, -1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0]}"#;

    /// As NAM prewarms a model on load and reset, silence comes out silent from the first
    /// frame, after a reset too.
    #[test]
    fn silence_stays_silent_from_the_first_frame() {
        let rate = 48_000;
        let model = NamModel::from_json(SETTLING_MODEL.as_bytes()).unwrap();
        let mut fx = NamEffect::with_model(model, 0.0, 0.0, rate);
        let ctx = context(rate);
        let peak = |fx: &mut NamEffect| {
            let mut peak = 0.0f32;
            for _ in 0..8 {
                let (mut left, mut right) = (vec![0.0f32; 512], vec![0.0f32; 512]);
                fx.process(&mut left, &mut right, &ctx);
                peak = left.iter().chain(&right).fold(peak, |m, s| m.max(s.abs()));
            }
            peak
        };
        let from_load = peak(&mut fx);
        assert!(from_load < 1e-6, "from load: {from_load}");
        let (mut left, mut right) = (vec![0.3f32; 4096], vec![0.3f32; 4096]);
        fx.process(&mut left, &mut right, &ctx);
        fx.reset();
        let after_reset = peak(&mut fx);
        assert!(after_reset < 1e-6, "after a reset: {after_reset}");
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
                control.load(NamVoice::new(scaled(-0.5)));
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
