//! MIDI item sources (RPP `<SOURCE MIDI`): events stored as delta ticks, in quarter notes of the
//! project tempo map.

use crate::rpp::{Chunk, Node, line_key, tokenize};

#[derive(Debug, Clone, PartialEq)]
pub struct MidiEvent {
    /// Absolute position in ticks from the source start.
    pub tick: u64,
    pub bytes: [u8; 3],
    pub len: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MidiSource {
    pub ticks_per_qn: u32,
    pub events: Vec<MidiEvent>,
    /// Source length in ticks: the position of the last event, which REAPER writes as an
    /// end-of-source marker.
    pub length_ticks: u64,
}

/// A note with its duration, in quarter notes from the source start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Note {
    pub start_qn: f64,
    pub end_qn: f64,
    pub channel: u8,
    pub key: u8,
    pub velocity: u8,
}

impl MidiSource {
    /// Parses `E <delta> <status> <d1> <d2>` lines (hex bytes; `e` marks selected events).
    /// Returns `None` for chunks that are not MIDI or have no inline data.
    pub fn parse(c: &Chunk) -> Option<Self> {
        if c.args().first().map(String::as_str) != Some("MIDI") {
            return None;
        }
        let mut ticks_per_qn = 960;
        let mut tick = 0u64;
        let mut events = Vec::new();
        for node in &c.children {
            let Node::Line(l) = node else { continue };
            match line_key(l) {
                "HASDATA" => {
                    let t = tokenize(l);
                    ticks_per_qn = t.get(2).and_then(|v| v.parse().ok()).unwrap_or(960);
                }
                "E" | "e" => {
                    let t = tokenize(l);
                    let delta: u64 = t.get(1).and_then(|v| v.parse().ok()).unwrap_or(0);
                    tick += delta;
                    let mut bytes = [0u8; 3];
                    let mut len = 0;
                    for (i, b) in t.iter().skip(2).take(3).enumerate() {
                        if let Ok(v) = u8::from_str_radix(b, 16) {
                            bytes[i] = v;
                            len = i as u8 + 1;
                        }
                    }
                    events.push(MidiEvent { tick, bytes, len });
                }
                // Sysex and other long events only advance time.
                "X" | "x" => {
                    let t = tokenize(l);
                    tick += t.get(1).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
                }
                _ => {}
            }
        }
        Some(Self { ticks_per_qn: ticks_per_qn.max(1), length_ticks: tick, events })
    }

    pub fn length_qn(&self) -> f64 {
        self.length_ticks as f64 / self.ticks_per_qn as f64
    }

    /// Notes paired from note-on/note-off events. Notes still held at the end of the source end
    /// there.
    pub fn notes(&self) -> Vec<Note> {
        let ppq = self.ticks_per_qn as f64;
        let mut open: Vec<(u8, u8, u64, u8)> = Vec::new();
        let mut notes = Vec::new();
        for e in &self.events {
            let status = e.bytes[0] & 0xf0;
            let channel = e.bytes[0] & 0x0f;
            let (key, vel) = (e.bytes[1], e.bytes[2]);
            let is_on = status == 0x90 && vel > 0;
            let is_off = status == 0x80 || (status == 0x90 && vel == 0);
            if (is_off || is_on)
                && let Some(i) = open.iter().position(|o| o.0 == channel && o.1 == key)
            {
                let (ch, k, start, v) = open.remove(i);
                notes.push(Note {
                    start_qn: start as f64 / ppq,
                    end_qn: e.tick as f64 / ppq,
                    channel: ch,
                    key: k,
                    velocity: v,
                });
            }
            if is_on {
                open.push((channel, key, e.tick, vel));
            }
        }
        for (ch, k, start, v) in open {
            notes.push(Note {
                start_qn: start as f64 / ppq,
                end_qn: self.length_ticks.max(start) as f64 / ppq,
                channel: ch,
                key: k,
                velocity: v,
            });
        }
        notes.sort_by(|a, b| a.start_qn.total_cmp(&b.start_qn));
        notes
    }
}

impl MidiSource {
    /// The same source with its notes replaced by `notes`; other events (controllers, the
    /// end-of-source marker) stay. The source grows if a note ends past its end.
    pub fn with_notes(&self, notes: &[Note]) -> MidiSource {
        let ppq = self.ticks_per_qn as f64;
        let tick = |qn: f64| (qn.max(0.0) * ppq).round() as u64;
        let is_note = |e: &MidiEvent| matches!(e.bytes[0] & 0xf0, 0x80 | 0x90);
        let note_end = notes.iter().map(|n| tick(n.end_qn)).max().unwrap_or(0);
        let length = self.length_ticks.max(note_end);
        let mut events: Vec<MidiEvent> = self
            .events
            .iter()
            .filter(|e| !is_note(e))
            .map(|e| if e.tick == self.length_ticks { MidiEvent { tick: length, ..e.clone() } } else { e.clone() })
            .collect();
        for n in notes {
            let (start, end) = (tick(n.start_qn), tick(n.end_qn).max(tick(n.start_qn) + 1));
            events.push(MidiEvent { tick: start, bytes: [0x90 | n.channel, n.key, n.velocity.max(1)], len: 3 });
            events.push(MidiEvent { tick: end, bytes: [0x80 | n.channel, n.key, 0], len: 3 });
        }
        // Note-offs before note-ons at the same tick, so a repeated key is not cut short.
        events.sort_by_key(|e| (e.tick, e.bytes[0] & 0xf0 == 0x90, !is_note(e)));
        MidiSource { ticks_per_qn: self.ticks_per_qn, events, length_ticks: length }
    }

    /// Writes the events into a copy of the original chunk, keeping its other lines. Pooling
    /// is dropped: an edited source no longer shares events with its pool.
    pub fn write_chunk(&self, original: &Chunk) -> Chunk {
        let is_event = |n: &Node| matches!(n, Node::Line(l) if matches!(line_key(l), "E" | "e" | "X" | "x"));
        let first = original.children.iter().position(is_event).unwrap_or(original.children.len());
        let last = original.children.iter().rposition(is_event).map_or(first, |i| i + 1);
        let mut out = Chunk { header: original.header.clone(), children: Vec::new() };
        out.children.extend(
            original.children[..first]
                .iter()
                .filter(|n| !matches!(n, Node::Line(l) if line_key(l) == "POOLEDEVTS"))
                .cloned(),
        );
        let mut prev = 0;
        for e in &self.events {
            let bytes: Vec<String> = e.bytes[..e.len as usize].iter().map(|b| format!("{b:02x}")).collect();
            out.children.push(Node::Line(format!("E {} {}", e.tick - prev, bytes.join(" "))));
            prev = e.tick;
        }
        out.children.extend(original.children[last..].iter().cloned());
        out
    }
}

/// A note placed on the project timeline, in seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimedNote {
    pub start: f64,
    pub end: f64,
    pub channel: u8,
    pub key: u8,
    pub velocity: u8,
}

impl MidiSource {
    /// Places the notes of a take on the timeline: the source plays in quarter notes of the
    /// tempo map from the item start, shifted by the take offset, scaled by the play rate and
    /// repeated when the item loops. Notes starting outside the item are dropped and notes are
    /// cut at the item end.
    pub fn place(&self, item: &crate::Item, take: &crate::Take, timeline: &crate::tempo::Timeline) -> Vec<TimedNote> {
        let notes = self.notes();
        let item_qn = timeline.qn_at(item.position);
        let rate = if take.playrate > 0.0 { take.playrate } else { 1.0 };
        let offset_qn = take.source_offset * timeline.bpm_at(item.position) / 60.0;
        let end_qn = timeline.qn_at(item.end());
        let loop_len = self.length_qn();
        let mut out = Vec::new();
        let mut k = 0.0;
        loop {
            let base = item_qn + (k * loop_len - offset_qn) / rate;
            if base >= end_qn {
                break;
            }
            for n in &notes {
                let start = base + n.start_qn / rate;
                if start < item_qn - 1e-9 || start >= end_qn {
                    continue;
                }
                let end = (base + n.end_qn / rate).min(end_qn);
                out.push(TimedNote {
                    start: timeline.time_at_qn(start),
                    end: timeline.time_at_qn(end),
                    channel: n.channel,
                    key: n.key,
                    velocity: n.velocity,
                });
            }
            if !item.looped || loop_len <= 0.0 || out.len() > 1_000_000 {
                break;
            }
            k += 1.0;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpp::parse;

    #[test]
    fn rewrites_notes_and_keeps_the_rest() {
        let c = parse(
            "<SOURCE MIDI\n  HASDATA 1 960 QN\n  POOLEDEVTS {X}\n  E 0 b0 07 64\n  E 960 90 3c 50\n  E 960 80 3c 00\n  E 1920 b0 7b 00\n  GUID {Y}\n>\n",
        )
        .unwrap();
        let m = MidiSource::parse(&c).unwrap();
        let mut notes = m.notes();
        notes[0].key = 0x40;
        notes.push(Note { start_qn: 3.5, end_qn: 5.0, channel: 9, key: 36, velocity: 100 });
        let edited = m.with_notes(&notes);
        let chunk = edited.write_chunk(&c);
        let text = crate::rpp::write(&chunk);
        assert!(!text.contains("POOLEDEVTS") && text.contains("GUID {Y}") && text.contains("E 0 b0 07 64"));
        let again = MidiSource::parse(&chunk).unwrap();
        assert_eq!(again.length_qn(), 5.0);
        assert_eq!(again.notes(), notes);
    }

    #[test]
    fn pairs_notes() {
        let c = parse(
            "<SOURCE MIDI\n  HASDATA 1 960 QN\n  CCINTERP 32\n  E 960 99 24 64\n  e 480 89 24 00\n  E 0 90 3c 50\n  E 960 90 3c 00\n  E 0 b0 7b 00\n>\n",
        )
        .unwrap();
        let m = MidiSource::parse(&c).unwrap();
        assert_eq!(m.length_qn(), 2.5);
        let notes = m.notes();
        assert_eq!(notes.len(), 2);
        assert_eq!((notes[0].start_qn, notes[0].end_qn, notes[0].channel, notes[0].key), (1.0, 1.5, 9, 0x24));
        assert_eq!((notes[1].start_qn, notes[1].end_qn, notes[1].velocity), (1.5, 2.5, 0x50));
    }
}
