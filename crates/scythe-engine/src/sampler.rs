//! One-shot sampler that reads the settings ReaSamplOmatic5000 stores: the sample file, volume,
//! pan, note range, MIDI channel, velocity response and sample offsets. Like the preview synth
//! it is stateless: a voice is a function of the time since its note-on.

use std::path::Path;
use std::sync::Arc;

use scythe_core::units::PanLaw;

use crate::fx::{Effect, ProcessContext};
use crate::media::{AudioData, decode_sample, resample};

#[derive(Debug, Clone, PartialEq)]
pub struct Rs5kSettings {
    pub file: String,
    pub volume: f64,
    /// 0..1 with 0.5 at centre.
    pub pan: f64,
    pub min_velocity_gain: f64,
    pub note_lo: u8,
    pub note_hi: u8,
    /// 0 for omni, otherwise 1-based.
    pub channel: u8,
    pub obey_note_offs: bool,
    /// Voices that sound at once; a note beyond them takes over the oldest.
    pub max_voices: usize,
    /// Notes play only with a velocity in this range.
    pub velocities: (u8, u8),
    /// Attack time in seconds (the parameter spans 0..2 s).
    pub attack: f64,
    pub start: f64,
    pub end: f64,
}

impl Rs5kSettings {
    /// The chunk is the sample path (NUL-terminated) followed by f64 parameters in REAPER's
    /// parameter order: volume, pan, gain at minimum velocity, note range start and end (/127),
    /// pitch for start and end note, channel (/16), max voices, attack, release, obey note-offs,
    /// loop, sample start and end offset.
    pub fn parse(chunk: &[u8]) -> Option<Self> {
        let nul = chunk.iter().position(|&b| b == 0)?;
        let file = String::from_utf8_lossy(&chunk[..nul]).into_owned();
        let values: Vec<f64> = chunk[nul + 1..].as_chunks::<8>().0.iter().map(|b| f64::from_le_bytes(*b)).collect();
        let v = |i: usize, d: f64| values.get(i).copied().filter(|x| x.is_finite()).unwrap_or(d);
        let note = |x: f64| (x * 127.0).round().clamp(0.0, 127.0) as u8;
        Some(Self {
            file,
            volume: v(0, 1.0),
            pan: v(1, 0.5),
            min_velocity_gain: v(2, 1.0),
            note_lo: note(v(3, 0.0)),
            note_hi: note(v(4, 1.0)),
            channel: (v(7, 0.0) * 16.0).round() as u8,
            obey_note_offs: v(11, 0.0) >= 0.5,
            // Measured from REAPER's renders: 0 is one voice, 1/16 two, 1/2 nine.
            max_voices: (1.0 + 16.0 * v(8, 1.0).clamp(0.0, 1.0)).round() as usize,
            velocities: {
                // Past the 16 parameters the state mixes integers and doubles; the velocity range
                // is a pair of doubles 164 bytes in, as fractions of 127.
                let at = |o: usize| {
                    chunk[nul + 1..]
                        .get(o..o + 8)
                        .map(|b| f64::from_le_bytes(b.try_into().unwrap()))
                        .filter(|x| x.is_finite() && (0.0..=1.0).contains(x))
                };
                let vel = |x: f64| (x * 127.0).round() as u8;
                (at(164).map_or(1, vel), at(172).map_or(127, vel))
            },
            attack: v(9, 0.0005) * 2.0,
            start: v(13, 0.0).clamp(0.0, 1.0),
            end: v(14, 1.0).clamp(0.0, 1.0),
        })
    }
}

pub struct Rs5kCompat {
    settings: Rs5kSettings,
    sample: Arc<AudioData>,
    gain: (f32, f32),
    range: (usize, usize),
    /// Notes started by MIDI messages (live input, or an earlier effect's output) rather than
    /// the track's own notes.
    voices: Vec<Voice>,
    events: Vec<(usize, [u8; 3])>,
    /// What happens to each of the track's notes, for the note list it was worked out for.
    plan: Option<(usize, Vec<Fate>)>,
}

/// How a note of the track's own plays, once voices are taken over.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Fate {
    Plays,
    /// Taken over by a later note: fades out over REAPER's block from this frame.
    FadesFrom(i64),
    /// Taken over within the block it starts in, before it is heard.
    Silent,
}

struct Voice {
    channel: u8,
    key: u8,
    /// Frames played so far.
    played: usize,
    gain: (f32, f32),
}

impl Rs5kCompat {
    pub fn new(settings: Rs5kSettings, sample_rate: u32, project_dir: Option<&Path>) -> Result<Self, String> {
        if settings.file.is_empty() {
            return Err("no sample loaded".into());
        }
        let path = Path::new(&settings.file);
        let path = match (path.is_absolute(), project_dir) {
            (false, Some(dir)) => dir.join(path),
            _ => path.to_owned(),
        };
        let audio = decode_sample(&path).map_err(|e| format!("sample: {e}"))?;
        let audio = if audio.sample_rate == sample_rate {
            audio
        } else {
            resample(&audio, sample_rate).map_err(|e| e.to_string())?
        };
        let frames = audio.frames();
        let range = ((settings.start * frames as f64) as usize, ((settings.end * frames as f64) as usize).min(frames));
        let (l, r) = PanLaw::default().gains(settings.pan * 2.0 - 1.0);
        let gain = ((l * settings.volume) as f32, (r * settings.volume) as f32);
        Ok(Self { settings, sample: Arc::new(audio), gain, range, voices: Vec::new(), events: Vec::new(), plan: None })
    }

    fn velocity_gain(&self, velocity: u8) -> f32 {
        let t = (velocity.max(1) as f64 - 1.0) / 126.0;
        (self.settings.min_velocity_gain + (1.0 - self.settings.min_velocity_gain) * t) as f32
    }
}

/// Frames over which a voice that is taken over fades out: one REAPER block, from the start
/// of the block holding the note that takes it.
const TAKEOVER_FADE: i64 = 512;

impl Rs5kCompat {
    /// Plays the track's notes through `max_voices` voices: a note beyond them takes over the
    /// oldest sounding one.
    fn fates(&self, notes: &crate::synth::NoteList) -> Vec<Fate> {
        let len = self.range.1.saturating_sub(self.range.0) as i64;
        let mut fates = vec![Fate::Plays; notes.notes.len()];
        let mut sounding: Vec<(usize, i64, i64)> = Vec::new();
        for (i, note) in notes.notes.iter().enumerate() {
            if !self.accepts(note.channel, note.key) || !self.plays_velocity(note.velocity) {
                fates[i] = Fate::Silent;
                continue;
            }
            sounding.retain(|&(_, _, end)| end > note.start);
            if sounding.len() >= self.settings.max_voices.max(1) {
                let (oldest, start, _) = sounding.remove(0);
                let block = note.start - note.start.rem_euclid(TAKEOVER_FADE);
                fates[oldest] = if block <= start { Fate::Silent } else { Fate::FadesFrom(block) };
            }
            let mut end = note.start + len;
            if self.settings.obey_note_offs {
                end = end.min(note.end.min(note.cutoff));
            }
            sounding.push((i, note.start, end));
        }
        fates
    }

    fn plays_velocity(&self, velocity: u8) -> bool {
        (self.settings.velocities.0..=self.settings.velocities.1).contains(&velocity)
    }

    fn accepts(&self, channel: u8, key: u8) -> bool {
        (self.settings.channel == 0 || channel + 1 == self.settings.channel)
            && (self.settings.note_lo..=self.settings.note_hi).contains(&key)
    }

    /// Starts and stops voices at the messages' frames and mixes every sounding voice.
    fn play_events(&mut self, events: &[(usize, [u8; 3])], left: &mut [f32], right: &mut [f32], sample_rate: u32) {
        if events.is_empty() && self.voices.is_empty() {
            return;
        }
        let n = left.len();
        let mut at = 0;
        for i in 0..=events.len() {
            let until = events.get(i).map_or(n, |e| e.0.min(n));
            self.mix_voices(at..until, left, right, sample_rate);
            at = until;
            let Some(&(_, [status, key, vel])) = events.get(i) else { break };
            let channel = status & 0x0f;
            match status & 0xf0 {
                0x90 if vel > 0 => {
                    if self.accepts(channel, key) && self.plays_velocity(vel) {
                        if self.voices.len() >= self.settings.max_voices.max(1) {
                            self.voices.remove(0);
                        }
                        let vg = self.velocity_gain(vel);
                        let gain = (self.gain.0 * vg, self.gain.1 * vg);
                        self.voices.push(Voice { channel, key, played: 0, gain });
                    }
                }
                0x80 | 0x90 if self.settings.obey_note_offs => {
                    self.voices.retain(|v| v.channel != channel || v.key != key);
                }
                _ => {}
            }
        }
    }

    fn mix_voices(&mut self, frames: std::ops::Range<usize>, left: &mut [f32], right: &mut [f32], sample_rate: u32) {
        let len = self.range.1.saturating_sub(self.range.0);
        let attack = (self.settings.attack * sample_rate as f64) as usize;
        let lc = &self.sample.channels[0];
        let rc = self.sample.channels.get(1).unwrap_or(lc);
        for v in &mut self.voices {
            for o in frames.clone() {
                if v.played >= len {
                    break;
                }
                let s = self.range.0 + v.played;
                let env = if v.played < attack { v.played as f32 / attack as f32 } else { 1.0 };
                left[o] += lc[s] * v.gain.0 * env;
                right[o] += rc[s] * v.gain.1 * env;
                v.played += 1;
            }
        }
        self.voices.retain(|v| v.played < len);
    }
}

impl Effect for Rs5kCompat {
    fn is_instrument(&self) -> bool {
        true
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], ctx: &ProcessContext) {
        let mut events = std::mem::take(&mut self.events);
        events.clear();
        match ctx.chain_midi(left.len()) {
            Some(chain) => events.extend(chain),
            None => events.extend(ctx.live_midi().map(|m| (0, m.bytes))),
        }
        self.play_events(&events, left, right, ctx.sample_rate);
        self.events = events;
        if ctx.midi.is_some() {
            return;
        }
        let Some(notes) = ctx.notes else { return };
        let len = self.range.1.saturating_sub(self.range.0) as i64;
        let n = left.len() as i64;
        let (block_start, block_end) = (ctx.position, ctx.position + n);
        let lc = &self.sample.channels[0];
        let rc = self.sample.channels.get(1).unwrap_or(lc);
        let key = notes as *const crate::synth::NoteList as usize;
        if self.plan.as_ref().is_none_or(|(k, f)| *k != key || f.len() != notes.notes.len()) {
            self.plan = Some((key, self.fates(notes)));
        }
        let Some((_, fates)) = &self.plan else { return };
        let window = notes.index_range(block_start - len..block_end);
        for (note, &fate) in notes.notes[window.clone()].iter().zip(&fates[window]) {
            if !self.accepts(note.channel, note.key) || fate == Fate::Silent {
                continue;
            }
            // The item end only sends a note-off, which a one-shot sample ignores.
            let mut stop = note.start + len;
            if self.settings.obey_note_offs {
                stop = stop.min(note.end);
            }
            let fade = match fate {
                Fate::FadesFrom(block) => {
                    stop = stop.min(block + TAKEOVER_FADE);
                    Some(block)
                }
                _ => None,
            };
            let vg = self.velocity_gain(note.velocity);
            let (gl, gr) = (self.gain.0 * vg, self.gain.1 * vg);
            let attack = (self.settings.attack * ctx.sample_rate as f64) as i64;
            for f in block_start.max(note.start)..block_end.min(stop) {
                let rel = f - note.start;
                let s = self.range.0 + rel as usize;
                let mut env = if rel < attack { rel as f32 / attack as f32 } else { 1.0 };
                if let Some(fade) = fade.filter(|&b| f >= b) {
                    env *= 1.0 - (f - fade) as f32 / TAKEOVER_FADE as f32;
                }
                let o = (f - block_start) as usize;
                left[o] += lc[s] * gl * env;
                right[o] += rc[s] * gr * env;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::SynthNote;

    #[test]
    fn parses_path_and_parameters() {
        let mut d = b"/kit/kick.wav\0".to_vec();
        for v in [
            0.5,
            0.25,
            1.0,
            36.0 / 127.0,
            36.0 / 127.0,
            0.06875,
            0.86875,
            10.0 / 16.0,
            0.0,
            0.0005,
            0.0005,
            0.0,
            0.0,
            0.1,
            0.9,
        ] {
            d.extend(f64::to_le_bytes(v));
        }
        let s = Rs5kSettings::parse(&d).unwrap();
        assert_eq!(s.attack, 0.001);
        assert_eq!(s.file, "/kit/kick.wav");
        assert_eq!((s.note_lo, s.note_hi, s.channel), (36, 36, 10));
        assert_eq!((s.volume, s.pan, s.start, s.end), (0.5, 0.25, 0.1, 0.9));
    }

    #[test]
    fn plays_matching_notes_from_their_start() {
        let settings = Rs5kSettings {
            file: "x".into(),
            volume: 1.0,
            pan: 0.5,
            min_velocity_gain: 1.0,
            note_lo: 36,
            note_hi: 36,
            channel: 0,
            obey_note_offs: false,
            max_voices: 16,
            velocities: (1, 127),
            attack: 0.0,
            start: 0.0,
            end: 1.0,
        };
        let audio = AudioData { sample_rate: 100, channels: vec![(1..=10).map(|i| i as f32).collect()] };
        let mut s = Rs5kCompat {
            settings,
            sample: Arc::new(audio),
            gain: (1.0, 1.0),
            range: (0, 10),
            voices: Vec::new(),
            events: Vec::new(),
            plan: None,
        };
        let note = |start, key| SynthNote { start, end: start + 1, cutoff: i64::MAX, channel: 9, key, velocity: 100 };
        let notes = crate::synth::NoteList::new(vec![note(5, 36), note(6, 38)], 100);
        let (mut l, mut r) = (vec![0.0; 20], vec![0.0; 20]);
        let ctx = ProcessContext {
            midi: None,
            midi_from: 0,
            sample_rate: 100,
            position: 0,
            playing: true,
            notes: Some(&notes),
            live: &[],
            live_channel: None,
            aux: None,
            bpm: 120.0,
        };
        s.process(&mut l, &mut r, &ctx);
        assert_eq!(&l[4..8], &[0.0, 1.0, 2.0, 3.0]);
        assert_eq!(l[15], 0.0);

        // MIDI an earlier effect sent replaces the track's notes, and voices carry on into the
        // next block.
        let chain = crate::fx::MidiBlock {
            short: vec![(3, [0x99, 36, 100]), (4, [0x99, 38, 100])],
            long: Vec::new(),
            bused: Vec::new(),
        };
        let (mut l, mut r) = (vec![0.0; 6], vec![0.0; 6]);
        s.process(&mut l, &mut r, &ProcessContext { midi: Some(&chain), ..ctx });
        assert_eq!(l, [0.0, 0.0, 0.0, 1.0, 2.0, 3.0]);
        let (mut l, mut r) = (vec![0.0; 10], vec![0.0; 10]);
        let none = crate::fx::MidiBlock::default();
        s.process(&mut l, &mut r, &ProcessContext { midi: Some(&none), position: 6, ..ctx });
        assert_eq!(&l[..8], &[4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 0.0]);
    }
}
