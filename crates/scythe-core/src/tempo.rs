/// A tempo or time-signature change (RPP `TEMPOENVEX` point).
#[derive(Debug, Clone, PartialEq)]
pub struct TempoPoint {
    pub time: f64,
    pub bpm: f64,
    /// Ramp linearly to the next point's tempo instead of jumping (envelope shape 0).
    pub linear: bool,
    /// A new time signature starting a bar at this point.
    pub time_sig: Option<(u32, u32)>,
    /// Original tokens of the `PT` line, so unmodelled fields survive a save.
    pub raw: Vec<String>,
}

impl TempoPoint {
    pub fn new(time: f64, bpm: f64) -> Self {
        Self { time, bpm, linear: false, time_sig: None, raw: Vec::new() }
    }
}

/// The project tempo: a base tempo and time signature (RPP `TEMPO`) plus optional changes.
/// Tempo is in quarter notes per minute; a beat is one `beat_unit` note.
#[derive(Debug, Clone, PartialEq)]
pub struct TempoMap {
    pub bpm: f64,
    pub beats_per_bar: u32,
    pub beat_unit: u32,
    pub points: Vec<TempoPoint>,
}

impl Default for TempoMap {
    fn default() -> Self {
        Self::constant(120.0, 4, 4)
    }
}

/// Musical position, all fields 1-based except `fraction` (0.0..1.0 within the beat).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BarBeat {
    pub bar: i64,
    pub beat: u32,
    pub fraction: f64,
}

impl TempoMap {
    pub fn constant(bpm: f64, beats_per_bar: u32, beat_unit: u32) -> Self {
        Self { bpm, beats_per_bar, beat_unit, points: Vec::new() }
    }

    /// Changes the starting tempo, keeping a tempo point at the project start in step.
    pub fn set_base_bpm(&mut self, bpm: f64) {
        self.bpm = bpm;
        if let Some(first) = self.points.first_mut().filter(|p| p.time <= 0.0) {
            first.bpm = bpm;
        }
    }

    /// Sets the tempo in effect at `seconds`: the last tempo point at or before it, or the base
    /// tempo when there is none.
    pub fn set_bpm_at(&mut self, seconds: f64, bpm: f64) {
        match self.points.iter_mut().filter(|p| p.time <= seconds + 1e-9).last() {
            Some(p) => {
                p.bpm = bpm;
                if p.time <= 0.0 {
                    self.bpm = bpm;
                }
            }
            None => self.bpm = bpm,
        }
    }

    /// Adds a tempo change at `seconds` keeping the tempo that was in effect there. A map
    /// without points first gets one at the start for the base tempo.
    pub fn insert_point(&mut self, seconds: f64) {
        if self.points.is_empty() {
            self.points.push(TempoPoint {
                time_sig: Some((self.beats_per_bar, self.beat_unit)),
                ..TempoPoint::new(0.0, self.bpm)
            });
        }
        if self.points.iter().any(|p| (p.time - seconds).abs() < 1e-6) {
            return;
        }
        let bpm = self.bpm_at(seconds);
        self.points.push(TempoPoint::new(seconds, bpm));
        self.points.sort_by(|a, b| a.time.total_cmp(&b.time));
    }

    pub fn timeline(&self) -> Timeline {
        Timeline::new(self)
    }

    /// Seconds per beat at the project start.
    pub fn beat_seconds(&self) -> f64 {
        60.0 / self.bpm * 4.0 / self.beat_unit as f64
    }

    /// Bar length at the project start.
    pub fn bar_seconds(&self) -> f64 {
        self.beat_seconds() * self.beats_per_bar as f64
    }

    pub fn bar_seconds_at(&self, seconds: f64) -> f64 {
        let tl = self.timeline();
        let bar = tl.bar_beat(seconds).bar;
        tl.bar_start(bar + 1) - tl.bar_start(bar)
    }

    pub fn bar_start(&self, bar: i64) -> f64 {
        self.timeline().bar_start(bar)
    }

    pub fn bar_beat(&self, seconds: f64) -> BarBeat {
        self.timeline().bar_beat(seconds)
    }

    pub fn bpm_at(&self, seconds: f64) -> f64 {
        self.timeline().bpm_at(seconds)
    }

    pub fn time_sig_at(&self, seconds: f64) -> (u32, u32) {
        let tl = self.timeline();
        let m = tl.measure_at_qn(tl.qn_at(seconds));
        (m.num, m.denom)
    }

    /// REAPER-style `bar.beat.hundredths` display, e.g. `37.2.50`.
    pub fn format_bar_beat(&self, seconds: f64) -> String {
        let bb = self.bar_beat(seconds);
        format!("{}.{}.{:02}", bb.bar, bb.beat, (bb.fraction * 100.0).floor() as u32)
    }

    pub fn snap_to_beat(&self, seconds: f64) -> f64 {
        self.timeline().snap_to_beat(seconds)
    }
}

#[derive(Debug, Clone, Copy)]
struct Anchor {
    time: f64,
    qn: f64,
    bpm: f64,
    /// Tempo at the next anchor when ramping, otherwise `bpm`.
    end_bpm: f64,
    /// Segment length in seconds (infinite for the last one).
    duration: f64,
}

impl Anchor {
    fn qn_after(&self, dt: f64) -> f64 {
        if self.duration.is_finite() && self.end_bpm != self.bpm {
            (self.bpm * dt + (self.end_bpm - self.bpm) * dt * dt / (2.0 * self.duration)) / 60.0
        } else {
            self.bpm * dt / 60.0
        }
    }

    fn time_after(&self, dq: f64) -> f64 {
        if self.duration.is_finite() && self.end_bpm != self.bpm {
            let a = (self.end_bpm - self.bpm) / (2.0 * self.duration);
            let disc = (self.bpm * self.bpm + 4.0 * a * 60.0 * dq).max(0.0);
            (-self.bpm + disc.sqrt()) / (2.0 * a)
        } else {
            60.0 * dq / self.bpm
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Measure {
    qn: f64,
    bar: i64,
    num: u32,
    denom: u32,
}

impl Measure {
    fn bar_qn(&self) -> f64 {
        self.num as f64 * self.beat_qn()
    }

    fn beat_qn(&self) -> f64 {
        4.0 / self.denom as f64
    }
}

/// Precomputed conversions between seconds, quarter notes and bars. Querying never allocates,
/// so the audio thread can use it.
#[derive(Debug, Clone)]
pub struct Timeline {
    anchors: Vec<Anchor>,
    measures: Vec<Measure>,
}

impl Timeline {
    fn new(map: &TempoMap) -> Self {
        let valid_bpm = |b: f64| if b.is_finite() && b > 0.0 { b } else { 120.0 };
        let mut points: Vec<&TempoPoint> = map.points.iter().filter(|p| p.time >= 0.0).collect();
        points.sort_by(|a, b| a.time.total_cmp(&b.time));
        let mut raw: Vec<TempoPoint> =
            points.iter().map(|p| TempoPoint { bpm: valid_bpm(p.bpm), raw: Vec::new(), ..(*p).clone() }).collect();
        if raw.first().is_none_or(|p| p.time > 0.0) {
            raw.insert(0, TempoPoint::new(0.0, valid_bpm(map.bpm)));
        }

        let mut anchors = Vec::with_capacity(raw.len());
        let mut qn = 0.0;
        for (i, p) in raw.iter().enumerate() {
            let (time, bpm) = (p.time, p.bpm);
            let next = raw.get(i + 1);
            let duration = next.map_or(f64::INFINITY, |n| n.time - time);
            let end_bpm = match next {
                Some(n) if p.linear && duration > 0.0 => n.bpm,
                _ => bpm,
            };
            let a = Anchor { time, qn, bpm, end_bpm, duration };
            if duration.is_finite() {
                qn += a.qn_after(duration);
            }
            anchors.push(a);
        }

        let sig = |s: Option<(u32, u32)>| s.filter(|&(n, d)| n > 0 && d > 0);
        let (num, denom) = sig(raw[0].time_sig).unwrap_or((map.beats_per_bar.max(1), map.beat_unit.max(1)));
        let mut measures = vec![Measure { qn: 0.0, bar: 1, num, denom }];
        for (a, r) in anchors.iter().zip(&raw).skip(1) {
            let Some((num, denom)) = sig(r.time_sig) else { continue };
            let prev = *measures.last().unwrap();
            // A signature change always starts a bar; a cut-short bar before it still counts.
            let bars = ((a.qn - prev.qn) / prev.bar_qn() - 1e-9).ceil().max(0.0) as i64;
            measures.push(Measure { qn: a.qn, bar: prev.bar + bars, num, denom });
        }
        Self { anchors, measures }
    }

    fn anchor_at_time(&self, t: f64) -> &Anchor {
        let i = self.anchors.partition_point(|a| a.time <= t);
        &self.anchors[i.saturating_sub(1)]
    }

    fn anchor_at_qn(&self, q: f64) -> &Anchor {
        let i = self.anchors.partition_point(|a| a.qn <= q);
        &self.anchors[i.saturating_sub(1)]
    }

    fn measure_at_qn(&self, q: f64) -> Measure {
        let i = self.measures.partition_point(|m| m.qn <= q + 1e-9);
        self.measures[i.saturating_sub(1)]
    }

    /// Position in quarter notes since the project start.
    pub fn qn_at(&self, t: f64) -> f64 {
        let a = self.anchor_at_time(t);
        a.qn + a.qn_after(t - a.time)
    }

    pub fn time_at_qn(&self, q: f64) -> f64 {
        let a = self.anchor_at_qn(q);
        a.time + a.time_after(q - a.qn)
    }

    pub fn bpm_at(&self, t: f64) -> f64 {
        let a = self.anchor_at_time(t);
        if a.duration.is_finite() && a.duration > 0.0 {
            a.bpm + (a.end_bpm - a.bpm) * ((t - a.time) / a.duration).clamp(0.0, 1.0)
        } else {
            a.bpm
        }
    }

    pub fn bar_beat(&self, t: f64) -> BarBeat {
        let q = self.qn_at(t);
        let m = self.measure_at_qn(q);
        let rel = q - m.qn;
        let bars = (rel / m.bar_qn() + 1e-9).floor();
        let in_bar = (rel - bars * m.bar_qn()).max(0.0);
        let beat = (in_bar / m.beat_qn() + 1e-9).floor();
        let fraction = ((in_bar - beat * m.beat_qn()) / m.beat_qn()).clamp(0.0, 1.0);
        BarBeat { bar: m.bar + bars as i64, beat: beat as u32 + 1, fraction }
    }

    /// Start time of 1-based `bar`.
    pub fn bar_start(&self, bar: i64) -> f64 {
        let i = self.measures.partition_point(|m| m.bar <= bar);
        let m = self.measures[i.saturating_sub(1)];
        self.time_at_qn(m.qn + (bar - m.bar) as f64 * m.bar_qn())
    }

    pub fn snap_to_beat(&self, t: f64) -> f64 {
        let q = self.qn_at(t);
        let m = self.measure_at_qn(q);
        let snapped = m.qn + ((q - m.qn) / m.beat_qn()).round() * m.beat_qn();
        self.time_at_qn(snapped)
    }

    /// Snaps to the nearest multiple of `step_qn` quarter notes, counted from the start of the
    /// time-signature section.
    pub fn snap_to(&self, t: f64, step_qn: f64) -> f64 {
        let q = self.qn_at(t);
        let m = self.measure_at_qn(q);
        let step = step_qn.max(1e-6);
        self.time_at_qn(m.qn + ((q - m.qn) / step).round() * step)
    }

    /// Calls `f(time, is_downbeat)` for every beat in `[t0, t1)`.
    pub fn for_each_beat(&self, t0: f64, t1: f64, mut f: impl FnMut(f64, bool)) {
        let (q0, q1) = (self.qn_at(t0), self.qn_at(t1));
        let mut m = self.measure_at_qn(q0);
        let mut k = ((q0 - m.qn) / m.beat_qn() - 1e-9).ceil().max(0.0) as i64;
        loop {
            let q = m.qn + k as f64 * m.beat_qn();
            let next = self.measures.iter().find(|n| n.qn > m.qn + 1e-9);
            if let Some(n) = next
                && q >= n.qn - 1e-9
            {
                m = *n;
                k = 0;
                continue;
            }
            if q >= q1 {
                break;
            }
            f(self.time_at_qn(q), k % m.num as i64 == 0);
            k += 1;
        }
    }
}

/// `m:ss.mmm`, or `h:mm:ss.mmm` past an hour.
pub fn format_time(seconds: f64) -> String {
    let neg = seconds < 0.0;
    let total_ms = (seconds.abs() * 1000.0).round() as u64;
    let (h, rem) = (total_ms / 3_600_000, total_ms % 3_600_000);
    let (m, rem) = (rem / 60_000, rem % 60_000);
    let (s, ms) = (rem / 1000, rem % 1000);
    let sign = if neg { "-" } else { "" };
    if h > 0 { format!("{sign}{h}:{m:02}:{s:02}.{ms:03}") } else { format!("{sign}{m}:{s:02}.{ms:03}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn bar_beat_conversion() {
        let t = TempoMap::constant(120.0, 4, 4);
        assert_eq!(t.bar_seconds(), 2.0);
        let bb = t.bar_beat(2.75);
        assert_eq!((bb.bar, bb.beat), (2, 2));
        assert!((bb.fraction - 0.5).abs() < 1e-9);
        assert_eq!(t.format_bar_beat(0.0), "1.1.00");
        assert_eq!(t.bar_start(3), 4.0);
    }

    #[test]
    fn compound_meter_uses_beat_unit() {
        let t = TempoMap::constant(120.0, 6, 8);
        assert_eq!(t.beat_seconds(), 0.25);
        assert_eq!(t.bar_seconds(), 1.5);
        assert!(close(t.bar_start(2), 1.5));
    }

    #[test]
    fn tempo_jump() {
        let mut t = TempoMap::constant(120.0, 4, 4);
        t.points = vec![TempoPoint::new(0.0, 120.0), TempoPoint::new(4.0, 60.0)];
        assert!(close(t.bar_start(3), 4.0));
        assert!(close(t.bar_start(4), 8.0));
        assert_eq!(t.bpm_at(5.0), 60.0);
        assert!(close(t.snap_to_beat(5.4), 5.0));
    }

    #[test]
    fn linear_ramp_round_trips() {
        let mut t = TempoMap::constant(100.0, 4, 4);
        t.points = vec![TempoPoint { linear: true, ..TempoPoint::new(0.0, 100.0) }, TempoPoint::new(10.0, 200.0)];
        let tl = t.timeline();
        assert!(close(tl.bpm_at(5.0), 150.0));
        // Average tempo over the ramp is 150 bpm: 25 quarter notes in 10 s.
        assert!(close(tl.qn_at(10.0), 25.0));
        for s in [0.5, 3.0, 9.9, 12.0] {
            assert!(close(tl.time_at_qn(tl.qn_at(s)), s), "{s}");
        }
    }

    #[test]
    fn time_signature_change_starts_a_bar() {
        let mut t = TempoMap::constant(120.0, 4, 4);
        let seven_eight = TempoPoint { time_sig: Some((7, 8)), ..TempoPoint::new(4.0, 120.0) };
        t.points = vec![TempoPoint::new(0.0, 120.0), seven_eight];
        assert_eq!(t.bar_beat(4.0).bar, 3);
        assert!(close(t.bar_start(4), 4.0 + 1.75));
        assert_eq!(t.time_sig_at(5.0), (7, 8));
        let mut beats = Vec::new();
        t.timeline().for_each_beat(3.0, 4.6, |time, down| beats.push((time, down)));
        assert_eq!(beats, [(3.0, false), (3.5, false), (4.0, true), (4.25, false), (4.5, false)]);
    }

    #[test]
    fn editing_points() {
        let mut t = TempoMap::constant(120.0, 4, 4);
        t.set_bpm_at(10.0, 100.0);
        assert_eq!(t.bpm, 100.0);
        t.insert_point(6.0);
        assert_eq!(t.points.len(), 2);
        t.set_bpm_at(7.0, 150.0);
        assert_eq!((t.bpm_at(1.0), t.bpm_at(7.0)), (100.0, 150.0));
        t.set_bpm_at(1.0, 90.0);
        assert_eq!((t.bpm, t.points[0].bpm), (90.0, 90.0));
    }

    #[test]
    fn time_format() {
        assert_eq!(format_time(84.512), "1:24.512");
        assert_eq!(format_time(3725.0), "1:02:05.000");
    }
}
