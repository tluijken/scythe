//! Projects that isolate a few items for gluing or for applying a track's effects to them.

use crate::model::{ItemId, Project, TimeRange, Track, TrackId};
use crate::units::PanLaw;

/// A project that plays only `items` of `track`, at unity gain and centred, through the
/// track's effects when `with_fx` (as REAPER applies track FX: before the fader and the track
/// envelopes). Returns it with the range the items cover, or `None` when none of them is on
/// the track.
pub fn isolate(project: &Project, track: TrackId, items: &[ItemId], with_fx: bool) -> Option<(Project, TimeRange)> {
    let source = project.track(track)?;
    let kept: Vec<_> = source.items.iter().filter(|it| items.contains(&it.id)).cloned().collect();
    let start = kept.iter().map(|it| it.position).fold(f64::INFINITY, f64::min);
    let end = kept.iter().map(|it| it.end()).fold(f64::NEG_INFINITY, f64::max);
    if kept.is_empty() || end <= start {
        return None;
    }
    let mut t = Track::new(source.name.clone());
    t.items = kept;
    if with_fx {
        t.fx = source.fx.clone();
        t.fx_chain_prelude = source.fx_chain_prelude.clone();
    }
    let isolated = Project {
        path: project.path.clone(),
        tempo: project.tempo.clone(),
        sample_rate: project.sample_rate,
        pan_law: PanLaw::default(),
        tracks: vec![t],
        ..Project::default()
    };
    Some((isolated, TimeRange::new(start, end)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Item, Take};

    #[test]
    fn keeps_only_the_items_at_unity() {
        let mut p = Project::default();
        let id = p.add_track("gtr");
        let t = p.track_mut(id).unwrap();
        (t.volume, t.pan, t.muted) = (0.5, -1.0, true);
        let a = Item::new(1.0, 2.0, Take::from_file("a.wav".into()));
        let b = Item::new(4.0, 1.0, Take::from_file("b.wav".into()));
        let c = Item::new(9.0, 1.0, Take::from_file("c.wav".into()));
        let ids = [a.id, b.id];
        t.items = vec![a, b, c];
        p.add_track("other");
        let (iso, range) = isolate(&p, id, &ids, false).unwrap();
        assert_eq!(range, TimeRange::new(1.0, 5.0));
        assert_eq!(iso.tracks.len(), 1);
        let t = &iso.tracks[0];
        assert_eq!((t.volume, t.pan, t.muted, t.items.len()), (1.0, 0.0, false, 2));
        assert!(isolate(&p, id, &[], false).is_none());
    }
}
