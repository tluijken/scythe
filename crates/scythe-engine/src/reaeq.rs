//! A parametric EQ that reads the settings ReaEQ stores in a project, so projects using it play
//! with their equalisation. The state layout (bands of type, enable, frequency, gain and
//! bandwidth) is read from the saved chunk; the filters are RBJ-style biquads.

use crate::fx::{Effect, ProcessContext};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BandKind {
    LowShelf,
    HighShelf,
    Peak,
    /// ReaEQ's alternative band (code 2), wider than `Peak` at the same bandwidth.
    PeakWide,
    LowPass,
    HighPass,
    AllPass,
    Notch,
    BandPass,
}

impl BandKind {
    /// Band type codes as stored by ReaEQ, matched against REAPER renders.
    pub fn from_code(code: u32) -> Option<Self> {
        Some(match code {
            0 => Self::LowShelf,
            1 => Self::HighShelf,
            2 => Self::PeakWide,
            3 => Self::LowPass,
            4 => Self::HighPass,
            5 => Self::AllPass,
            6 => Self::Notch,
            7 | 10 => Self::BandPass,
            8 | 9 => Self::Peak,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    pub kind: BandKind,
    pub enabled: bool,
    pub freq: f64,
    /// Linear gain.
    pub gain: f64,
    /// Bandwidth in octaves.
    pub bandwidth: f64,
}

#[derive(Debug, Clone, Copy, Default)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z: [[f64; 2]; 2],
}

impl Biquad {
    fn design(band: &Band, sample_rate: f64) -> Self {
        let f = band.freq.clamp(1.0, sample_rate * 0.49);
        let w0 = std::f64::consts::TAU * f / sample_rate;
        let (sin, cos) = w0.sin_cos();
        let bw = band.bandwidth.clamp(0.01, 8.0);
        // REAPER widens the bandwidth by w0/sin(w0) up to a quarter of the sample rate and
        // holds that factor at pi/2 above it.
        let warp = w0.min(std::f64::consts::FRAC_PI_2);
        let alpha = sin * (std::f64::consts::LN_2 / 2.0 * bw * warp / warp.sin()).sinh();
        let a = band.gain.max(1e-9).sqrt();
        // Measured against REAPER: the alternative band's width grows with its gain (alpha
        // times sqrt(gain)), and shelves use the slope parameter S = 1/bw², capped at 1.2.
        let alpha = if band.kind == BandKind::PeakWide { alpha * a } else { alpha };
        let shelf_alpha = {
            let s = (1.0 / (bw * bw)).min(1.2);
            sin / 2.0 * ((a + 1.0 / a) * (1.0 / s - 1.0) + 2.0).max(0.0).sqrt()
        };
        let (b0, b1, b2, a0, a1, a2) = match band.kind {
            BandKind::Peak | BandKind::PeakWide => {
                (1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a)
            }
            BandKind::LowShelf => {
                let k = 2.0 * a.sqrt() * shelf_alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cos + k),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                    a * ((a + 1.0) - (a - 1.0) * cos - k),
                    (a + 1.0) + (a - 1.0) * cos + k,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                    (a + 1.0) + (a - 1.0) * cos - k,
                )
            }
            BandKind::HighShelf => {
                let k = 2.0 * a.sqrt() * shelf_alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cos + k),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                    a * ((a + 1.0) + (a - 1.0) * cos - k),
                    (a + 1.0) - (a - 1.0) * cos + k,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos),
                    (a + 1.0) - (a - 1.0) * cos - k,
                )
            }
            BandKind::LowPass => {
                ((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
            }
            BandKind::HighPass => {
                ((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
            }
            BandKind::BandPass => (alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            BandKind::Notch => (1.0, -2.0 * cos, 1.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            BandKind::AllPass => (1.0 - alpha, -2.0 * cos, 1.0 + alpha, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
        };
        Self { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0, z: [[0.0; 2]; 2] }
    }

    fn run(&mut self, ch: usize, x: f64) -> f64 {
        let z = &mut self.z[ch];
        let y = self.b0 * x + z[0];
        z[0] = self.b1 * x - self.a1 * y + z[1];
        z[1] = self.b2 * x - self.a2 * y;
        y
    }
}

pub struct ReaEqCompat {
    pub bands: Vec<Band>,
    /// Linear output gain.
    pub output: f64,
    filters: Vec<Biquad>,
    /// The stored chunk and each band's byte offset in it, so edits can be written back.
    chunk: Vec<u8>,
    offsets: Vec<usize>,
    sample_rate: u32,
}

impl ReaEqCompat {
    /// Reads ReaEQ's chunk: `u32 version, u32 band count`, then per band `u32 type, u32 enabled,
    /// f64 frequency, f64 gain, f64 bandwidth, u8 flag`.
    pub fn from_chunk(chunk: &[u8], sample_rate: u32) -> Option<Self> {
        let u32_at = |o: usize| chunk.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
        let f64_at = |o: usize| chunk.get(o..o + 8).map(|b| f64::from_le_bytes(b.try_into().unwrap()));
        let count = u32_at(4)? as usize;
        if count > 64 {
            return None;
        }
        let mut bands = Vec::with_capacity(count);
        let mut offsets = Vec::with_capacity(count);
        for i in 0..count {
            let o = 8 + i * 33;
            let Some(kind) = BandKind::from_code(u32_at(o)?) else { continue };
            offsets.push(o);
            bands.push(Band {
                kind,
                enabled: u32_at(o + 4)? != 0,
                freq: f64_at(o + 8)?,
                gain: f64_at(o + 16)?,
                bandwidth: f64_at(o + 24)?,
            });
        }
        // After the bands: two u32 flags, then the output gain.
        let output = f64_at(8 + count * 33 + 8).filter(|g| g.is_finite() && (0.0..=16.0).contains(g)).unwrap_or(1.0);
        Some(Self { output, chunk: chunk.to_vec(), offsets, ..Self::new(bands, sample_rate) })
    }

    pub fn new(bands: Vec<Band>, sample_rate: u32) -> Self {
        let filters = bands.iter().filter(|b| b.enabled).map(|b| Biquad::design(b, sample_rate as f64)).collect();
        Self { bands, output: 1.0, filters, chunk: Vec::new(), offsets: Vec::new(), sample_rate }
    }

    /// New coefficients for the current band settings. Filter memories carry over when the
    /// set of enabled bands stays the same, so automated parameters do not click.
    fn redesign(&mut self) {
        let sr = self.sample_rate as f64;
        let fresh: Vec<Biquad> = self.bands.iter().filter(|b| b.enabled).map(|b| Biquad::design(b, sr)).collect();
        if fresh.len() == self.filters.len() {
            for (old, new) in self.filters.iter_mut().zip(fresh) {
                *old = Biquad { z: old.z, ..new };
            }
        } else {
            self.filters = fresh;
        }
    }
}

/// The gain in dB `band` applies at `freq`, from the same filter design ReaEQ plays.
pub fn response_db(band: &Band, freq: f64, sample_rate: f64) -> f64 {
    let f = Biquad::design(band, sample_rate);
    let w = std::f64::consts::TAU * freq / sample_rate;
    let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
    let num = (f.b0 + f.b1 * c1 + f.b2 * c2).powi(2) + (f.b1 * s1 + f.b2 * s2).powi(2);
    let den = (1.0 + f.a1 * c1 + f.a2 * c2).powi(2) + (f.a1 * s1 + f.a2 * s2).powi(2);
    10.0 * (num / den.max(1e-30)).max(1e-30).log10()
}

/// Bytes a band takes in ReaEQ's chunk.
const BAND_BYTES: usize = 33;

impl BandKind {
    /// The code ReaEQ stores for this type (the plain band as REAPER's default "Band").
    pub fn code(self) -> u32 {
        match self {
            Self::LowShelf => 0,
            Self::HighShelf => 1,
            Self::PeakWide => 2,
            Self::LowPass => 3,
            Self::HighPass => 4,
            Self::AllPass => 5,
            Self::Notch => 6,
            Self::BandPass => 7,
            Self::Peak => 8,
        }
    }

    /// Whether the type has a gain (shelves and bands); the others only cut or shift phase.
    pub fn has_gain(self) -> bool {
        matches!(self, Self::LowShelf | Self::HighShelf | Self::Peak | Self::PeakWide)
    }

    pub fn name(self) -> &'static str {
        kind_name(self)
    }
}

impl ReaEqCompat {
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate as f64
    }

    /// The whole EQ's gain in dB at `freq`: every enabled band and the output gain.
    pub fn response_db(&self, freq: f64) -> f64 {
        let sr = self.sample_rate as f64;
        self.bands.iter().filter(|b| b.enabled).map(|b| response_db(b, freq, sr)).sum::<f64>()
            + 20.0 * self.output.max(1e-9).log10()
    }

    /// Changes band `i`'s type.
    pub fn set_kind(&mut self, i: usize, kind: BandKind) {
        let Some(b) = self.bands.get_mut(i) else { return };
        if b.kind == kind {
            return;
        }
        b.kind = kind;
        if let Some(&o) = self.offsets.get(i)
            && let Some(bytes) = self.chunk.get_mut(o..o + 4)
        {
            bytes.copy_from_slice(&kind.code().to_le_bytes());
        }
        self.redesign_all();
    }

    /// Adds a band after the others; returns its index.
    pub fn add_band(&mut self, band: Band) -> usize {
        if self.chunk.len() >= 8 {
            let count = self.band_count();
            let at = 8 + count * BAND_BYTES;
            let flag = self.offsets.last().and_then(|&o| self.chunk.get(o + 32).copied()).unwrap_or(1);
            let mut bytes = Vec::with_capacity(BAND_BYTES);
            bytes.extend(band.kind.code().to_le_bytes());
            bytes.extend((band.enabled as u32).to_le_bytes());
            bytes.extend(band.freq.to_le_bytes());
            bytes.extend(band.gain.to_le_bytes());
            bytes.extend(band.bandwidth.to_le_bytes());
            bytes.push(flag);
            if at <= self.chunk.len() {
                self.chunk.splice(at..at, bytes);
                self.chunk[4..8].copy_from_slice(&((count + 1) as u32).to_le_bytes());
                self.offsets.push(at);
            }
        }
        self.bands.push(band);
        self.redesign_all();
        self.bands.len() - 1
    }

    /// Removes band `i`.
    pub fn remove_band(&mut self, i: usize) {
        if i >= self.bands.len() {
            return;
        }
        self.bands.remove(i);
        if i < self.offsets.len() {
            let o = self.offsets.remove(i);
            let count = self.band_count();
            if o + BAND_BYTES <= self.chunk.len() && count > 0 {
                self.chunk.drain(o..o + BAND_BYTES);
                self.chunk[4..8].copy_from_slice(&((count - 1) as u32).to_le_bytes());
                for later in self.offsets.iter_mut().filter(|later| **later > o) {
                    *later -= BAND_BYTES;
                }
            }
        }
        self.redesign_all();
    }

    fn band_count(&self) -> usize {
        self.chunk.get(4..8).map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
    }

    /// New filters from scratch, for changes to the set of bands or their types.
    fn redesign_all(&mut self) {
        let sr = self.sample_rate as f64;
        self.filters = self.bands.iter().filter(|b| b.enabled).map(|b| Biquad::design(b, sr)).collect();
    }
}

fn kind_name(kind: BandKind) -> &'static str {
    match kind {
        BandKind::LowShelf => "low shelf",
        BandKind::HighShelf => "high shelf",
        BandKind::Peak | BandKind::PeakWide => "band",
        BandKind::LowPass => "low pass",
        BandKind::HighPass => "high pass",
        BandKind::AllPass => "all pass",
        BandKind::Notch => "notch",
        BandKind::BandPass => "band pass",
    }
}

impl Effect for ReaEqCompat {
    /// Per band (id = band × 4): enabled, frequency (Hz), gain (dB), bandwidth (octaves).
    fn params(&mut self) -> Vec<crate::clap_host::ParamInfo> {
        let mut out = Vec::new();
        for (i, b) in self.bands.iter().enumerate() {
            let name = |what: &str| format!("{} {} · {what}", i + 1, kind_name(b.kind));
            let id = i as u32 * 4;
            out.push(crate::clap_host::ParamInfo {
                id,
                name: name("on"),
                min: 0.0,
                max: 1.0,
                default: 1.0,
                log: false,
            });
            out.push(crate::clap_host::ParamInfo {
                id: id + 1,
                name: name("Hz"),
                min: 20.0,
                max: 20000.0,
                default: b.freq,
                log: true,
            });
            if !matches!(
                b.kind,
                BandKind::LowPass | BandKind::HighPass | BandKind::AllPass | BandKind::Notch | BandKind::BandPass
            ) {
                out.push(crate::clap_host::ParamInfo {
                    id: id + 2,
                    name: name("dB"),
                    min: -24.0,
                    max: 24.0,
                    default: 0.0,
                    log: false,
                });
            }
            out.push(crate::clap_host::ParamInfo {
                id: id + 3,
                name: name("width"),
                min: 0.05,
                max: 4.0,
                default: b.bandwidth,
                log: true,
            });
        }
        out
    }

    fn param(&mut self, id: u32) -> Option<(f64, String)> {
        let b = self.bands.get((id / 4) as usize)?;
        Some(match id % 4 {
            0 => (b.enabled as u8 as f64, if b.enabled { "on" } else { "off" }.into()),
            1 => (b.freq, format!("{:.0} Hz", b.freq)),
            2 => {
                let db = 20.0 * b.gain.max(1e-9).log10();
                (db, format!("{db:+.1} dB"))
            }
            _ => (b.bandwidth, format!("{:.2} oct", b.bandwidth)),
        })
    }

    fn set_param(&mut self, id: u32, value: f64) {
        let Some(b) = self.bands.get_mut((id / 4) as usize) else { return };
        match id % 4 {
            0 => b.enabled = value >= 0.5,
            1 => b.freq = value.clamp(10.0, 24000.0),
            2 => b.gain = 10f64.powf(value.clamp(-60.0, 36.0) / 20.0),
            _ => b.bandwidth = value.clamp(0.01, 8.0),
        }
        self.redesign();
    }

    fn set_normalized(&mut self, index: usize, v: f64) -> bool {
        use crate::normalized::{eq_freq, eq_gain_db, linear};
        let (band, k) = (index / 3, index % 3);
        if band < self.bands.len() {
            let id = band as u32 * 4;
            match k {
                0 => self.set_param(id + 1, eq_freq(v)),
                1 => self.set_param(id + 2, eq_gain_db(v)),
                _ => self.set_param(id + 3, linear(v, 0.0, 4.0)),
            }
            return true;
        }
        if index == self.bands.len() * 3 {
            self.output = 10f64.powf(eq_gain_db(v) / 20.0);
            return true;
        }
        false
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        if self.chunk.is_empty() {
            return None;
        }
        let mut c = self.chunk.clone();
        for (b, &o) in self.bands.iter().zip(&self.offsets) {
            c.get_mut(o + 4..o + 8)?.copy_from_slice(&(b.enabled as u32).to_le_bytes());
            c.get_mut(o + 8..o + 16)?.copy_from_slice(&b.freq.to_le_bytes());
            c.get_mut(o + 16..o + 24)?.copy_from_slice(&b.gain.to_le_bytes());
            c.get_mut(o + 24..o + 32)?.copy_from_slice(&b.bandwidth.to_le_bytes());
        }
        let count = u32::from_le_bytes(c.get(4..8)?.try_into().ok()?) as usize;
        let at = 8 + count * BAND_BYTES + 8;
        if let Some(stored) = c.get_mut(at..at + 8)
            && Some(f64::from_le_bytes((&*stored).try_into().ok()?))
                .filter(|g| g.is_finite() && (0.0..=16.0).contains(g))
                .unwrap_or(1.0)
                != self.output
        {
            stored.copy_from_slice(&self.output.to_le_bytes());
        }
        Some(c)
    }

    /// Takes new band settings and output gain in place when the bands keep their types.
    fn update_state(&mut self, chunk: &[u8]) -> bool {
        let Some(new) = Self::from_chunk(chunk, self.sample_rate) else { return false };
        let kinds = |eq: &Self| eq.bands.iter().map(|b| b.kind).collect::<Vec<_>>();
        if kinds(&new) != kinds(self) || new.offsets.len() != new.bands.len() {
            return false;
        }
        self.bands = new.bands;
        self.output = new.output;
        self.chunk = new.chunk;
        self.offsets = new.offsets;
        self.redesign();
        true
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], _ctx: &ProcessContext) {
        for f in &mut self.filters {
            for s in left.iter_mut() {
                *s = f.run(0, *s as f64) as f32;
            }
            for s in right.iter_mut() {
                *s = f.run(1, *s as f64) as f32;
            }
        }
        if self.output != 1.0 {
            let g = self.output as f32;
            left.iter_mut().chain(right.iter_mut()).for_each(|s| *s *= g);
        }
    }

    fn reset(&mut self) {
        for f in &mut self.filters {
            f.z = [[0.0; 2]; 2];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(band: Band, freq: f64) -> f64 {
        let sr = 48000.0;
        let mut f = Biquad::design(&band, sr);
        let n = 48000;
        let mut peak: f64 = 0.0;
        for i in 0..n {
            let y = f.run(0, (std::f64::consts::TAU * freq * i as f64 / sr).sin());
            if i > n / 2 {
                peak = peak.max(y.abs());
            }
        }
        peak
    }

    #[test]
    fn peak_band_boosts_its_frequency_only() {
        let band = Band { kind: BandKind::Peak, enabled: true, freq: 1000.0, gain: 4.0, bandwidth: 1.0 };
        assert!((response(band, 1000.0) - 4.0).abs() < 0.05);
        assert!((response(band, 100.0) - 1.0).abs() < 0.05);
    }

    #[test]
    fn shelves_and_passes() {
        let low = Band { kind: BandKind::LowShelf, enabled: true, freq: 200.0, gain: 0.25, bandwidth: 1.0 };
        assert!((response(low, 30.0) - 0.25).abs() < 0.03);
        assert!((response(low, 5000.0) - 1.0).abs() < 0.03);
        let hp = Band { kind: BandKind::HighPass, enabled: true, freq: 1000.0, gain: 1.0, bandwidth: 2.0 };
        assert!(response(hp, 100.0) < 0.05);
    }
    fn eq_chunk(bands: &[(u32, f64, f64, f64)]) -> Vec<u8> {
        let mut c = Vec::new();
        c.extend(10u32.to_le_bytes());
        c.extend((bands.len() as u32).to_le_bytes());
        for &(code, freq, gain, bw) in bands {
            c.extend(code.to_le_bytes());
            c.extend(1u32.to_le_bytes());
            c.extend(freq.to_le_bytes());
            c.extend(gain.to_le_bytes());
            c.extend(bw.to_le_bytes());
            c.push(1);
        }
        c.extend(1u32.to_le_bytes());
        c.extend(0u32.to_le_bytes());
        c.extend(1.0f64.to_le_bytes());
        c
    }

    /// The curve the editor draws is the gain the EQ applies to a steady sine.
    #[test]
    fn drawn_curve_matches_what_plays() {
        let chunk =
            eq_chunk(&[(0, 120.0, 2.0, 1.0), (8, 1000.0, 0.5, 0.7), (4, 40.0, 1.0, 1.5), (1, 8000.0, 1.4, 1.0)]);
        let mut eq = ReaEqCompat::from_chunk(&chunk, 48000).unwrap();
        for hz in [30.0, 100.0, 700.0, 1000.0, 3000.0, 12000.0] {
            eq.reset();
            let n = 48000;
            let sine: Vec<f32> =
                (0..n).map(|i| (std::f64::consts::TAU * hz * i as f64 / 48000.0).sin() as f32).collect();
            let (mut l, mut r) = (sine.clone(), sine);
            eq.process(&mut l, &mut r, &crate::stock::testing::context());
            let tail = &l[n / 2..];
            let peak = (tail.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() * 2.0 / tail.len() as f64).sqrt();
            let measured = 20.0 * peak.log10();
            assert!((measured - eq.response_db(hz)).abs() < 0.05, "{hz} Hz: {measured} vs {}", eq.response_db(hz));
        }
    }

    /// Bands added, retyped and removed in the editor survive the saved state.
    #[test]
    fn band_edits_round_trip_through_the_chunk() {
        let mut eq = ReaEqCompat::from_chunk(&eq_chunk(&[(0, 100.0, 1.0, 1.0), (8, 1000.0, 1.0, 1.0)]), 48000).unwrap();
        let added = eq.add_band(Band { kind: BandKind::Notch, enabled: true, freq: 3000.0, gain: 1.0, bandwidth: 0.5 });
        assert_eq!(added, 2);
        eq.set_kind(0, BandKind::HighPass);
        eq.set_param(4 + 2, 6.0);
        eq.remove_band(1);
        let back = ReaEqCompat::from_chunk(&eq.save_state().unwrap(), 48000).unwrap();
        assert_eq!(back.bands.len(), 2);
        assert_eq!(back.bands[0].kind, BandKind::HighPass);
        assert_eq!(back.bands[1].kind, BandKind::Notch);
        assert_eq!(back.bands[1].freq, 3000.0);
        assert_eq!(back.output, 1.0);
    }
}
