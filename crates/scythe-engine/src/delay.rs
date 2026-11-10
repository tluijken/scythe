//! Delay that reads the settings ReaDelay stores. Its tap layout and scalings were matched
//! against REAPER renders (delay time and musical length, feedback, pan); its filters are close
//! but not identical.

use scythe_core::units::PanLaw;

use crate::fx::{Effect, ProcessContext};

/// Longest delay line, in seconds.
const MAX_DELAY: f64 = 12.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tap {
    pub enabled: bool,
    pub time_ms: f64,
    /// In eighth notes.
    pub musical: f64,
    pub feedback: f64,
    pub lowpass: f64,
    pub hipass: f64,
    pub width: f64,
    pub volume: f64,
    /// -1..1.
    pub pan: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReaDelaySettings {
    pub wet: f64,
    pub dry: f64,
    pub taps: Vec<Tap>,
}

impl ReaDelaySettings {
    /// `u32 ?, u32 ?, u32 tap count, u32 tap size (44), u32 ?, u32 ?, f32 wet, f32 dry`, then
    /// per tap 11 f32: unused, enabled, time (×10 s), musical (×256 eighths), feedback,
    /// lowpass (×20 kHz), hipass (×20 kHz), resolution, width (0..1 → -1..1), volume, pan.
    pub fn parse(chunk: &[u8]) -> Option<Self> {
        let u = |i: usize| chunk.get(i * 4..i * 4 + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
        let f = |i: usize| chunk.get(i * 4..i * 4 + 4).map(|b| f32::from_le_bytes(b.try_into().unwrap()) as f64);
        let count = u(2)? as usize;
        let size = u(3)? as usize / 4;
        if size < 11 || count > 64 {
            return None;
        }
        let taps = (0..count)
            .map(|t| {
                let at = 8 + t * size;
                Some(Tap {
                    enabled: f(at + 1)? >= 0.5,
                    time_ms: f(at + 2)? * 10_000.0,
                    musical: f(at + 3)? * 256.0,
                    feedback: f(at + 4)?,
                    lowpass: f(at + 5)? * 20_000.0,
                    hipass: f(at + 6)? * 20_000.0,
                    width: f(at + 8)? * 2.0 - 1.0,
                    volume: f(at + 9)?,
                    pan: f(at + 10)? * 2.0 - 1.0,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self { wet: f(6)?, dry: f(7)?, taps })
    }
}

/// Butterworth (Q = 1/√2) section; `None` when the cutoff makes it transparent.
#[derive(Debug, Clone, Copy)]
struct Filter {
    b: [f64; 3],
    a: [f64; 2],
    z: [[f64; 2]; 2],
}

impl Filter {
    fn new(cutoff: f64, sample_rate: f64, high: bool) -> Option<Self> {
        // The slider ends (0 Hz high-pass, 20 kHz low-pass) switch the filter off.
        if (high && cutoff < 1.0) || (!high && (cutoff >= 20_000.0 || cutoff >= sample_rate * 0.49)) {
            return None;
        }
        let w0 = std::f64::consts::TAU * cutoff.max(1.0) / sample_rate;
        let (sin, cos) = w0.sin_cos();
        // A Q of 1, as fitted to ReaDelay's impulse responses.
        let alpha = sin / 2.0;
        let a0 = 1.0 + alpha;
        let b = if high {
            [(1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0]
        } else {
            [(1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0]
        };
        Some(Self { b: b.map(|v| v / a0), a: [-2.0 * cos / a0, (1.0 - alpha) / a0], z: [[0.0; 2]; 2] })
    }

    fn run(&mut self, ch: usize, x: f64) -> f64 {
        let z = &mut self.z[ch];
        let y = self.b[0] * x + z[0];
        z[0] = self.b[1] * x - self.a[0] * y + z[1];
        z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

struct TapState {
    tap: Tap,
    gain: (f64, f64),
    filters: Vec<Filter>,
}

pub struct ReaDelayCompat {
    settings: ReaDelaySettings,
    /// The stored chunk, patched with edited values on save.
    pub chunk: Vec<u8>,
    taps: Vec<TapState>,
    /// Per tap and channel: input plus feedback.
    lines: Vec<[Vec<f32>; 2]>,
    pos: usize,
    sample_rate: f64,
    /// Which stored tap each tap came from (`None` for taps added since), so a save keeps
    /// the words of each.
    sources: Vec<Option<usize>>,
}

impl ReaDelayCompat {
    pub fn new(settings: ReaDelaySettings, sample_rate: u32) -> Self {
        let sr = sample_rate as f64;
        let len = (MAX_DELAY * sr) as usize;
        let taps: Vec<TapState> = settings.taps.iter().map(|&tap| TapState::new(tap, sr)).collect();
        let lines = taps.iter().map(|_| [vec![0.0; len], vec![0.0; len]]).collect();
        let sources = (0..taps.len()).map(Some).collect();
        Self { settings, taps, lines, pos: 0, chunk: Vec::new(), sample_rate: sr, sources }
    }

    pub fn taps(&self) -> Vec<Tap> {
        self.taps.iter().map(|t| t.tap).collect()
    }

    pub fn wet_dry(&self) -> (f64, f64) {
        (self.settings.wet, self.settings.dry)
    }

    /// Replaces tap `i`'s settings.
    pub fn set_tap(&mut self, i: usize, tap: Tap) {
        if let Some(t) = self.taps.get_mut(i) {
            let filters_same = (t.tap.lowpass, t.tap.hipass) == (tap.lowpass, tap.hipass);
            let fresh = TapState::new(tap, self.sample_rate);
            if filters_same {
                t.tap = fresh.tap;
                t.gain = fresh.gain;
            } else {
                *t = fresh;
            }
        }
    }

    /// Adds a tap after the others; returns its index.
    pub fn add_tap(&mut self, tap: Tap) -> usize {
        if self.taps.len() >= 64 {
            return self.taps.len() - 1;
        }
        let len = (MAX_DELAY * self.sample_rate) as usize;
        self.taps.push(TapState::new(tap, self.sample_rate));
        self.lines.push([vec![0.0; len], vec![0.0; len]]);
        self.sources.push(None);
        self.taps.len() - 1
    }

    /// Removes tap `i` (the last one stays).
    pub fn remove_tap(&mut self, i: usize) {
        if i < self.taps.len() && self.taps.len() > 1 {
            self.taps.remove(i);
            self.lines.remove(i);
            self.sources.remove(i);
        }
    }
}

impl TapState {
    fn new(tap: Tap, sr: f64) -> Self {
        let (l, r) = PanLaw::default().gains(tap.pan);
        let filters =
            [Filter::new(tap.lowpass, sr, false), Filter::new(tap.hipass, sr, true)].into_iter().flatten().collect();
        TapState { tap, gain: (l * tap.volume, r * tap.volume), filters }
    }
}

impl Effect for ReaDelayCompat {
    fn params(&mut self) -> Vec<crate::clap_host::ParamInfo> {
        use crate::stock::param;
        vec![
            param(0, "Wet (dB)", -60.0, 6.0, -6.0, false),
            param(1, "Dry (dB)", -60.0, 6.0, 0.0, false),
            param(2, "Time (ms)", 0.0, 4000.0, 0.0, false),
            param(3, "Length (eighths)", 0.0, 32.0, 2.0, false),
            param(4, "Feedback (dB)", -60.0, 0.0, -60.0, false),
        ]
    }

    fn param(&mut self, id: u32) -> Option<(f64, String)> {
        let tap = self.taps.first().map(|t| t.tap);
        Some(match id {
            0 => crate::stock::db_value(self.settings.wet),
            1 => crate::stock::db_value(self.settings.dry),
            2 => crate::stock::ms_value(tap?.time_ms),
            3 => (tap?.musical, format!("{:.2}", tap?.musical)),
            4 => crate::stock::db_value(tap?.feedback),
            _ => return None,
        })
    }

    fn set_param(&mut self, id: u32, value: f64) {
        let gain = |db: f64| if db <= -59.9 { 0.0 } else { 10f64.powf(db / 20.0) };
        match id {
            0 => self.settings.wet = gain(value),
            1 => self.settings.dry = gain(value),
            _ => {
                let Some(t) = self.taps.first_mut() else { return };
                match id {
                    2 => t.tap.time_ms = value.max(0.0),
                    3 => t.tap.musical = value.max(0.0),
                    4 => t.tap.feedback = gain(value).min(1.0),
                    _ => {}
                }
            }
        }
    }

    fn set_normalized(&mut self, index: usize, v: f64) -> bool {
        use crate::normalized::{amp2_db, linear};
        match index {
            0 | 1 => self.set_param(index as u32, amp2_db(v)),
            3 => self.set_param(2, linear(v, 0.0, 10000.0)),
            4 => self.set_param(3, linear(v, 0.0, 256.0)),
            5 => self.set_param(4, amp2_db(v)),
            _ => return false,
        }
        true
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        let old = ReaDelaySettings::parse(&self.chunk)?;
        let word = |i: usize| self.chunk.get(i * 4..i * 4 + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
        let size = word(3)? as usize / 4;
        let mut c = self.chunk.clone();
        let unchanged =
            old.taps.len() == self.taps.len() && self.sources.iter().enumerate().all(|(k, s)| *s == Some(k));
        if !unchanged {
            // Taps added or removed: each keeps its stored words, new ones start from the first.
            let tap_words = |k: usize| c.get((8 + k * size) * 4..(8 + (k + 1) * size) * 4).map(<[u8]>::to_vec);
            let template = tap_words(0).unwrap_or_else(|| vec![0; size * 4]);
            let mut rebuilt = c[..32].to_vec();
            rebuilt[8..12].copy_from_slice(&(self.taps.len() as u32).to_le_bytes());
            for k in 0..self.taps.len() {
                let kept = self.sources.get(k).copied().flatten().and_then(tap_words);
                rebuilt.extend(kept.unwrap_or_else(|| template.clone()));
            }
            rebuilt.extend(c.get((8 + old.taps.len() * size) * 4..).unwrap_or_default());
            c = rebuilt;
        }
        let mut put = |i: usize, v: f64| -> Option<()> {
            c.get_mut(i * 4..i * 4 + 4)?.copy_from_slice(&(v as f32).to_le_bytes());
            Some(())
        };
        put(6, self.settings.wet)?;
        put(7, self.settings.dry)?;
        if let Some(t) = self.taps.first() {
            put(10, t.tap.time_ms / 10_000.0)?;
            put(11, t.tap.musical / 256.0)?;
            put(12, t.tap.feedback)?;
        }
        for (k, state) in self.taps.iter().enumerate() {
            let t = state.tap;
            let stored = self.sources.get(k).copied().flatten().and_then(|s| old.taps.get(s)).copied();
            let at = 8 + k * size;
            let fields = [
                (1, t.enabled as u8 as f64, stored.map(|s| s.enabled as u8 as f64)),
                (2, t.time_ms / 10_000.0, stored.map(|s| s.time_ms / 10_000.0)),
                (3, t.musical / 256.0, stored.map(|s| s.musical / 256.0)),
                (4, t.feedback, stored.map(|s| s.feedback)),
                (5, t.lowpass / 20_000.0, stored.map(|s| s.lowpass / 20_000.0)),
                (6, t.hipass / 20_000.0, stored.map(|s| s.hipass / 20_000.0)),
                (8, (t.width + 1.0) / 2.0, stored.map(|s| (s.width + 1.0) / 2.0)),
                (9, t.volume, stored.map(|s| s.volume)),
                (10, (t.pan + 1.0) / 2.0, stored.map(|s| (s.pan + 1.0) / 2.0)),
            ];
            for (field, value, was) in fields {
                if was != Some(value) {
                    put(at + field, value)?;
                }
            }
        }
        Some(c)
    }

    /// `_Wet`, `_Dry` and per tap `_<n>__Enabled`, `_<n>__Volume`, `_<n>__Feedback`.
    fn set_named_param(&mut self, name: &str, value: f64) -> bool {
        match name {
            "_Wet" => self.settings.wet = value,
            "_Dry" => self.settings.dry = value,
            _ => {
                let Some((tap, field)) = name.strip_prefix('_').and_then(|n| n.split_once("__")) else { return false };
                let Some(state) = tap.parse::<usize>().ok().and_then(|n| self.taps.get_mut(n.wrapping_sub(1))) else {
                    return false;
                };
                match field {
                    "Enabled" => state.tap.enabled = value >= 0.5,
                    "Feedback" => state.tap.feedback = value,
                    "Volume" => {
                        let (l, r) = PanLaw::default().gains(state.tap.pan);
                        state.tap.volume = value;
                        state.gain = (l * value, r * value);
                    }
                    _ => return false,
                }
            }
        }
        true
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], ctx: &ProcessContext) {
        let sr = ctx.sample_rate as f64;
        let len = self.lines.first().map_or(1, |l| l[0].len());
        let (wet, dry) = (self.settings.wet as f32, self.settings.dry as f32);
        let mut delays = [1usize; 64];
        for (d, t) in delays.iter_mut().zip(&self.taps) {
            let secs = t.tap.time_ms / 1000.0 + t.tap.musical * 30.0 / ctx.bpm.max(1.0);
            *d = ((secs * sr).round() as usize).clamp(1, len - 1);
        }
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (x_l, x_r) = (*l, *r);
            let (mut out_l, mut out_r) = (0.0f32, 0.0f32);
            for ((state, line), &d) in self.taps.iter_mut().zip(&mut self.lines).zip(&delays) {
                if !state.tap.enabled {
                    continue;
                }
                let read = (self.pos + len - d) % len;
                let mut ys = [line[0][read] as f64, line[1][read] as f64];
                for f in &mut state.filters {
                    ys = [f.run(0, ys[0]), f.run(1, ys[1])];
                }
                // Width scales the side signal of the echo.
                let (mid, side) = ((ys[0] + ys[1]) / 2.0, (ys[0] - ys[1]) / 2.0 * state.tap.width);
                let (el, er) = (mid + side, mid - side);
                out_l += (el * state.gain.0) as f32;
                out_r += (er * state.gain.1) as f32;
                let fb = state.tap.feedback;
                line[0][self.pos] = x_l + (ys[0] * fb) as f32;
                line[1][self.pos] = x_r + (ys[1] * fb) as f32;
            }
            self.pos = (self.pos + 1) % len;
            *l = x_l * dry + out_l * wet;
            *r = x_r * dry + out_r * wet;
        }
    }

    fn reset(&mut self) {
        for line in &mut self.lines {
            line[0].fill(0.0);
            line[1].fill(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(wet: f32, dry: f32, tap: [f32; 10]) -> Vec<u8> {
        let mut d = Vec::new();
        for v in [0u32, 0, 1, 44, 2, 0] {
            d.extend(v.to_le_bytes());
        }
        d.extend(wet.to_le_bytes());
        d.extend(dry.to_le_bytes());
        d.extend(0f32.to_le_bytes());
        for v in tap {
            d.extend(v.to_le_bytes());
        }
        d
    }

    fn impulse_response(settings: ReaDelaySettings, bpm: f64, n: usize) -> (Vec<f32>, Vec<f32>) {
        let mut d = ReaDelayCompat::new(settings, 1000);
        let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
        l[0] = 1.0;
        r[0] = 1.0;
        d.process(
            &mut l,
            &mut r,
            &ProcessContext {
                midi: None,
                midi_from: 0,
                sample_rate: 1000,
                position: 0,
                playing: true,
                notes: None,
                live: &[],
                live_channel: None,
                aux: None,
                bpm,
            },
        );
        (l, r)
    }

    /// Echo positions and levels measured from REAPER renders (`examples/readelay_ref.rs`).
    #[test]
    fn matches_reaper_timing_feedback_and_pan() {
        let base = [1.0, 0.0, 2.0 / 256.0, 0.0, 1.0, 0.0, 1.0, 1.0, 1.0, 0.5];
        // Musical length 2 = one beat at 120 bpm.
        let (l, _) = impulse_response(ReaDelaySettings::parse(&chunk(0.5, 1.0, base)).unwrap(), 120.0, 1000);
        assert_eq!((l[0], l[500]), (1.0, 0.5));
        let mut fb = base;
        fb[1] = 0.025;
        fb[2] = 0.0;
        fb[3] = 0.5;
        let (l, _) = impulse_response(ReaDelaySettings::parse(&chunk(1.0, 0.0, fb)).unwrap(), 120.0, 1000);
        assert_eq!((l[250], l[500], l[750]), (1.0, 0.5, 0.25));
        let mut pan = fb;
        pan[3] = 0.0;
        pan[9] = 0.0;
        let (l, r) = impulse_response(ReaDelaySettings::parse(&chunk(1.0, 0.0, pan)).unwrap(), 120.0, 400);
        assert_eq!((l[250], r[250]), (1.0, 0.0));
    }
    /// The tap list the editor changes is what a save stores, and an untouched delay saves
    /// its chunk unchanged.
    #[test]
    fn tap_edits_round_trip_through_the_chunk() {
        let c = chunk(0.5, 1.0, [1.0, 0.25, 0.0, 0.5, 1.0, 0.0, 0.5, 1.0, 1.0, 0.5]);
        let mut d = ReaDelayCompat::new(ReaDelaySettings::parse(&c).unwrap(), 48000);
        d.chunk = c.clone();
        assert_eq!(d.save_state().unwrap(), c);
        let mut second = d.taps()[0];
        (second.time_ms, second.pan, second.lowpass) = (750.0, -0.5, 5000.0);
        assert_eq!(d.add_tap(second), 1);
        let mut first = d.taps()[0];
        first.volume = 0.5;
        d.set_tap(0, first);
        let back = ReaDelaySettings::parse(&d.save_state().unwrap()).unwrap();
        assert_eq!(back.taps.len(), 2);
        assert_eq!(back.taps[0].volume, 0.5);
        assert!((back.taps[1].time_ms - 750.0).abs() < 1e-3);
        assert!((back.taps[1].pan + 0.5).abs() < 1e-6);
        assert!((back.taps[1].lowpass - 5000.0).abs() < 1e-2);
        d.remove_tap(0);
        let back = ReaDelaySettings::parse(&d.save_state().unwrap()).unwrap();
        assert_eq!(back.taps.len(), 1);
        assert!((back.taps[0].time_ms - 750.0).abs() < 1e-3);
    }
}
