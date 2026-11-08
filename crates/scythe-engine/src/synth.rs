//! Built-in preview instrument for MIDI items until instrument plugins are hosted: synthesised
//! drums on channel 10 and a soft polyphonic synth on the other channels.
//!
//! Every voice is a pure function of the time since its note-on (and note-off), so rendering
//! needs no state: seeking, looping and graph swaps cannot leave hanging notes.

use std::f64::consts::TAU;

/// A note resolved to project frames at the engine rate.
#[derive(Debug, Clone, Copy)]
pub struct SynthNote {
    pub start: i64,
    pub end: i64,
    /// Frame after which the item cuts the voice off (the item end).
    pub cutoff: i64,
    pub channel: u8,
    pub key: u8,
    pub velocity: u8,
}

const DRUM_CHANNEL: u8 = 9;
const RELEASE: f64 = 0.15;
/// Longest ring-out after note-off (drums ignore note-off and ring for at most this long).
pub const MAX_TAIL_SECONDS: f64 = 2.0;

/// A channel message other than a note (controller, pitch bend…) at a project frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControlEvent {
    pub frame: i64,
    pub bytes: [u8; 3],
    pub len: u8,
}

/// Notes sorted by start, with the longest sounding span so a block can find the notes that
/// overlap it with a binary search, and the controller events sorted by frame.
#[derive(Debug, Default)]
pub struct NoteList {
    pub notes: Vec<SynthNote>,
    max_span: i64,
    controls: Vec<ControlEvent>,
}

impl NoteList {
    pub fn new(mut notes: Vec<SynthNote>, sample_rate: u32) -> Self {
        notes.sort_by_key(|n| n.start);
        let tail = (MAX_TAIL_SECONDS * sample_rate as f64) as i64;
        let max_span = notes.iter().map(|n| n.end - n.start + tail).max().unwrap_or(0);
        Self { notes, max_span, controls: Vec::new() }
    }

    pub fn with_controls(mut self, mut controls: Vec<ControlEvent>) -> Self {
        controls.sort_by_key(|c| c.frame);
        self.controls = controls;
        self
    }

    pub fn controls(&self) -> &[ControlEvent] {
        &self.controls
    }

    /// Controller events in `range` (project frames).
    pub fn controls_in(&self, range: std::ops::Range<i64>) -> &[ControlEvent] {
        let first = self.controls.partition_point(|c| c.frame < range.start);
        let last = self.controls.partition_point(|c| c.frame < range.end);
        &self.controls[first..last.max(first)]
    }

    /// Longest time from a note's start to the end of its sound, in frames.
    pub fn max_span(&self) -> i64 {
        self.max_span
    }

    /// Notes that start in `range` (project frames).
    pub fn starting_in(&self, range: std::ops::Range<i64>) -> &[SynthNote] {
        &self.notes[self.index_range(range)]
    }

    /// Where in `notes` the notes that start in `range` are.
    pub fn index_range(&self, range: std::ops::Range<i64>) -> std::ops::Range<usize> {
        let last = self.notes.partition_point(|x| x.start < range.end);
        let first = self.notes[..last].partition_point(|x| x.start < range.start);
        first..last
    }

    /// Adds the voices sounding in `[block_start, block_start + out.len())`.
    pub fn render(&self, block_start: i64, sample_rate: u32, gain: (f32, f32), out_l: &mut [f32], out_r: &mut [f32]) {
        let n = out_l.len() as i64;
        let block_end = block_start + n;
        let last = self.notes.partition_point(|x| x.start < block_end);
        let first = self.notes[..last].partition_point(|x| x.start < block_start - self.max_span);
        let sr = sample_rate as f64;
        for note in &self.notes[first..last] {
            let tail = voice_tail(note);
            let sound_end = ((note.end as f64 + tail * sr) as i64).min(note.cutoff);
            let from = block_start.max(note.start);
            let to = block_end.min(sound_end);
            for f in from..to {
                let since_on = (f - note.start) as f64 / sr;
                let since_off = (f >= note.end).then(|| (f - note.end) as f64 / sr);
                let s = voice(note, since_on, since_off, f - note.start) as f32;
                let o = (f - block_start) as usize;
                out_l[o] += s * gain.0;
                out_r[o] += s * gain.1;
            }
        }
    }
}

fn voice_tail(note: &SynthNote) -> f64 {
    if note.channel == DRUM_CHANNEL { MAX_TAIL_SECONDS } else { RELEASE }
}

/// Deterministic white noise in -1..1 for sample `i` of a voice.
fn noise(i: i64, seed: u8) -> f64 {
    let mut x = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (seed as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93);
    x ^= x >> 33;
    x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    x ^= x >> 29;
    (x >> 11) as f64 / (1u64 << 52) as f64 - 1.0
}

fn voice(note: &SynthNote, t: f64, since_off: Option<f64>, i: i64) -> f64 {
    let vel = note.velocity as f64 / 127.0;
    if note.channel == DRUM_CHANNEL {
        return drum(note.key, t, i) * vel;
    }
    let freq = 440.0 * 2f64.powf((note.key as f64 - 69.0) / 12.0);
    let wave: f64 = (1..=5).map(|h| (TAU * freq * h as f64 * t).sin() / h as f64).sum::<f64>() * 0.5;
    let attack = (t / 0.005).min(1.0);
    let decay = 0.6 + 0.4 * (-t / 0.2).exp();
    let release = since_off.map_or(1.0, |r| (1.0 - r / RELEASE).max(0.0));
    wave * attack * decay * release * vel * 0.25
}

fn drum(key: u8, t: f64, i: i64) -> f64 {
    let hp_noise = |seed| (noise(i, seed) - noise(i - 1, seed)) * 0.5;
    let sweep = |base: f64, extra: f64, rate: f64| (TAU * (base * t + extra * (1.0 - (-rate * t).exp()) / rate)).sin();
    match key {
        35 | 36 => sweep(48.0, 110.0, 30.0) * (-t * 7.0).exp() * 0.9,
        38 | 40 => noise(i, key) * (-t * 18.0).exp() * 0.45 + (TAU * 185.0 * t).sin() * (-t * 22.0).exp() * 0.4,
        37 | 39 => noise(i, key) * (-t * 35.0).exp() * 0.5,
        42 | 44 => hp_noise(key) * (-t * 55.0).exp() * 0.45,
        46 => hp_noise(key) * (-t * 7.0).exp() * 0.35,
        41 | 43 | 45 | 47 | 48 | 50 => {
            let base = 70.0 + (key as f64 - 41.0) * 14.0;
            sweep(base, base * 0.6, 14.0) * (-t * 6.0).exp() * 0.6
        }
        49 | 52 | 55 | 57 => hp_noise(key) * (-t * 2.2).exp() * 0.35,
        51 | 53 | 59 => hp_noise(key) * (-t * 3.5).exp() * 0.2 + (TAU * 3150.0 * t).sin() * (-t * 5.0).exp() * 0.06,
        _ => hp_noise(key) * (-t * 40.0).exp() * 0.3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(start: i64, end: i64, channel: u8, key: u8) -> SynthNote {
        SynthNote { start, end, cutoff: i64::MAX, channel, key, velocity: 100 }
    }

    #[test]
    fn notes_sound_only_from_their_start() {
        let list = NoteList::new(vec![note(100, 200, 0, 60)], 1000);
        let (mut l, mut r) = (vec![0.0; 400], vec![0.0; 400]);
        list.render(0, 1000, (1.0, 1.0), &mut l, &mut r);
        assert!(l[..100].iter().all(|&s| s == 0.0));
        assert!(l[101..200].iter().any(|&s| s != 0.0));
        // Released after 150 ms.
        assert!(l[351..].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn rendering_is_stateless() {
        let list = NoteList::new(vec![note(0, 4000, 9, 38), note(500, 900, 0, 64)], 8000);
        let (mut a_l, mut a_r) = (vec![0.0; 1000], vec![0.0; 1000]);
        list.render(0, 8000, (1.0, 0.5), &mut a_l, &mut a_r);
        let (mut b_l, mut b_r) = (vec![0.0; 300], vec![0.0; 300]);
        list.render(600, 8000, (1.0, 0.5), &mut b_l, &mut b_r);
        assert_eq!(&a_l[600..900], &b_l[..]);
        assert_eq!(&a_r[600..900], &b_r[..]);
    }

    #[test]
    fn item_end_cuts_voices() {
        let mut n = note(0, 100, 9, 49);
        n.cutoff = 50;
        let list = NoteList::new(vec![n], 1000);
        let (mut l, mut r) = (vec![0.0; 100], vec![0.0; 100]);
        list.render(0, 1000, (1.0, 1.0), &mut l, &mut r);
        assert!(l[50..].iter().all(|&s| s == 0.0));
    }
}
