//! The nudge/set dialog ("Item edit: Nudge/Set...") and ReaScript's `ApplyNudge`, with
//! REAPER's unit numbering. Measured in REAPER 7.73: note units count from 1/256 (3) up to a
//! whole note (15) with triplets in between, measures.beats reads `1.15` as one measure and
//! 1.5 beats, and "left edge" moves the edge with the content while "left trim" leaves the
//! content in place.

use crate::{ItemId, Project};

/// What a nudge moves (`nudgewhat`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NudgeWhat {
    #[default]
    Position,
    /// The left edge, the content staying where it is.
    LeftTrim,
    /// The left edge with the content.
    LeftEdge,
    RightEdge,
    /// The content inside the item.
    Contents,
    /// Copies placed one nudge apart.
    Duplicate,
    EditCursor,
}

impl NudgeWhat {
    pub fn from_code(code: i64) -> Option<Self> {
        Some(match code {
            0 => Self::Position,
            1 => Self::LeftTrim,
            2 => Self::LeftEdge,
            3 => Self::RightEdge,
            4 => Self::Contents,
            5 => Self::Duplicate,
            6 => Self::EditCursor,
            _ => return None,
        })
    }

    pub const ALL: [(Self, &'static str); 7] = [
        (Self::Position, "Position"),
        (Self::LeftTrim, "Left trim"),
        (Self::LeftEdge, "Left edge"),
        (Self::RightEdge, "Right edge"),
        (Self::Contents, "Contents"),
        (Self::Duplicate, "Duplicate"),
        (Self::EditCursor, "Edit cursor"),
    ];
}

/// The unit of a nudge (`nudgeunits`), 0 to 21.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NudgeUnit(pub u8);

impl Default for NudgeUnit {
    fn default() -> Self {
        Self::MS
    }
}

impl NudgeUnit {
    pub const MS: Self = Self(0);
    pub const SECONDS: Self = Self(1);
    pub const GRID: Self = Self(2);
    pub const MEASURES_BEATS: Self = Self(16);
    pub const SAMPLES: Self = Self(17);
    pub const FRAMES: Self = Self(18);
    pub const PIXELS: Self = Self(19);
    pub const ITEM_LENGTHS: Self = Self(20);
    pub const ITEM_SELECTIONS: Self = Self(21);

    /// Whole notes of the note units 3 to 15.
    const NOTES: [f64; 13] = [
        1.0 / 256.0,
        1.0 / 128.0,
        1.0 / 64.0,
        1.0 / 48.0,
        1.0 / 32.0,
        1.0 / 24.0,
        1.0 / 16.0,
        1.0 / 12.0,
        1.0 / 8.0,
        1.0 / 6.0,
        1.0 / 4.0,
        1.0 / 2.0,
        1.0,
    ];

    pub fn label(self) -> String {
        match self.0 {
            0 => "milliseconds".into(),
            1 => "seconds".into(),
            2 => "grid units".into(),
            3..=15 => {
                let whole = Self::NOTES[self.0 as usize - 3];
                let den = (1.0 / whole).round() as i64;
                if den % 3 == 0 { format!("1/{} triplet notes", den * 2 / 3) } else { format!("1/{den} notes") }
            }
            16 => "measures.beats".into(),
            17 => "samples".into(),
            18 => "frames".into(),
            19 => "pixels".into(),
            20 => "item lengths".into(),
            21 => "item selections".into(),
            _ => "?".into(),
        }
    }

    pub const COUNT: u8 = 22;
}

/// One nudge or set, as the dialog and `ApplyNudge` take it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Nudge {
    /// Set to `value` instead of nudging by it.
    pub set: bool,
    /// Snap the result to the grid.
    pub snap: bool,
    pub what: NudgeWhat,
    pub unit: NudgeUnit,
    pub value: f64,
    /// Nudge left (earlier) instead of right.
    pub reverse: bool,
    /// Copies for `Duplicate`.
    pub copies: u32,
}

impl Default for Nudge {
    /// "Nudge position by 1 ms": what REAPER's "Nudge left/right by last nudge dialog
    /// settings" do before the dialog was ever used.
    fn default() -> Self {
        Self {
            set: false,
            snap: false,
            what: NudgeWhat::Position,
            unit: NudgeUnit::MS,
            value: 1.0,
            reverse: false,
            copies: 1,
        }
    }
}

/// What a nudge also needs to know: the edit cursor and the width of a pixel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NudgeContext {
    pub cursor: f64,
    /// Seconds per pixel at the current zoom.
    pub pixel: f64,
}

impl Nudge {
    /// The nudge in seconds, counted from `at` (note and grid units follow the tempo there).
    fn seconds(&self, p: &Project, at: f64, item_len: f64, selection_len: f64, pixel: f64) -> f64 {
        let tl = p.tempo.timeline();
        let in_qn = |qn: f64| tl.time_at_qn(tl.qn_at(at.max(0.0)) + qn) - at.max(0.0);
        let v = self.value;
        match self.unit.0 {
            0 => v / 1000.0,
            1 => v,
            2 => v * p.editing.grid.step_seconds(&p.tempo, at),
            u @ 3..=15 => in_qn(v * 4.0 * NudgeUnit::NOTES[u as usize - 3]),
            16 => {
                let measures = v.trunc();
                let beats = (v - measures) * 10.0;
                let (num, den) = p.tempo.time_sig_at(at.max(0.0));
                in_qn(measures * num as f64 * 4.0 / den as f64 + beats * 4.0 / den as f64)
            }
            17 => v / p.sample_rate.unwrap_or(44_100) as f64,
            18 => v / 30.0,
            19 => v * pixel,
            20 => v * item_len,
            _ => v * selection_len,
        }
    }

    /// Applies the nudge to `items` (their positions follow ripple, groups and locks like any
    /// move). Answers the items to select afterwards.
    pub fn apply(&self, p: &mut Project, items: &[ItemId], ctx: &mut NudgeContext) -> Vec<ItemId> {
        let sign = if self.reverse && !self.set { -1.0 } else { 1.0 };
        let found: Vec<(ItemId, f64, f64)> =
            items.iter().filter_map(|id| p.find_item(*id).map(|(_, it)| (*id, it.position, it.length))).collect();
        let first = found.iter().map(|f| f.1).fold(f64::INFINITY, f64::min);
        let last_end = found.iter().map(|f| f.1 + f.2).fold(f64::NEG_INFINITY, f64::max);
        let selection_len = (last_end - first).max(0.0);
        let snap = |p: &Project, t: f64| if self.snap { p.editing.grid.nearest_line(&p.tempo, t) } else { t };
        // A set value is a time in the unit; note and grid units count from the start.
        let target = |p: &Project, len: f64| snap(p, self.seconds(p, 0.0, len, selection_len, ctx.pixel));
        match self.what {
            NudgeWhat::EditCursor => {
                ctx.cursor = if self.set {
                    target(p, 0.0)
                } else {
                    snap(p, ctx.cursor + sign * self.seconds(p, ctx.cursor, 0.0, selection_len, ctx.pixel))
                }
                .max(0.0);
                items.to_vec()
            }
            NudgeWhat::Position => {
                if found.is_empty() {
                    return Vec::new();
                }
                let len = found[0].2;
                let to = if self.set {
                    target(p, len)
                } else {
                    snap(p, first + sign * self.seconds(p, first, len, selection_len, ctx.pixel))
                };
                p.move_items(items, to - first);
                items.to_vec()
            }
            NudgeWhat::Duplicate => {
                let mut placed = Vec::new();
                for (id, pos, len) in &found {
                    let step = sign * self.seconds(p, *pos, *len, selection_len, ctx.pixel);
                    let Some((track, it)) = p.find_item(*id).map(|(t, it)| (t, it.clone())) else { continue };
                    for c in 1..=self.copies.max(1) {
                        let mut copy = it.duplicate();
                        copy.position = snap(p, (pos + step * c as f64).max(0.0));
                        placed.push(copy.id);
                        if let Some(t) = p.track_mut(track) {
                            t.items.push(copy);
                        }
                    }
                    if let Some(t) = p.track_mut(track) {
                        t.items.sort_by(|a, b| a.position.total_cmp(&b.position));
                    }
                }
                placed.last().copied().into_iter().collect()
            }
            what => {
                for (id, pos, len) in &found {
                    let edge = match what {
                        NudgeWhat::RightEdge => pos + len,
                        _ => *pos,
                    };
                    let d = self.seconds(p, edge, *len, selection_len, ctx.pixel);
                    let to = if self.set { target(p, *len) } else { snap(p, edge + sign * d) };
                    let edges_locked = p.find_item(*id).is_some_and(|(_, it)| p.edges_locked(it));
                    let Some(it) = p.item_mut_anywhere(*id) else { continue };
                    match what {
                        NudgeWhat::LeftTrim if !edges_locked => it.trim_start(to),
                        NudgeWhat::LeftEdge if !edges_locked => {
                            let to = to.clamp(0.0, it.end() - crate::model::MIN_ITEM_LENGTH);
                            it.length -= to - it.position;
                            it.position = to;
                        }
                        NudgeWhat::RightEdge if !edges_locked => it.trim_end(to),
                        NudgeWhat::Contents => {
                            let shift = if self.set { to - edge } else { sign * d };
                            for take in &mut it.takes {
                                take.source_offset -= shift * take.playrate;
                                take.shift_markers(shift * take.playrate);
                            }
                        }
                        _ => {}
                    }
                }
                let tracks: Vec<_> = found.iter().filter_map(|(id, ..)| p.find_item(*id).map(|(t, _)| t)).collect();
                for t in tracks {
                    p.auto_crossfade(t);
                }
                items.to_vec()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Item, Take};

    fn project() -> (Project, ItemId) {
        let mut p = Project { sample_rate: Some(48_000), ..Project::default() };
        let t = p.add_track("");
        let mut it = Item::new(1.0, 1.0, Take::from_file("a.wav".into()));
        it.takes[0].source_offset = 0.5;
        let id = it.id;
        p.track_mut(t).unwrap().items.push(it);
        (p, id)
    }

    fn run(n: Nudge) -> (f64, f64, f64, usize) {
        let (mut p, id) = project();
        let mut ctx = NudgeContext { cursor: 0.25, pixel: 0.01 };
        n.apply(&mut p, &[id], &mut ctx);
        let it = &p.tracks[0].items[0];
        (it.position, it.length, it.takes[0].source_offset, p.tracks[0].items.len())
    }

    fn close(a: (f64, f64, f64, usize), b: (f64, f64, f64, usize)) -> bool {
        (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6 && (a.2 - b.2).abs() < 1e-6 && a.3 == b.3
    }

    /// The results of REAPER 7.73's `ApplyNudge` at 120 BPM with a 1/4 grid.
    #[test]
    fn matches_apply_nudge() {
        let n = |what, unit: u8, value, set, snap| Nudge {
            what,
            unit: NudgeUnit(unit),
            value,
            set,
            snap,
            ..Nudge::default()
        };
        use NudgeWhat::*;
        assert!(close(run(n(Position, 0, 10.0, false, false)), (1.01, 1.0, 0.5, 1)));
        assert!(close(run(Nudge { reverse: true, ..n(Position, 0, 10.0, false, false) }), (0.99, 1.0, 0.5, 1)));
        assert!(close(run(n(Position, 2, 1.0, false, false)), (1.5, 1.0, 0.5, 1)));
        assert!(close(run(n(Position, 1, 2.2, true, false)), (2.2, 1.0, 0.5, 1)));
        assert!(close(run(n(Position, 1, 2.2, true, true)), (2.0, 1.0, 0.5, 1)));
        assert!(close(run(n(LeftTrim, 1, 0.1, false, false)), (1.1, 0.9, 0.6, 1)));
        assert!(close(run(n(LeftEdge, 1, 0.1, false, false)), (1.1, 0.9, 0.5, 1)));
        assert!(close(run(n(RightEdge, 1, 0.1, false, false)), (1.0, 1.1, 0.5, 1)));
        assert!(close(run(n(Contents, 1, 0.1, false, false)), (1.0, 1.0, 0.4, 1)));
        assert!(close(run(Nudge { copies: 2, ..n(Duplicate, 1, 0.5, false, false) }), (1.0, 1.0, 0.5, 3)));
        assert!(close(run(n(Position, 16, 0.2, false, false)), (2.0, 1.0, 0.5, 1)));
        assert!(close(run(n(Position, 17, 100.0, false, false)), (1.0 + 100.0 / 48_000.0, 1.0, 0.5, 1)));
        assert!(close(run(n(LeftTrim, 1, 0.5, true, false)), (0.5, 1.5, 0.0, 1)));
        assert!(close(run(n(RightEdge, 1, 2.5, true, false)), (1.0, 1.5, 0.5, 1)));
        assert!(close(run(n(Position, 20, 1.0, false, false)), (2.0, 1.0, 0.5, 1)));
        for (unit, want) in [(3, 1.0078125), (6, 1.0 + 1.0 / 24.0), (10, 1.0 + 1.0 / 6.0), (13, 1.5), (15, 3.0)] {
            assert!(close(run(n(Position, unit, 1.0, false, false)), (want, 1.0, 0.5, 1)), "unit {unit}");
        }
        let (mut p, id) = project();
        let mut ctx = NudgeContext { cursor: 0.25, pixel: 0.01 };
        n(EditCursor, 1, 0.5, false, false).apply(&mut p, &[id], &mut ctx);
        assert!((ctx.cursor - 0.75).abs() < 1e-9);
    }
}
