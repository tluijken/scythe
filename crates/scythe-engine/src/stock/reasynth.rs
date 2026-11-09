//! ReaSynth, from the parameters it stores. Oscillators, mix laws, envelope and voice gating
//! were measured from REAPER 7.73 renders of single notes (`examples/stock_ref.rs`):
//!
//! - the oscillators are naive (not band-limited): `-sin`, a pulse, a rising saw and a
//!   triangle of amplitude 1/4, all from one phase that starts at 0 one frame after the
//!   note-on; square mix counts as its fourth power, saw and triangle mixes as their squares,
//!   and the sine takes what is left (none once they add up past 1). The extra sine crossfades
//!   with all of that by its mix and runs at `2 atan(w / 2)` instead of `w` (a slightly flat
//!   recursive oscillator); the phase is kept in radians, which decides the frame a pulse or
//!   saw edge falls on;
//! - attack is a linear ramp to the velocity's level, decay an exponential towards the
//!   sustain level (its factor rounded to a float), release a linear fall of the level; a
//!   note-off takes effect after its own frame, and a note released during its attack goes
//!   over to the decay at the start of REAPER's 512-frame block holding the note-off;
//! - a voice stays silent, its phase held, until its attack level passes -80 dB, and REAPER
//!   drops a decayed voice below -80 dB at its 512-frame block boundaries.
//!
//! Not matched: REAPER pauses a decaying level for one frame at a block boundary after the
//! attack, sometimes on the frame after it (differences up to 2e-4 with a sustain below
//! 0 dB); portamento, which makes REAPER glide monophonically, is ignored (voices stay
//! polyphonic at their own pitch).

use std::f64::consts::TAU;

use super::marked_params;
use super::params::{Params, Spec, Unit, spec};
use crate::clap_host::ParamInfo;
use crate::fx::{Effect, ProcessContext};

const SPECS: &[Spec] = &[
    spec(0, "Attack (ms)", Unit::Ms(500.0), 1.0),
    spec(1, "Release (ms)", Unit::Ms(5000.0), 1.0),
    spec(2, "Square mix", Unit::Linear(0.0, 1.0, 2), 1.0),
    spec(3, "Saw mix", Unit::Linear(0.0, 1.0, 2), 1.0),
    spec(4, "Triangle mix", Unit::Linear(0.0, 1.0, 2), 1.0),
    spec(5, "Volume", Unit::Gain, 2.0),
    spec(6, "Decay (ms)", Unit::Linear(1.0, 15001.0, 0), 1.0),
    spec(7, "Extra sine mix", Unit::Linear(0.0, 1.0, 2), 1.0),
    spec(8, "Extra sine tuning (cents)", Unit::Linear(-2400.0, 2400.0, 0), 1.0),
    spec(9, "Sustain", Unit::Gain, 2.0),
    spec(10, "Pulse width", Unit::Linear(0.1, 0.5, 2), 1.0),
    spec(11, "Global detune (cents)", Unit::Linear(-1200.0, 1200.0, 0), 1.0),
    spec(12, "Legacy oscillator mode", Unit::Switch, 1.0),
    spec(13, "Portamento (ms)", Unit::Ms(1000.0), 1.0),
    spec(14, "Broken portamento extra sine", Unit::Switch, 1.0),
];

/// A fresh ReaSynth's parameters as REAPER 7.73 stores them.
pub(crate) const DEFAULTS: [f32; 15] =
    [0.006, 0.0016, 0.0, 0.0, 0.0, 0.501_187_2, 0.066_604_44, 0.0, 0.5, 1.0, 1.0, 0.5, 0.0, 0.0, 0.0];

/// -80 dB: below it a starting voice stays silent and a decayed one is dropped.
const SILENCE: f64 = 1e-4;
const BLOCK: i64 = 512;

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Attack,
    Decay,
    Release,
}

struct Voice {
    channel: u8,
    key: u8,
    velocity: f64,
    stage: Stage,
    /// The envelope times the velocity.
    level: f64,
    started: bool,
    /// Note-off received, applied after the current frame.
    releasing: bool,
    phase: f64,
    extra: f64,
    /// Project frame of the note-on.
    start: i64,
    /// The next block boundary holds the level for a frame (REAPER moves into the decay
    /// stage in its block processing).
    hold: bool,
    /// Phase steps per frame: cycles for the main oscillators, radians for the extra sine.
    inc: f64,
    extra_inc: f64,
}

/// What the parameters work out to.
#[derive(Default)]
struct Shape {
    attack: f64,
    /// The decay's factor per frame, rounded to a float as REAPER keeps it.
    decay_step: f64,
    release: f64,
    sustain: f64,
    volume: f64,
    square: f64,
    saw: f64,
    triangle: f64,
    sine: f64,
    extra: f64,
    extra_ratio: f64,
    pulse: f64,
    detune: f64,
}

pub struct Synth {
    params: Params,
    sample_rate: u32,
    shape: Shape,
    voices: Vec<Voice>,
    events: Vec<(usize, [u8; 3])>,
    offs: Vec<(u8, u8)>,
    /// Pitch wheel per channel, in semitones (two either way).
    bend: [f64; 16],
}

impl Synth {
    /// Parameters 0..=14: attack, release, square, saw and triangle mix, volume, decay, extra
    /// sine mix and tuning, sustain, pulse width, detune, legacy mode, portamento, broken
    /// portamento.
    pub fn from_reasynth(chunk: &[u8], sample_rate: u32) -> Option<Self> {
        let mut synth = Self {
            params: Params::new(marked_params(chunk)?, SPECS, &DEFAULTS),
            sample_rate,
            shape: Shape::default(),
            voices: Vec::new(),
            events: Vec::new(),
            offs: Vec::new(),
            bend: [0.0; 16],
        };
        synth.configure();
        Some(synth)
    }

    fn configure(&mut self) {
        let p = &self.params;
        let sr = self.sample_rate as f64;
        let get = |i: usize| p.get(i) as f64;
        let (square, saw, triangle) = (get(2).powi(4), get(3).powi(2), get(4).powi(2));
        self.shape = Shape {
            attack: (p.get(0) * 0.5 * sr as f32) as f64,
            decay_step: ((-1000.0 / ((1.0 + get(6) * 15000.0) * sr)).exp() as f32) as f64,
            release: (p.get(1) * 5.0 * sr as f32) as f64,
            sustain: get(9),
            volume: get(5),
            square,
            saw,
            triangle,
            sine: (1.0 - square - saw - triangle).max(0.0),
            extra: get(7),
            extra_ratio: 2f64.powf((get(8) - 0.5) * 4800.0 / 1200.0),
            pulse: 0.1 + 0.4 * get(10),
            detune: (get(11) - 0.5) * 2400.0,
        };
        self.tune_voices();
    }

    fn note_on(&mut self, start: i64, channel: u8, key: u8, velocity: u8) {
        self.voices.push(Voice {
            channel,
            key,
            // REAPER keeps the velocity gain as a float, which decides the frame the attack ends on.
            velocity: (velocity as f32 / 127.0) as f64,
            stage: Stage::Attack,
            level: 0.0,
            started: false,
            releasing: false,
            phase: 0.0,
            extra: 0.0,
            start,
            hold: false,
            inc: 0.0,
            extra_inc: 0.0,
        });
        self.tune_voices();
    }

    fn note_off(&mut self, channel: u8, key: u8) {
        for v in &mut self.voices {
            if v.channel == channel && v.key == key && v.stage != Stage::Release {
                v.releasing = true;
            }
        }
    }

    /// REAPER ends the attack of a note released in the coming block at the block's start,
    /// going on with the decay stage; the track's notes tell which are.
    fn cut_attacks(&mut self, notes: &crate::synth::NoteList, frame: i64) {
        for v in self.voices.iter_mut().filter(|v| v.stage == Stage::Attack && !v.releasing) {
            let released = notes.starting_in(v.start..v.start + 1).iter().any(|n| {
                let off = n.end.min(n.cutoff);
                n.key == v.key && n.channel == v.channel && (frame..frame + BLOCK).contains(&off)
            });
            if released {
                v.stage = Stage::Decay;
                v.hold = true;
            }
        }
    }

    /// REAPER takes the pitch wheel at the start of the 512-frame block it falls in.
    fn bend_ahead(&mut self, notes: &crate::synth::NoteList, frame: i64) {
        let mut changed = false;
        for c in notes.controls_in(frame..frame + BLOCK).iter().filter(|c| c.bytes[0] & 0xf0 == 0xe0) {
            let value = (c.bytes[1] as i32 | (c.bytes[2] as i32) << 7) - 8192;
            self.bend[(c.bytes[0] & 0x0f) as usize] = 2.0 * value as f64 / 8192.0;
            changed = true;
        }
        if changed {
            self.tune_voices();
        }
    }

    fn tune_voices(&mut self) {
        let sr = self.sample_rate as f64;
        for v in &mut self.voices {
            let semis = v.key as f64 - 69.0 + self.bend[v.channel as usize];
            let f = 440.0 * 2f64.powf(semis / 12.0 + self.shape.detune / 1200.0);
            v.inc = TAU * f / sr;
            v.extra_inc = 2.0 * (TAU * f * self.shape.extra_ratio / sr / 2.0).atan();
        }
    }

    /// One frame of `voice`, `None` once it has ended.
    fn run(s: &Shape, v: &mut Voice, block_start: bool) -> Option<f64> {
        if v.releasing {
            v.releasing = false;
            v.stage = Stage::Release;
        }
        if !v.started && v.level >= SILENCE {
            v.started = true;
        }
        let full = v.velocity;
        match v.stage {
            Stage::Attack => {
                if s.attack < 1.0 {
                    v.level = full;
                } else {
                    v.level += full / s.attack;
                }
                if v.level >= full {
                    v.level = full;
                    v.stage = Stage::Decay;
                    v.hold = true;
                }
            }
            Stage::Decay => {
                if block_start && v.level < SILENCE {
                    return None;
                }
                if block_start && v.hold {
                    v.hold = false;
                } else {
                    let sustain = s.sustain * full;
                    v.level = sustain + (v.level - sustain) * s.decay_step;
                }
            }
            Stage::Release => {
                if s.release < 1.0 || block_start && v.level < SILENCE {
                    return None;
                }
                v.level -= 1.0 / s.release;
                if v.level <= 0.0 {
                    return None;
                }
            }
        }
        if !v.started {
            return Some(0.0);
        }
        let mut osc = -v.phase.sin() * s.sine;
        let p = v.phase / TAU;
        if s.square != 0.0 {
            osc += s.square * if v.phase < s.pulse * TAU { 1.0 } else { -1.0 };
        }
        if s.saw != 0.0 {
            osc += s.saw * (2.0 * p - 1.0);
        }
        if s.triangle != 0.0 {
            let t = if p < 0.25 {
                -p
            } else if p < 0.75 {
                p - 0.5
            } else {
                1.0 - p
            };
            osc += s.triangle * t;
        }
        if s.extra != 0.0 {
            osc = osc * (1.0 - s.extra) - s.extra * v.extra.sin();
        }
        v.phase += v.inc;
        if v.phase >= TAU {
            v.phase -= TAU;
        }
        v.extra = (v.extra + v.extra_inc) % TAU;
        Some(osc * v.level * s.volume)
    }
}

impl Effect for Synth {
    fn is_instrument(&self) -> bool {
        true
    }

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

    fn process(&mut self, left: &mut [f32], right: &mut [f32], ctx: &ProcessContext) {
        let mut events = std::mem::take(&mut self.events);
        ctx.block_midi(left.len(), &mut events);
        // Rendering from the track's notes, the coming block's pitch wheel is known ahead.
        let ahead = ctx.midi.is_none() && ctx.notes.is_some();
        let mut next = 0;
        for i in 0..left.len() {
            while let Some(&(_, msg)) = events.get(next).filter(|e| e.0 <= i) {
                next += 1;
                let (status, channel) = (msg[0] & 0xf0, msg[0] & 0x0f);
                match status {
                    0x90 if msg[2] > 0 => self.note_on(ctx.position + i as i64, channel, msg[1], msg[2]),
                    0x80 | 0x90 => self.offs.push((channel, msg[1])),
                    0xb0 if msg[1] == 123 || msg[1] == 120 => self.voices.iter_mut().for_each(|v| v.releasing = true),
                    0xe0 if !ahead => {
                        let value = (msg[1] as i32 | (msg[2] as i32) << 7) - 8192;
                        self.bend[channel as usize] = 2.0 * value as f64 / 8192.0;
                        self.tune_voices();
                    }
                    _ => {}
                }
            }
            let block_start = (ctx.position + i as i64).rem_euclid(BLOCK) == 0;
            if block_start
                && ctx.midi.is_none()
                && let Some(notes) = ctx.notes
            {
                let frame = ctx.position + i as i64;
                self.cut_attacks(notes, frame);
                self.bend_ahead(notes, frame);
            }
            let mut sum = 0.0;
            let mut k = 0;
            while k < self.voices.len() {
                match Self::run(&self.shape, &mut self.voices[k], block_start) {
                    Some(v) => {
                        sum += v;
                        k += 1;
                    }
                    None => {
                        self.voices.remove(k);
                    }
                }
            }
            for (channel, key) in std::mem::take(&mut self.offs) {
                self.note_off(channel, key);
            }
            left[i] += sum as f32;
            right[i] += sum as f32;
        }
        self.events = events;
    }

    fn reset(&mut self) {
        self.voices.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::MidiBlock;
    use crate::stock::testing::{chunk, context};

    /// REAPER 7.73 with attack and release at 0: silent on the note-on frame, then `-sin` from
    /// phase 0 at the velocity's level and -6 dB.
    #[test]
    fn a_note_starts_like_reaper() {
        let mut params = DEFAULTS.to_vec();
        (params[0], params[1]) = (0.0, 0.0);
        let mut synth = Synth::from_reasynth(&chunk(&params), 48000).unwrap();
        let midi = MidiBlock { short: vec![(0, [0x90, 69, 100]), (100, [0x80, 69, 0])], ..Default::default() };
        let mut ctx = context();
        ctx.midi = Some(&midi);
        let (mut l, mut r) = (vec![0.0f32; 200], vec![0.0f32; 200]);
        synth.process(&mut l, &mut r, &ctx);
        assert_eq!((l[0], l[1]), (0.0, 0.0));
        assert!((l[2] + 0.022_716_81).abs() < 1e-7, "{}", l[2]);
        assert!(l[100] != 0.0 && l[101] == 0.0);
        assert_eq!(l, r);
    }
}
