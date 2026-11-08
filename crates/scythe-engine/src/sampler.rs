//! One-shot sampler that reads the settings ReaSamplOmatic5000 stores: the sample file, volume,
//! pan, note range, MIDI channel, velocity response and sample offsets. Like the preview synth
//! it is stateless: a voice is a function of the time since its note-on.

use std::path::Path;
use std::sync::Arc;

use scythe_core::units::PanLaw;

use crate::fx::{Effect, ProcessContext};
use crate::media::{AudioData, decode_file, resample};

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
        let audio = decode_file(&path).map_err(|e| format!("sample: {e}"))?;
        let audio = if audio.sample_rate == sample_rate {
            audio
        } else {
            resample(&audio, sample_rate).map_err(|e| e.to_string())?
        };
        let frames = audio.frames();
        let range = ((settings.start * frames as f64) as usize, ((settings.end * frames as f64) as usize).min(frames));
        let (l, r) = PanLaw::default().gains(settings.pan * 2.0 - 1.0);
        let gain = ((l * settings.volume) as f32, (r * settings.volume) as f32);
        Ok(Self { settings, sample: Arc::new(audio), gain, range })
    }

    fn velocity_gain(&self, velocity: u8) -> f32 {
        let t = (velocity.max(1) as f64 - 1.0) / 126.0;
        (self.settings.min_velocity_gain + (1.0 - self.settings.min_velocity_gain) * t) as f32
    }
}

impl Effect for Rs5kCompat {
    fn is_instrument(&self) -> bool {
        true
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], ctx: &ProcessContext) {
        let Some(notes) = ctx.notes else { return };
        let len = self.range.1.saturating_sub(self.range.0) as i64;
        let n = left.len() as i64;
        let (block_start, block_end) = (ctx.position, ctx.position + n);
        let lc = &self.sample.channels[0];
        let rc = self.sample.channels.get(1).unwrap_or(lc);
        let plays = |note: &crate::synth::SynthNote| {
            (self.settings.channel == 0 || note.channel + 1 == self.settings.channel)
                && (self.settings.note_lo..=self.settings.note_hi).contains(&note.key)
        };
        for note in notes.starting_in(block_start - len..block_end) {
            if !plays(note) {
                continue;
            }
            // The item end only sends a note-off, which a one-shot sample ignores.
            let mut stop = note.start + len;
            if self.settings.obey_note_offs {
                stop = stop.min(note.end);
            }
            let vg = self.velocity_gain(note.velocity);
            let (gl, gr) = (self.gain.0 * vg, self.gain.1 * vg);
            let attack = (self.settings.attack * ctx.sample_rate as f64) as i64;
            for f in block_start.max(note.start)..block_end.min(stop) {
                let rel = f - note.start;
                let s = self.range.0 + rel as usize;
                let env = if rel < attack { rel as f32 / attack as f32 } else { 1.0 };
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
            attack: 0.0,
            start: 0.0,
            end: 1.0,
        };
        let audio = AudioData { sample_rate: 100, channels: vec![(1..=10).map(|i| i as f32).collect()] };
        let mut s = Rs5kCompat { settings, sample: Arc::new(audio), gain: (1.0, 1.0), range: (0, 10) };
        let note = |start, key| SynthNote { start, end: start + 1, cutoff: i64::MAX, channel: 9, key, velocity: 100 };
        let notes = crate::synth::NoteList::new(vec![note(5, 36), note(6, 38)], 100);
        let (mut l, mut r) = (vec![0.0; 20], vec![0.0; 20]);
        let ctx = ProcessContext { sample_rate: 100, position: 0, playing: true, notes: Some(&notes), bpm: 120.0 };
        s.process(&mut l, &mut r, &ctx);
        assert_eq!(&l[4..8], &[0.0, 1.0, 2.0, 3.0]);
        assert_eq!(l[15], 0.0);
    }
}
