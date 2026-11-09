//! ReaFIR in its EQ mode, from the state it stores:
//! `u32 0x129c, f32 display floor dB, f32 display top dB, u32 FFT size, f32 show analysis,
//! f32 processing mode (≥ 0.5 minimum phase), u32 points, u32 points, (f32 Hz, f32 dB) per
//! point, u32 mode (0 EQ, 1 gate, 2 compressor, 3 subtract, 4 convolve), f32 gate floor,
//! f32 compressor ratio, f32 output gain, f32 analysis floor, f32 legacy modes, u32, u32,
//! u32, f32 graph offset`.
//!
//! The model, measured on REAPER 7.73 impulse responses (`examples/stock_ref.rs`): the curve is
//! drawn in dB over the bins of an FFT of `M = max(8192, N)` points, `N` being the FFT size;
//! each point sits on bin `floor(Hz × M / rate)` and the curve is read `0.5 + M / 2N` bins
//! later. Three or more points are joined by straight lines in bins (linear in Hz); two points
//! by a cubic Bézier in log frequency whose handles are flat and a quarter of the way across;
//! fewer points leave the curve flat at 0 dB. Beyond the outer points the curve stays at their
//! level. The zero-phase response of that curve, cut to its middle `N` samples, is the filter;
//! it delays the audio by `N / 2` frames, reported as latency. Straight segments at FFT sizes
//! 2048 to 8192 follow REAPER within 1e-6; steep many-point curves, the two-point Bézier and
//! other FFT sizes within 3e-4. Minimum-phase processing and the gate, compressor, subtract
//! and convolve modes are refused, so REAPER's own binary runs them when it is installed.

use super::param;
use crate::clap_host::ParamInfo;
use crate::convolve::Convolver;
use crate::fx::{Effect, ProcessContext};

const MAGIC: u32 = 0x129c;

#[derive(Debug, Clone, PartialEq)]
struct State {
    fft: usize,
    minimum_phase: bool,
    points: Vec<(f32, f32)>,
    mode: u32,
    gain: f32,
}

fn word(chunk: &[u8], at: usize) -> Option<u32> {
    chunk.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()))
}

fn float(chunk: &[u8], at: usize) -> Option<f32> {
    word(chunk, at).map(f32::from_bits)
}

impl State {
    fn parse(chunk: &[u8]) -> Option<Self> {
        if word(chunk, 0)? != MAGIC {
            return None;
        }
        let n = word(chunk, 24)? as usize;
        let points =
            (0..n).map(|i| Some((float(chunk, 32 + i * 8)?, float(chunk, 36 + i * 8)?))).collect::<Option<Vec<_>>>()?;
        let tail = 32 + n * 8;
        Some(Self {
            fft: word(chunk, 12)? as usize,
            minimum_phase: float(chunk, 20)? >= 0.5,
            points,
            mode: word(chunk, tail)?,
            gain: float(chunk, tail + 12)?,
        })
    }

    /// Offset of the output gain in the chunk.
    fn gain_at(&self) -> usize {
        32 + self.points.len() * 8 + 12
    }
}

/// x(t) of the Bézier with flat handles a quarter of the way across, solved for t.
fn bezier_y(u: f64) -> f64 {
    let mut t = u;
    for _ in 0..30 {
        let x = 0.75 * t + 0.75 * t * t - 0.5 * t * t * t;
        let dx = 0.75 + 1.5 * t - 1.5 * t * t;
        t = (t - (x - u) / dx).clamp(0.0, 1.0);
    }
    t * t * (3.0 - 2.0 * t)
}

/// The curve in dB at each bin of an `m`-point FFT.
fn curve(points: &[(f32, f32)], m: usize, shift: f64, sample_rate: u32) -> Vec<f64> {
    let mut pts: Vec<(f64, f64)> =
        points.iter().map(|&(hz, db)| ((hz as f64 * m as f64 / sample_rate as f64).floor(), db as f64)).collect();
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    (0..=m / 2)
        .map(|k| {
            let x = k as f64 - shift;
            match pts.as_slice() {
                [] | [_] => 0.0,
                [(b0, y0), (b1, y1)] => {
                    if x <= *b0 {
                        *y0
                    } else if x >= *b1 {
                        *y1
                    } else {
                        let u = (x / b0.max(1e-9)).ln() / (b1 / b0.max(1e-9)).ln();
                        y0 + (y1 - y0) * bezier_y(u.clamp(0.0, 1.0))
                    }
                }
                _ => {
                    let first = pts[0];
                    let last = pts[pts.len() - 1];
                    if x <= first.0 {
                        return first.1;
                    }
                    if x >= last.0 {
                        return last.1;
                    }
                    let i = pts.partition_point(|p| p.0 <= x).max(1);
                    let ((x0, y0), (x1, y1)) = (pts[i - 1], pts[i]);
                    if x1 > x0 { y0 + (y1 - y0) * (x - x0) / (x1 - x0) } else { y1 }
                }
            }
        })
        .collect()
}

/// The `fft`-tap linear-phase filter REAPER builds from `points`.
fn impulse_response(points: &[(f32, f32)], fft: usize, sample_rate: u32) -> Vec<f32> {
    use realfft::RealFftPlanner;
    use realfft::num_complex::Complex64;
    let m = fft.max(8192);
    let shift = 0.5 + m as f64 / (2 * fft) as f64;
    let mut spectrum: Vec<Complex64> =
        curve(points, m, shift, sample_rate).into_iter().map(|db| Complex64::new(10f64.powf(db / 20.0), 0.0)).collect();
    let mut time = vec![0.0f64; m];
    RealFftPlanner::<f64>::new().plan_fft_inverse(m).process(&mut spectrum, &mut time).expect("fft size");
    (0..fft).map(|j| (time[(j + m - fft / 2) % m] / m as f64) as f32).collect()
}

pub struct Fir {
    chunk: Vec<u8>,
    state: State,
    sample_rate: u32,
    filters: [Convolver; 2],
}

impl Fir {
    /// ReaFIR in EQ mode with a linear-phase filter; other modes are not modelled.
    pub fn from_reafir(chunk: &[u8], sample_rate: u32) -> Result<Self, String> {
        let state = State::parse(chunk).ok_or("unreadable ReaFIR settings")?;
        if state.mode != 0 {
            return Err("ReaFIR's gate, compressor, subtract and convolve modes are not emulated".into());
        }
        if state.minimum_phase {
            return Err("ReaFIR's minimum-phase mode is not emulated".into());
        }
        if !state.fft.is_power_of_two() || !(64..=65536).contains(&state.fft) {
            return Err(format!("unsupported ReaFIR FFT size {}", state.fft));
        }
        let ir = impulse_response(&state.points, state.fft, sample_rate);
        let ir: Vec<f32> = ir.iter().map(|h| h * state.gain).collect();
        Ok(Self { chunk: chunk.to_vec(), filters: [Convolver::new(&ir), Convolver::new(&ir)], state, sample_rate })
    }

    fn rebuild(&mut self) {
        let ir = impulse_response(&self.state.points, self.state.fft, self.sample_rate);
        let ir: Vec<f32> = ir.iter().map(|h| h * self.state.gain).collect();
        self.filters = [Convolver::new(&ir), Convolver::new(&ir)];
    }
}

impl Effect for Fir {
    fn params(&mut self) -> Vec<ParamInfo> {
        vec![param(4, "Output Gain", -60.0, 6.0, 0.0, false)]
    }

    fn param(&mut self, id: u32) -> Option<(f64, String)> {
        (id == 4).then(|| super::db_value(self.state.gain as f64))
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if id == 4 {
            self.state.gain = if value <= -59.9 { 0.0 } else { 10f64.powf(value.min(6.0) / 20.0) as f32 };
            self.rebuild();
        }
    }

    fn set_normalized(&mut self, index: usize, v: f64) -> bool {
        if index != 4 {
            return false;
        }
        self.state.gain = (v.clamp(0.0, 1.0) * 2.0) as f32;
        self.rebuild();
        true
    }

    fn latency(&mut self) -> usize {
        self.state.fft / 2
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        let mut chunk = self.chunk.clone();
        let at = self.state.gain_at();
        chunk.get_mut(at..at + 4)?.copy_from_slice(&self.state.gain.to_le_bytes());
        Some(chunk)
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], _ctx: &ProcessContext) {
        let [fl, fr] = &mut self.filters;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            *l = fl.process_sample(*l);
            *r = fr.process_sample(*r);
        }
    }

    fn reset(&mut self) {
        self.filters.iter_mut().for_each(Convolver::reset);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(points: &[(f32, f32)], fft: u32, mode: u32) -> Vec<u8> {
        let mut b = Vec::new();
        for w in [MAGIC, (-90f32).to_bits(), 24f32.to_bits(), fft, 1f32.to_bits(), 0, 0, 0] {
            b.extend(w.to_le_bytes());
        }
        b[24..28].copy_from_slice(&(points.len() as u32).to_le_bytes());
        b[28..32].copy_from_slice(&(points.len() as u32).to_le_bytes());
        for (hz, db) in points {
            b.extend(hz.to_le_bytes());
            b.extend(db.to_le_bytes());
        }
        b.extend(mode.to_le_bytes());
        for v in [0.0f32, 0.008_016_032, 1.0, 3.162_277_8e-5, 0.125] {
            b.extend(v.to_le_bytes());
        }
        for w in [0u32, 0, 0x0200_0002] {
            b.extend(w.to_le_bytes());
        }
        b.extend(1f32.to_le_bytes());
        b
    }

    /// The default state: two points at 0 dB, a flat curve that passes audio unchanged.
    #[test]
    fn a_flat_curve_is_a_delay_of_half_the_fft() {
        let fir = Fir::from_reafir(&chunk(&[(382.45, 0.0), (3202.7, 0.0)], 4096, 0), 48000).unwrap();
        let ir = impulse_response(&fir.state.points, 4096, 48000);
        assert!((ir[2048] - 1.0).abs() < 1e-6);
        assert!(ir.iter().enumerate().all(|(i, h)| i == 2048 || h.abs() < 1e-6));
    }

    /// Taps REAPER 7.73 rendered for points (1000 Hz, 0), (2000, -3), (5000, -12) at 4096.
    #[test]
    fn straight_segments_match_reaper() {
        let ir = impulse_response(&[(1000.0, 0.0), (2000.0, -3.0), (5000.0, -12.0)], 4096, 48000);
        assert!((ir[2048] - 0.331_059_7).abs() < 1e-6);
    }

    #[test]
    fn other_modes_are_refused() {
        assert!(Fir::from_reafir(&chunk(&[], 4096, 1), 48000).is_err());
        assert_eq!(State::parse(&chunk(&[(1.0, 2.0)], 1024, 3)).unwrap().mode, 3);
    }
}
