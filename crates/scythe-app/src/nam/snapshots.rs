//! A NAM slot's snapshots: a few model and level settings to switch between with one click.
//! They live in the track's `<EXT` block (REAPER's per-track extension state, kept by REAPER
//! when it saves), under `scythe_nam:<FXID>`, as base64 JSON so the value is one token.

use base64::Engine as _;
use scythe_core::Track;
use scythe_core::rpp::{Chunk, Node, line_key, tokenize};
use serde::{Deserialize, Serialize};

pub const SNAPSHOTS: usize = 4;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub model: Option<String>,
    pub input_db: f64,
    pub output_db: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SlotExtras {
    #[serde(default)]
    pub snapshots: Vec<Option<Snapshot>>,
}

impl SlotExtras {
    pub fn get(&self, index: usize) -> Option<&Snapshot> {
        self.snapshots.get(index).and_then(Option::as_ref)
    }

    pub fn set(&mut self, index: usize, snapshot: Option<Snapshot>) {
        if self.snapshots.len() <= index {
            self.snapshots.resize(index + 1, None);
        }
        self.snapshots[index] = snapshot;
        while self.snapshots.last().is_some_and(Option::is_none) {
            self.snapshots.pop();
        }
    }

    fn is_empty(&self) -> bool {
        self.snapshots.iter().all(Option::is_none)
    }
}

fn key(fx_id: &str) -> String {
    format!("scythe_nam:{fx_id}")
}

fn ext(track: &Track) -> Option<&Chunk> {
    track.extra.iter().find_map(|n| match n {
        Node::Chunk(c) if c.name() == "EXT" => Some(c),
        _ => None,
    })
}

pub fn read(track: &Track, fx_id: &str) -> SlotExtras {
    let key = key(fx_id);
    ext(track)
        .and_then(|c| {
            c.children.iter().find_map(|n| match n {
                Node::Line(l) if line_key(l) == key => tokenize(l).get(1).cloned(),
                _ => None,
            })
        })
        .and_then(|v| base64::engine::general_purpose::STANDARD.decode(v).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn write(track: &mut Track, fx_id: &str, extras: &SlotExtras) {
    let key = key(fx_id);
    let line = (!extras.is_empty()).then(|| {
        let json = serde_json::to_vec(extras).unwrap_or_default();
        format!("{key} {}", base64::engine::general_purpose::STANDARD.encode(json))
    });
    let at = track.extra.iter().position(|n| matches!(n, Node::Chunk(c) if c.name() == "EXT"));
    let at = match (at, &line) {
        (Some(at), _) => at,
        (None, None) => return,
        (None, Some(_)) => {
            track.extra.push(Node::Chunk(Chunk::new("EXT", &[])));
            track.extra.len() - 1
        }
    };
    let Node::Chunk(chunk) = &mut track.extra[at] else { return };
    chunk.children.retain(|n| !matches!(n, Node::Line(l) if line_key(l) == key));
    if let Some(line) = line {
        chunk.children.push(Node::Line(line));
    }
    if chunk.children.is_empty() {
        track.extra.remove(at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_survive_a_project_round_trip_beside_other_extensions() {
        let mut project = scythe_core::Project::default();
        let id = project.add_track("Guitar");
        let track = project.track_mut(id).unwrap();
        let mut ext = Chunk::new("EXT", &[]);
        ext.line(["other_ext:key", "value"]);
        track.extra.push(Node::Chunk(ext));
        let mut extras = SlotExtras::default();
        extras.set(2, Some(Snapshot { model: Some("/m/a b.nam".into()), input_db: -3.0, output_db: 1.5 }));
        write(track, "{FX-1}", &extras);
        assert_eq!(extras.snapshots.len(), 3);
        let text = scythe_core::rpp::project_to_rpp(&project);
        assert!(text.contains("scythe_nam:{FX-1} "), "{text}");
        let back = scythe_core::rpp::project_from_rpp(&text).unwrap();
        let track = back.tracks.iter().find(|t| t.name == "Guitar").unwrap();
        assert_eq!(read(track, "{FX-1}"), extras);
        assert_eq!(read(track, "{FX-2}"), SlotExtras::default());

        let mut track = track.clone();
        extras.set(2, None);
        assert!(extras.snapshots.is_empty());
        write(&mut track, "{FX-1}", &extras);
        let Some(Node::Chunk(ext)) = track.extra.iter().find(|n| matches!(n, Node::Chunk(c) if c.name() == "EXT"))
        else {
            panic!("the other extension's state is kept");
        };
        assert_eq!(ext.children, [Node::Line("other_ext:key value".into())]);
        let mut bare = Track::new("x");
        write(&mut bare, "{FX-1}", &SlotExtras::default());
        assert!(bare.extra.is_empty());
    }
}
