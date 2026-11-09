//! The project a render job plays: the whole mix, or one track or item on its own, as
//! REAPER's render sources (`RENDER_STEMS`) take it.

use crate::model::{ItemId, Project, SoloMode, TrackId};
use crate::render_settings::RenderSource;

/// A copy of `p` whose master mix is what `source` renders.
pub fn project_for(p: &Project, source: RenderSource) -> Project {
    let mut p = p.clone();
    match source {
        RenderSource::Master => {}
        RenderSource::Stem(track) => isolate(&mut p, track),
        RenderSource::TrackViaMaster(track) => {
            for t in &mut p.tracks {
                (t.solo, t.solo_defeat) = (t.id == track, false);
                if t.id == track {
                    t.solo_mode = SoloMode::InPlace;
                }
            }
        }
        RenderSource::Item(item) => {
            if let Some((track, _)) = p.find_item(item) {
                keep_only_item(&mut p, item);
                isolate(&mut p, track);
            }
        }
        RenderSource::ItemViaMaster(item) => keep_only_item(&mut p, item),
    }
    p
}

fn keep_only_item(p: &mut Project, item: ItemId) {
    for t in &mut p.tracks {
        t.items.retain(|i| i.id == item);
    }
}

/// Makes the master play `track`'s own output and nothing else: the track and its children
/// move to the end of the track list as a top-level folder, every other top-level track stops
/// feeding the master (sends into the stem still play), and the master passes audio unchanged.
fn isolate(p: &mut Project, track: TrackId) {
    let Some(i) = p.track_index(track) else { return };
    let parents = p.folder_parents();
    let descends = |mut j: usize| loop {
        match parents[j] {
            Some(parent) if parent == i => return true,
            Some(parent) => j = parent,
            None => return false,
        }
    };
    let mut end = i + 1;
    while end < p.tracks.len() && descends(end) {
        end += 1;
    }
    let closes: i32 = p.tracks[i..end].iter().map(|t| t.folder_depth).sum();
    let mut stem: Vec<_> = p.tracks.drain(i..end).collect();
    if i > 0 {
        p.tracks[i - 1].folder_depth += closes;
    }
    if let Some(last) = stem.last_mut() {
        last.folder_depth -= closes;
    }
    let levels = p.folder_levels();
    for (t, level) in p.tracks.iter_mut().zip(levels) {
        if level == 0 {
            t.main_send = false;
        }
    }
    stem[0].main_send = true;
    stem[0].main_send_offset = 0;
    p.tracks.extend(stem);

    let m = &mut p.master;
    (m.volume, m.pan, m.width, m.muted, m.phase) = (1.0, 0.0, 1.0, false, false);
    m.fx.clear();
    m.envelopes.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Track;

    fn folder_project() -> Project {
        let mut p = Project::default();
        for (name, depth) in [("a", 0), ("folder", 1), ("child", 0), ("grandparent", 1), ("inner", -2), ("b", 0)] {
            let mut t = Track::new(name);
            t.folder_depth = depth;
            p.tracks.push(t);
        }
        p
    }

    #[test]
    fn a_stem_leaves_its_folder_and_the_rest_stops_feeding_the_master() {
        let p = folder_project();
        let grandparent = p.tracks[3].id;
        let q = project_for(&p, RenderSource::Stem(grandparent));
        let names: Vec<_> = q.tracks.iter().map(|t| (t.name.as_str(), t.folder_depth, t.main_send)).collect();
        assert_eq!(
            names,
            [
                ("a", 0, false),
                ("folder", 1, false),
                ("child", -1, true),
                ("b", 0, false),
                ("grandparent", 1, true),
                ("inner", -1, true),
            ]
        );
        assert_eq!(q.folder_levels(), [0, 0, 1, 0, 0, 1]);
    }

    #[test]
    fn an_item_renders_alone() {
        let mut p = folder_project();
        let take = crate::model::Take::from_file("x.wav".into());
        p.tracks[0].items.push(crate::model::Item::new(0.0, 1.0, take.clone()));
        p.tracks[5].items.push(crate::model::Item::new(0.0, 1.0, take));
        let item = p.tracks[5].items[0].id;
        let q = project_for(&p, RenderSource::ItemViaMaster(item));
        assert_eq!(q.tracks.iter().map(|t| t.items.len()).sum::<usize>(), 1);
        assert_eq!(q.tracks[5].items[0].id, item);
    }
}
