//! Zero-latency convolution: the first partition of the impulse response runs directly in the
//! time domain, the rest as a uniformly partitioned overlap-save convolution whose result for a
//! block only needs input blocks that are already complete.

use std::sync::Arc;

use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

const P: usize = 128;

pub struct Convolver {
    head: Vec<f32>,
    /// Spectra of the impulse response partitions after the first.
    parts: Vec<Vec<Complex32>>,
    /// Spectra of past input blocks, newest first (frequency-domain delay line).
    history: Vec<Vec<Complex32>>,
    /// The last 2P input samples, for the overlap-save transform and the direct part.
    input: Vec<f32>,
    /// Output of the FFT partitions for the current block.
    tail_out: Vec<f32>,
    pos: usize,
    fft: Arc<dyn RealToComplex<f32>>,
    ifft: Arc<dyn ComplexToReal<f32>>,
    time: Vec<f32>,
    spec: Vec<Complex32>,
    acc: Vec<Complex32>,
}

impl Convolver {
    pub fn new(ir: &[f32]) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(2 * P);
        let ifft = planner.plan_fft_inverse(2 * P);
        let head = ir[..ir.len().min(P)].to_vec();
        let parts: Vec<Vec<Complex32>> = ir
            .chunks(P)
            .skip(1)
            .map(|chunk| {
                let mut buf = vec![0.0f32; 2 * P];
                buf[..chunk.len()].copy_from_slice(chunk);
                let mut spec = fft.make_output_vec();
                fft.process(&mut buf, &mut spec).expect("fft size");
                spec
            })
            .collect();
        let bins = P + 1;
        Self {
            head,
            history: vec![vec![Complex32::default(); bins]; parts.len()],
            parts,
            input: vec![0.0; 2 * P],
            tail_out: vec![0.0; P],
            pos: 0,
            time: vec![0.0; 2 * P],
            spec: vec![Complex32::default(); bins],
            acc: vec![Complex32::default(); bins],
            fft,
            ifft,
        }
    }

    pub fn reset(&mut self) {
        self.input.fill(0.0);
        self.tail_out.fill(0.0);
        for h in &mut self.history {
            h.fill(Complex32::default());
        }
        self.pos = 0;
    }

    /// Starts a block: transforms the block that just completed and sums the partitions.
    fn next_block(&mut self) {
        if self.parts.is_empty() {
            return;
        }
        self.time.copy_from_slice(&self.input);
        self.fft.process(&mut self.time, &mut self.spec).expect("fft size");
        self.history.rotate_right(1);
        self.history[0].copy_from_slice(&self.spec);
        self.acc.fill(Complex32::default());
        for (h, x) in self.parts.iter().zip(&self.history) {
            for ((a, h), x) in self.acc.iter_mut().zip(h).zip(x) {
                *a += h * x;
            }
        }
        self.acc[0].im = 0.0;
        self.acc[P].im = 0.0;
        self.ifft.process(&mut self.acc, &mut self.time).expect("fft size");
        let scale = 1.0 / (2 * P) as f32;
        for (o, t) in self.tail_out.iter_mut().zip(&self.time[P..]) {
            *o = t * scale;
        }
    }

    pub fn process_sample(&mut self, x: f32) -> f32 {
        if self.pos == P {
            self.next_block();
            self.input.copy_within(P.., 0);
            self.pos = 0;
        }
        self.input[P + self.pos] = x;
        let newest = P + self.pos;
        let mut y = self.tail_out[self.pos];
        for (j, h) in self.head.iter().enumerate() {
            y += h * self.input[newest - j];
        }
        self.pos += 1;
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn direct(ir: &[f32], x: &[f32]) -> Vec<f32> {
        (0..x.len()).map(|n| (0..ir.len()).filter(|&k| k <= n).map(|k| ir[k] * x[n - k]).sum()).collect()
    }

    #[test]
    fn matches_direct_convolution() {
        let ir: Vec<f32> =
            (0..700).map(|i| ((i * 37 % 101) as f32 / 101.0 - 0.5) * (-(i as f32) / 300.0).exp()).collect();
        let x: Vec<f32> = (0..2000).map(|i| (i * 13 % 29) as f32 / 29.0 - 0.5).collect();
        let expect = direct(&ir, &x);
        let mut c = Convolver::new(&ir);
        let got: Vec<f32> = x.iter().map(|&s| c.process_sample(s)).collect();
        for (i, (a, b)) in got.iter().zip(&expect).enumerate() {
            assert!((a - b).abs() < 1e-4, "sample {i}: {a} vs {b}");
        }
    }

    #[test]
    fn delta_ir_is_a_pure_delay() {
        let mut ir = vec![0.0; 1000];
        ir[700] = 0.5;
        let mut c = Convolver::new(&ir);
        let out: Vec<f32> = (0..1000).map(|i| c.process_sample(if i == 0 { 1.0 } else { 0.0 })).collect();
        assert!((out[700] - 0.5).abs() < 1e-5);
        assert!(out.iter().enumerate().all(|(i, v)| i == 700 || v.abs() < 1e-5));
    }
}
