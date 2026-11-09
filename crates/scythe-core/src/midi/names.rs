//! Note and controller names: REAPER's note name files (`60 Kick`, `CC7 Loudness`, `#`
//! comments) and the `<MIDINOTENAMES` chunk a track keeps them in (`channel key name 0 key`,
//! channel -1 for all channels).

use crate::rpp::{Chunk, Node, quote, tokenize};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NoteNames {
    /// (channel or -1 for all, key, name).
    pub notes: Vec<(i8, u8, String)>,
    /// (controller, name) from a note name file.
    pub controllers: Vec<(u8, String)>,
}

impl NoteNames {
    /// The name of `key` on `channel` (0-15): a channel's own name before an all-channel one.
    pub fn note(&self, channel: u8, key: u8) -> Option<&str> {
        let find = |ch: i8| self.notes.iter().find(|n| n.0 == ch && n.1 == key).map(|n| n.2.as_str());
        find(channel as i8).or_else(|| find(-1))
    }

    pub fn controller(&self, cc: u8) -> Option<&str> {
        self.controllers.iter().find(|c| c.0 == cc).map(|c| c.1.as_str())
    }

    pub fn set_note(&mut self, channel: i8, key: u8, name: &str) {
        self.notes.retain(|n| !(n.0 == channel && n.1 == key));
        if !name.is_empty() {
            self.notes.push((channel, key, name.to_owned()));
            self.notes.sort_by_key(|n| (n.1, n.0));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.notes.is_empty() && self.controllers.is_empty()
    }

    /// Reads a note name file. Its note names apply to all channels.
    pub fn parse_file(text: &str) -> Self {
        let mut out = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
                continue;
            }
            let (head, name) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
            let name = name.trim().to_owned();
            if let Some(cc) = head.strip_prefix("CC").or_else(|| head.strip_prefix("cc")) {
                if let Ok(cc) = cc.parse::<u8>()
                    && cc < 128
                {
                    out.controllers.push((cc, name));
                }
            } else if let Ok(key) = head.parse::<u8>()
                && key < 128
            {
                out.set_note(-1, key, &name);
            }
        }
        out
    }

    /// Writes a note name file REAPER can load.
    pub fn to_file(&self) -> String {
        let mut out = String::from("# MIDI note name map\n");
        for (_, key, name) in self.notes.iter().filter(|n| n.0 == -1) {
            out.push_str(&format!("{key} {name}\n"));
        }
        for (cc, name) in &self.controllers {
            out.push_str(&format!("CC{cc} {name}\n"));
        }
        out
    }

    /// From a track's `<MIDINOTENAMES` chunk.
    pub fn from_chunk(c: &Chunk) -> Self {
        let mut out = Self::default();
        for node in &c.children {
            let Node::Line(l) = node else { continue };
            let t = tokenize(l);
            let (Some(ch), Some(key), Some(name)) =
                (t.first().and_then(|v| v.parse::<i8>().ok()), t.get(1).and_then(|v| v.parse::<u8>().ok()), t.get(2))
            else {
                continue;
            };
            if key < 128 {
                out.notes.push((ch.clamp(-1, 15), key, name.clone()));
            }
        }
        out
    }

    pub fn to_chunk(&self) -> Chunk {
        let mut c = Chunk::new("MIDINOTENAMES", &[]);
        for (ch, key, name) in &self.notes {
            c.children.push(Node::Line(format!("{ch} {key} {} 0 {key}", quote(name))));
        }
        c
    }
}

impl crate::Track {
    /// The track's note names, kept as REAPER keeps them.
    pub fn note_names(&self) -> NoteNames {
        self.extra
            .iter()
            .find_map(|n| match n {
                Node::Chunk(c) if c.name() == "MIDINOTENAMES" => Some(NoteNames::from_chunk(c)),
                _ => None,
            })
            .unwrap_or_default()
    }

    pub fn set_note_names(&mut self, names: &NoteNames) {
        let at = self.extra.iter().position(|n| matches!(n, Node::Chunk(c) if c.name() == "MIDINOTENAMES"));
        let chunk = (!names.notes.is_empty()).then(|| Node::Chunk(names.to_chunk()));
        match (at, chunk) {
            (Some(i), Some(c)) => self.extra[i] = c,
            (Some(i), None) => {
                self.extra.remove(i);
            }
            (None, Some(c)) => self.extra.push(c),
            (None, None) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpp::{parse, write};

    #[test]
    fn reads_a_note_name_file() {
        let n = NoteNames::parse_file("# MIDI note name map.\n#\n60 Middle C\n36 Kick\nCC7 Loudness\n");
        assert_eq!(n.note(9, 36), Some("Kick"));
        assert_eq!(n.note(0, 60), Some("Middle C"));
        assert_eq!(n.controller(7), Some("Loudness"));
        assert_eq!(NoteNames::parse_file(&n.to_file()), n);
    }

    #[test]
    fn round_trips_reaper_track_names() {
        // As REAPER 7.73 saves `SetTrackMIDINoteNameEx` names.
        let text = "<MIDINOTENAMES\n  0 60 Kick 0 60\n  -1 62 Snare 0 62\n>\n";
        let n = NoteNames::from_chunk(&parse(text).unwrap());
        assert_eq!(n.note(0, 60), Some("Kick"));
        assert_eq!(n.note(5, 60), None);
        assert_eq!(n.note(5, 62), Some("Snare"));
        assert_eq!(write(&n.to_chunk()), text);
    }
}
