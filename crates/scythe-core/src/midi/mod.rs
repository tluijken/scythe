//! MIDI item sources (RPP `<SOURCE MIDI`): events stored as delta ticks, in quarter notes of the
//! project tempo map.

use crate::rpp::{Chunk, Node, line_key, tokenize};

pub mod actions;
pub mod edit;
pub mod groove;
pub mod lanes;
pub mod names;
pub mod scale;
mod shape;
pub mod view;

pub use shape::{CcShape, ShapeKind};

#[derive(Debug, Clone, PartialEq)]
pub struct MidiEvent {
    /// Absolute position in ticks from the source start.
    pub tick: u64,
    pub bytes: [u8; 3],
    pub len: u8,
    /// [`SELECTED`] and [`MUTED`], as the line's keyword marks them (`e`, `Em`).
    pub flags: u8,
    /// How a controller moves on to the next event of its lane (the `ENV` line after it).
    pub shape: Option<CcShape>,
}

pub const SELECTED: u8 = 1;

/// The `CCINTERP` REAPER plays shaped controllers at when the take sets none (0 or below).
pub const DEFAULT_CC_INTERP: u32 = 32;
pub const MUTED: u8 = 2;

impl MidiEvent {
    pub fn muted(&self) -> bool {
        self.flags & MUTED != 0
    }

    /// The RPP keyword: `E`, `e` when selected, with `m` added when muted.
    pub(crate) fn keyword(&self) -> &'static str {
        match (self.flags & SELECTED != 0, self.muted()) {
            (false, false) => "E",
            (true, false) => "e",
            (false, true) => "Em",
            (true, true) => "em",
        }
    }
}

/// A system exclusive or meta event (`<X delta flags …` with base64 data), kept whole.
#[derive(Debug, Clone, PartialEq)]
pub struct LongEvent {
    pub tick: u64,
    /// The chunk's arguments after the delta, written back as they were.
    pub args: Vec<String>,
    /// The message as stored: `F0 … F7` for system exclusive, `FF type …` for meta text.
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MidiSource {
    pub ticks_per_qn: u32,
    pub events: Vec<MidiEvent>,
    pub long: Vec<LongEvent>,
    /// Source length in ticks: the position of the last event, which REAPER writes as an
    /// end-of-source marker.
    pub length_ticks: u64,
    /// Steps per quarter note at which shaped controllers are played (`CCINTERP`).
    pub cc_interp: u32,
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
        let mut cc_interp = None;
        let mut tick = 0u64;
        let mut events: Vec<MidiEvent> = Vec::new();
        let mut long = Vec::new();
        for node in &c.children {
            if let Node::Chunk(x) = node
                && matches!(x.name(), "X" | "x")
            {
                let args = x.args();
                tick += args.first().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
                let text: String = x
                    .children
                    .iter()
                    .filter_map(|n| match n {
                        Node::Line(l) => Some(l.trim()),
                        Node::Chunk(_) => None,
                    })
                    .collect();
                use base64::Engine as _;
                let data = base64::engine::general_purpose::STANDARD.decode(text).unwrap_or_default();
                long.push(LongEvent { tick, args: args.iter().skip(1).cloned().collect(), data });
                continue;
            }
            let Node::Line(l) = node else { continue };
            match line_key(l) {
                "HASDATA" => {
                    let t = tokenize(l);
                    ticks_per_qn = t.get(2).and_then(|v| v.parse().ok()).unwrap_or(960);
                }
                "CCINTERP" if cc_interp.is_none() => {
                    cc_interp = tokenize(l).get(1).and_then(|v| v.parse::<i64>().ok());
                }
                "ENV" => {
                    if let Some(e) = events.last_mut() {
                        e.shape = CcShape::parse(&tokenize(l));
                    }
                }
                key @ ("E" | "e" | "Em" | "em") => {
                    let flags =
                        if key.starts_with('e') { SELECTED } else { 0 } | if key.ends_with('m') { MUTED } else { 0 };
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
                    events.push(MidiEvent { tick, bytes, len, flags, shape: None });
                }
                // Sysex and other long events only advance time.
                "X" | "x" => {
                    let t = tokenize(l);
                    tick += t.get(1).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
                }
                _ => {}
            }
        }
        let cc_interp = cc_interp.filter(|&n| n > 0).map_or(DEFAULT_CC_INTERP, |n| n.min(10_000) as u32);
        Some(Self { ticks_per_qn: ticks_per_qn.max(1), length_ticks: tick, events, long, cc_interp })
    }

    /// A copy with the same settings and no events.
    pub fn without_events(&self) -> MidiSource {
        MidiSource {
            ticks_per_qn: self.ticks_per_qn,
            events: Vec::new(),
            long: self.long.clone(),
            length_ticks: self.length_ticks,
            cc_interp: self.cc_interp,
        }
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
        // Muted notes do not play; the editor leaves them as they are.
        for e in self.events.iter().filter(|e| !e.muted()) {
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
            .filter(|e| !is_note(e) || e.muted())
            .map(|e| if e.tick == self.length_ticks { MidiEvent { tick: length, ..e.clone() } } else { e.clone() })
            .collect();
        for n in notes {
            let (start, end) = (tick(n.start_qn), tick(n.end_qn).max(tick(n.start_qn) + 1));
            events.push(MidiEvent {
                tick: start,
                bytes: [0x90 | n.channel, n.key, n.velocity.max(1)],
                len: 3,
                flags: 0,
                shape: None,
            });
            events.push(MidiEvent { tick: end, bytes: [0x80 | n.channel, n.key, 0], len: 3, flags: 0, shape: None });
        }
        // Note-offs before note-ons at the same tick, so a repeated key is not cut short.
        events.sort_by_key(|e| (e.tick, e.bytes[0] & 0xf0 == 0x90, !is_note(e)));
        MidiSource { events, length_ticks: length, ..self.without_events() }
    }

    /// Writes the events into a copy of the original chunk, keeping its other lines. Pooling
    /// is dropped: an edited source no longer shares events with its pool.
    pub fn write_chunk(&self, original: &Chunk) -> Chunk {
        let is_event = |n: &Node| match n {
            Node::Line(l) => matches!(line_key(l), "E" | "e" | "Em" | "em" | "X" | "x"),
            Node::Chunk(c) => matches!(c.name(), "X" | "x"),
        };
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
        let mut long = self.long.iter().peekable();
        // Long events go after the channel messages at their tick, before later ones.
        let mut write_long_until = |out: &mut Chunk, prev: &mut u64, before: Option<u64>| {
            while let Some(x) = long.next_if(|x| before.is_none_or(|t| x.tick < t)) {
                use base64::Engine as _;
                let delta = (x.tick - *prev).to_string();
                let args: Vec<&str> =
                    std::iter::once(delta.as_str()).chain(x.args.iter().map(String::as_str)).collect();
                let mut chunk = Chunk::new("X", &args);
                chunk.children.push(Node::Line(base64::engine::general_purpose::STANDARD.encode(&x.data)));
                out.children.push(Node::Chunk(chunk));
                *prev = x.tick;
            }
        };
        for e in &self.events {
            write_long_until(&mut out, &mut prev, Some(e.tick));
            let bytes: Vec<String> = e.bytes[..e.len as usize].iter().map(|b| format!("{b:02x}")).collect();
            out.children.push(Node::Line(format!("{} {} {}", e.keyword(), e.tick - prev, bytes.join(" "))));
            if let Some(shape) = e.shape {
                out.children.push(Node::Line(shape.line()));
            }
            prev = e.tick;
        }
        write_long_until(&mut out, &mut prev, None);
        out.children.extend(original.children[last..].iter().cloned());
        out
    }
}

impl MidiSource {
    /// REAPER's end-of-source marker: all notes off at the very end.
    fn is_end_marker(&self, e: &MidiEvent) -> bool {
        e.tick == self.length_ticks && e.bytes[0] & 0xf0 == 0xb0 && e.bytes[1] == 0x7b
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

/// A channel message other than a note (controller, pitch bend, program change, aftertouch)
/// placed on the project timeline, in seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimedEvent {
    pub time: f64,
    pub bytes: [u8; 3],
    pub len: u8,
}

/// Where the passes of a placed take start, in quarter notes of the project.
struct Placement {
    item_qn: f64,
    end_qn: f64,
    rate: f64,
    bases: Vec<f64>,
}

impl MidiSource {
    fn placement(&self, item: &crate::Item, take: &crate::Take, timeline: &crate::tempo::Timeline) -> Placement {
        let item_qn = timeline.qn_at(item.position);
        let rate = if take.playrate > 0.0 { take.playrate } else { 1.0 };
        let offset_qn = take.source_offset * timeline.bpm_at(item.position) / 60.0;
        let end_qn = timeline.qn_at(item.end());
        let loop_len = self.length_qn();
        let mut bases = Vec::new();
        loop {
            let base = item_qn + (bases.len() as f64 * loop_len - offset_qn) / rate;
            if base >= end_qn {
                break;
            }
            bases.push(base);
            if !item.looped || loop_len <= 0.0 || bases.len() > 100_000 {
                break;
            }
        }
        Placement { item_qn, end_qn, rate, bases }
    }

    /// Places the notes of a take on the timeline: the source plays in quarter notes of the
    /// tempo map from the item start, shifted by the take offset, scaled by the play rate and
    /// repeated when the item loops. Notes starting outside the item are dropped and notes are
    /// cut at the item end.
    pub fn place(&self, item: &crate::Item, take: &crate::Take, timeline: &crate::tempo::Timeline) -> Vec<TimedNote> {
        let notes = self.notes();
        let p = self.placement(item, take, timeline);
        let mut out = Vec::new();
        for base in &p.bases {
            for n in &notes {
                let start = base + n.start_qn / p.rate;
                if start < p.item_qn - 1e-9 || start >= p.end_qn {
                    continue;
                }
                let end = (base + n.end_qn / p.rate).min(p.end_qn);
                out.push(TimedNote {
                    start: timeline.time_at_qn(start),
                    end: timeline.time_at_qn(end),
                    channel: n.channel,
                    key: n.key,
                    velocity: n.velocity,
                });
            }
            if out.len() > 1_000_000 {
                break;
            }
        }
        out
    }

    /// The take's system exclusive and meta events, placed like [`MidiSource::place`].
    pub fn place_long(
        &self,
        item: &crate::Item,
        take: &crate::Take,
        timeline: &crate::tempo::Timeline,
    ) -> Vec<(f64, Vec<u8>)> {
        let ppq = self.ticks_per_qn as f64;
        let p = self.placement(item, take, timeline);
        let mut out = Vec::new();
        for base in &p.bases {
            for x in &self.long {
                let at = base + x.tick as f64 / ppq / p.rate;
                if at < p.item_qn - 1e-9 || at >= p.end_qn {
                    continue;
                }
                out.push((timeline.time_at_qn(at), x.data.clone()));
            }
            if out.len() > 100_000 {
                break;
            }
        }
        out
    }

    /// The take's channel messages other than notes, placed like [`MidiSource::place`], with
    /// shaped controllers played as the steps REAPER sends between their events. REAPER's
    /// end-of-source marker (all notes off at the very end) is left out.
    pub fn place_events(
        &self,
        item: &crate::Item,
        take: &crate::Take,
        timeline: &crate::tempo::Timeline,
    ) -> Vec<TimedEvent> {
        let ppq = self.ticks_per_qn as f64;
        let controls = self.shaped_controls();
        let p = self.placement(item, take, timeline);
        let mut out = Vec::new();
        for base in &p.bases {
            for e in &controls {
                let at = base + e.tick / ppq / p.rate;
                if at < p.item_qn - 1e-9 || at >= p.end_qn {
                    continue;
                }
                out.push(TimedEvent { time: timeline.time_at_qn(at), bytes: e.bytes, len: e.len });
            }
            if out.len() > 1_000_000 {
                break;
            }
        }
        out
    }
}

impl crate::Project {
    /// The source chunk of the item's active take, when it is MIDI.
    pub fn midi_chunk(&self, item: crate::ItemId) -> Option<&Chunk> {
        let (_, it) = self.find_item(item)?;
        match &it.take()?.source {
            crate::Source::Raw(c) if c.args().first().map(String::as_str) == Some("MIDI") => Some(c),
            _ => None,
        }
    }

    pub fn midi_source(&self, item: crate::ItemId) -> Option<MidiSource> {
        MidiSource::parse(self.midi_chunk(item)?)
    }

    /// Changes the source chunk of the item's active take; false when it is not MIDI.
    pub fn edit_midi_chunk(&mut self, item: crate::ItemId, f: impl FnOnce(&mut Chunk)) -> bool {
        let Some(it) = self.tracks.iter_mut().find_map(|t| t.item_mut(item)) else { return false };
        let active = it.active_take;
        let Some(take) = it.takes.get_mut(active) else { return false };
        match &mut take.source {
            crate::Source::Raw(c) if c.args().first().map(String::as_str) == Some("MIDI") => {
                f(c);
                true
            }
            _ => false,
        }
    }

    /// Changes the events of the item's active take; false when it is not MIDI.
    pub fn edit_midi(&mut self, item: crate::ItemId, f: impl FnOnce(&MidiSource) -> MidiSource) -> bool {
        self.edit_midi_chunk(item, |c| {
            if let Some(src) = MidiSource::parse(c) {
                *c = f(&src).write_chunk(c);
            }
        })
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
    fn sysex_keeps_its_time_and_survives_edits() {
        // As REAPER 7.73 saves a take with a system exclusive and a text event.
        let c = parse(
            "<SOURCE MIDI\n  HASDATA 1 960 QN\n  E 0 90 3c 64\n  E 240 80 3c 00\n  <X 240 0\n    8PB+fwYB9/c=\n  >\n  <X 240 0 0 0 1 hello\n    /wFoZWxsbw==\n  >\n  E 240 90 3e 64\n  E 10 80 3e 00\n  E 0 b0 7b 00\n>\n",
        )
        .unwrap();
        let m = MidiSource::parse(&c).unwrap();
        assert_eq!(m.long.iter().map(|x| x.tick).collect::<Vec<_>>(), [480, 720]);
        assert_eq!(m.long[0].data, [0xf0, 0xf0, 0x7e, 0x7f, 0x06, 0x01, 0xf7, 0xf7]);
        assert_eq!(m.long[1].data[..2], [0xff, 0x01]);
        assert_eq!(m.notes()[1].start_qn, 1.0, "events after a long one keep their time");
        let again = MidiSource::parse(&m.with_notes(&m.notes()).write_chunk(&c)).unwrap();
        assert_eq!((again.notes(), again.long), (m.notes(), m.long.clone()));
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

    #[test]
    fn places_controllers_with_the_item() {
        let c = parse("<SOURCE MIDI\n  HASDATA 1 960 QN\n  E 0 b0 07 64\n  E 480 e0 00 50\n  E 480 90 3c 50\n  E 960 80 3c 00\n  E 0 b0 7b 00\n>\n")
            .unwrap();
        let m = MidiSource::parse(&c).unwrap();
        let mut take = crate::Take::from_file("x.mid".into());
        take.source = crate::Source::Raw(c);
        let mut item = crate::Item::new(1.0, 2.0, take.clone());
        item.looped = true;
        // 120 bpm: a quarter note lasts half a second, the source is 2 qn (1 s) long.
        let tempo = crate::TempoMap::default();
        let events = m.place_events(&item, &take, &tempo.timeline());
        let times: Vec<(f64, u8)> = events.iter().map(|e| (e.time, e.bytes[0])).collect();
        assert_eq!(times, [(1.0, 0xb0), (1.25, 0xe0), (2.0, 0xb0), (2.25, 0xe0)]);
    }
}
