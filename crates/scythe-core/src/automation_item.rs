//! Automation items: pooled stretches of envelope (`<POOLEDENV` in the project) placed on an
//! envelope by `POOLEDENVINST` lines. A pool keeps its points in quarter notes from its start
//! and its values normalised to 0..1 of the envelope's range; an instance plays the pool from
//! a start offset at a play rate, looped or not, scaled around a baseline by an amplitude.
//!
//! How REAPER 7.73 plays them (measured from renders): inside an item the item replaces the
//! underlying envelope; overlapping items multiply on volume envelopes and add on the others;
//! a looped item wraps from its last point back to its first; an unlooped one holds its last
//! value. Each item edge ramps linearly over the item's transition time (12 ms by default):
//! from the value without the item at its start to the value with it a transition later, and
//! from the value a transition before its end to the value without it at its end. Ramps that
//! meet merge into one.

use std::sync::Arc;

use crate::envelope::{EnvPoint, Envelope, EnvelopeKind, PointShape};
use crate::fader::{envelope_gain, envelope_position};
use crate::rpp::{Chunk, Node, join_tokens, line_key, tokenize};
use crate::tempo::{TempoMap, Timeline};

/// REAPER starts an item's ramp this long before the item (it shows on the first frame).
const START_LEAD: f64 = 1e-6;

/// Default transition time of new items, in seconds.
pub const DEFAULT_TRANSITION: f64 = 0.012;

/// A pool's LFO settings (`LFO <shape> <phase> <amp skew> <pulse width> <cycles> <freq skew>
/// <tilt>`), which REAPER keeps for its properties dialog; the points it generates are stored
/// with the pool.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lfo {
    pub shape: LfoShape,
    /// 0..1 of a cycle.
    pub phase: f64,
    /// Cycles over the pool's length.
    pub cycles: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LfoShape {
    Sine,
    Triangle,
    Square,
    SawUp,
    SawDown,
}

impl LfoShape {
    pub const ALL: [LfoShape; 5] = [Self::Sine, Self::Triangle, Self::Square, Self::SawUp, Self::SawDown];

    pub fn code(self) -> i64 {
        match self {
            Self::Sine => 1,
            Self::Triangle => 2,
            Self::Square => 3,
            Self::SawUp => 4,
            Self::SawDown => 5,
        }
    }

    pub fn from_code(code: i64) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.code() == code)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Sine => "sine",
            Self::Triangle => "triangle",
            Self::Square => "square",
            Self::SawUp => "saw_up",
            Self::SawDown => "saw_down",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }
}

impl Lfo {
    /// Points for `length_qn` quarter notes: per cycle, the turning points of the shape (a
    /// sine as slow start/end segments between its peaks), values in 0..1.
    pub fn points(&self, length_qn: f64) -> Vec<EnvPoint> {
        let cycles = self.cycles.max(0.0);
        if cycles <= 0.0 || length_qn <= 0.0 {
            return vec![EnvPoint::new(0.0, 0.5)];
        }
        let period = length_qn / cycles;
        // Shape over one cycle: (phase 0..1, value, shape to the next point).
        let shape: &[(f64, f64, PointShape)] = match self.shape {
            LfoShape::Sine => &[
                (0.0, 0.5, PointShape::FastStart),
                (0.25, 1.0, PointShape::SlowStartEnd),
                (0.75, 0.0, PointShape::FastEnd),
            ],
            LfoShape::Triangle => {
                &[(0.0, 0.5, PointShape::Linear), (0.25, 1.0, PointShape::Linear), (0.75, 0.0, PointShape::Linear)]
            }
            LfoShape::Square => &[(0.0, 1.0, PointShape::Square), (0.5, 0.0, PointShape::Square)],
            LfoShape::SawUp => &[(0.0, 0.0, PointShape::Linear), (1.0 - 1e-6, 1.0, PointShape::Square)],
            LfoShape::SawDown => &[(0.0, 1.0, PointShape::Linear), (1.0 - 1e-6, 0.0, PointShape::Square)],
        };
        let start = -self.phase.rem_euclid(1.0) * period;
        let mut pts = Vec::new();
        let mut cycle = 0.0;
        while start + cycle * period < length_qn {
            for &(at, value, s) in shape {
                let time = start + (cycle + at) * period;
                if (0.0..length_qn).contains(&time) {
                    pts.push(EnvPoint { shape: s, ..EnvPoint::new(time, value) });
                }
            }
            cycle += 1.0;
        }
        // The curve at the pool's start when the phase cut into a cycle.
        if pts.first().is_none_or(|p| p.time > 0.0) {
            let probe =
                Envelope { points: shifted(shape, start, period, length_qn), ..Envelope::new(EnvelopeKind::Pan) };
            let value = probe.value_at(0.0).unwrap_or(0.5);
            pts.insert(0, EnvPoint { shape: PointShape::Linear, ..EnvPoint::new(0.0, value) });
        }
        pts
    }
}

fn shifted(shape: &[(f64, f64, PointShape)], start: f64, period: f64, length: f64) -> Vec<EnvPoint> {
    let mut out = Vec::new();
    let mut cycle = -1.0;
    while start + cycle * period < length + period {
        for &(at, value, s) in shape {
            out.push(EnvPoint { shape: s, ..EnvPoint::new(start + (cycle + at) * period, value) });
        }
        cycle += 1.0;
    }
    out
}

/// A pool (`<POOLEDENV`): the points every pooled instance plays.
#[derive(Debug, Clone, PartialEq)]
pub struct AutomationPool {
    pub id: i64,
    pub name: String,
    /// Source length in quarter notes (`SRCLEN`).
    pub length_qn: f64,
    /// Points in quarter notes from the pool's start, values 0..1 of the envelope's range.
    pub points: Vec<EnvPoint>,
    pub lfo: Option<Lfo>,
    /// The original chunk, so its other lines survive a save.
    pub raw: Option<Chunk>,
}

impl AutomationPool {
    pub fn new(id: i64, length_qn: f64) -> Self {
        Self { id, name: id.to_string(), length_qn, points: Vec::new(), lfo: None, raw: None }
    }

    pub fn parse(c: &Chunk) -> Self {
        let mut pool = Self::new(0, 0.0);
        for node in &c.children {
            let Node::Line(l) = node else { continue };
            let t = tokenize(l);
            let num = |i: usize| t.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
            match t[0].as_str() {
                "ID" => pool.id = num(1) as i64,
                "NAME" => pool.name = t.get(1).cloned().unwrap_or_default(),
                "SRCLEN" => pool.length_qn = num(1),
                "LFO" => {
                    pool.lfo =
                        LfoShape::from_code(num(1) as i64).map(|shape| Lfo { shape, phase: num(2), cycles: num(5) })
                }
                // `PPT <qn> <value> <shape> <bezier tension> <selected>`
                "PPT" => pool.points.push(EnvPoint {
                    time: num(1),
                    value: num(2),
                    shape: PointShape::from_rpp(num(3) as i64, num(4)),
                    raw: t.clone(),
                }),
                _ => {}
            }
        }
        pool.points.sort_by(|a, b| a.time.total_cmp(&b.time));
        pool.raw = Some(c.clone());
        pool
    }

    pub fn write(&self) -> Chunk {
        let mut c = Chunk::new("POOLEDENV", &[]);
        let kept: Vec<Node> = self
            .raw
            .iter()
            .flat_map(|r| &r.children)
            .filter(|n| !matches!(n, Node::Line(l) if matches!(line_key(l), "ID" | "NAME" | "SRCLEN" | "LFO" | "PPT")))
            .cloned()
            .collect();
        c.line(["ID".to_owned(), self.id.to_string()]);
        c.line(["NAME".to_owned(), self.name.clone()]);
        c.line(["SRCLEN".to_owned(), fmt(self.length_qn)]);
        let mut lfo = self
            .raw
            .iter()
            .flat_map(|r| &r.children)
            .find_map(|n| match n {
                Node::Line(l) if line_key(l) == "LFO" => Some(tokenize(l)),
                _ => None,
            })
            .unwrap_or_else(|| ["LFO", "0", "0", "0", "0", "0", "0", "0"].map(String::from).to_vec());
        lfo.resize(lfo.len().max(8), "0".into());
        match self.lfo {
            Some(l) => {
                lfo[1] = l.shape.code().to_string();
                lfo[2] = fmt(l.phase);
                lfo[5] = fmt(l.cycles);
            }
            None => lfo[1] = "0".into(),
        }
        c.line(lfo);
        for p in &self.points {
            let mut t = if p.raw.len() >= 6 {
                p.raw.clone()
            } else {
                ["PPT", "0", "0", "0", "0", "0"].map(String::from).to_vec()
            };
            t[1] = fmt(p.time);
            t[2] = fmt(p.value);
            t[3] = p.shape.rpp_code().to_string();
            t[4] = match p.shape {
                PointShape::Bezier(k) => fmt(k),
                _ => "0".into(),
            };
            c.line(t);
        }
        c.children.extend(kept);
        c
    }

    /// The pool's points as `item` plays them, in project seconds with normalised values: each
    /// point lands where its quarter note falls in the tempo map (REAPER interpolates in time
    /// between them, so a tempo ramp bends the item piecewise), repeated per loop, with two
    /// neighbours beyond each end for the edge segments and bezier tangents.
    fn curve(&self, item: &AutomationItem, timeline: &Timeline) -> Envelope {
        const MAX_POINTS: usize = 200_000;
        let mut env = Envelope::new(EnvelopeKind::Pan);
        if self.points.is_empty() {
            return env;
        }
        let rate = if item.rate > 0.0 { item.rate } else { 1.0 };
        let q0 = timeline.qn_at(item.position);
        let offset = timeline.qn_at(item.position + item.start_offset) - q0;
        let span = timeline.qn_at(item.end()) - q0;
        let at = |src: f64| timeline.time_at_qn(q0 + (src - offset) / rate);
        let len = self.length_qn;
        let (first, last) = (offset, offset + span * rate);
        let mut pts: Vec<EnvPoint> = Vec::new();
        if item.looped && len > 0.0 {
            let mut k = (first / len).floor() - 1.0;
            while k * len <= last + len && pts.len() < MAX_POINTS {
                pts.extend(self.points.iter().map(|p| EnvPoint {
                    time: at(k * len + p.time),
                    value: p.value,
                    shape: p.shape,
                    raw: Vec::new(),
                }));
                k += 1.0;
            }
        } else {
            pts.extend(self.points.iter().map(|p| EnvPoint {
                time: at(p.time),
                value: p.value,
                shape: p.shape,
                raw: Vec::new(),
            }));
        }
        let lo = pts.partition_point(|p| p.time < item.position).saturating_sub(2);
        let hi = (pts.partition_point(|p| p.time <= item.end()) + 2).min(pts.len());
        env.points = pts[lo..hi].to_vec();
        env
    }
}

/// How normalised values map onto an envelope's own.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scale {
    Linear {
        min: f64,
        max: f64,
    },
    /// Volume in fader scaling: the normalised value is a share of the fader position at the
    /// top of the range (+6 dB).
    Fader,
}

impl Scale {
    pub fn of(env: &Envelope) -> Self {
        if env.fader_scaled {
            return Self::Fader;
        }
        let (min, max) = env.range.unwrap_or_else(|| env.kind.range());
        Self::Linear { min, max }
    }

    pub fn denormalize(self, n: f64) -> f64 {
        match self {
            Self::Linear { min, max } => min + n * (max - min),
            Self::Fader => envelope_gain(n * envelope_position(2.0)),
        }
    }

    pub fn normalize(self, v: f64) -> f64 {
        match self {
            Self::Linear { min, max } if max != min => (v - min) / (max - min),
            Self::Linear { .. } => 0.0,
            Self::Fader => envelope_position(v) / envelope_position(2.0),
        }
    }
}

/// What an instance plays, worked out from its pool and the tempo map.
#[derive(Debug)]
pub struct ItemSource {
    /// Normalised values in project seconds.
    curve: Envelope,
    scale: Scale,
    multiply: bool,
    /// Whether the edges ramp: REAPER skips the transition on FX parameter envelopes.
    transitions: bool,
}

/// The resolved source, compared as always equal so envelopes keep their derived equality.
#[derive(Debug, Clone, Default)]
pub struct Resolved(pub Option<Arc<ItemSource>>);

impl PartialEq for Resolved {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

/// An automation item on an envelope (`POOLEDENVINST`).
#[derive(Debug, Clone, PartialEq)]
pub struct AutomationItem {
    pub pool: i64,
    pub position: f64,
    pub length: f64,
    /// Seconds into the pool where the item starts.
    pub start_offset: f64,
    pub rate: f64,
    /// 0..1, the value the middle of the pool's range plays at.
    pub baseline: f64,
    /// -1..1, how far the pool's values reach from the baseline.
    pub amplitude: f64,
    pub looped: bool,
    pub selected: bool,
    /// Seconds of the linear ramps at the item's edges.
    pub transition: f64,
    /// Original tokens of the line, so unmodelled fields survive a save.
    pub raw: Vec<String>,
    pub source: Resolved,
}

impl AutomationItem {
    pub fn new(pool: i64, position: f64, length: f64) -> Self {
        Self {
            pool,
            position,
            length,
            start_offset: 0.0,
            rate: 1.0,
            baseline: 0.5,
            amplitude: 1.0,
            looped: true,
            selected: false,
            transition: DEFAULT_TRANSITION,
            raw: Vec::new(),
            source: Resolved(None),
        }
    }

    pub fn end(&self) -> f64 {
        self.position + self.length
    }

    /// `POOLEDENVINST <pool> <position> <length> <start offset> <rate> <selected> <baseline>
    /// <amplitude> <loop> … <transition> …`
    pub fn parse(t: &[String]) -> Self {
        let num = |i: usize, d: f64| t.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(d);
        Self {
            pool: num(1, 0.0) as i64,
            position: num(2, 0.0),
            length: num(3, 0.0),
            start_offset: num(4, 0.0),
            rate: num(5, 1.0),
            selected: num(6, 0.0) != 0.0,
            baseline: num(7, 0.5),
            amplitude: num(8, 1.0),
            looped: num(9, 1.0) != 0.0,
            transition: num(15, DEFAULT_TRANSITION),
            raw: t.to_vec(),
            source: Resolved(None),
        }
    }

    pub fn tokens(&self, instance: usize) -> Vec<String> {
        let mut t = if self.raw.len() >= 18 {
            self.raw.clone()
        } else {
            let mut t = [
                "POOLEDENVINST",
                "0",
                "0",
                "0",
                "0",
                "1",
                "0",
                "0.5",
                "1",
                "1",
                "0",
                "0",
                "1",
                "0",
                "0",
                "0.012",
                "0",
                "1",
            ]
            .map(String::from)
            .to_vec();
            t[12] = instance.to_string();
            t
        };
        let set = [
            (1, self.pool.to_string()),
            (2, fmt(self.position)),
            (3, fmt(self.length)),
            (4, fmt(self.start_offset)),
            (5, fmt(self.rate)),
            (6, (self.selected as u8).to_string()),
            (7, fmt(self.baseline)),
            (8, fmt(self.amplitude)),
            (9, (self.looped as u8).to_string()),
            (15, fmt(self.transition)),
        ];
        for (i, v) in set {
            t[i] = v;
        }
        t
    }

    /// Fills in what the item plays from `pool`.
    pub fn resolve(
        &mut self,
        pool: Option<&AutomationPool>,
        timeline: &Arc<Timeline>,
        scale: Scale,
        multiply: bool,
        transitions: bool,
    ) {
        let curve = pool.map(|p| p.curve(self, timeline)).unwrap_or_else(|| Envelope::new(EnvelopeKind::Pan));
        self.source = Resolved(Some(Arc::new(ItemSource { curve, scale, multiply, transitions })));
    }

    /// Times of the points the item plays, in project seconds, within the item.
    pub fn point_times(&self) -> impl Iterator<Item = f64> + '_ {
        let points = self.source.0.as_ref().map(|s| s.curve.points.as_slice()).unwrap_or_default();
        points.iter().map(|p| p.time).filter(|&t| t > self.position && t < self.end())
    }

    fn active(&self, t: f64) -> bool {
        t >= self.position && t < self.end()
    }

    /// The item's own value at `t`, in the envelope's units.
    fn value(&self, src: &ItemSource, t: f64) -> f64 {
        let n = src.curve.value_at(t).unwrap_or(0.5);
        src.scale.denormalize(self.baseline + self.amplitude * (n - 0.5))
    }
}

/// An envelope's value at `t` with its automation items over `under`, the underlying curve.
pub(crate) fn value_with_items(items: &[AutomationItem], t: f64, under: impl Fn(f64) -> f64) -> f64 {
    let hard = |t: f64| {
        let mut active = items.iter().filter_map(|i| Some((i, i.source.0.as_deref()?))).filter(|(i, _)| i.active(t));
        let Some((first, src)) = active.next() else { return under(t) };
        active.fold(first.value(src, t), |v, (i, s)| if s.multiply { v * i.value(s, t) } else { v + i.value(s, t) })
    };
    let windows = || {
        items.iter().filter(|i| i.transition > 0.0 && i.source.0.as_ref().is_some_and(|s| s.transitions)).flat_map(
            |i| {
                let tr = i.transition.min(i.length / 2.0);
                [(i.position - START_LEAD, i.position + tr), (i.end() - tr, i.end())]
            },
        )
    };
    let Some(mut span) = windows().find(|&(a, b)| t >= a && t < b) else { return hard(t) };
    loop {
        let grown = windows().fold(span, |(a, b), (c, d)| if c <= b && d >= a { (a.min(c), b.max(d)) } else { (a, b) });
        if grown == span {
            break;
        }
        span = grown;
    }
    let (a, b) = span;
    let (va, vb) = (hard(a), hard(b));
    let k = (t - a) / (b - a);
    // Fader-scaled envelopes ramp in fader position, as their points do.
    match items.iter().find_map(|i| i.source.0.as_ref()).map(|s| s.scale) {
        Some(Scale::Fader) => {
            let (pa, pb) = (envelope_position(va), envelope_position(vb));
            envelope_gain(pa + (pb - pa) * k)
        }
        _ => va + (vb - va) * k,
    }
}

/// Resolves every automation item of `env` against `pools`.
pub fn resolve_envelope(env: &mut Envelope, pools: &[AutomationPool], timeline: &Arc<Timeline>) {
    if env.items.is_empty() {
        return;
    }
    let scale = Scale::of(env);
    let multiply = env.kind.is_volume() && env.range.is_none();
    for item in &mut env.items {
        let pool = pools.iter().find(|p| p.id == item.pool);
        item.resolve(pool, timeline, scale, multiply, env.range.is_none());
    }
}

/// Pools and the tempo map, for resolving items of envelopes kept outside the project model
/// (FX parameter envelopes in their chunks).
#[derive(Debug, Clone, Default)]
pub struct AutomationContext {
    pub pools: Arc<Vec<AutomationPool>>,
    pub timeline: Option<Arc<Timeline>>,
}

impl AutomationContext {
    pub fn new(pools: &[AutomationPool], tempo: &TempoMap) -> Self {
        Self { pools: Arc::new(pools.to_vec()), timeline: Some(Arc::new(tempo.timeline())) }
    }

    pub fn resolve(&self, env: &mut Envelope) {
        let timeline = self.timeline.clone().unwrap_or_else(|| Arc::new(TempoMap::default().timeline()));
        resolve_envelope(env, &self.pools, &timeline);
    }
}

/// Points of the envelope over `[start, start + length)` as pool points: quarter notes from
/// `start`, normalised values, edge points at both ends. What a new item starts from.
pub fn capture(env: &Envelope, start: f64, length: f64, timeline: &Timeline) -> (f64, Vec<EnvPoint>) {
    let scale = Scale::of(env);
    let q0 = timeline.qn_at(start);
    let length_qn = timeline.qn_at(start + length) - q0;
    let at = |t: f64| scale.normalize(env.value_at(t).unwrap_or_else(|| env.kind.neutral()));
    let mut pts = vec![EnvPoint::new(0.0, at(start))];
    pts.extend(env.points.iter().filter(|p| p.time > start && p.time < start + length).map(|p| EnvPoint {
        time: timeline.qn_at(p.time) - q0,
        value: scale.normalize(p.value),
        shape: p.shape,
        raw: Vec::new(),
    }));
    let end = (length_qn - 1e-6).max(0.0);
    pts.push(EnvPoint::new(end, at(start + length - 1e-9)));
    (length_qn, pts)
}

fn fmt(v: f64) -> String {
    crate::rpp::fmt_num(v)
}

/// Writes an envelope's `POOLEDENVINST` lines in place of the old ones (before its points).
pub(crate) fn write_instances(c: &mut Chunk, items: &[AutomationItem]) {
    let at = c
        .children
        .iter()
        .position(|n| matches!(n, Node::Line(l) if matches!(line_key(l), "POOLEDENVINST" | "PT")))
        .unwrap_or(c.children.len());
    c.children.retain(|n| !matches!(n, Node::Line(l) if line_key(l) == "POOLEDENVINST"));
    let lines = items.iter().enumerate().map(|(i, item)| Node::Line(join_tokens(item.tokens(i + 1))));
    let at = at.min(c.children.len());
    c.children.splice(at..at, lines);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(points: &[(f64, f64)]) -> AutomationPool {
        let mut p = AutomationPool::new(1, 4.0);
        p.points = points.iter().map(|&(t, v)| EnvPoint::new(t, v)).collect();
        p
    }

    fn volume(items: Vec<AutomationItem>, pools: &[AutomationPool]) -> Envelope {
        let mut e = Envelope::new(EnvelopeKind::Volume);
        e.points = vec![EnvPoint::new(0.0, 0.6)];
        e.items = items;
        resolve_envelope(&mut e, pools, &Arc::new(TempoMap::default().timeline()));
        e
    }

    // Values read from REAPER 7.73 renders of a DC source (probes `basic`, `rateoffs`,
    // `noloop`, `overlap`, `adjacent` in the WP10 notes).
    #[test]
    fn items_play_as_reaper_renders_them() {
        let tri = [pool(&[(0.0, 0.0), (2.0, 1.0)])];
        let basic = volume(vec![AutomationItem::new(1, 1.0, 2.0)], &tri);
        let at = |e: &Envelope, t: f64| e.value_at(t).unwrap();
        for (t, want) in [(0.5, 0.6), (1.25, 0.5), (2.0, 2.0), (2.5, 1.0), (2.99, 0.12), (3.0, 0.6), (1.001, 0.551956)]
        {
            assert!((at(&basic, t) - want).abs() < 2e-5, "basic at {t}: {}", at(&basic, t));
        }
        let shifted = |looped: bool| {
            volume(
                vec![AutomationItem { start_offset: 0.5, rate: 1.5, looped, ..AutomationItem::new(1, 0.5, 3.0) }],
                &tri,
            )
        };
        assert!((at(&shifted(true), 0.99) - 1.53).abs() < 1e-9);
        assert!((at(&shifted(false), 3.0) - 2.0).abs() < 1e-9);
        let overlap = volume(
            vec![AutomationItem::new(1, 0.5, 2.0), AutomationItem { rate: 0.5, ..AutomationItem::new(1, 1.5, 2.0) }],
            &tri,
        );
        assert!((at(&overlap, 2.25) - 0.375).abs() < 1e-9);
        assert!((at(&overlap, 72576.0 / 48000.0) - 0.023712).abs() < 1e-6);
        let adjacent = volume(
            vec![
                AutomationItem::new(1, 0.5, 1.0),
                AutomationItem { baseline: 0.3, ..AutomationItem::new(1, 1.5, 1.0) },
            ],
            &tri,
        );
        assert!((at(&adjacent, 1.5) - 0.8).abs() < 1e-4, "{}", at(&adjacent, 1.5));
        assert!((at(&adjacent, 72600.0 / 48000.0) + 0.375).abs() < 1e-4);
    }

    #[test]
    fn instances_round_trip() {
        let line = "POOLEDENVINST 1 4 2 0.25 2 1 0.3 0.7 1 0 0 2 0 0 0.012 0 1";
        let item = AutomationItem::parse(&tokenize(line));
        assert_eq!((item.position, item.rate, item.baseline, item.selected), (4.0, 2.0, 0.3, true));
        assert_eq!(join_tokens(item.tokens(2)), line);
    }

    #[test]
    fn projects_keep_pools_items_and_the_override() {
        let text = "<REAPER_PROJECT 0.1 \"7.73\" 0\n  GLOBAL_AUTO 6\n  <POOLEDENV\n    ID 1\n    NAME \"hello\"\n    SRCLEN 4\n    LFO 0 0 0 0 0 0 0\n    PPT 0 0 5 0.7 0\n    PPT 1 0.75 5 -0.3 0\n    PPT 2 1 1 0 0\n  >\n  <TRACK\n    <PANENV2\n      ACT 1 -1\n      POOLEDENVINST 1 1 2 0 1 0 0.5 1 1 0 0 1 0 0 0.012 0 1\n      PT 0 0 0\n    >\n  >\n>\n";
        let mut p = crate::rpp::project_from_rpp(text).unwrap();
        assert_eq!(p.global_automation, Some(crate::model::GlobalAutomation::Bypass));
        assert_eq!(p.automation_pools[0].points[1].shape, PointShape::Bezier(-0.3));
        let env = &p.tracks[0].envelopes[0];
        assert_eq!((env.items.len(), env.value_at(1.5)), (1, Some(0.5)));
        let saved = crate::rpp::project_to_rpp(&p);
        for line in [
            "GLOBAL_AUTO 6",
            "PPT 1 0.75 5 -0.3 0",
            "NAME hello",
            "POOLEDENVINST 1 1 2 0 1 0 0.5 1 1 0 0 1 0 0 0.012 0 1",
        ] {
            assert!(saved.contains(line), "{line} in {saved}");
        }
        p.global_automation = None;
        p.tracks[0].envelopes[0].items[0].position = 3.0;
        p.tracks[0].envelopes[0].points.push(EnvPoint::new(2.0, 0.5));
        let saved = crate::rpp::project_to_rpp(&p);
        assert!(saved.contains("GLOBAL_AUTO -1") && saved.contains("POOLEDENVINST 1 3 2 "), "{saved}");
        assert!(saved.contains("\n      PT 2 0.5 0\n"), "{saved}");
    }

    #[test]
    fn lfo_points_cover_the_pool() {
        let lfo = Lfo { shape: LfoShape::Triangle, phase: 0.0, cycles: 2.0 };
        let pts = lfo.points(4.0);
        let mut e = Envelope::new(EnvelopeKind::Pan);
        e.points = pts;
        assert!((e.value_at(0.5).unwrap() - 1.0).abs() < 1e-9);
        assert!((e.value_at(1.5).unwrap() - 0.0).abs() < 1e-9);
    }
}
