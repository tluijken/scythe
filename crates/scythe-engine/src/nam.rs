//! Neural Amp Modeler: loads `.nam` WaveNet models and runs them, so tracks that used the NAM
//! plugin play through the same amp models. The network follows the open NAM architecture:
//! stacked layer arrays of dilated convolutions with a residual stream, a conditioning input
//! (the dry signal) mixed into every layer and a summed head.

use std::path::Path;

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

/// The NAM plugin as stored in a project: a model plus input and output levels in dB.
pub struct NamEffect {
    model: NamModel,
    input_gain: f32,
    output_gain: f32,
}

impl NamEffect {
    pub fn new(model_path: &Path, input_db: f64, output_db: f64) -> Result<Self, NamError> {
        let model = NamModel::load(model_path)?;
        let db = |v: f64| 10f64.powf(v / 20.0) as f32;
        Ok(Self { model, input_gain: db(input_db), output_gain: db(output_db) })
    }
}

impl Effect for NamEffect {
    fn process(&mut self, left: &mut [f32], right: &mut [f32], _ctx: &ProcessContext) {
        // The plugin runs mono on the left input and feeds both outputs.
        left.iter_mut().for_each(|s| *s *= self.input_gain);
        self.model.process(left);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            *l *= self.output_gain;
            *r = *l;
        }
    }

    fn reset(&mut self) {
        self.model.reset();
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

    #[test]
    fn rejects_wrong_weight_counts() {
        let json = br#"{"architecture": "WaveNet", "config": {"layers": [{"input_size": 1, "condition_size": 1,
            "head_size": 1, "channels": 1, "kernel_size": 2, "dilations": [1], "activation": "Tanh", "gated": false,
            "head_bias": false}]}, "weights": [1.0, 2.0]}"#;
        assert!(matches!(NamModel::from_json(json), Err(NamError::WeightCount { .. })));
    }
}
