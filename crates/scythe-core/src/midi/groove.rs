//! Groove templates in the `.rgt` form REAPER's groove tool reads and writes:
//!
//! ```text
//! Version: 1
//! Number of beats in groove: 2
//! Groove: 4 positions
//! 0.0000000000
//! 0.5600000000
//! ```
//!
//! Positions are in beats (quarter notes) from the pattern start; a second number on a line is
//! that hit's level (0-1), used for velocities. The pattern repeats every `beats`.

use super::edit::{EditNote, targets};

#[derive(Debug, Clone, PartialEq)]
pub struct Groove {
    pub beats: f64,
    /// (position in beats, level).
    pub hits: Vec<(f64, Option<f64>)>,
}

impl Groove {
    pub fn parse(text: &str) -> Option<Self> {
        let mut beats = None;
        let mut hits = Vec::new();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if let Some((key, value)) = line.split_once(':') {
                if key.trim().eq_ignore_ascii_case("Number of beats in groove") {
                    beats = value.trim().parse::<f64>().ok();
                }
                continue;
            }
            let mut nums = line.split_whitespace().map(|v| v.parse::<f64>());
            let Some(Ok(pos)) = nums.next() else { continue };
            hits.push((pos, nums.next().and_then(Result::ok)));
        }
        let beats = beats.filter(|b| *b > 0.0)?;
        hits.retain(|h| h.0 >= 0.0 && h.0 < beats);
        hits.sort_by(|a, b| a.0.total_cmp(&b.0));
        (!hits.is_empty()).then_some(Self { beats, hits })
    }

    pub fn write(&self) -> String {
        let levels = self.hits.iter().any(|h| h.1.is_some());
        let mut out = format!(
            "Version: {}\nNumber of beats in groove: {}\nGroove: {} positions\n",
            if levels { 2 } else { 1 },
            self.beats,
            self.hits.len()
        );
        for (pos, level) in &self.hits {
            match level {
                Some(l) if levels => out.push_str(&format!("{pos:.10} {l:.10}\n")),
                _ => out.push_str(&format!("{pos:.10}\n")),
            }
        }
        out
    }

    /// A groove from note starts (quarter notes), folded into a pattern `beats` long; levels
    /// come from the velocities.
    pub fn from_hits(hits: &[(f64, u8)], beats: f64) -> Option<Self> {
        if beats <= 0.0 {
            return None;
        }
        let mut out: Vec<(f64, Option<f64>)> =
            hits.iter().map(|&(qn, vel)| (qn.rem_euclid(beats), Some(vel as f64 / 127.0))).collect();
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        out.dedup_by(|b, a| (a.0 - b.0).abs() < 1e-6);
        (!out.is_empty()).then_some(Self { beats, hits: out })
    }

    /// The hit nearest `qn` (quarter notes from the pattern's start), with its level.
    pub fn nearest(&self, qn: f64) -> (f64, Option<f64>) {
        let base = (qn / self.beats).floor() * self.beats;
        [base - self.beats, base, base + self.beats]
            .into_iter()
            .flat_map(|b| self.hits.iter().map(move |&(p, l)| (b + p, l)))
            .min_by(|a, b| (a.0 - qn).abs().total_cmp(&(b.0 - qn).abs()))
            .unwrap_or((qn, None))
    }

    /// Where `qn` moves with `strength` (0-1), and the level of the hit it moves to.
    pub fn apply(&self, qn: f64, strength: f64) -> (f64, Option<f64>) {
        let (target, level) = self.nearest(qn);
        (qn + (target - qn) * strength.clamp(0.0, 1.0), level)
    }
}

/// Moves the target notes' starts (keeping lengths) towards the groove by `strength`, and
/// their velocities towards the hits' levels by `velocity_strength`. `offset_qn` is where the
/// source starts in the groove's time (the item's position in project quarter notes).
pub fn apply_to_notes(
    notes: &mut [EditNote],
    ppq: u32,
    groove: &Groove,
    offset_qn: f64,
    strength: f64,
    velocity_strength: f64,
) {
    let ppq = ppq as f64;
    for i in targets(notes) {
        let n = &mut notes[i];
        let (qn, level) = groove.apply(offset_qn + n.start as f64 / ppq, strength);
        let len = n.length();
        n.start = ((qn - offset_qn) * ppq).round().max(0.0) as u64;
        n.end = n.start + len;
        if let Some(level) = level {
            let v = n.velocity as f64;
            let target = level.clamp(0.0, 1.0) * 127.0;
            n.velocity = (v + (target - v) * velocity_strength.clamp(0.0, 1.0)).round().clamp(1.0, 127.0) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_rgt() {
        let text = "Version: 1\nNumber of beats in groove: 2\nGroove: 4 positions\n0.0000000000\n0.5600000000\n1.0000000000\n1.5600000000\n";
        let g = Groove::parse(text).unwrap();
        assert_eq!(g.beats, 2.0);
        assert_eq!(g.hits.len(), 4);
        assert_eq!(g.write(), text);
        assert_eq!(g.nearest(2.6), (2.56, None), "the pattern repeats");
        assert!((g.apply(0.5, 0.5).0 - 0.53).abs() < 1e-12);
    }

    #[test]
    fn grooves_notes_and_their_velocities() {
        let g = Groove { beats: 1.0, hits: vec![(0.0, Some(1.0)), (0.6, Some(0.5))] };
        let mut n = vec![EditNote::new(480, 600, 0, 60, 100), EditNote::new(960, 1000, 0, 60, 100)];
        apply_to_notes(&mut n, 960, &g, 0.0, 1.0, 1.0);
        assert_eq!((n[0].start, n[0].end, n[0].velocity), (576, 696, 64));
        assert_eq!((n[1].start, n[1].velocity), (960, 127));
        let made = Groove::from_hits(&[(0.0, 127), (0.56, 64), (1.0, 127), (1.56, 64)], 1.0).unwrap();
        assert_eq!(made.hits.len(), 2);
    }
}
