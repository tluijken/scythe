//! The arithmetic behind the effect editors, apart from drawing: where values sit on a graph,
//! how keys and wheels nudge them, how typed text reads, and the levels kept for history
//! graphs.

use std::collections::VecDeque;

use egui::{Pos2, Rect, pos2};
use scythe_engine::clap_host::ParamInfo;

/// One axis of a graph: linear or logarithmic between `lo` and `hi`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scale {
    pub lo: f64,
    pub hi: f64,
    pub log: bool,
}

impl Scale {
    pub const fn lin(lo: f64, hi: f64) -> Self {
        Self { lo, hi, log: false }
    }

    pub const fn log(lo: f64, hi: f64) -> Self {
        Self { lo, hi, log: true }
    }

    /// Where `v` sits, 0 at `lo` and 1 at `hi` (beyond them outside 0..1).
    pub fn pos(&self, v: f64) -> f64 {
        if self.log {
            (v.max(1e-12) / self.lo).ln() / (self.hi / self.lo).ln()
        } else {
            (v - self.lo) / (self.hi - self.lo)
        }
    }

    pub fn value(&self, t: f64) -> f64 {
        if self.log { self.lo * (self.hi / self.lo).powf(t) } else { self.lo + t * (self.hi - self.lo) }
    }
}

/// A graph's rectangle on screen with its two axes; y grows upwards.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plot {
    pub rect: Rect,
    pub x: Scale,
    pub y: Scale,
}

impl Plot {
    pub fn new(rect: Rect, x: Scale, y: Scale) -> Self {
        Self { rect, x, y }
    }

    pub fn sx(&self, v: f64) -> f32 {
        self.rect.left() + self.x.pos(v) as f32 * self.rect.width()
    }

    pub fn sy(&self, v: f64) -> f32 {
        self.rect.bottom() - self.y.pos(v) as f32 * self.rect.height()
    }

    pub fn point(&self, x: f64, y: f64) -> Pos2 {
        pos2(self.sx(x), self.sy(y))
    }

    /// The values at screen position `p`.
    pub fn values(&self, p: Pos2) -> (f64, f64) {
        let tx = ((p.x - self.rect.left()) / self.rect.width().max(1.0)) as f64;
        let ty = ((self.rect.bottom() - p.y) / self.rect.height().max(1.0)) as f64;
        (self.x.value(tx), self.y.value(ty))
    }

    /// The x values of each `step` points across, for drawing curves.
    pub fn xs(&self, step: f32) -> Vec<f64> {
        let n = (self.rect.width() / step.max(0.5)).ceil().max(2.0) as usize;
        (0..=n).map(|i| self.x.value(i as f64 / n as f64)).collect()
    }
}

/// How far one nudge moves a parameter: a 1/100 of its range, or of its octaves for
/// logarithmic ones (fine: 1/1000).
pub fn nudge(info: &ParamInfo, value: f64, steps: f64, fine: bool, switch: bool) -> f64 {
    let per = if fine { 0.001 } else { 0.01 };
    let scale =
        if info.log && info.min > 0.0 { Scale::log(info.min, info.max) } else { Scale::lin(info.min, info.max) };
    let moved = if switch { value + steps.signum() } else { scale.value(scale.pos(value) + steps * per) };
    moved.clamp(info.min.min(info.max), info.max.max(info.min))
}

/// Whether a parameter showing `text` is an on/off switch.
pub fn is_switch(text: &str) -> bool {
    matches!(text, "on" | "off")
}

/// A drag of `dy` points (up is negative) over a control `span` points tall.
pub fn drag(info: &ParamInfo, value: f64, dy: f32, span: f32, fine: bool) -> f64 {
    let travel = -(dy as f64) / span.max(20.0) as f64 * if fine { 0.1 } else { 1.0 };
    let scale =
        if info.log && info.min > 0.0 { Scale::log(info.min, info.max) } else { Scale::lin(info.min, info.max) };
    scale.value(scale.pos(value) + travel).clamp(info.min.min(info.max), info.max.max(info.min))
}

/// Reads a typed value: a number with an optional `k` (thousands) and unit (`Hz`, `kHz`, `dB`,
/// `ms`, `s`, `%`, `oct`, `:1`), in the units `unit_of` says the parameter shows.
pub fn parse_value(text: &str, shown: &str) -> Option<f64> {
    let t = text.trim().to_lowercase().replace(',', ".");
    if t.is_empty() {
        return None;
    }
    if t == "inf" || t == "-inf" || t.starts_with("-inf") {
        return Some(if t.starts_with('-') { -150.0 } else { f64::INFINITY });
    }
    let shown = shown.to_lowercase();
    let number_end = t
        .char_indices()
        .find(|&(i, c)| !(c.is_ascii_digit() || c == '.' || (i == 0 && (c == '-' || c == '+'))))
        .map_or(t.len(), |(i, _)| i);
    let mut v: f64 = t[..number_end].parse().ok()?;
    let unit = t[number_end..].trim();
    match unit {
        "" => {}
        "k" | "khz" => v *= 1000.0,
        "s" | "sec" if shown.contains("ms") => v *= 1000.0,
        "ms" if shown.ends_with(" s") => v /= 1000.0,
        "hz" | "db" | "ms" | "s" | "sec" | "%" | "oct" | ":1" | "frames" | "cents" | "db/s" => {}
        _ => return None,
    }
    v.is_finite().then_some(v)
}

/// The last levels of a meter, for a history graph: one per frame, oldest dropped.
#[derive(Debug, Clone)]
pub struct History {
    values: VecDeque<f32>,
    capacity: usize,
}

impl History {
    pub fn new(capacity: usize) -> Self {
        Self { values: VecDeque::with_capacity(capacity), capacity: capacity.max(1) }
    }

    pub fn push(&mut self, v: f32) {
        if self.values.len() == self.capacity {
            self.values.pop_front();
        }
        self.values.push_back(v);
    }

    /// Oldest first.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = f32> + '_ {
        self.values.iter().copied()
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn last(&self) -> Option<f32> {
        self.values.back().copied()
    }
}

/// A meter reading that falls back slowly after peaks, as meters do: `fall` dB per second.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ballistic {
    pub db: f32,
    pub peak: f32,
    held: f32,
}

impl Default for Ballistic {
    fn default() -> Self {
        Self { db: FLOOR, peak: FLOOR, held: 0.0 }
    }
}

/// The bottom of the meters, in dB.
pub const FLOOR: f32 = -90.0;

impl Ballistic {
    /// A reading that starts at 0 (gain reduction meters).
    pub fn zero() -> Self {
        Self { db: 0.0, peak: 0.0, held: 0.0 }
    }

    pub fn update(&mut self, db: f32, dt: f32, fall: f32) {
        let db = db.max(FLOOR);
        self.db = if db > self.db { db } else { (self.db - fall * dt).max(db) };
        if db >= self.peak {
            self.peak = db;
            self.held = 0.0;
        } else {
            self.held += dt;
            if self.held > 1.5 {
                self.peak = (self.peak - fall * dt).max(self.db);
            }
        }
    }
}

pub fn amp_db(v: f32) -> f32 {
    if v > 1e-9 { 20.0 * v.log10() } else { FLOOR }
}

/// Frequency labels for a log axis between `lo` and `hi`.
pub fn freq_ticks(lo: f64, hi: f64) -> Vec<(f64, String)> {
    [20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0]
        .into_iter()
        .filter(|f| (lo..=hi).contains(f))
        .map(|f| (f, if f >= 1000.0 { format!("{}k", f / 1000.0) } else { format!("{f}") }))
        .collect()
}

/// Labels on a dB axis every `step` dB from `lo` to `hi`.
pub fn db_ticks(lo: f64, hi: f64, step: f64) -> Vec<f64> {
    let first = (lo / step).ceil() as i64;
    let last = (hi / step).floor() as i64;
    (first..=last).map(|k| k as f64 * step).collect()
}

/// How to show a frequency briefly.
pub fn hz_text(hz: f64) -> String {
    if hz >= 10000.0 {
        format!("{:.1} kHz", hz / 1000.0)
    } else if hz >= 1000.0 {
        format!("{:.2} kHz", hz / 1000.0)
    } else {
        format!("{hz:.0} Hz")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;

    fn info(min: f64, max: f64, log: bool) -> ParamInfo {
        ParamInfo { id: 0, name: "x".into(), min, max, default: min, log }
    }

    #[test]
    fn plots_map_values_to_the_screen_and_back() {
        let plot = Plot::new(
            Rect::from_min_size(pos2(10.0, 20.0), vec2(600.0, 300.0)),
            Scale::log(20.0, 20000.0),
            Scale::lin(-24.0, 24.0),
        );
        for (hz, db) in [(20.0, -24.0), (1000.0, 0.0), (5432.1, 7.5), (20000.0, 24.0)] {
            let p = plot.point(hz, db);
            let (h, d) = plot.values(p);
            assert!((h / hz - 1.0).abs() < 1e-4 && (d - db).abs() < 1e-3, "{hz} {db} -> {h} {d}");
        }
        assert_eq!(plot.sx(20.0), 10.0);
        assert_eq!(plot.sy(24.0), 20.0);
        assert!((plot.sx(632.455_5) - 310.0).abs() < 0.01);
    }

    #[test]
    fn nudges_move_by_a_share_of_the_range() {
        let hz = info(20.0, 20000.0, true);
        let up = nudge(&hz, 1000.0, 1.0, false, false);
        assert!((up / 1000.0 - 1000f64.powf(0.01)).abs() < 1e-9);
        assert!((nudge(&hz, up, -1.0, false, false) - 1000.0).abs() < 1e-9);
        let db = info(-24.0, 24.0, false);
        assert!((nudge(&db, 0.0, 1.0, false, false) - 0.48).abs() < 1e-9);
        assert!((nudge(&db, 0.0, -1.0, true, false) + 0.048).abs() < 1e-9);
        assert_eq!(nudge(&db, 23.9, 5.0, false, false), 24.0);
        let switch = info(0.0, 1.0, false);
        assert_eq!(nudge(&switch, 0.0, 1.0, false, true), 1.0);
    }

    #[test]
    fn drags_cover_the_range_over_the_control() {
        let db = info(-60.0, 0.0, false);
        assert!((drag(&db, -60.0, -100.0, 100.0, false)).abs() < 1e-9);
        assert!((drag(&db, -30.0, 10.0, 100.0, false) + 36.0).abs() < 1e-9);
        assert!((drag(&db, -30.0, 10.0, 100.0, true) + 30.6).abs() < 1e-9);
    }

    #[test]
    fn typed_values_take_units() {
        assert_eq!(parse_value("1.5k", "1000 Hz"), Some(1500.0));
        assert_eq!(parse_value("2 kHz", "1000 Hz"), Some(2000.0));
        assert_eq!(parse_value("-3 dB", "+0.0 dB"), Some(-3.0));
        assert_eq!(parse_value("0,5", "0.25 oct"), Some(0.5));
        assert_eq!(parse_value("1.2 s", "250.0 ms"), Some(1200.0));
        assert_eq!(parse_value("-inf", "+0.0 dB"), Some(-150.0));
        assert_eq!(parse_value("abc", "1 Hz"), None);
        assert_eq!(parse_value("4 apples", "1 Hz"), None);
        assert_eq!(parse_value("", "1 Hz"), None);
    }

    #[test]
    fn history_keeps_the_newest_values() {
        let mut h = History::new(3);
        for v in [1.0, 2.0, 3.0, 4.0] {
            h.push(v);
        }
        assert_eq!(h.iter().collect::<Vec<_>>(), vec![2.0, 3.0, 4.0]);
        assert_eq!(h.last(), Some(4.0));
    }

    #[test]
    fn meters_fall_slowly_and_hold_their_peak() {
        let mut m = Ballistic::default();
        m.update(-6.0, 0.02, 20.0);
        assert_eq!((m.db, m.peak), (-6.0, -6.0));
        m.update(-40.0, 0.5, 20.0);
        assert_eq!(m.db, -16.0);
        assert_eq!(m.peak, -6.0);
        for _ in 0..10 {
            m.update(-40.0, 0.5, 20.0);
        }
        assert_eq!(m.db, -40.0);
        assert!(m.peak < -6.0);
    }

    #[test]
    fn ticks_cover_the_axis() {
        assert_eq!(freq_ticks(20.0, 1000.0).last().unwrap().1, "1k");
        assert_eq!(db_ticks(-24.0, 24.0, 12.0), vec![-24.0, -12.0, 0.0, 12.0, 24.0]);
        assert_eq!(hz_text(1234.0), "1.23 kHz");
    }
}
