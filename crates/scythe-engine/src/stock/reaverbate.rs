//! ReaVerbate, from the parameters it stores. It is Jezar's public-domain Freeverb with ten
//! combs and six allpasses per channel, run on each channel separately; tunings, gains,
//! the room and damping ranges, width, pre-delay and the wet filters were measured from
//! REAPER 7.73 impulse responses and follow its renders sample for sample
//! (`examples/stock_ref.rs`).

use super::params::{Params, Spec, Unit, spec};
use super::{Biquad, marked_params};
use crate::clap_host::ParamInfo;
use crate::fx::{Effect, ProcessContext};

/// Delays at 44.1 kHz, scaled to the project rate and truncated; the right channel's are
/// `SPREAD` longer before scaling.
const COMBS: [usize; 10] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617, 1685, 1748];
const ALLPASSES: [usize; 6] = [556, 441, 341, 225, 180, 153];
const SPREAD: usize = 23;
const INPUT_GAIN: f64 = 0.015;

const SPECS: &[Spec] = &[
    spec(0, "Wet", Unit::Gain, 2.0),
    spec(1, "Dry", Unit::Gain, 2.0),
    spec(2, "Room size", Unit::Linear(30.0, 98.0, 0), 1.0),
    spec(3, "Dampening", Unit::Linear(0.0, 100.0, 0), 1.0),
    spec(4, "Width", Unit::Linear(-1.0, 1.0, 2), 1.0),
    spec(5, "Delay (ms)", Unit::Ms(500.0), 1.0),
    spec(6, "Lowpass (Hz)", Unit::Hz, 1.0),
    spec(7, "Highpass (Hz)", Unit::Hz, 1.0),
];

/// A fresh ReaVerbate's parameters as REAPER 7.73 stores them.
pub(crate) const DEFAULTS: [f32; 8] = [0.500_034_5, 1.0, 0.294_117_66, 0.5, 1.0, 0.0, 1.0, 0.0];

struct Comb {
    buf: Vec<f64>,
    at: usize,
    store: f64,
}

struct Allpass {
    buf: Vec<f64>,
    at: usize,
}

struct Channel {
    combs: Vec<Comb>,
    allpasses: Vec<Allpass>,
}

impl Channel {
    fn new(sample_rate: u32, spread: usize) -> Self {
        let frames = |t: usize| (((t + spread) as f64 * sample_rate as f64 / 44100.0) as usize).max(1);
        Self {
            combs: COMBS.iter().map(|&t| Comb { buf: vec![0.0; frames(t)], at: 0, store: 0.0 }).collect(),
            allpasses: ALLPASSES.iter().map(|&t| Allpass { buf: vec![0.0; frames(t)], at: 0 }).collect(),
        }
    }

    fn run(&mut self, x: f64, feedback: f64, damp: f64) -> f64 {
        let input = x * INPUT_GAIN;
        let mut out = 0.0;
        for c in &mut self.combs {
            let y = c.buf[c.at];
            c.store = y * (1.0 - damp) + c.store * damp;
            c.buf[c.at] = input + c.store * feedback;
            c.at = (c.at + 1) % c.buf.len();
            out += y;
        }
        for a in &mut self.allpasses {
            let delayed = a.buf[a.at];
            a.buf[a.at] = out + delayed * 0.5;
            a.at = (a.at + 1) % a.buf.len();
            out = delayed - out;
        }
        out
    }

    fn reset(&mut self) {
        for c in &mut self.combs {
            c.buf.fill(0.0);
            c.store = 0.0;
        }
        for a in &mut self.allpasses {
            a.buf.fill(0.0);
        }
    }
}

pub struct Reverb {
    params: Params,
    sample_rate: u32,
    channels: [Channel; 2],
    feedback: f64,
    damp: f64,
    wet: [f64; 2],
    dry: f64,
    predelay: Vec<[f64; 2]>,
    at: usize,
    filters: Vec<[Biquad; 2]>,
}

impl Reverb {
    /// Parameters 0..=7: wet, dry, room size, dampening, width, delay, lowpass, highpass.
    pub fn from_reaverbate(chunk: &[u8], sample_rate: u32) -> Option<Self> {
        let mut verb = Self {
            params: Params::new(marked_params(chunk)?, SPECS, &DEFAULTS),
            sample_rate,
            channels: [Channel::new(sample_rate, 0), Channel::new(sample_rate, SPREAD)],
            feedback: 0.0,
            damp: 0.0,
            wet: [0.0; 2],
            dry: 0.0,
            predelay: Vec::new(),
            at: 0,
            filters: Vec::new(),
        };
        verb.configure();
        Some(verb)
    }

    fn configure(&mut self) {
        let p = &self.params;
        self.feedback = 0.3 + 0.68 * p.get(2) as f64;
        self.damp = 0.4 * p.get(3) as f64;
        let (wet, width) = (p.get(0) as f64, 2.0 * p.get(4) as f64 - 1.0);
        self.wet = [wet * (width / 2.0 + 0.5), wet * (1.0 - width) / 2.0];
        self.dry = p.get(1) as f64;
        let delay = (p.get(5) as f64 * 0.5 * self.sample_rate as f64) as usize;
        if delay != self.predelay.len() {
            self.predelay = vec![[0.0; 2]; delay];
            self.at = 0;
        }
        let (lp, hp) = (p.get(6) as f64 * 20000.0, p.get(7) as f64 * 20000.0);
        let mut filters = Vec::new();
        if lp < 20000.0 {
            filters.push([Biquad::lowpass(lp.max(1.0), 1.0, self.sample_rate); 2]);
        }
        if hp > 0.0 {
            filters.push([Biquad::highpass(hp, 1.0, self.sample_rate); 2]);
        }
        if filters.len() == self.filters.len() {
            for (old, new) in self.filters.iter_mut().zip(&filters) {
                old[0].retune(&new[0]);
                old[1].retune(&new[1]);
            }
        } else {
            self.filters = filters;
        }
    }
}

impl Reverb {
    /// Seconds the reverb takes to fall by 60 dB at `hz`, from its combs' feedback and
    /// damping (averaged over the combs).
    pub fn decay_seconds(&self, hz: f64) -> f64 {
        let sr = self.sample_rate as f64;
        let mean = COMBS.iter().map(|&t| t as f64 * sr / 44100.0).sum::<f64>() / COMBS.len() as f64;
        let w = std::f64::consts::TAU * hz / sr;
        let d = self.damp;
        let lowpass = (1.0 - d) / ((1.0 - d * w.cos()).powi(2) + (d * w.sin()).powi(2)).sqrt();
        let g = (self.feedback * lowpass).clamp(1e-9, 0.999_999);
        -3.0 * mean / (sr * g.log10())
    }

    /// The pre-delay in seconds.
    pub fn predelay(&self) -> f64 {
        self.predelay.len() as f64 / self.sample_rate as f64
    }
}

impl Effect for Reverb {
    fn params(&mut self) -> Vec<ParamInfo> {
        self.params.infos()
    }

    fn param(&mut self, id: u32) -> Option<(f64, String)> {
        self.params.display(id)
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if self.params.set(id, value) {
            self.configure();
        }
    }

    fn set_normalized(&mut self, index: usize, v: f64) -> bool {
        let set = self.params.set_normalized(index, v);
        if set {
            self.configure();
        }
        set
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        Some(self.params.chunk())
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], _ctx: &ProcessContext) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (dl, dr) = (*l as f64, *r as f64);
            let (il, ir) = if self.predelay.is_empty() {
                (dl, dr)
            } else {
                let [pl, pr] = std::mem::replace(&mut self.predelay[self.at], [dl, dr]);
                self.at = (self.at + 1) % self.predelay.len();
                (pl, pr)
            };
            let [cl, cr] = &mut self.channels;
            let vl = cl.run(il, self.feedback, self.damp);
            let vr = cr.run(ir, self.feedback, self.damp);
            let (mut wl, mut wr) = (vl * self.wet[0] + vr * self.wet[1], vr * self.wet[0] + vl * self.wet[1]);
            for [fl, fr] in &mut self.filters {
                wl = fl.run(wl);
                wr = fr.run(wr);
            }
            *l = (wl + dl * self.dry) as f32;
            *r = (wr + dr * self.dry) as f32;
        }
    }

    fn reset(&mut self) {
        self.channels.iter_mut().for_each(Channel::reset);
        self.predelay.fill([0.0; 2]);
        self.filters.iter_mut().flatten().for_each(Biquad::reset);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stock::testing::{chunk, context};

    /// The first taps of REAPER 7.73's impulse response (left in, wet only, damping off): the
    /// shortest comb, then its sum with the shortest allpass, at 48 kHz.
    #[test]
    fn impulse_taps_match_reaper() {
        let mut params = DEFAULTS.to_vec();
        (params[1], params[3]) = (0.0, 0.0);
        let mut verb = Reverb::from_reaverbate(&chunk(&params), 48000).unwrap();
        let (mut l, mut r) = (vec![0.0f32; 2000], vec![0.0f32; 2000]);
        l[0] = 1.0;
        verb.process(&mut l, &mut r, &context());
        let first = l.iter().position(|v| v.abs() > 1e-9).unwrap();
        assert_eq!(first, 1214);
        assert!((l[1214] - 0.007_500_518).abs() < 1e-8);
        assert!((l[1214 + 166] + 0.007_500_518).abs() < 1e-8);
        assert!(r.iter().all(|v| *v == 0.0));
    }
    /// The decay the editor shows is how fast the impulse response falls.
    #[test]
    fn shown_decay_follows_the_impulse_response() {
        let mut params = DEFAULTS.to_vec();
        (params[1], params[3]) = (0.0, 0.0);
        let mut verb = Reverb::from_reaverbate(&chunk(&params), 48000).unwrap();
        let n = 96000;
        let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
        l[0] = 1.0;
        r[0] = 1.0;
        verb.process(&mut l, &mut r, &context());
        let window = 960;
        let levels: Vec<f64> = l
            .chunks(window)
            .map(|w| 10.0 * (w.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / window as f64).max(1e-30).log10())
            .collect();
        let top = levels.iter().copied().fold(f64::MIN, f64::max);
        let when = |drop: f64| levels.iter().rposition(|&v| v > top - drop).unwrap() as f64 * 0.02;
        let measured = (when(45.0) - when(15.0)) * 2.0;
        let shown = verb.decay_seconds(0.0);
        assert!((measured / shown - 1.0).abs() < 0.25, "{measured} vs {shown}");
    }
}
