//! Takes whose speed or pitch changes over the item: stretch markers (with or without slopes)
//! and pitch envelopes. Such a take is rendered once, for the item's length at the engine
//! rate, and then plays like a take at rate 1.
//!
//! Without pitch preservation and without a pitch change the source is read with REAPER's
//! sinc at the position the markers give, the kernel following the local speed, so a
//! resampled take with markers nulls like one with a plain play rate. Anything that needs the
//! pitch shifter is cut into pieces of near-constant speed and pitch, each run through the
//! shifter with some source on either side and joined with short crossfades.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use scythe_core::render_settings::ResampleMode;
use scythe_core::{Item, Processing, Source, Take};

use crate::media::{AudioData, SincKernel};

/// Output frames per piece where speed or pitch keep changing.
const PIECE: usize = 4096;
/// Source frames run through the shifter on either side of a piece.
const MARGIN: usize = 4096;
/// Crossfade between pieces, in output frames.
const FADE: usize = 256;

/// What decides the sound of a warped take.
#[derive(Debug, Clone, PartialEq)]
pub struct Warp {
    take: Take,
    length: f64,
    looped: bool,
    mode: i32,
}

impl Warp {
    /// The warp of `take` in `item`, when it has stretch markers or a pitch envelope.
    pub fn of(item: &Item, take: &Take, default_mode: i32) -> Option<Self> {
        take.is_warped().then(|| Self {
            take: take.clone(),
            length: item.length,
            looped: item.looped,
            mode: if take.pitch_mode < 0 { default_mode } else { take.pitch_mode },
        })
    }

    /// Identifies the rendered audio, for caches.
    pub fn key(&self) -> u64 {
        let mut h = DefaultHasher::new();
        let t = &self.take;
        for m in &t.stretch_markers {
            (m.pos.to_bits(), m.src.to_bits(), m.slope.to_bits()).hash(&mut h);
        }
        (t.playrate.to_bits(), t.source_offset.to_bits(), t.preserve_pitch, t.pitch.to_bits()).hash(&mut h);
        if let Some(env) = t.pitch_envelope() {
            for p in &env.points {
                (p.time.to_bits(), p.value.to_bits(), format!("{:?}", p.shape)).hash(&mut h);
            }
        }
        if let Source::Section(s) = &t.source {
            (s.start.to_bits(), s.length.to_bits(), s.reverse).hash(&mut h);
        }
        (self.length.to_bits(), self.looped, self.mode).hash(&mut h);
        h.finish()
    }
}

/// The part of the file a take plays, with its section, reverse and loop settings.
struct View<'a> {
    audio: &'a AudioData,
    start: f64,
    len: f64,
    reverse: bool,
    looped: bool,
}

impl<'a> View<'a> {
    fn new(audio: &'a AudioData, source: &Source, looped: bool) -> Self {
        let frames = audio.frames() as f64;
        let sr = audio.sample_rate as f64;
        let (start, len, reverse) = match source {
            Source::Section(s) => {
                let start = (s.start * sr).clamp(0.0, frames);
                let avail = frames - start;
                (start, if s.length > 0.0 { (s.length * sr).min(avail) } else { avail }, s.reverse)
            }
            _ => (0.0, frames, false),
        };
        Self { audio, start, len, reverse, looped }
    }

    /// The file position of view position `p`, `None` where the take is silent.
    fn place(&self, p: f64) -> Option<f64> {
        let p = if self.looped && self.len > 0.0 { p.rem_euclid(self.len) } else { p };
        (0.0..self.len).contains(&p).then_some(if self.reverse {
            self.start + self.len - 1.0 - p
        } else {
            self.start + p
        })
    }

    /// View frames `lo..hi`, silent outside the view.
    fn frames(&self, lo: i64, hi: i64) -> AudioData {
        let channels = self
            .audio
            .channels
            .iter()
            .map(|ch| {
                (lo..hi)
                    .map(|p| self.place(p as f64).and_then(|f| ch.get(f.round() as usize).copied()).unwrap_or(0.0))
                    .collect()
            })
            .collect();
        AudioData { sample_rate: self.audio.sample_rate, channels }
    }
}

/// Sinc kernels by read step, built as the speed calls for them.
struct Kernels {
    half: usize,
    tabled: bool,
    built: HashMap<i64, SincKernel>,
}

impl Kernels {
    fn new(mode: ResampleMode) -> Self {
        let (half, tabled) = match mode {
            ResampleMode::Sinc { half, tabled } => (half as usize, tabled),
            _ => (96, true),
        };
        Self { half, tabled, built: HashMap::new() }
    }

    /// The kernel for reading `step` frames apart, by its key in `built`.
    fn key(&mut self, step: f64) -> i64 {
        // Steps under 1 all use the full band.
        let key = (step.max(1.0) * 1e4).round() as i64;
        let (half, tabled) = (self.half, self.tabled);
        self.built.entry(key).or_insert_with(|| SincKernel::new(key as f64 / 1e4, half, tabled));
        key
    }

    fn get(&mut self, step: f64) -> &SincKernel {
        let key = self.key(step);
        &self.built[&key]
    }
}

/// Renders `warp` from `base` (the file at the engine rate): the item's length at rate 1.
pub fn render(base: &AudioData, warp: &Warp, resample: ResampleMode) -> AudioData {
    let sr = base.sample_rate as f64;
    let frames = (warp.length * sr).ceil().max(0.0) as usize;
    let view = View::new(base, &warp.take.source, warp.looped);
    let take = &warp.take;
    let mut kernels = Kernels::new(resample);
    let mut out = vec![vec![0.0f32; frames]; base.channels.len()];
    let source_pos = |rel: f64| take.source_pos(rel);
    let shifts = take.preserve_pitch || take.pitch != 0.0 || take.pitch_envelope().is_some();
    if !shifts {
        let reads: Vec<Option<(f64, i64)>> = (0..frames)
            .map(|j| {
                let rel = j as f64 / sr;
                Some((view.place(source_pos(rel) * sr)?, kernels.key(take.speed_at(rel).abs())))
            })
            .collect();
        for (o, ch) in out.iter_mut().zip(&base.channels) {
            for (s, read) in o.iter_mut().zip(&reads) {
                if let Some((at, key)) = read {
                    *s = kernels.built[key].sample(ch, *at);
                }
            }
        }
        return AudioData { sample_rate: base.sample_rate, channels: out };
    }
    let src = |j: usize| source_pos(j as f64 / sr) * sr;
    let pitch = |j: f64| {
        let env = take.pitch_envelope().and_then(|e| e.value_at(j / sr)).unwrap_or(0.0);
        2f64.powf((take.pitch + env) / 12.0)
    };
    let pieces = pieces(take, frames, sr, &src, &pitch);
    let count = pieces.len();
    for (n, &(a, b)) in pieces.iter().enumerate() {
        let (sa, sb) = (src(a), src(b));
        let speed = (sb - sa) / (b - a) as f64;
        if speed.is_nan() || speed <= 1e-6 {
            continue;
        }
        let tail = if n + 1 < count { FADE } else { 0 };
        let len = (b - a + tail).min(frames - a);
        let lo = sa.floor() as i64 - MARGIN as i64;
        let hi = (sb + tail as f64 * speed).ceil() as i64 + MARGIN as i64 + 1;
        let span = view.frames(lo, hi);
        let offset = sa - lo as f64;
        let p = pitch((a + b) as f64 / 2.0);
        let piece: Vec<Vec<f32>> = if take.preserve_pitch {
            let time = 1.0 / speed;
            let done = crate::stretch::apply(&span, &Processing { time, pitch: p, rate: 1.0, mode: warp.mode });
            let first = (offset * time).round() as usize;
            done.channels
                .iter()
                .map(|ch| (0..len).map(|k| ch.get(first + k).copied().unwrap_or(0.0)).collect())
                .collect()
        } else {
            let shifted = if (p - 1.0).abs() > 1e-9 {
                crate::stretch::apply(&span, &Processing { time: 1.0, pitch: p, rate: 1.0, mode: warp.mode })
            } else {
                span
            };
            let kernel = kernels.get(speed);
            shifted
                .channels
                .iter()
                .map(|ch| (0..len).map(|k| kernel.sample(ch, offset + k as f64 * speed)).collect())
                .collect()
        };
        for (c, ch) in piece.iter().enumerate() {
            for (k, &s) in ch.iter().enumerate() {
                let mut w = 1.0;
                if n > 0 && k < FADE {
                    w = k as f32 / FADE as f32;
                }
                if k >= b - a {
                    w = 1.0 - (k - (b - a)) as f32 / FADE as f32;
                }
                out[c][a + k] += s * w;
            }
        }
    }
    AudioData { sample_rate: base.sample_rate, channels: out }
}

/// Output ranges of near-constant speed and pitch: cut at the stretch markers and every
/// `PIECE` frames, with neighbours that play alike joined again.
fn pieces(
    take: &Take,
    frames: usize,
    sr: f64,
    src: &dyn Fn(usize) -> f64,
    pitch: &dyn Fn(f64) -> f64,
) -> Vec<(usize, usize)> {
    let rate = if take.playrate > 0.0 { take.playrate } else { 1.0 };
    let mut cuts: Vec<usize> = (0..frames).step_by(PIECE).chain([frames]).collect();
    cuts.extend(
        take.stretch_markers
            .iter()
            .map(|m| (m.pos / rate * sr).round())
            .filter(|&f| f > 0.0 && f < frames as f64)
            .map(|f| f as usize),
    );
    cuts.sort_unstable();
    cuts.dedup();
    let mut pieces: Vec<(usize, usize, f64, f64)> = Vec::new();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let speed = (src(b) - src(a)) / (b - a) as f64;
        let p = pitch((a + b) as f64 / 2.0);
        match pieces.last_mut() {
            Some(last) if (last.2 - speed).abs() < 1e-9 && (last.3 - p).abs() < 1e-9 => last.1 = b,
            Some(last) if b - a < FADE => last.1 = b,
            _ => pieces.push((a, b, speed, p)),
        }
    }
    pieces.into_iter().map(|(a, b, _, _)| (a, b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use scythe_core::stretch::StretchMarker;
    use std::path::PathBuf;

    fn ramp(frames: usize) -> AudioData {
        AudioData { sample_rate: 1000, channels: vec![(0..frames).map(|i| i as f32 / 1000.0).collect()] }
    }

    fn warp(markers: Vec<StretchMarker>, preserve: bool) -> Warp {
        let mut take = Take::from_file(PathBuf::from("x.wav"));
        take.stretch_markers = markers;
        take.preserve_pitch = preserve;
        let item = Item::new(0.0, 2.0, take.clone());
        Warp::of(&item, &take, 0).unwrap()
    }

    #[test]
    fn resampled_markers_read_where_they_point() {
        let w = warp(vec![StretchMarker::new(0.5, 0.8), StretchMarker::new(1.5, 1.3)], false);
        let out = render(&ramp(4000), &w, ResampleMode::default());
        for (t, want) in [(0.25, 0.55), (1.0, 1.05), (1.75, 1.55)] {
            let got = out.channels[0][(t * 1000.0) as usize];
            assert!((got - want).abs() < 1e-3, "{t}: {got} vs {want}");
        }
    }

    #[test]
    fn stretched_pieces_keep_the_level_and_place() {
        let w = warp(vec![StretchMarker::new(0.0, 0.0), StretchMarker::new(1.0, 0.5)], true);
        let tone = AudioData {
            sample_rate: 8000,
            channels: vec![(0..24000).map(|i| (i as f32 * 0.25).sin() * 0.5).collect()],
        };
        let out = render(&tone, &w, ResampleMode::default());
        assert_eq!(out.frames(), 16000);
        let peak = out.channels[0][2000..6000].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.5).abs() < 0.1, "peak {peak}");
    }

    #[test]
    fn keys_follow_what_changes_the_sound() {
        let a = warp(vec![StretchMarker::new(0.5, 0.8)], false);
        let mut b = a.clone();
        assert_eq!(a.key(), b.key());
        b.take.stretch_markers[0].slope = 0.2;
        assert_ne!(a.key(), b.key());
        b.take.volume = 0.5;
        let mut c = a.clone();
        c.take.volume = 0.5;
        assert_eq!(a.key(), c.key());
    }
}
