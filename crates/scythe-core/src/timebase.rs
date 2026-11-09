//! What follows the beat when the tempo changes (REAPER's timebase), and the project play rate.
//!
//! The project timebase (`TIMELOCKMODE`) applies to items, envelopes and markers; a track
//! (`BEAT` in the track) and an item (`BEAT` in the item) can override it. With
//! [`Timebase::Beats`] an item keeps its position and end in quarter notes and its take speeds
//! up or slows down to fit; with [`Timebase::BeatsPosition`] only the start keeps its quarter
//! note. Tempo markers keep their quarter notes when `TEMPOENVLOCKMODE` is 1.

use std::borrow::Cow;

use crate::model::{Item, Project, Source, Take};
use crate::tempo::{TempoMap, TempoPoint};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Timebase {
    /// Positions stay in seconds.
    Time,
    /// Position, length and play rate follow the beat. REAPER's default for new projects.
    #[default]
    Beats,
    /// The position follows the beat; length and rate stay.
    BeatsPosition,
}

impl Timebase {
    pub const ALL: [Timebase; 3] = [Self::Time, Self::Beats, Self::BeatsPosition];

    /// `TIMELOCKMODE` and `BEAT` codes; -1 (the default of the level above) is `None`.
    pub fn from_rpp(code: i64) -> Option<Self> {
        match code {
            0 => Some(Self::Time),
            1 => Some(Self::Beats),
            2 => Some(Self::BeatsPosition),
            _ => None,
        }
    }

    pub fn rpp_code(self) -> i64 {
        match self {
            Self::Time => 0,
            Self::Beats => 1,
            Self::BeatsPosition => 2,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Time => "Time",
            Self::Beats => "Beats (position, length, rate)",
            Self::BeatsPosition => "Beats (position only)",
        }
    }
}

/// The project play rate (RPP `PLAYRATE rate preserve min max`): playback and renders run
/// `rate` times as fast, resampled or, with `preserve_pitch`, time-stretched.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayRate {
    pub rate: f64,
    pub preserve_pitch: bool,
}

impl Default for PlayRate {
    fn default() -> Self {
        Self { rate: 1.0, preserve_pitch: false }
    }
}

impl PlayRate {
    pub const MIN: f64 = 0.25;
    pub const MAX: f64 = 4.0;

    pub fn is_normal(&self) -> bool {
        (self.rate - 1.0).abs() < 1e-12 || self.rate.is_nan() || self.rate <= 0.0
    }
}

/// Moves the tempo markers after `after` seconds so they keep the quarter note they had under
/// `old`, as REAPER does with `TEMPOENVLOCKMODE 1`. Markers that changed themselves stay.
pub fn keep_tempo_beats(old: &TempoMap, new: &mut TempoMap, after: f64) {
    let before = old.timeline();
    new.points.sort_by(|a, b| a.time.total_cmp(&b.time));
    for j in 0..new.points.len() {
        let t = new.points[j].time;
        if t <= after + 1e-9 || !old.points.iter().any(|o| same_point(o, &new.points[j])) {
            continue;
        }
        // Points after `j` have not moved yet, so the map up to `j` is final.
        let qn = before.qn_at(t);
        let timeline = new.timeline();
        let (from, bpm) = match j.checked_sub(1).map(|i| &new.points[i]) {
            Some(p @ TempoPoint { linear: true, .. }) => (p.time, (p.bpm + new.points[j].bpm) / 2.0),
            Some(p) => (p.time, p.bpm),
            None => (0.0, new.bpm),
        };
        if bpm > 0.0 {
            new.points[j].time = from + (qn - timeline.qn_at(from)) * 60.0 / bpm;
        }
    }
}

fn same_point(a: &TempoPoint, b: &TempoPoint) -> bool {
    (a.time, a.bpm, a.linear, a.time_sig) == (b.time, b.bpm, b.linear, b.time_sig)
}

/// The earliest time where `new` differs from `old`: later tempo markers are the ones to keep
/// their beat.
fn first_change(old: &TempoMap, new: &TempoMap) -> f64 {
    if old.bpm != new.bpm || (old.beats_per_bar, old.beat_unit) != (new.beats_per_bar, new.beat_unit) {
        return 0.0;
    }
    let n = old.points.iter().zip(&new.points).take_while(|(a, b)| same_point(a, b)).count();
    let at = |p: &[TempoPoint]| p.get(n).map_or(f64::INFINITY, |p| p.time);
    at(&old.points).min(at(&new.points))
}

impl Project {
    /// The timebase that applies to `item` on track `track`.
    pub fn item_timebase(&self, track: usize, item: &Item) -> Timebase {
        item.timebase.or(self.tracks.get(track).and_then(|t| t.timebase)).unwrap_or(self.timebase)
    }

    /// Changes the tempo map with `edit`, then moves what follows the beat: later tempo markers
    /// when `tempo_follows_beats`, and items, envelopes and markers by their timebase.
    pub fn change_tempo(&mut self, edit: impl FnOnce(&mut TempoMap)) {
        let old = self.tempo.clone();
        edit(&mut self.tempo);
        if self.tempo_follows_beats {
            let after = first_change(&old, &self.tempo);
            keep_tempo_beats(&old, &mut self.tempo, after);
        }
        self.follow_tempo(&old);
    }

    /// Moves items, envelope points and markers that follow the beat after the tempo map
    /// changed from `old` to the current one.
    pub fn follow_tempo(&mut self, old: &TempoMap) {
        let (before, after) = (old.timeline(), self.tempo.timeline());
        let map = |t: f64| after.time_at_qn(before.qn_at(t));
        let project = self.timebase;
        for ti in 0..self.tracks.len() {
            let track_base = self.tracks[ti].timebase.unwrap_or(project);
            let track = &mut self.tracks[ti];
            for item in &mut track.items {
                match item.timebase.unwrap_or(track_base) {
                    Timebase::Time => {}
                    Timebase::BeatsPosition => item.position = map(item.position),
                    Timebase::Beats => {
                        let (start, end) = (map(item.position), map(item.end()));
                        let factor = if item.length > 0.0 { (end - start) / item.length } else { 1.0 };
                        item.position = start;
                        item.length = end - start;
                        if (factor - 1.0).abs() > 1e-12 {
                            for take in &mut item.takes {
                                stretch_take(take, factor);
                            }
                        }
                    }
                }
            }
            if track_base != Timebase::Time {
                let envelopes = track.envelopes.iter_mut().chain(track.sends.iter_mut().flat_map(|s| &mut s.envelopes));
                for env in envelopes {
                    for p in &mut env.points {
                        p.time = map(p.time);
                    }
                }
            }
        }
        if project != Timebase::Time {
            for env in &mut self.master.envelopes {
                for p in &mut env.points {
                    p.time = map(p.time);
                }
            }
            for m in &mut self.markers {
                m.position = map(m.position);
                m.region_end = m.region_end.map(map);
            }
        }
    }

    /// The project as it plays at its play rate: every time divided by the rate and every take
    /// sped up by it. Unchanged at rate 1.
    pub fn at_playrate(&self) -> Cow<'_, Project> {
        if self.playrate.is_normal() {
            return Cow::Borrowed(self);
        }
        let mut p = self.clone();
        let rate = self.playrate.rate;
        let preserve = self.playrate.preserve_pitch;
        let scale = |t: &mut f64| *t /= rate;
        p.tempo.bpm *= rate;
        for pt in &mut p.tempo.points {
            scale(&mut pt.time);
            pt.bpm *= rate;
        }
        for m in &mut p.markers {
            scale(&mut m.position);
            if let Some(end) = m.region_end.as_mut() {
                scale(end);
            }
        }
        if let Some(sel) = p.time_selection.as_mut() {
            scale(&mut sel.start);
            scale(&mut sel.end);
        }
        scale(&mut p.cursor);
        for track in std::iter::once(&mut p.master).chain(p.tracks.iter_mut()) {
            let envelopes = track.envelopes.iter_mut().chain(track.sends.iter_mut().flat_map(|s| &mut s.envelopes));
            for env in envelopes {
                env.points.iter_mut().for_each(|pt| scale(&mut pt.time));
            }
            for item in &mut track.items {
                scale(&mut item.position);
                scale(&mut item.length);
                scale(&mut item.fade_in.length);
                scale(&mut item.fade_out.length);
                for take in &mut item.takes {
                    for env in &mut take.envelopes {
                        env.points.iter_mut().for_each(|pt| scale(&mut pt.time));
                    }
                    if is_midi(&take.source) {
                        continue;
                    }
                    // A take stretched with its pitch kept while the project resamples is shifted
                    // up by the rate, and a resampled one while the project keeps pitch is
                    // shifted down; takes at rate 1 just take the project's way.
                    let shift = 12.0 * rate.log2();
                    let changed = (take.playrate - 1.0).abs() > 1e-9 || !take.stretch_markers.is_empty();
                    match (take.preserve_pitch && changed, !take.preserve_pitch && changed, preserve) {
                        (true, _, false) => take.pitch += shift,
                        (_, true, true) => take.pitch -= shift,
                        (false, false, _) => take.preserve_pitch = preserve,
                        _ => {}
                    }
                    take.playrate *= rate;
                }
            }
        }
        p.playrate = PlayRate::default();
        Cow::Owned(p)
    }
}

fn is_midi(source: &Source) -> bool {
    matches!(source, Source::Raw(c) if c.args().first().is_some_and(|k| k == "MIDI"))
}

/// Makes `take` last `factor` times as long: stretch markers and take envelopes spread out, or
/// the play rate drops when there are no markers. MIDI keeps its rate; it is in quarter notes.
fn stretch_take(take: &mut Take, factor: f64) {
    for env in &mut take.envelopes {
        env.points.iter_mut().for_each(|p| p.time *= factor);
    }
    if !take.stretch_markers.is_empty() {
        take.stretch_markers.iter_mut().for_each(|m| m.pos *= factor);
    } else if !is_midi(&take.source) && take.playrate > 0.0 {
        take.playrate /= factor;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Item, Marker, Take, Track};
    use crate::stretch::StretchMarker;
    use std::path::PathBuf;

    fn project(timebase: Timebase) -> Project {
        let mut p = Project { timebase, ..Project::default() };
        p.tempo.points = vec![TempoPoint::new(0.0, 120.0), TempoPoint::new(4.0, 120.0)];
        let mut t = Track::new("a");
        for tb in [None, Some(Timebase::Time), Some(Timebase::Beats), Some(Timebase::BeatsPosition)] {
            let mut it = Item::new(2.0, 1.0, Take::from_file(PathBuf::from("x.wav")));
            it.timebase = tb;
            t.items.push(it);
        }
        let mut sm = Item::new(5.0, 1.0, Take::from_file(PathBuf::from("x.wav")));
        sm.timebase = Some(Timebase::Beats);
        sm.takes[0].stretch_markers = vec![StretchMarker::new(0.5, 0.7)];
        t.items.push(sm);
        p.tracks.push(t);
        p.markers.push(Marker {
            index: 1,
            position: 3.0,
            name: String::new(),
            color: None,
            region_end: None,
            render_matrix: Vec::new(),
            lines: Default::default(),
        });
        p
    }

    fn summary(p: &Project) -> Vec<(f64, f64, f64)> {
        p.tracks[0].items.iter().map(|i| (i.position, i.length, i.takes[0].playrate)).collect()
    }

    fn close(a: &[(f64, f64, f64)], b: &[(f64, f64, f64)]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x.0 - y.0).abs() < 1e-9 && (x.1 - y.1).abs() < 1e-9 && (x.2 - y.2).abs() < 1e-9)
    }

    /// REAPER 7.73, `SetTempoTimeSigMarker(0, 0, 0, -1, -1, 60, 4, 4, false)` on the same project.
    #[test]
    fn halving_the_tempo_moves_what_follows_the_beat() {
        for (base, default) in [(Timebase::Time, (2.0, 1.0, 1.0)), (Timebase::Beats, (4.0, 2.0, 0.5))] {
            let mut p = project(base);
            let old = p.tempo.clone();
            p.tempo.set_base_bpm(60.0);
            keep_tempo_beats(&old, &mut p.tempo, 0.0);
            assert!((p.tempo.points[1].time - 8.0).abs() < 1e-9);
            p.follow_tempo(&old);
            let want = [default, (2.0, 1.0, 1.0), (4.0, 2.0, 0.5), (4.0, 1.0, 1.0), (9.0, 1.0, 1.0)];
            assert!(close(&summary(&p), &want), "{base:?}: {:?}", summary(&p));
            let marker = if base == Timebase::Time { 3.0 } else { 6.0 };
            assert!((p.markers[0].position - marker).abs() < 1e-9);
        }
    }

    /// REAPER: a linear ramp from 120 to 60 bpm reaching 60 at 4 s.
    #[test]
    fn a_ramp_stretches_items_and_their_markers() {
        let mut p = project(Timebase::Beats);
        let old = p.tempo.clone();
        p.tempo.points[0].linear = true;
        p.tempo.points[1].bpm = 60.0;
        p.follow_tempo(&old);
        let s = summary(&p);
        assert!((s[0].0 - 2.343145751).abs() < 1e-8 && (s[0].1 - 1.656854249).abs() < 1e-8, "{s:?}");
        assert!((s[0].2 - 0.603553391).abs() < 1e-8);
        assert!((s[4].0 - 8.0).abs() < 1e-9 && (s[4].1 - 2.0).abs() < 1e-9 && s[4].2 == 1.0);
        assert_eq!(p.tracks[0].items[4].takes[0].stretch_markers[0].pos, 1.0);
    }

    #[test]
    fn a_new_marker_pushes_later_ones_by_their_beats() {
        let mut p = project(Timebase::Beats);
        let old = p.tempo.clone();
        p.tempo.points.insert(1, TempoPoint::new(1.0, 90.0));
        keep_tempo_beats(&old, &mut p.tempo, 1.0);
        assert!((p.tempo.points[2].time - 5.0).abs() < 1e-9);
        p.follow_tempo(&old);
        assert!((p.tracks[0].items[0].position - 2.333333333).abs() < 1e-8);
        assert!((p.markers[0].position - 3.666666667).abs() < 1e-8);
    }

    #[test]
    fn play_rate_scales_time_and_takes() {
        let mut p = project(Timebase::Time);
        p.playrate = PlayRate { rate: 2.0, preserve_pitch: false };
        let played = p.at_playrate();
        let it = &played.tracks[0].items[0];
        assert_eq!((it.position, it.length, it.takes[0].playrate), (1.0, 0.5, 2.0));
        assert_eq!((it.takes[0].pitch, it.takes[0].preserve_pitch), (0.0, false));
        let stretched = &played.tracks[0].items[4].takes[0];
        assert_eq!((stretched.pitch, stretched.preserve_pitch), (12.0, true));
        assert_eq!(played.tempo.bpm_at(0.0), 240.0);
        assert_eq!(played.markers[0].position, 1.5);
    }
}
