use std::collections::HashMap;
use std::sync::Arc;

use scythe_core::tempo::Timeline;
use scythe_core::units::PanLaw;
use scythe_core::{Envelope, EnvelopeKind, Fade, Project, Source, TrackId};

use crate::fx::{FxInstance, FxRegistry, ParamAutomation, ProcessContext};
use crate::media::{AudioData, MediaPool};
use rayon::prelude::*;

use crate::loudness::Loudness;
use crate::meter::Meter;
use crate::synth::{NoteList, SynthNote};

/// One item, pre-resolved to frames at the engine rate.
#[derive(Debug, Clone)]
pub struct RenderItem {
    pub start: i64,
    pub length: i64,
    /// Source position (in source frames) at `start`.
    pub source_offset: f64,
    pub playrate: f64,
    pub gain_l: f32,
    pub gain_r: f32,
    pub fade_in: (i64, Fade),
    pub fade_out: (i64, Fade),
    pub looped: bool,
    /// Range of the source the item plays, in source frames; a zero length means up to the end.
    pub section_start: f64,
    pub section_len: f64,
    pub reverse: bool,
    pub audio: ItemAudio,
}

#[derive(Debug, Clone)]
pub enum ItemAudio {
    Media(Arc<AudioData>),
    /// A metronome source (RPP `<SOURCE CLICK`): clicks on every beat of the tempo map.
    Click,
    /// A MIDI source played by the built-in preview instrument.
    Midi(Arc<NoteList>),
}

/// Synthesised metronome clicks at the engine rate.
#[derive(Debug)]
pub struct ClickSounds {
    pub accent: Vec<f32>,
    pub beat: Vec<f32>,
}

impl ClickSounds {
    pub fn new(sample_rate: u32) -> Self {
        let tone = |freq: f64, gain: f64| {
            let sr = sample_rate as f64;
            let n = (sr * 0.03) as usize;
            (0..n)
                .map(|i| {
                    let t = i as f64 / sr;
                    let env = (-t * 160.0).exp() * (i as f64 / 24.0).min(1.0);
                    (gain * env * (std::f64::consts::TAU * freq * t).sin()) as f32
                })
                .collect()
        };
        Self { accent: tone(1760.0, 0.6), beat: tone(1320.0, 0.45) }
    }
}

#[derive(Debug, Clone)]
pub struct RenderTrack {
    pub id: TrackId,
    pub gain_l: f32,
    pub gain_r: f32,
    pub audible: bool,
    pub items: Vec<RenderItem>,
    pub meter: Arc<Meter>,
    /// (index into `RenderGraph::tracks`, gain l, gain r)
    pub sends: Vec<(usize, f32, f32)>,
    /// Where the post-fader output goes: the parent folder, or the master when `None`.
    pub parent: Option<usize>,
    pub main_send: bool,
    pub automation: Option<Arc<Automation>>,
    pub fx: Vec<Arc<FxInstance>>,
    /// Parameter envelopes of each effect in `fx`.
    pub fx_automation: Vec<Vec<ParamAutomation>>,
    /// MIDI of the track's items when an instrument in the chain plays it.
    pub notes: Option<Arc<NoteList>>,
    /// Live input channels (left, optional right) for an armed, monitored track.
    pub monitor: Option<(usize, Option<usize>)>,
}

/// Live input for one render call: per hardware channel, the samples aligned with the output.
pub struct LiveInput<'a> {
    pub channels: [Option<&'a [f32]>; LiveInput::MAX_CHANNELS],
    pub play_items: bool,
}

impl Default for LiveInput<'_> {
    fn default() -> Self {
        Self { channels: [None; Self::MAX_CHANNELS], play_items: true }
    }
}

impl<'a> LiveInput<'a> {
    pub const MAX_CHANNELS: usize = 8;

    fn slice(&self, from: usize, len: usize) -> LiveInput<'a> {
        LiveInput {
            channels: self.channels.map(|c| c.and_then(|s| s.get(from..(from + len).min(s.len())))),
            play_items: self.play_items,
        }
    }
}

/// Active track envelopes and what they modulate. Gains are evaluated every
/// `Automation::STEP` frames and ramped in between.
#[derive(Debug)]
pub struct Automation {
    pub volume: f64,
    pub pan: f64,
    pub pan_law: PanLaw,
    /// Envelopes whose values multiply the volume (volume, pre-FX volume, trim).
    pub gains: Vec<Envelope>,
    pub pan_env: Option<Envelope>,
    pub mute: Option<Envelope>,
}

impl Automation {
    const STEP: usize = 32;

    fn gains_at(&self, t: f64) -> (f32, f32) {
        let mut vol = self.volume;
        for e in &self.gains {
            vol *= e.value_at(t).unwrap_or(1.0);
        }
        if self.mute.as_ref().and_then(|e| e.value_at(t)).is_some_and(|v| v < 0.5) {
            vol = 0.0;
        }
        let pan = self.pan - self.pan_env.as_ref().and_then(|e| e.value_at(t)).unwrap_or(0.0);
        let (l, r) = self.pan_law.gains(pan.clamp(-1.0, 1.0));
        ((vol * l) as f32, (vol * r) as f32)
    }

    /// Whether any envelope has a point in `(t0, t1]`, where ramping would smear a step.
    fn point_between(&self, t0: f64, t1: f64) -> bool {
        self.gains.iter().chain(&self.pan_env).chain(&self.mute).any(|e| {
            let i = e.points.partition_point(|p| p.time <= t0);
            e.points.get(i).is_some_and(|p| p.time <= t1)
        })
    }

    fn from_track(t: &scythe_core::Track, pan_law: PanLaw) -> Option<Self> {
        let active = |kind| {
            t.envelope(kind).filter(|e| e.active && !e.points.is_empty()).map(|e| Envelope { raw: None, ..e.clone() })
        };
        let gains: Vec<Envelope> = [EnvelopeKind::Volume, EnvelopeKind::VolumePreFx, EnvelopeKind::Trim]
            .into_iter()
            .filter_map(active)
            .collect();
        let pan_env = active(EnvelopeKind::Pan);
        let mute = active(EnvelopeKind::Mute);
        if gains.is_empty() && pan_env.is_none() && mute.is_none() {
            return None;
        }
        Some(Self { volume: t.volume, pan: t.pan, pan_law, gains, pan_env, mute })
    }
}

/// Immutable snapshot of everything the audio thread needs. Rebuilt on the UI side whenever the
/// project changes and swapped in atomically.
#[derive(Debug, Clone)]
pub struct RenderGraph {
    pub sample_rate: u32,
    pub tracks: Vec<RenderTrack>,
    /// Tracks in an order where every send source precedes its destination.
    pub order: Vec<usize>,
    /// Dependency level of every track: tracks only receive audio from lower levels.
    pub level_of: Vec<usize>,
    pub level_sizes: Vec<usize>,
    pub level_count: usize,
    pub master_gain_l: f32,
    pub master_gain_r: f32,
    pub master_meter: Arc<Meter>,
    pub master_loudness: Option<Arc<Loudness>>,
    pub master_fx: Vec<Arc<FxInstance>>,
    pub timeline: Arc<Timeline>,
    pub clicks: Arc<ClickSounds>,
    /// Gain of the transport metronome; zero when it is off.
    pub metronome: f32,
}

/// Scratch space owned by the audio callback so rendering never allocates.
#[derive(Debug)]
pub struct RenderScratch {
    track_bufs: Vec<(Vec<f32>, Vec<f32>)>,
    capacity: usize,
}

impl RenderScratch {
    pub const MAX_BLOCK: usize = 2048;

    pub fn new(max_tracks: usize) -> Self {
        Self {
            track_bufs: (0..max_tracks).map(|_| (vec![0.0; Self::MAX_BLOCK], vec![0.0; Self::MAX_BLOCK])).collect(),
            capacity: Self::MAX_BLOCK,
        }
    }

    fn ensure(&mut self, tracks: usize) -> bool {
        self.track_bufs.len() >= tracks
    }
}

/// Meters live as long as the track does across graph rebuilds, so the UI can keep its handle.
#[derive(Debug, Default)]
pub struct MeterRegistry {
    tracks: HashMap<TrackId, Arc<Meter>>,
    pub master: Arc<Meter>,
    pub loudness: Option<Arc<Loudness>>,
}

impl MeterRegistry {
    pub fn track(&mut self, id: TrackId) -> Arc<Meter> {
        self.tracks.entry(id).or_default().clone()
    }

    /// The master loudness meter for `sample_rate`, created on first use.
    pub fn loudness(&mut self, sample_rate: u32) -> Arc<Loudness> {
        self.loudness.get_or_insert_with(|| Arc::new(Loudness::new(sample_rate))).clone()
    }

    pub fn get(&self, id: TrackId) -> Option<&Arc<Meter>> {
        self.tracks.get(&id)
    }

    pub fn retain(&mut self, project: &Project) {
        self.tracks.retain(|id, _| project.track(*id).is_some());
    }
}

impl RenderGraph {
    pub fn empty(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            tracks: Vec::new(),
            order: Vec::new(),
            level_of: Vec::new(),
            level_sizes: Vec::new(),
            level_count: 0,
            master_gain_l: 1.0,
            master_gain_r: 1.0,
            master_meter: Arc::default(),
            master_loudness: None,
            master_fx: Vec::new(),
            timeline: Arc::new(scythe_core::TempoMap::default().timeline()),
            clicks: Arc::new(ClickSounds::new(sample_rate)),
            metronome: 0.0,
        }
    }

    /// Items whose media is not decoded yet are skipped; the caller rebuilds once it is.
    pub fn build(
        project: &Project,
        pool: &MediaPool,
        meters: &mut MeterRegistry,
        fx: &mut FxRegistry,
        sample_rate: u32,
    ) -> Self {
        let sr = sample_rate as f64;
        let index: HashMap<TrackId, usize> = project.tracks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
        let timeline = Arc::new(project.tempo.timeline());
        let parents = project.folder_parents();
        let tracks = project
            .tracks
            .iter()
            .enumerate()
            .map(|(ti, t)| {
                let (pl, pr) = project.pan_law_for(t).gains(t.pan);
                let items = t
                    .items
                    .iter()
                    .filter(|it| !it.muted && it.length > 0.0)
                    .filter_map(|it| {
                        let take = it.take()?;
                        let (section_start, section_len, reverse) = match &take.source {
                            Source::Section(s) => (s.start, s.length.max(0.0), s.reverse),
                            _ => (0.0, 0.0, false),
                        };
                        let audio = match &take.source {
                            Source::Raw(c) if c.args().first().is_some_and(|k| k == "CLICK") => ItemAudio::Click,
                            Source::Raw(c) if c.args().first().is_some_and(|k| k == "MIDI") => {
                                ItemAudio::Midi(Arc::new(midi_notes(c, it, take, &timeline, sample_rate)?))
                            }
                            source => {
                                let path = scythe_core::rpp::resolve_media_path(project, source.file()?);
                                ItemAudio::Media(pool.ready(&path)?.audio.clone())
                            }
                        };
                        // Take pan never attenuates the centre in REAPER renders, whatever the project law.
                        let (tl, tr) = scythe_core::units::PanLaw::default().gains(take.pan);
                        let src_sr = sr;
                        let g = (it.volume * take.volume) as f32;
                        Some(RenderItem {
                            start: (it.position * sr).round() as i64,
                            length: (it.length * sr).round() as i64,
                            source_offset: take.source_offset * sr,
                            playrate: take.playrate,
                            gain_l: g * tl as f32,
                            gain_r: g * tr as f32,
                            fade_in: ((it.fade_in.effective_length() * sr).round() as i64, it.fade_in),
                            fade_out: ((it.fade_out.effective_length() * sr).round() as i64, it.fade_out),
                            looped: it.looped,
                            section_start: section_start * src_sr,
                            section_len: section_len * src_sr,
                            reverse,
                            audio,
                        })
                    })
                    .collect::<Vec<_>>();
                let (chain, fx_automation) = fx.chain_with_automation(Some(t.id), &t.fx);
                // With an instrument in the chain, MIDI items feed it instead of the preview synth.
                let (items, notes) = if chain.iter().any(|f| f.instrument) {
                    let (midi, audio): (Vec<_>, Vec<_>) =
                        items.into_iter().partition(|i| matches!(i.audio, ItemAudio::Midi(_)));
                    let merged = midi
                        .iter()
                        .flat_map(|i| match &i.audio {
                            ItemAudio::Midi(list) => list.notes.clone(),
                            _ => Vec::new(),
                        })
                        .collect();
                    (audio, Some(Arc::new(NoteList::new(merged, sample_rate))))
                } else {
                    (items, None)
                };
                let sends = t
                    .sends
                    .iter()
                    .filter(|s| !s.muted)
                    .filter_map(|s| {
                        let (l, r) = project.pan_law.gains(s.pan);
                        Some((*index.get(&s.dest)?, (s.volume * l) as f32, (s.volume * r) as f32))
                    })
                    .collect();
                RenderTrack {
                    id: t.id,
                    gain_l: (t.volume * pl) as f32,
                    gain_r: (t.volume * pr) as f32,
                    audible: project.is_audible(t),
                    items,
                    meter: meters.track(t.id),
                    sends,
                    parent: parents[ti],
                    main_send: t.main_send,
                    automation: Automation::from_track(t, project.pan_law_for(t)).map(Arc::new),
                    fx: chain,
                    fx_automation,
                    notes,
                    monitor: (t.armed && t.monitor && t.rec_input >= 0 && t.rec_input & (512 | 2048 | 4096) == 0).then(
                        || {
                            let first = (t.rec_input & 511) as usize;
                            (first, (t.rec_input & 1024 != 0).then_some(first + 1))
                        },
                    ),
                }
            })
            .collect::<Vec<_>>();
        let order = send_order(&tracks);
        let level_of = dependency_levels(&tracks, &order);
        let level_count = level_of.iter().max().map_or(0, |m| m + 1);
        let mut level_sizes = vec![0; level_count];
        for &l in &level_of {
            level_sizes[l] += 1;
        }
        // REAPER renders show no centre attenuation on the master, whatever the project law.
        let (ml, mr) = scythe_core::units::PanLaw::default().gains(project.master.pan);
        let mv = if project.master.muted { 0.0 } else { project.master.volume };
        Self {
            sample_rate,
            order,
            level_of,
            level_sizes,
            level_count,
            tracks,
            master_gain_l: (mv * ml) as f32,
            master_gain_r: (mv * mr) as f32,
            master_meter: meters.master.clone(),
            master_loudness: Some(meters.loudness(sample_rate)),
            master_fx: fx.chain(None, &project.master.fx),
            timeline,
            clicks: Arc::new(ClickSounds::new(sample_rate)),
            metronome: 0.0,
        }
    }

    /// Mixes `out_l.len()` frames starting at project frame `start` into `out_l`/`out_r`
    /// (overwriting them). Blocks longer than `RenderScratch::MAX_BLOCK` are split.
    pub fn render(&self, start: i64, out_l: &mut [f32], out_r: &mut [f32], scratch: &mut RenderScratch) {
        self.render_with(start, out_l, out_r, scratch, &LiveInput::default());
    }

    /// Whether any track listens to the live input.
    pub fn monitoring(&self) -> bool {
        self.tracks.iter().any(|t| t.monitor.is_some())
    }

    /// Like `render`, with live input for monitored tracks. With `play_items` false only the
    /// live input and effect tails sound (the transport is stopped).
    pub fn render_with(
        &self,
        start: i64,
        out_l: &mut [f32],
        out_r: &mut [f32],
        scratch: &mut RenderScratch,
        input: &LiveInput,
    ) {
        let n = out_l.len().min(out_r.len());
        let mut done = 0;
        while done < n {
            let len = (n - done).min(scratch.capacity);
            let block = input.slice(done, len);
            self.render_block(
                start + done as i64,
                &mut out_l[done..done + len],
                &mut out_r[done..done + len],
                scratch,
                &block,
            );
            done += len;
        }
    }

    fn render_block(
        &self,
        start: i64,
        out_l: &mut [f32],
        out_r: &mut [f32],
        scratch: &mut RenderScratch,
        input: &LiveInput,
    ) {
        out_l.fill(0.0);
        out_r.fill(0.0);
        let n = out_l.len();
        if !scratch.ensure(self.tracks.len()) {
            // Graph grew beyond what the callback preallocated; stay silent rather than allocate.
            return;
        }
        for (l, r) in scratch.track_bufs.iter_mut().take(self.tracks.len()) {
            l[..n].fill(0.0);
            r[..n].fill(0.0);
        }
        let tracks = self.tracks.len();
        for level in 0..self.level_count {
            // Tracks within a level do not feed each other, so they render in parallel; routing
            // into parents, send destinations and the master happens afterwards, in order.
            let bufs = &mut scratch.track_bufs[..tracks];
            let in_level = |i: usize| self.level_of[i] == level;
            if self.level_sizes[level] > 1 {
                bufs.par_iter_mut().enumerate().filter(|(i, _)| in_level(*i)).for_each(|(i, (l, r))| {
                    self.process_track(i, start, &mut l[..n], &mut r[..n], input);
                });
            } else if let Some(i) = (0..tracks).find(|&i| in_level(i)) {
                let (l, r) = &mut bufs[i];
                self.process_track(i, start, &mut l[..n], &mut r[..n], input);
            }
            for &ti in self.order.iter().filter(|&&i| in_level(i)) {
                self.route_track(ti, n, scratch, out_l, out_r);
            }
        }
        if !self.master_fx.is_empty() {
            let ctx = ProcessContext {
                sample_rate: self.sample_rate,
                position: start,
                playing: true,
                notes: None,
                bpm: self.timeline.bpm_at(start as f64 / self.sample_rate as f64),
            };
            for fx in &self.master_fx {
                fx.process(out_l, out_r, &ctx);
            }
        }
        for s in out_l.iter_mut() {
            *s *= self.master_gain_l;
        }
        for s in out_r.iter_mut() {
            *s *= self.master_gain_r;
        }
        self.master_meter.record(out_l, out_r);
        if let Some(l) = &self.master_loudness {
            l.record(out_l, out_r);
        }
        if self.metronome > 0.0 && input.play_items {
            let m = self.metronome;
            self.mix_clicks(start, start..start + n as i64, m, m, out_l, out_r);
        }
    }

    /// Items, FX and fader of one track, in its own buffer.
    fn process_track(&self, ti: usize, start: i64, tl: &mut [f32], tr: &mut [f32], input: &LiveInput) {
        let track = &self.tracks[ti];
        let n = tl.len();
        if let Some((cl, cr)) = track.monitor
            && let Some(left) = input.channels.get(cl).copied().flatten()
        {
            let right = cr.and_then(|c| input.channels.get(c).copied().flatten()).unwrap_or(left);
            for i in 0..n.min(left.len()).min(right.len()) {
                tl[i] += left[i];
                tr[i] += right[i];
            }
        }
        let items: &[RenderItem] = if input.play_items { &track.items } else { &[] };
        for item in items {
            match &item.audio {
                ItemAudio::Media(audio) => mix_item(item, audio, start, tl, tr),
                ItemAudio::Midi(notes) => notes.render(start, self.sample_rate, (item.gain_l, item.gain_r), tl, tr),
                ItemAudio::Click => {
                    let from = start.max(item.start);
                    let to = (start + n as i64).min(item.start + item.length);
                    self.mix_clicks(start, from..to, item.gain_l, item.gain_r, tl, tr);
                }
            }
        }
        if track.audible && !track.fx.is_empty() {
            let ctx = ProcessContext {
                sample_rate: self.sample_rate,
                position: start,
                playing: true,
                // While stopped the position stands still; feeding notes would retrigger them.
                notes: if input.play_items { track.notes.as_deref() } else { None },
                bpm: self.timeline.bpm_at(start as f64 / self.sample_rate as f64),
            };
            let t = start as f64 / self.sample_rate as f64;
            for (fx, automation) in track.fx.iter().zip(&track.fx_automation) {
                if automation.is_empty() {
                    fx.process(tl, tr, &ctx);
                } else {
                    // Automated effects run in short slices so parameter changes land close to
                    // where the envelope puts them.
                    const SLICE: usize = 64;
                    let mut at = 0;
                    while at < n {
                        let len = SLICE.min(n - at);
                        let slice_ctx = ProcessContext { position: start + at as i64, ..ctx };
                        let st = t + at as f64 / self.sample_rate as f64;
                        fx.process_automated(&mut tl[at..at + len], &mut tr[at..at + len], &slice_ctx, automation, st);
                        at += len;
                    }
                }
            }
        }
        match (&track.automation, track.audible) {
            (_, false) => {
                tl.fill(0.0);
                tr.fill(0.0);
            }
            (None, true) => {
                for (sl, sr) in tl.iter_mut().zip(tr.iter_mut()) {
                    *sl *= track.gain_l;
                    *sr *= track.gain_r;
                }
            }
            (Some(auto), true) => {
                let sr_f = self.sample_rate as f64;
                let mut at = 0;
                let mut g0 = auto.gains_at(start as f64 / sr_f);
                while at < n {
                    let len = Automation::STEP.min(n - at);
                    let t0 = (start + at as i64) as f64 / sr_f;
                    let g1 = auto.gains_at((start + (at + len) as i64) as f64 / sr_f);
                    if auto.point_between(t0, t0 + len as f64 / sr_f) {
                        for i in 0..len {
                            let g = auto.gains_at((start + (at + i) as i64) as f64 / sr_f);
                            tl[at + i] *= g.0;
                            tr[at + i] *= g.1;
                        }
                        g0 = g1;
                        at += len;
                        continue;
                    }
                    for i in 0..len {
                        let k = i as f32 / len as f32;
                        tl[at + i] *= g0.0 + (g1.0 - g0.0) * k;
                        tr[at + i] *= g0.1 + (g1.1 - g0.1) * k;
                    }
                    g0 = g1;
                    at += len;
                }
            }
        }
        track.meter.record(tl, tr);
    }

    /// Adds a rendered track to its send destinations and its parent folder or the master.
    fn route_track(&self, ti: usize, n: usize, scratch: &mut RenderScratch, out_l: &mut [f32], out_r: &mut [f32]) {
        let track = &self.tracks[ti];
        let (l, r) = std::mem::take(&mut scratch.track_bufs[ti]);
        let (tl, tr) = (&l[..n], &r[..n]);
        for &(dest, sgl, sgr) in &track.sends {
            let (dl, dr) = &mut scratch.track_bufs[dest];
            for i in 0..n {
                dl[i] += tl[i] * sgl;
                dr[i] += tr[i] * sgr;
            }
        }
        if track.main_send {
            let (dl, dr): (&mut [f32], &mut [f32]) = match track.parent {
                Some(p) => {
                    let (pl, pr) = &mut scratch.track_bufs[p];
                    (&mut pl[..n], &mut pr[..n])
                }
                None => (&mut *out_l, &mut *out_r),
            };
            for i in 0..n {
                dl[i] += tl[i];
                dr[i] += tr[i];
            }
        }
        scratch.track_bufs[ti] = (l, r);
    }

    /// Adds the clicks sounding in project frames `range` to buffers that start at `block_start`.
    fn mix_clicks(
        &self,
        block_start: i64,
        range: std::ops::Range<i64>,
        gain_l: f32,
        gain_r: f32,
        out_l: &mut [f32],
        out_r: &mut [f32],
    ) {
        if range.is_empty() {
            return;
        }
        let sr = self.sample_rate as f64;
        let tail = self.clicks.accent.len().max(self.clicks.beat.len()) as i64;
        let t0 = (range.start - tail) as f64 / sr;
        let t1 = range.end as f64 / sr;
        self.timeline.for_each_beat(t0.max(0.0), t1, |time, downbeat| {
            let sound = if downbeat { &self.clicks.accent } else { &self.clicks.beat };
            let at = (time * sr).round() as i64;
            let from = range.start.max(at);
            let to = range.end.min(at + sound.len() as i64);
            for f in from..to {
                let s = sound[(f - at) as usize];
                let o = (f - block_start) as usize;
                out_l[o] += s * gain_l;
                out_r[o] += s * gain_r;
            }
        });
    }
}

/// Topological order over send and folder edges, so a destination is mixed after all of its
/// sources.
/// Tracks caught in a send cycle are appended in project order; the send that closes the cycle
/// then lands in an already-mixed buffer and is effectively ignored.
/// Level 0 for tracks nothing feeds; otherwise one more than the highest level feeding it.
fn dependency_levels(tracks: &[RenderTrack], order: &[usize]) -> Vec<usize> {
    let mut level = vec![0usize; tracks.len()];
    for &i in order {
        let t = &tracks[i];
        for dest in t.sends.iter().map(|s| s.0).chain(t.parent) {
            level[dest] = level[dest].max(level[i] + 1);
        }
    }
    level
}

fn send_order(tracks: &[RenderTrack]) -> Vec<usize> {
    let n = tracks.len();
    let edges = |t: &RenderTrack| t.sends.iter().map(|s| s.0).chain(t.parent).collect::<Vec<_>>();
    let mut indegree = vec![0usize; n];
    for t in tracks {
        for d in edges(t) {
            indegree[d] += 1;
        }
    }
    let mut ready: Vec<usize> = (0..n).filter(|&i| indegree[i] == 0).rev().collect();
    let mut order = Vec::with_capacity(n);
    while let Some(i) = ready.pop() {
        order.push(i);
        for d in edges(&tracks[i]) {
            indegree[d] -= 1;
            if indegree[d] == 0 {
                ready.push(d);
            }
        }
    }
    for i in 0..n {
        if !order.contains(&i) {
            order.push(i);
        }
    }
    order
}

/// Resolves a MIDI item's notes to project frames, following the tempo map and repeating the
/// source when the item loops.
fn midi_notes(
    chunk: &scythe_core::rpp::Chunk,
    item: &scythe_core::Item,
    take: &scythe_core::Take,
    timeline: &Timeline,
    sample_rate: u32,
) -> Option<NoteList> {
    let source = scythe_core::midi::MidiSource::parse(chunk)?;
    let sr = sample_rate as f64;
    let cutoff = (item.end() * sr).round() as i64;
    let notes = source
        .place(item, take, timeline)
        .into_iter()
        .map(|n| SynthNote {
            start: (n.start * sr).round() as i64,
            end: (n.end * sr).round() as i64,
            cutoff,
            channel: n.channel,
            key: n.key,
            velocity: n.velocity,
        })
        .collect();
    Some(NoteList::new(notes, sample_rate))
}

fn mix_item(item: &RenderItem, audio: &AudioData, block_start: i64, out_l: &mut [f32], out_r: &mut [f32]) {
    let n = out_l.len() as i64;
    let from = block_start.max(item.start);
    let to = (block_start + n).min(item.start + item.length);
    if from >= to {
        return;
    }
    let src_frames = audio.frames() as f64;
    if src_frames == 0.0 {
        return;
    }
    let sec_start = item.section_start.clamp(0.0, src_frames);
    let available = src_frames - sec_start;
    let sec_len = if item.section_len > 0.0 { item.section_len.min(available) } else { available };
    if sec_len <= 0.0 {
        return;
    }
    let left = &audio.channels[0];
    let right = audio.channels.get(1).unwrap_or(left);
    for t in from..to {
        let rel = t - item.start;
        let mut pos = item.source_offset + rel as f64 * item.playrate;
        if item.looped {
            pos = pos.rem_euclid(sec_len);
        }
        if pos < 0.0 || pos >= sec_len {
            continue;
        }
        let pos = if item.reverse { sec_start + (sec_len - 1.0 - pos).max(0.0) } else { sec_start + pos };
        let i = pos as usize;
        let frac = (pos - i as f64) as f32;
        let next = (i + 1).min(audio.frames() - 1);
        let (sl, sr) = if frac == 0.0 {
            (left[i], right[i])
        } else {
            (left[i] + (left[next] - left[i]) * frac, right[i] + (right[next] - right[i]) * frac)
        };
        let fade = fade_gain(rel, item.length, &item.fade_in, &item.fade_out);
        let o = (t - block_start) as usize;
        out_l[o] += sl * item.gain_l * fade;
        out_r[o] += sr * item.gain_r * fade;
    }
}

fn fade_gain(rel: i64, length: i64, fade_in: &(i64, Fade), fade_out: &(i64, Fade)) -> f32 {
    let mut g = 1.0;
    let (len, shape) = fade_in;
    if *len > 0 && rel < *len {
        g *= shape.gain(rel as f64 / *len as f64);
    }
    let to_end = length - rel;
    let (len, shape) = fade_out;
    if *len > 0 && to_end < *len {
        g *= shape.gain(to_end as f64 / *len as f64);
    }
    g as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use scythe_core::{Item, Send, Take};

    fn graph_with(project: &Project, audio: AudioData) -> RenderGraph {
        let audio = Arc::new(audio);
        let mut meters = MeterRegistry::default();
        let pool = MediaPool::default();
        let mut g = RenderGraph::build(project, &pool, &mut meters, &mut FxRegistry::new(100), 100);
        // The pool is empty, so inject media directly; the pool path is covered by engine tests.
        for (gt, pt) in g.tracks.iter_mut().zip(&project.tracks) {
            gt.items = pt
                .items
                .iter()
                .map(|it| RenderItem {
                    start: (it.position * 100.0) as i64,
                    length: (it.length * 100.0) as i64,
                    source_offset: it.takes[0].source_offset * 100.0,
                    playrate: 1.0,
                    gain_l: 1.0,
                    gain_r: 1.0,
                    fade_in: (0, Fade::default()),
                    fade_out: (0, Fade::default()),
                    looped: it.looped,
                    section_start: 0.0,
                    section_len: 0.0,
                    reverse: false,
                    audio: ItemAudio::Media(audio.clone()),
                })
                .collect();
        }
        g
    }

    fn ones(frames: usize) -> AudioData {
        AudioData { sample_rate: 100, channels: vec![vec![1.0; frames]] }
    }

    fn project_with_item(pos: f64, len: f64) -> Project {
        let mut p = Project::default();
        let t = p.add_track("t");
        p.track_mut(t).unwrap().items.push(Item::new(pos, len, Take::from_file("a.wav".into())));
        p
    }

    fn render(g: &RenderGraph, start: i64, n: usize) -> (Vec<f32>, Vec<f32>) {
        let mut l = vec![9.0; n];
        let mut r = vec![9.0; n];
        let mut s = RenderScratch::new(8);
        g.render(start, &mut l, &mut r, &mut s);
        (l, r)
    }

    #[test]
    fn item_plays_only_inside_its_range() {
        let p = project_with_item(0.1, 0.2);
        let g = graph_with(&p, ones(1000));
        let (l, _) = render(&g, 0, 40);
        let expect = 1.0;
        assert_eq!(l[9], 0.0);
        assert!((l[10] - expect).abs() < 1e-6);
        assert!((l[29] - expect).abs() < 1e-6);
        assert_eq!(l[30], 0.0);
    }

    #[test]
    fn mute_and_solo_silence_tracks() {
        let mut p = project_with_item(0.0, 1.0);
        let other = p.add_track("other");
        p.track_mut(other).unwrap().solo = true;
        let g = graph_with(&p, ones(100));
        let (l, _) = render(&g, 0, 10);
        assert!(l.iter().all(|&s| s == 0.0));
        p.track_mut(other).unwrap().solo = false;
        p.tracks[0].muted = true;
        let (l, _) = render(&graph_with(&p, ones(100)), 0, 10);
        assert!(l.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn hard_pan_moves_signal_to_one_side() {
        let mut p = project_with_item(0.0, 1.0);
        p.tracks[0].pan = 1.0;
        let g = graph_with(&p, ones(100));
        let (l, r) = render(&g, 0, 10);
        assert!(l[0].abs() < 1e-6);
        assert!((r[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn source_offset_and_loop() {
        let audio = AudioData { sample_rate: 100, channels: vec![(0..10).map(|i| i as f32).collect()] };
        let mut p = project_with_item(0.0, 0.25);
        p.tracks[0].items[0].takes[0].source_offset = 0.05;
        p.tracks[0].items[0].looped = true;
        p.tracks[0].pan = -1.0;
        let g = graph_with(&p, audio);
        let (l, _) = render(&g, 0, 8);
        assert_eq!(&l[..7], &[5.0, 6.0, 7.0, 8.0, 9.0, 0.0, 1.0]);
    }

    #[test]
    fn sends_feed_destination_track() {
        let mut p = project_with_item(0.0, 1.0);
        let bus = p.add_track("bus");
        let src = p.tracks[0].id;
        p.tracks[0].volume = 0.0;
        p.track_mut(src).unwrap().sends.push(Send { volume: 0.5, ..Send::new(bus) });
        let g = graph_with(&p, ones(100));
        assert_eq!(g.order, [0, 1]);
        let (l, _) = render(&g, 0, 4);
        // Sends are post-fader: the source fader at zero silences the send too.
        assert_eq!(l[0], 0.0);
    }

    #[test]
    fn send_order_respects_edges() {
        let mut p = Project::default();
        let a = p.add_track("a");
        let b = p.add_track("b");
        p.track_mut(b).unwrap().sends.push(Send { volume: 1.0, ..Send::new(a) });
        let g = graph_with(&p, ones(10));
        assert_eq!(g.order, [1, 0]);
    }

    #[test]
    fn folder_children_mix_through_parent() {
        let mut p = project_with_item(0.0, 1.0);
        p.tracks[0].pan = -1.0;
        let bus = p.add_track("bus");
        p.tracks.swap(0, 1);
        p.tracks[0].folder_depth = 1;
        p.tracks[1].folder_depth = -1;
        p.track_mut(bus).unwrap().volume = 0.5;
        let g = graph_with(&p, ones(100));
        assert_eq!(g.tracks[1].parent, Some(0));
        assert_eq!(g.order, [1, 0]);
        let (l, _) = render(&g, 0, 4);
        let expect = 0.5;
        assert!((l[0] - expect).abs() < 1e-6, "{}", l[0]);

        p.tracks[1].main_send = false;
        let (l, _) = render(&graph_with(&p, ones(100)), 0, 4);
        assert_eq!(l[0], 0.0);
    }

    #[test]
    fn fades_ramp() {
        let lin = (10, Fade { shape: 0, ..Fade::new(0.1) });
        assert_eq!(fade_gain(0, 100, &lin, &lin), 0.0);
        assert_eq!(fade_gain(5, 100, &lin, &lin), 0.5);
        assert_eq!(fade_gain(50, 100, &lin, &lin), 1.0);
        assert_eq!(fade_gain(95, 100, &lin, &lin), 0.5);
        let fast = (10, Fade { shape: 1, ..Fade::new(0.1) });
        assert!(fade_gain(5, 100, &fast, &fast) > 0.5);
    }

    #[test]
    fn metronome_clicks_on_beats() {
        let p = Project { tempo: scythe_core::TempoMap::constant(60.0, 4, 4), ..Project::default() };
        let mut g = RenderGraph::build(
            &p,
            &MediaPool::default(),
            &mut MeterRegistry::default(),
            &mut FxRegistry::new(1000),
            1000,
        );
        g.metronome = 1.0;
        let (l, _) = render(&g, 990, 30);
        assert!(l[..10].iter().all(|&s| s == 0.0));
        assert!(l[10..].iter().any(|&s| s != 0.0));
        let (quiet, _) = render(&g, 500, 400);
        assert!(quiet.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn monitored_tracks_hear_the_input() {
        let mut p = project_with_item(0.0, 1.0);
        let live_track = p.add_track("guitar");
        let t = p.track_mut(live_track).unwrap();
        t.armed = true;
        t.rec_input = 1;
        let g = graph_with(&p, ones(100));
        assert!(g.monitoring());
        let input_ch1 = [0.25f32; 10];
        let mut channels = [None; LiveInput::MAX_CHANNELS];
        channels[1] = Some(&input_ch1[..]);
        let (mut l, mut r) = (vec![0.0; 10], vec![0.0; 10]);
        let mut s = RenderScratch::new(8);
        g.render_with(0, &mut l, &mut r, &mut s, &LiveInput { channels, play_items: false });
        // Stopped: only the input, on both sides; the item on the other track stays quiet.
        assert_eq!((l[3], r[3]), (0.25, 0.25));
        g.render_with(0, &mut l, &mut r, &mut s, &LiveInput { channels, play_items: true });
        assert_eq!(l[3], 1.25);
        p.track_mut(live_track).unwrap().monitor = false;
        assert!(!graph_with(&p, ones(100)).monitoring());
    }

    #[test]
    fn reversed_section_plays_backwards() {
        let audio = AudioData { sample_rate: 100, channels: vec![(0..10).map(|i| i as f32).collect()] };
        let mut p = project_with_item(0.0, 0.04);
        p.tracks[0].pan = -1.0;
        let mut g = graph_with(&p, audio);
        let item = &mut g.tracks[0].items[0];
        item.section_start = 2.0;
        item.section_len = 4.0;
        item.reverse = true;
        let (l, _) = render(&g, 0, 5);
        assert_eq!(&l[..5], &[5.0, 4.0, 3.0, 2.0, 0.0]);
    }
}
