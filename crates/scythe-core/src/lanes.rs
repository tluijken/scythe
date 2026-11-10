//! Fixed item lanes (REAPER 7): lanes, the lane recording goes into, and comping. A comp lane
//! holds copies of pieces of the other lanes; each piece comes from a comp area
//! (`LINKEDLANE start end lane ...`) and the comp lane's items are rebuilt from the areas
//! whenever they change, as REAPER does when you swipe.

use crate::fade::Fade;
use crate::model::{MIN_ITEM_LENGTH, Track};

/// Overlap of neighbouring comp areas, and the fades at a comp area's own edges.
pub const COMP_FADE: f64 = 0.01;

/// One comp area: the comp lane plays `lane` from `start` to `end`.
#[derive(Debug, Clone, PartialEq)]
pub struct CompArea {
    pub start: f64,
    pub end: f64,
    /// The source lane.
    pub lane: usize,
    pub fade_in: f64,
    pub fade_out: f64,
    /// `LINKEDLANE` fields 4 and 5, kept as REAPER wrote them.
    pub extra: [String; 2],
}

impl CompArea {
    pub fn new(start: f64, end: f64, lane: usize) -> Self {
        Self { start, end, lane, fade_in: COMP_FADE, fade_out: COMP_FADE, extra: ["0".into(), "-1".into()] }
    }
}

/// What `LANESOLO` stores to play only `lanes`.
pub fn solo_words(lanes: impl IntoIterator<Item = usize>) -> Vec<u32> {
    let mut words = vec![0u32; 8];
    for lane in lanes {
        if let Some(w) = words.get_mut(lane / 32) {
            *w |= 1 << (lane % 32);
        }
    }
    words
}

impl Track {
    /// Number of fixed lanes: one for a track without them.
    pub fn lane_count(&self) -> usize {
        if !self.fixed_lanes {
            return 1;
        }
        let from_items = self
            .items
            .iter()
            .map(|it| match it.ypos {
                Some((_, h)) if h > 0.0 => ((1.0 / h).round() as usize).max(it.lane() + 1),
                _ => it.lane() + 1,
            })
            .max()
            .unwrap_or(1);
        from_items.max(self.lane_names.len()).max(1)
    }

    /// How many track heights the track takes in the arrange view. With REAPER's big lanes
    /// (`C_LANESETTINGS` &8, on by default) each fixed lane is as tall as the track height, so
    /// the track grows with its lanes; without them the lanes share one height.
    pub fn height_in_lanes(&self) -> usize {
        if self.fixed_lanes && self.lane_settings() & 8 != 0 { self.lane_count() } else { 1 }
    }

    /// The comp lane being built, when there is one.
    pub fn comp_lane(&self) -> Option<usize> {
        let lane = self.lane_rec?[1];
        (self.fixed_lanes && lane >= 0).then_some(lane as usize)
    }

    /// The `FIXEDLANES` settings (`C_LANESETTINGS`): 2 keeps new recordings out of the comp,
    /// 16 adds new recordings at the bottom.
    pub fn lane_settings(&self) -> i64 {
        self.extra
            .iter()
            .find_map(|n| match n {
                crate::rpp::Node::Line(l) if crate::rpp::tokenize(l).first().is_some_and(|k| k == "FIXEDLANES") => {
                    crate::rpp::tokenize(l).get(1).and_then(|v| v.parse().ok())
                }
                _ => None,
            })
            .unwrap_or(9)
    }

    /// Turns fixed lanes on, items staying in the lanes their placement says.
    pub fn set_fixed_lanes(&mut self, on: bool) {
        if on == self.fixed_lanes {
            return;
        }
        self.fixed_lanes = on;
        if on {
            let n = self.lane_count();
            self.layout_lanes(n);
        }
    }

    /// Places every item in its lane for `n` lanes, names lanes that have none and keeps
    /// `n` names.
    pub fn layout_lanes(&mut self, n: usize) {
        let n = n.max(1);
        for it in &mut self.items {
            let lane = it.lane().min(n - 1);
            it.ypos = Some((lane as f64 / n as f64, 1.0 / n as f64));
        }
        while self.lane_names.len() < n {
            let next = self.lane_names.iter().filter(|s| !s.starts_with('C')).count() + 1;
            self.lane_names.push(next.to_string());
        }
        self.lane_names.truncate(n);
    }

    /// Inserts an empty lane at `at`; what was there and below moves down one lane.
    pub fn insert_lane(&mut self, at: usize, name: &str) {
        let n = self.lane_count();
        let at = at.min(n);
        self.fixed_lanes = true;
        self.layout_lanes(n);
        let shift = |lane: usize| if lane >= at { lane + 1 } else { lane };
        let h = 1.0 / (n + 1) as f64;
        for it in &mut self.items {
            let lane = shift(it.lane());
            it.ypos = Some((lane as f64 * h, h));
        }
        for a in &mut self.comp_areas {
            a.lane = shift(a.lane);
        }
        if let Some(rec) = &mut self.lane_rec {
            for i in [0, 1, 3] {
                if rec[i] >= at as i64 {
                    rec[i] += 1;
                }
            }
        }
        if let Some(words) = &mut self.lane_solo {
            let plays: Vec<usize> =
                (0..n).filter(|&l| words.get(l / 32).is_some_and(|w| w >> (l % 32) & 1 != 0)).collect();
            *words = solo_words(plays.into_iter().map(shift));
        }
        self.lane_names.insert(at, name.to_owned());
    }

    /// Puts item `id` in fixed lane `lane`, adding lanes down to it (`I_FIXEDLANE`). Does
    /// nothing on a track without fixed lanes.
    pub fn set_item_lane(&mut self, id: crate::model::ItemId, lane: usize) -> bool {
        if !self.fixed_lanes || self.item(id).is_none() {
            return false;
        }
        let n = self.lane_count().max(lane + 1);
        self.layout_lanes(n);
        if let Some(it) = self.items.iter_mut().find(|it| it.id == id) {
            it.ypos = Some((lane as f64 / n as f64, 1.0 / n as f64));
        }
        true
    }

    /// Adds an empty lane below the others; returns its index.
    pub fn add_lane(&mut self) -> usize {
        let n = self.lane_count();
        if !self.fixed_lanes {
            self.fixed_lanes = true;
            self.layout_lanes(n);
            if self.items.is_empty() {
                return 0;
            }
        }
        self.layout_lanes(n);
        let next = self.lane_names.iter().filter(|s| !s.starts_with('C')).count() + 1;
        self.insert_lane(n, &next.to_string());
        n
    }

    /// Plays only `lanes` (`LANESOLO`).
    pub fn play_lanes(&mut self, lanes: impl IntoIterator<Item = usize>) {
        self.lane_solo = Some(solo_words(lanes));
    }

    /// Lanes that play.
    pub fn playing_lanes(&self) -> Vec<usize> {
        (0..self.lane_count()).filter(|&l| self.lane_plays(l)).collect()
    }

    /// Whether `lane` has an item overlapping `from..to`.
    pub fn lane_busy(&self, lane: usize, from: f64, to: f64) -> bool {
        self.items.iter().any(|it| it.lane() == lane && it.position < to - 1e-9 && it.end() > from + 1e-9)
    }

    /// "Comp into new empty lane": a comp lane on top that alone plays, with no areas yet.
    /// Returns its index (0).
    pub fn create_comp_lane(&mut self) -> usize {
        let n = self.lane_names.iter().filter(|s| s.starts_with('C')).count() + 1;
        if !self.fixed_lanes {
            self.fixed_lanes = true;
            self.layout_lanes(self.lane_count());
        }
        self.insert_lane(0, &format!("C{n}"));
        let mut rec = self.lane_rec.unwrap_or([-1; 4]);
        rec[1] = 0;
        self.lane_rec = Some(rec);
        self.comp_areas.clear();
        self.play_lanes([0]);
        0
    }

    /// Stops comping: the comp lane stays as an ordinary lane.
    pub fn stop_comping(&mut self) {
        if let Some(rec) = &mut self.lane_rec {
            rec[1] = -1;
        }
        self.comp_areas.clear();
    }

    /// Swipes `start..end` of `lane` into the comp: the new area wins its whole range, and the
    /// areas it overlaps are cut back so they cross-fade into it over [`COMP_FADE`].
    pub fn swipe(&mut self, lane: usize, start: f64, end: f64) -> bool {
        let Some(comp) = self.comp_lane() else { return false };
        if lane == comp || end - start <= MIN_ITEM_LENGTH || lane >= self.lane_count() {
            return false;
        }
        let mut areas = Vec::with_capacity(self.comp_areas.len() + 2);
        for a in self.comp_areas.drain(..) {
            if a.end <= start || a.start >= end {
                areas.push(a);
                continue;
            }
            if a.start < start {
                areas.push(CompArea { end: start + COMP_FADE, ..a.clone() });
            }
            if a.end > end {
                areas.push(CompArea { start: end - COMP_FADE, ..a });
            }
        }
        areas.push(CompArea::new(start, end, lane));
        areas.sort_by(|a, b| a.start.total_cmp(&b.start));
        self.comp_areas = areas;
        self.recomp();
        true
    }

    /// Removes the comp area at `at` (seconds), leaving a gap in the comp.
    pub fn remove_comp_area(&mut self, at: f64) -> bool {
        let before = self.comp_areas.len();
        self.comp_areas.retain(|a| !(a.start <= at && at < a.end));
        let removed = self.comp_areas.len() != before;
        if removed {
            self.recomp();
        }
        removed
    }

    /// Rebuilds the comp lane's items from the comp areas.
    pub fn recomp(&mut self) {
        let Some(comp) = self.comp_lane() else { return };
        let n = self.lane_count();
        let ypos = Some((comp as f64 / n as f64, 1.0 / n as f64));
        self.items.retain(|it| it.lane() != comp);
        let mut pieces = Vec::new();
        for (i, area) in self.comp_areas.iter().enumerate() {
            let prev = i.checked_sub(1).and_then(|p| self.comp_areas.get(p)).filter(|p| p.end > area.start);
            let next = self.comp_areas.get(i + 1).filter(|n| n.start < area.end);
            for src in self.items.iter().filter(|it| it.lane() == area.lane) {
                let (from, to) = (area.start.max(src.position), area.end.min(src.end()));
                if to - from <= MIN_ITEM_LENGTH {
                    continue;
                }
                let mut piece = src.duplicate();
                piece.trim_start(from);
                piece.trim_end(to);
                piece.ypos = ypos;
                let (fade_in, line_in) = comp_fade(
                    "FADEIN",
                    prev.filter(|_| from <= area.start + 1e-9).map(|p| (p.end - area.start, p.lane == area.lane)),
                    area.fade_in,
                );
                let (fade_out, line_out) = comp_fade(
                    "FADEOUT",
                    next.filter(|_| to >= area.end - 1e-9).map(|n| (area.end - n.start, n.lane == area.lane)),
                    area.fade_out,
                );
                piece.fade_in = fade_in;
                piece.fade_out = fade_out;
                piece.lines.insert("FADEIN".into(), line_in);
                piece.lines.insert("FADEOUT".into(), line_out);
                pieces.push(piece);
            }
        }
        self.items.extend(pieces);
        self.items.sort_by(|a, b| a.position.total_cmp(&b.position));
    }
}

/// A comp piece's fade: a cross-fade with a neighbouring area (overlap, same source lane) as
/// REAPER writes it (shape 0 between pieces of one lane, `1.1` between lanes), else the area's
/// own edge fade.
fn comp_fade(key: &str, neighbour: Option<(f64, bool)>, own: f64) -> (Fade, Vec<String>) {
    let line = |shape: &str, len: f64, auto: f64, flag: &str| {
        [key, shape, &fmt(len), &fmt(auto), "1", flag, "0", "0"].map(str::to_owned).to_vec()
    };
    match neighbour {
        Some((overlap, same)) => {
            let overlap = (overlap * 1e9).round() / 1e9;
            let shape = if same { 0 } else { 1 };
            (
                Fade { length: overlap, auto_length: overlap, shape, curve: 0.0, power: !same },
                line(if same { "0" } else { "1.1" }, overlap, overlap, "1"),
            )
        }
        None => (Fade::new(own), line("1", own, 0.0, "0")),
    }
}

fn fmt(v: f64) -> String {
    crate::rpp::fmt_num(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Item, Take};

    fn item(lane: usize, lanes: usize, pos: f64, len: f64, file: &str) -> Item {
        let mut it = Item::new(pos, len, Take::from_file(file.into()));
        it.ypos = Some((lane as f64 / lanes as f64, 1.0 / lanes as f64));
        it
    }

    /// REAPER 7.73: 136 px tracks are 272, 544, 1088 px tall with 2, 4, 8 fixed lanes (and 8 lanes
    /// of a 40 px track 320), but stay 136 with `C_LANESETTINGS` 1, without big lanes.
    #[test]
    fn big_lanes_grow_the_track_as_reaper_does() {
        let mut t = Track::new("lanes");
        assert_eq!(t.height_in_lanes(), 1);
        t.items = (0..8).map(|l| item(l, 8, l as f64, 0.5, "a.wav")).collect();
        assert_eq!(t.height_in_lanes(), 1, "without fixed lanes");
        t.fixed_lanes = true;
        assert_eq!(t.height_in_lanes(), 8);
        let mut u = Track::new("set");
        u.items = (0..4).map(|l| Item::new(l as f64, 0.5, Take::from_file("a.wav".into()))).collect();
        let ids: Vec<_> = u.items.iter().map(|i| i.id).collect();
        assert!(!u.set_item_lane(ids[1], 1), "no lanes to put it in");
        u.fixed_lanes = true;
        for (lane, id) in ids.iter().enumerate().rev() {
            assert!(u.set_item_lane(*id, lane));
        }
        assert_eq!(u.items.iter().map(|i| i.lane()).collect::<Vec<_>>(), [0, 1, 2, 3]);
        assert_eq!(u.height_in_lanes(), 4);
        t.extra.push(crate::rpp::Node::Line("FIXEDLANES 1 0 0 0 0".into()));
        assert_eq!(t.height_in_lanes(), 1);
    }

    /// Three lanes a, b, c, then "comp into new empty lane" and the swipes of the REAPER probe
    /// (razor edits turned into comp areas), compared with REAPER 7.73's saved project.
    #[test]
    fn swipes_match_reaper() {
        let mut t = Track::new("comp");
        t.fixed_lanes = true;
        t.items = vec![item(0, 3, 1.0, 8.0, "b.wav"), item(1, 3, 0.5, 8.0, "c.wav"), item(2, 3, 1.0, 4.0, "a.wav")];
        t.lane_names = vec!["1".into(), "2".into(), "3".into()];
        assert_eq!(t.create_comp_lane(), 0);
        assert_eq!(t.lane_names, ["C1", "1", "2", "3"]);
        assert_eq!(t.lane_rec, Some([-1, 0, -1, -1]));
        assert_eq!(t.lane_solo.as_ref().unwrap()[0], 1);
        assert!(t.items.iter().all(|i| i.ypos.unwrap().1 == 0.25));

        assert!(t.swipe(1, 1.5, 2.5));
        let comp: Vec<_> = t.items.iter().filter(|i| i.lane() == 0).collect();
        assert_eq!(comp.len(), 1);
        assert_eq!((comp[0].position, comp[0].length, comp[0].takes[0].source_offset), (1.5, 1.0, 0.5));
        t.swipe(2, 2.5, 3.0);
        t.swipe(3, 3.5, 4.25);
        t.swipe(2, 2.0, 2.75);
        let areas: Vec<_> = t.comp_areas.iter().map(|a| (a.start, a.end, a.lane)).collect();
        let r = |v: f64| (v * 1e9).round() / 1e9;
        let areas: Vec<_> = areas.into_iter().map(|(a, b, l)| (r(a), r(b), l)).collect();
        assert_eq!(areas, [(1.5, 2.01, 1), (2.0, 2.75, 2), (2.74, 3.0, 2), (3.5, 4.25, 3)]);
        let comp: Vec<_> = t.items.iter().filter(|i| i.lane() == 0).collect();
        let got: Vec<_> = comp
            .iter()
            .map(|i| {
                (
                    r(i.position),
                    r(i.length),
                    r(i.takes[0].source_offset),
                    i.lines["FADEIN"][1..4].join(" "),
                    i.lines["FADEOUT"][1..4].join(" "),
                )
            })
            .collect();
        let want = [
            (1.5, 0.51, 0.5, "1 0.01 0", "1.1 0.01 0.01"),
            (2.0, 0.75, 1.5, "1.1 0.01 0.01", "0 0.01 0.01"),
            (2.74, 0.26, 2.24, "0 0.01 0.01", "1 0.01 0"),
            (3.5, 0.75, 2.5, "1 0.01 0", "1 0.01 0"),
        ];
        assert_eq!(got.len(), want.len());
        for (g, w) in got.iter().zip(want) {
            assert_eq!((g.0, g.1, g.2, g.3.as_str(), g.4.as_str()), w);
        }
        assert!(t.remove_comp_area(3.6));
        assert_eq!(t.items.iter().filter(|i| i.lane() == 0).count(), 3);
    }

    #[test]
    fn lanes_grow_and_shift() {
        let mut t = Track::new("l");
        t.items = vec![Item::new(0.0, 1.0, Take::from_file("a.wav".into()))];
        assert_eq!(t.lane_count(), 1);
        assert_eq!(t.add_lane(), 1);
        assert!(t.fixed_lanes);
        assert_eq!(t.lane_count(), 2);
        assert_eq!(t.lane_names, ["1", "2"]);
        t.play_lanes([1]);
        t.insert_lane(0, "x");
        assert_eq!(t.items[0].lane(), 1);
        assert_eq!(t.playing_lanes(), [2]);
        assert!(t.lane_busy(1, 0.5, 2.0) && !t.lane_busy(2, 0.5, 2.0));
    }
}
