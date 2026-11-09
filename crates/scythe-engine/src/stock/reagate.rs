//! ReaGate, from the parameters it stores. Detector, filters, RMS, timing, hysteresis, pre-open,
//! wet/dry and invert follow REAPER 7.73 renders sample for sample (`examples/stock_ref.rs`);
//! the noise is white like REAPER's but from its own generator.

use super::params::{Params, Spec, Unit, spec};
use super::{DetectorFilter, DetectorInput, Lookahead, Rms, marked_params};
use crate::clap_host::ParamInfo;
use crate::fx::{Effect, MidiBlock, ProcessContext};

const SPECS: &[Spec] = &[
    spec(0, "Threshold", Unit::Gain, 2.0),
    spec(1, "Attack (ms)", Unit::Ms(500.0), 1.0),
    spec(2, "Release (ms)", Unit::Ms(5000.0), 1.0),
    spec(3, "Pre-open (ms)", Unit::Ms(250.0), 1.0),
    spec(4, "Hold (ms)", Unit::Ms(1000.0), 1.0),
    spec(5, "Detector lowpass (Hz)", Unit::Hz, 1.0),
    spec(6, "Detector highpass (Hz)", Unit::Hz, 1.0),
    spec(9, "Dry", Unit::Gain, 2.0),
    spec(10, "Wet", Unit::Gain, 2.0),
    spec(11, "Noise", Unit::Gain, 2.0),
    spec(12, "Hysteresis", Unit::Gain, 2.0),
    spec(13, "Preview filter", Unit::Switch, 1.0),
    spec(14, "RMS size (ms)", Unit::Ms(100.0), 10.0),
    spec(15, "Send MIDI", Unit::Switch, 1.0),
    spec(16, "MIDI note", Unit::Steps(127.0), 1.0),
    spec(17, "MIDI channel", Unit::Steps(15.0), 1.0),
    spec(18, "Invert wet", Unit::Steps(2.0), 2.0),
];

/// A fresh ReaGate's parameters as REAPER 7.73 stores them.
const DEFAULTS: [f32; 21] = [
    3.162_277_6e-8,
    0.006,
    0.02,
    0.0,
    0.0,
    1.0,
    0.0,
    0.0,
    0.0,
    3.162_277_6e-8,
    1.0,
    3.162_277_6e-8,
    1.0,
    0.0,
    0.0,
    0.0,
    0.543_307_1,
    0.0,
    0.0,
    0.0,
    0.0,
];

#[derive(Clone, Copy, PartialEq)]
enum Invert {
    Off,
    /// "Legacy mode": the wet path flips polarity.
    Legacy,
    /// The wet path passes what the gate takes away.
    On,
}

pub struct Gate {
    params: Params,
    sample_rate: u32,
    threshold: f64,
    close_threshold: f64,
    /// Frames the gain takes to open or close fully (linear ramps).
    attack: f64,
    release: f64,
    /// Frames below the closing threshold before the gate starts closing.
    close_after: usize,
    dry: f32,
    wet: f32,
    noise: f32,
    invert: Invert,
    preview: bool,
    midi: Option<(u8, u8)>,
    input: DetectorInput,
    filter: DetectorFilter,
    rms: Rms,
    window: usize,
    lookahead: Lookahead,
    gain: f64,
    below: usize,
    open: bool,
    note_on: bool,
    rng: u32,
    /// The MIDI passed on in this block (the chain's and the gate's notes), by frame.
    notes: Vec<(usize, [u8; 3])>,
}

impl Gate {
    /// Parameters 0..=20: threshold, attack, release, pre-open, hold, lowpass, highpass,
    /// detector input, audio input, dry, wet, noise, hysteresis, filter preview, RMS size,
    /// send MIDI, MIDI note, MIDI channel, invert wet, multichannel mode, metering index.
    pub fn from_reagate(chunk: &[u8], sample_rate: u32) -> Option<Self> {
        let params = Params::new(marked_params(chunk)?, SPECS, &DEFAULTS);
        let mut gate = Self {
            params,
            sample_rate,
            threshold: 0.0,
            close_threshold: 0.0,
            attack: 0.0,
            release: 0.0,
            close_after: 0,
            dry: 0.0,
            wet: 1.0,
            noise: 0.0,
            invert: Invert::Off,
            preview: false,
            midi: None,
            input: DetectorInput::new(0.0),
            filter: DetectorFilter::new(1.0, 0.0, sample_rate),
            rms: Rms::new(1),
            window: 1,
            lookahead: Lookahead::new(0),
            gain: 0.0,
            below: 0,
            open: false,
            note_on: false,
            rng: 0x2545_f491,
            notes: Vec::new(),
        };
        gate.configure(true);
        Some(gate)
    }

    fn configure(&mut self, fresh: bool) {
        let p = &self.params;
        let sr = self.sample_rate as f64;
        let frames = |raw: f32, full_ms: f64| raw as f64 * full_ms * 0.001 * sr;
        self.threshold = p.get(0) as f64;
        self.close_threshold = self.threshold * p.get(12) as f64;
        self.attack = frames(p.get(1), 500.0);
        self.release = frames(p.get(2), 5000.0);
        let pre = frames(p.get(3), 250.0) as usize;
        self.close_after = frames(p.get(4), 1000.0) as usize + self.attack as usize + pre;
        self.dry = p.get(9);
        self.wet = p.get(10);
        self.noise = p.get(11);
        self.invert = match p.get(18) {
            v if v >= 1.5 => Invert::On,
            v if v >= 0.5 => Invert::Legacy,
            _ => Invert::Off,
        };
        self.preview = p.get(13) >= 0.5;
        self.midi = (p.get(15) >= 0.5)
            .then(|| ((p.get(16) * 127.0).round().clamp(0.0, 127.0) as u8, (p.get(17) * 15.0).round().clamp(0.0, 15.0) as u8));
        self.input = DetectorInput::new(p.get(7));
        self.filter = DetectorFilter::new(p.get(5), p.get(6), self.sample_rate);
        let window = (frames(p.get(14), 100.0) as usize).max(1);
        if fresh || window != self.window {
            self.window = window;
            self.rms = Rms::new(window);
        }
        if fresh || pre != self.lookahead.l.len() {
            self.lookahead = Lookahead::new(pre);
        }
    }

    /// Uniform white noise in -1..1.
    fn noise_sample(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32
    }
}

impl Effect for Gate {
    fn params(&mut self) -> Vec<ParamInfo> {
        self.params.infos()
    }

    fn param(&mut self, id: u32) -> Option<(f64, String)> {
        self.params.display(id)
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if self.params.set(id, value) {
            self.configure(false);
        }
    }

    fn set_normalized(&mut self, index: usize, v: f64) -> bool {
        let set = self.params.set_normalized(index, v);
        if set {
            self.configure(false);
        }
        set
    }

    fn latency(&mut self) -> usize {
        self.lookahead.l.len()
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        Some(self.params.chunk())
    }

    fn take_midi(&mut self, out: &mut MidiBlock) -> bool {
        if self.midi.is_none() {
            return false;
        }
        self.notes.sort_by_key(|n| n.0);
        out.short.append(&mut self.notes);
        true
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], ctx: &ProcessContext) {
        if self.midi.is_some()
            && let Some(incoming) = ctx.chain_midi(left.len())
        {
            self.notes.extend(incoming.map(|(at, msg)| (ctx.midi_from + at, msg)));
        }
        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            let (kl, kr) = self.input.frame(i, *l, *r, ctx);
            let (fl, fr) = self.filter.run(kl as f64, kr as f64);
            let level = self.rms.run(fl, fr);
            // While the gate is open or still closing, the hysteresis moves the threshold
            // towards the closing one in proportion to the gain.
            let threshold = self.threshold + (self.close_threshold - self.threshold) * self.gain;
            if level >= threshold {
                self.open = true;
                self.below = 0;
            } else if self.open {
                self.below += 1;
                if self.below >= self.close_after {
                    self.open = false;
                }
            }
            self.gain = if self.open {
                if self.attack < 1.0 { 1.0 } else { (self.gain + 1.0 / self.attack).min(1.0) }
            } else if self.release < 1.0 {
                0.0
            } else {
                (self.gain - 1.0 / self.release).max(0.0)
            };
            // The note starts when the gate opens and ends once it has closed completely.
            if let Some((note, channel)) = self.midi
                && self.note_on != (self.open || self.gain > 0.0)
            {
                self.note_on = !self.note_on;
                let msg = if self.note_on { [0x90 | channel, note, 127] } else { [0x80 | channel, note, 0] };
                self.notes.push((ctx.midi_from + i, msg));
            }
            let (dl, dr) = self.lookahead.swap(*l, *r);
            if self.preview {
                *l = fl as f32;
                *r = fr as f32;
                continue;
            }
            let g = self.gain as f32;
            let gain = match self.invert {
                Invert::Off => self.dry + self.wet * g,
                Invert::Legacy => self.dry - self.wet * g,
                Invert::On => self.dry + self.wet * (1.0 - g),
            };
            *l = dl * gain;
            *r = dr * gain;
            if self.noise > 0.0 && g > 0.0 {
                let a = self.noise * g;
                *l += a * self.noise_sample();
                *r += a * self.noise_sample();
            }
        }
    }

    fn reset(&mut self) {
        self.gain = 0.0;
        self.open = false;
        self.below = 0;
        self.rms.reset();
        self.filter.reset();
        self.lookahead = Lookahead::new(self.lookahead.l.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stock::testing::{check, chunk, run};

    #[test]
    fn gate_matches_reaper() {
        // Attack 10 ms, release 100 ms, then the same with 100 ms hold.
        let mut params = DEFAULTS.to_vec();
        (params[0], params[1], params[2], params[10], params[12]) = (0.2512, 0.02, 0.02, 1.0, 1.0);
        let gain = run(&mut Gate::from_reagate(&chunk(&params), 48000).unwrap());
        check(
            &gain,
            &[
                (14399, 0.0),
                (14400, 0.0021),
                (14480, 0.1688),
                (14640, 0.5021),
                (14880, 1.0),
                (34080, 0.9994),
                (34560, 0.8994),
                (36000, 0.5994),
                (43200, 0.0),
            ],
        );
        params[4] = 0.1;
        let gain = run(&mut Gate::from_reagate(&chunk(&params), 48000).unwrap());
        check(&gain, &[(38400, 1.0), (43200, 0.0994)]);
    }
}
