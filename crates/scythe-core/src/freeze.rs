//! Track freeze as REAPER keeps it: the track's effects and items move into a `<FREEZE` chunk
//! on the track, and a locked item playing the rendered file (marked with a `FREEZE` line)
//! takes their place. Freezing again stacks another level; unfreezing restores the last one.

use crate::fx_chain::{join_chain, split_chain};
use crate::model::{Item, Take, TimeRange, Track};
use crate::rpp::{Chunk, Node, line_key};

/// How many freeze levels the track holds.
pub fn levels(track: &Track) -> usize {
    track.extra.iter().filter(|n| matches!(n, Node::Chunk(c) if c.name() == "FREEZE")).count()
}

/// Whether `item` plays a freeze render.
pub fn is_freeze_item(item: &Item) -> bool {
    item.extra
        .iter()
        .chain(item.takes.iter().flat_map(|t| &t.extra))
        .any(|n| matches!(n, Node::Line(l) if line_key(l) == "FREEZE"))
}

/// Stores the track's effects and items as a freeze level and puts an item playing `file`
/// over `range` in their place.
pub fn freeze(track: &mut Track, file: &str, range: TimeRange) {
    let mut level = Chunk::new("FREEZE", &["0"]);
    let mut fx = Chunk::new("FXCHAIN", &[]);
    fx.children = track.fx_chain_prelude.clone();
    fx.children.extend(join_chain(&track.fx));
    level.push_chunk(fx);
    for item in &track.items {
        level.push_chunk(crate::rpp::project::write_item(item));
    }
    track.extra.push(Node::Chunk(level));
    track.fx.clear();
    let mut item = Item::new(range.start, range.end - range.start, Take::from_file(file.into()));
    item.looped = false;
    item.extra.push(Node::Line(format!("FREEZE 0 {} 0", crate::rpp::project::fmt_num(range.end - range.start))));
    item.extra.push(Node::Line("LOCK 1".into()));
    if let Some(t) = item.takes.first_mut() {
        t.name = format!("Freeze {}", levels(track));
    }
    track.items = vec![item];
}

/// Restores the last freeze level: its effects and items replace the freeze render. Returns
/// false when the track is not frozen.
pub fn unfreeze(track: &mut Track) -> bool {
    let Some(at) = track.extra.iter().rposition(|n| matches!(n, Node::Chunk(c) if c.name() == "FREEZE")) else {
        return false;
    };
    let Node::Chunk(level) = track.extra.remove(at) else { return false };
    let mut items = Vec::new();
    for node in &level.children {
        match node {
            Node::Chunk(c) if c.name() == "FXCHAIN" => {
                let (fx, prelude) = split_chain(&c.children);
                track.fx = fx;
                track.fx_chain_prelude = prelude;
            }
            Node::Chunk(c) if c.name() == "ITEM" => items.push(crate::rpp::project::parse_item(c)),
            _ => {}
        }
    }
    track.items.retain(|i| !is_freeze_item(i));
    track.items.extend(items);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freezing_stacks_and_unfreezing_restores() {
        let mut t = Track::new("Keys");
        t.items.push(Item::new(1.0, 2.0, Take::from_file("keys.wav".into())));
        t.fx.push(crate::model::FxSlot::container());
        freeze(&mut t, "freeze1.wav", TimeRange { start: 1.0, end: 4.0 });
        assert!(t.fx.is_empty());
        assert_eq!((t.items.len(), levels(&t)), (1, 1));
        assert!(is_freeze_item(&t.items[0]));
        freeze(&mut t, "freeze2.wav", TimeRange { start: 1.0, end: 5.0 });
        assert_eq!(levels(&t), 2);
        assert!(unfreeze(&mut t));
        assert_eq!(t.items[0].take().unwrap().source.file(), Some(&std::path::PathBuf::from("freeze1.wav")));
        assert!(unfreeze(&mut t));
        assert_eq!((t.fx.len(), t.items.len(), levels(&t)), (1, 1, 0));
        assert_eq!(t.items[0].position, 1.0);
        assert!(!unfreeze(&mut t));
    }
}
