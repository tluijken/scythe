//! Who is heard while tracks are soloed, as REAPER 7.73 renders it.
//!
//! Soloing does not mute tracks wholesale; it decides per track whether its own material
//! plays and per route (main output, each send) whether signal passes:
//! - a soloed track and its folder children play and receive everything sent to them, so the
//!   tracks feeding them play into those sends (soloing a bus hears its sources), while the
//!   feeders' own outputs stay silent;
//! - the folder parents of a soloed track pass its signal on but their own material is silent;
//! - soloed in place, the signal also goes through the sends of the soloed track, its children
//!   and parents, and the tracks those reach pass it on (their own material silent);
//!   ignoring routing, those sends stay silent;
//! - with anything soloed, a track with solo defeat acts soloed in place unless muted.
//!
//! Muting a track or one of its folder parents silences it whatever is soloed.

use crate::model::{Project, SoloMode};

/// The result of [`Project::solo_routing`].
#[derive(Debug, Clone, PartialEq)]
pub struct SoloRouting {
    /// Per track: its own material (items, live input, click) plays.
    pub content: Vec<bool>,
    /// Per track: its output reaches its parent or the master.
    pub main_out: Vec<bool>,
    /// Per track: its hardware outputs play (it is heard in its own right).
    pub hw_out: Vec<bool>,
    /// Per track and send (in `Track::sends` order): the send carries signal.
    pub sends: Vec<Vec<bool>>,
}

impl SoloRouting {
    /// Whether the track does anything at all.
    pub fn processes(&self, i: usize) -> bool {
        self.content[i] || self.main_out[i] || self.hw_out[i] || self.sends[i].iter().any(|s| *s)
    }

    /// Whether the track is heard: its material plays or it passes soloed signal on.
    pub fn heard(&self, i: usize) -> bool {
        self.content[i] || self.hw_out[i]
    }
}

impl Project {
    pub fn solo_routing(&self) -> SoloRouting {
        let n = self.tracks.len();
        let parents = self.folder_parents();
        let ancestors = |i: usize| std::iter::successors(parents[i], |&p| parents[p]);
        let muted: Vec<bool> =
            (0..n).map(|i| self.tracks[i].muted || ancestors(i).any(|p| self.tracks[p].muted)).collect();
        let index: std::collections::HashMap<_, _> = self.tracks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
        // Where each send goes, `None` when the send is muted or its destination is gone.
        let send_dest: Vec<Vec<Option<usize>>> = self
            .tracks
            .iter()
            .map(|t| t.sends.iter().map(|s| (!s.muted).then(|| index.get(&s.dest).copied()).flatten()).collect())
            .collect();
        if !self.any_solo() {
            let on: Vec<bool> = muted.iter().map(|m| !m).collect();
            return SoloRouting {
                content: on.clone(),
                main_out: on.clone(),
                hw_out: on.clone(),
                sends: send_dest.iter().enumerate().map(|(i, d)| vec![on[i]; d.len()]).collect(),
            };
        }
        let soloed: Vec<Option<SoloMode>> = (0..n)
            .map(|i| {
                let t = &self.tracks[i];
                match () {
                    _ if muted[i] => None,
                    _ if t.solo => Some(t.solo_mode),
                    _ if t.solo_defeat => Some(SoloMode::InPlace),
                    _ => None,
                }
            })
            .collect();
        let under = |i: usize, pred: &dyn Fn(usize) -> bool| pred(i) || ancestors(i).any(pred);
        let core: Vec<bool> = (0..n).map(|i| !muted[i] && under(i, &|j| soloed[j].is_some())).collect();
        let mut above = vec![false; n];
        for i in (0..n).filter(|&i| soloed[i].is_some()) {
            for p in ancestors(i) {
                above[p] = true;
            }
        }
        // Edges out of a track: its main output (to the parent, `None` for the master) and its
        // sends.
        let outs = |i: usize| -> Vec<Option<usize>> {
            let main = self.tracks[i].main_send.then_some(parents[i]);
            main.into_iter().chain(send_dest[i].iter().filter_map(|d| d.map(Some))).collect()
        };
        // In-place downstream: what the signal of in-place soloed tracks (with their children
        // and parents) reaches.
        let mut downstream = vec![false; n];
        let mut queue: Vec<usize> = Vec::new();
        for i in (0..n).filter(|&i| !muted[i]) {
            let in_place = under(i, &|j| soloed[j] == Some(SoloMode::InPlace));
            let parent_of_in_place =
                (0..n).any(|j| soloed[j] == Some(SoloMode::InPlace) && ancestors(j).any(|p| p == i));
            if in_place || parent_of_in_place {
                queue.push(i);
            }
        }
        while let Some(i) = queue.pop() {
            for d in outs(i).into_iter().flatten() {
                if !muted[d] && !downstream[d] {
                    downstream[d] = true;
                    queue.push(d);
                }
            }
        }
        let active: Vec<bool> = (0..n).map(|i| !muted[i] && (core[i] || above[i] || downstream[i])).collect();
        let mut feeds = vec![false; n];
        loop {
            let mut changed = false;
            for i in 0..n {
                if muted[i] || core[i] || feeds[i] {
                    continue;
                }
                if outs(i).into_iter().flatten().any(|d| core[d] || feeds[d]) {
                    feeds[i] = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let content: Vec<bool> = (0..n).map(|i| !muted[i] && (core[i] || feeds[i])).collect();
        let passes = |i: usize, dest: Option<usize>| -> bool {
            if muted[i] {
                return false;
            }
            match dest {
                None => active[i],
                Some(d) => (core[d] && (content[i] || active[i])) || (feeds[d] && feeds[i]) || (active[d] && active[i]),
            }
        };
        SoloRouting {
            main_out: (0..n).map(|i| passes(i, parents[i])).collect(),
            hw_out: active.clone(),
            sends: send_dest
                .iter()
                .enumerate()
                .map(|(i, ds)| ds.iter().map(|d| d.is_some_and(|d| passes(i, Some(d)))).collect())
                .collect(),
            content,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Send, Track};

    /// Plain tracks named `names`.
    fn project(names: &[&str]) -> Project {
        let mut p = Project::default();
        for n in names {
            p.tracks.push(Track::new(*n));
        }
        p
    }

    fn send(p: &mut Project, from: usize, to: usize) {
        let dest = p.tracks[to].id;
        p.tracks[from].sends.push(Send::new(dest));
    }

    fn solo(p: &mut Project, i: usize, mode: SoloMode) {
        p.tracks[i].solo = true;
        p.tracks[i].solo_mode = mode;
    }

    /// How many times each track's material reaches the master: along every route that passes.
    fn heard(p: &Project) -> Vec<usize> {
        let r = p.solo_routing();
        let parents = p.folder_parents();
        let index: std::collections::HashMap<_, _> = p.tracks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
        fn paths(
            p: &Project,
            r: &SoloRouting,
            parents: &[Option<usize>],
            idx: &std::collections::HashMap<crate::TrackId, usize>,
            i: usize,
        ) -> usize {
            let mut n = 0;
            if p.tracks[i].main_send && r.main_out[i] {
                n += parents[i].map_or(1, |q| paths(p, r, parents, idx, q));
            }
            for (k, s) in p.tracks[i].sends.iter().enumerate() {
                if r.sends[i][k] {
                    n += paths(p, r, parents, idx, idx[&s.dest]);
                }
            }
            n
        }
        (0..p.tracks.len()).map(|i| if r.content[i] { paths(p, &r, &parents, &index, i) } else { 0 }).collect()
    }

    // Each case was rendered by REAPER 7.73 with a different tone per track; the numbers are
    // how loud each tone came out (scythe-engine's null set holds the renders).
    #[test]
    fn solo_cases_match_reaper() {
        use SoloMode::*;
        for (mode, want_src, want_bus) in [(IgnoreRouting, [1, 0, 0], [1, 1, 0]), (InPlace, [2, 0, 0], [1, 1, 0])] {
            let mut p = project(&["a", "b", "c"]);
            send(&mut p, 0, 1);
            solo(&mut p, 0, mode);
            assert_eq!(heard(&p), want_src, "source soloed {mode:?}");
            p.tracks[0].solo = false;
            solo(&mut p, 1, mode);
            assert_eq!(heard(&p), want_bus, "bus soloed {mode:?}");
        }
        // A -> B -> D, soloing A.
        for (mode, want) in [(IgnoreRouting, [1, 0, 0, 0]), (InPlace, [3, 0, 0, 0])] {
            let mut p = project(&["a", "b", "c", "d"]);
            send(&mut p, 0, 1);
            send(&mut p, 1, 3);
            solo(&mut p, 0, mode);
            assert_eq!(heard(&p), want, "chain {mode:?}");
            p.tracks[0].solo = false;
            solo(&mut p, 1, mode);
            let want_mid = if mode == InPlace { [2, 2, 0, 0] } else { [1, 1, 0, 0] };
            assert_eq!(heard(&p), want_mid, "middle of a chain {mode:?}");
        }
        // Folder P (sending to R) holding A; soloing the child, then the parent.
        for (mode, child, parent) in
            [(IgnoreRouting, [0, 1, 0, 0], [1, 1, 0, 0]), (InPlace, [0, 2, 0, 0], [1, 1, 0, 0])]
        {
            let mut p = project(&["p", "a", "c", "r"]);
            p.tracks[0].folder_depth = 1;
            p.tracks[1].folder_depth = -1;
            send(&mut p, 0, 3);
            solo(&mut p, 1, mode);
            assert_eq!(heard(&p), child, "child soloed {mode:?}");
            p.tracks[1].solo = false;
            p.tracks[0].sends.clear();
            send(&mut p, 1, 3);
            solo(&mut p, 0, mode);
            let want = if mode == InPlace { [1, 2, 0, 0] } else { parent };
            assert_eq!(heard(&p), want, "parent soloed {mode:?}");
        }
    }

    #[test]
    fn solo_defeat_keeps_a_track_playing() {
        let mut p = project(&["a", "b", "c"]);
        send(&mut p, 0, 1);
        p.tracks[2].solo_defeat = true;
        assert_eq!(heard(&p), [2, 1, 1], "nothing soloed");
        solo(&mut p, 0, SoloMode::IgnoreRouting);
        assert_eq!(heard(&p), [1, 0, 1]);
        p.tracks[2].muted = true;
        assert_eq!(heard(&p), [1, 0, 0], "muting wins");
        // A monitor bus with solo defeat keeps hearing its sources whatever is soloed.
        let mut p = project(&["a", "iem", "c"]);
        send(&mut p, 0, 1);
        p.tracks[1].solo_defeat = true;
        solo(&mut p, 2, SoloMode::InPlace);
        assert_eq!(heard(&p), [1, 1, 1]);
    }
}
