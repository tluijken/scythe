use std::path::PathBuf;

use crate::midi::{MidiEvent, MidiSource};
use crate::model::{Item, MIN_ITEM_LENGTH, Source, Take, TimeRange};
use crate::tempo::Timeline;

/// Where the transport went while one file was recorded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecordPass {
    /// Project time recording started at.
    pub start: f64,
    /// Loop range the transport wrapped around, when repeat was on.
    pub looped: Option<TimeRange>,
    /// Only keep what falls inside this range (time-selection auto-punch).
    pub punch: Option<TimeRange>,
}

/// Turns a recorded file into an item. `file_len` is the file's length and `offset` how far
/// into it project time `start` was heard (input plus output latency), both in seconds.
///
/// When the transport wrapped around a loop, every pass becomes a take of one item spanning
/// the loop, the last complete pass active; as with REAPER's default, incomplete first and
/// last passes are dropped once at least one full pass was recorded.
pub fn place_recording(path: PathBuf, file_len: f64, offset: f64, pass: RecordPass) -> Option<Item> {
    let elapsed = file_len - offset;
    if elapsed <= MIN_ITEM_LENGTH {
        return None;
    }
    let take = |source_offset: f64| {
        let mut t = Take::from_file(path.clone());
        t.source_offset = source_offset;
        t
    };
    let wrap = pass.looped.filter(|r| !r.is_empty() && pass.start < r.end);
    let mut item = match wrap {
        Some(r) if elapsed > r.end - pass.start => {
            let len = r.length();
            let first = r.end - pass.start;
            let first_complete = pass.start <= r.start;
            let later = elapsed - first;
            let complete = (later / len + 1e-9).floor() as usize;
            let any_complete = first_complete || complete > 0;
            let pos = r.start.min(pass.start);
            // File time heard at the item's start, for a pass that began `elapsed` seconds into
            // the recording at project time `at`.
            let source = |elapsed: f64, at: f64| offset + elapsed - (at - pos);
            let mut takes = Vec::new();
            if first_complete || !any_complete {
                takes.push(take(source(0.0, pass.start)));
            }
            for k in 0..complete {
                takes.push(take(source(first + k as f64 * len, r.start)));
            }
            if !any_complete {
                takes.push(take(source(first, r.start)));
            }
            let active = takes.len() - 1;
            let mut item = Item::new(pos, r.end - pos, takes.remove(0));
            item.takes.extend(takes);
            item.active_take = active;
            item
        }
        _ => Item::new(pass.start, elapsed, take(offset)),
    };
    if let Some(p) = pass.punch.filter(|p| !p.is_empty()) {
        let (from, to) = (item.position.max(p.start), item.end().min(p.end));
        if to - from <= MIN_ITEM_LENGTH {
            return None;
        }
        item.trim_start(from);
        item.length = to - from;
    }
    Some(item)
}

/// Ticks per quarter note of recorded MIDI, as REAPER records it.
const RECORD_PPQ: u32 = 960;

/// Turns MIDI played during a recording into an in-project MIDI item. `events` are project
/// times with their message; `stop` is where the transport was when recording stopped.
///
/// Messages stay where they were played, so passes around a loop layer into one take (as
/// REAPER's MIDI overdub); the item then spans the loop. Notes still held end with the item and
/// note-offs without their note-on are dropped.
pub fn place_midi_recording(
    events: &[(f64, [u8; 3], u8)],
    pass: RecordPass,
    stop: f64,
    timeline: &Timeline,
) -> Option<Item> {
    let wrapped = events.iter().any(|e| e.0 < pass.start) || stop < pass.start;
    let (mut from, mut to) = match pass.looped.filter(|_| wrapped) {
        Some(r) => (pass.start.min(r.start), r.end),
        None => (pass.start, stop.max(events.iter().map(|e| e.0).fold(pass.start, f64::max))),
    };
    if let Some(p) = pass.punch.filter(|p| !p.is_empty()) {
        (from, to) = (from.max(p.start), to.min(p.end));
    }
    if to - from <= MIN_ITEM_LENGTH {
        return None;
    }
    let start_qn = timeline.qn_at(from);
    let length = ((timeline.qn_at(to) - start_qn) * RECORD_PPQ as f64).round() as u64;
    let tick = |t: f64| (((timeline.qn_at(t) - start_qn) * RECORD_PPQ as f64).round() as u64).min(length);
    let mut sorted: Vec<&(f64, [u8; 3], u8)> = events.iter().filter(|e| (from..to).contains(&e.0)).collect();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut held: Vec<(u8, u8)> = Vec::new();
    let mut out = Vec::new();
    for &&(t, bytes, len) in &sorted {
        let (status, key) = (bytes[0] & 0xf0, (bytes[0] & 0x0f, bytes[1]));
        let on = status == 0x90 && bytes[2] > 0;
        let off = status == 0x80 || (status == 0x90 && bytes[2] == 0);
        if off {
            let Some(i) = held.iter().position(|h| *h == key) else { continue };
            held.remove(i);
        } else if on {
            held.push(key);
        }
        out.push(MidiEvent { tick: tick(t), bytes, len });
    }
    if out.is_empty() {
        return None;
    }
    for (ch, k) in held {
        out.push(MidiEvent { tick: length, bytes: [0x80 | ch, k, 0], len: 3 });
    }
    let source = MidiSource { ticks_per_qn: RECORD_PPQ, events: out, long: Vec::new(), length_ticks: length };
    let mut take = Take::from_file(PathBuf::new());
    take.source = Source::Raw(source.to_chunk());
    Some(Item::new(from, to - from, take))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(len: f64, start: f64, looped: Option<(f64, f64)>, punch: Option<(f64, f64)>) -> Option<Item> {
        let r = |(a, b): (f64, f64)| TimeRange::new(a, b);
        place_recording("a.wav".into(), len, 0.25, RecordPass { start, looped: looped.map(r), punch: punch.map(r) })
    }

    fn offsets(item: &Item) -> Vec<f64> {
        item.takes.iter().map(|t| (t.source_offset * 1e6).round() / 1e6).collect()
    }

    #[test]
    fn linear_recording() {
        let it = place(10.25, 2.0, None, None).unwrap();
        assert_eq!((it.position, it.length), (2.0, 10.0));
        assert_eq!(offsets(&it), [0.25]);
        assert!(place(0.2, 2.0, None, None).is_none());
        // A loop that is never reached leaves a plain item.
        let it = place(3.25, 2.0, Some((8.0, 12.0)), None).unwrap();
        assert_eq!((it.position, it.length, it.takes.len()), (2.0, 3.0, 1));
    }

    #[test]
    fn loop_passes_become_takes() {
        // From the loop start, three full passes of 4 s and 1 s of a fourth.
        let it = place(13.25, 8.0, Some((8.0, 12.0)), None).unwrap();
        assert_eq!((it.position, it.length), (8.0, 4.0));
        assert_eq!(offsets(&it), [0.25, 4.25, 8.25]);
        assert_eq!(it.active_take, 2);
    }

    #[test]
    fn pre_roll_and_incomplete_passes() {
        // Starting 2 s before the loop: the first pass includes the pre-roll.
        let it = place(10.25, 6.0, Some((8.0, 12.0)), None).unwrap();
        assert_eq!((it.position, it.length), (6.0, 6.0));
        assert_eq!(offsets(&it), [0.25, 4.25]);
        // Starting inside the loop: the incomplete first pass is dropped.
        let it = place(11.25, 10.0, Some((8.0, 12.0)), None).unwrap();
        assert_eq!((it.position, it.length), (8.0, 4.0));
        assert_eq!(offsets(&it), [2.25, 6.25]);
        assert_eq!(it.active_take, 1);
        // Never a full pass: both pieces stay on the loop range, the first starting in silence.
        let it = place(4.25, 10.0, Some((8.0, 12.0)), None).unwrap();
        assert_eq!((it.position, it.length), (8.0, 4.0));
        assert_eq!(offsets(&it), [-1.75, 2.25]);
        assert_eq!(it.active_take, 1);
    }

    #[test]
    fn punch_trims_to_the_time_selection() {
        let it = place(10.25, 2.0, None, Some((4.0, 6.0))).unwrap();
        assert_eq!((it.position, it.length), (4.0, 2.0));
        assert_eq!(offsets(&it), [2.25]);
        assert!(place(10.25, 2.0, None, Some((20.0, 22.0))).is_none());
        // Loop recording with the loop as punch range keeps exactly the loop.
        let it = place(10.25, 6.0, Some((8.0, 12.0)), Some((8.0, 12.0))).unwrap();
        assert_eq!((it.position, it.length), (8.0, 4.0));
        assert_eq!(offsets(&it), [2.25, 6.25]);
    }

    #[test]
    fn midi_recording() {
        let tempo = crate::TempoMap::default();
        let tl = tempo.timeline();
        let pass = |looped: Option<(f64, f64)>, punch: Option<(f64, f64)>| RecordPass {
            start: 2.0,
            looped: looped.map(|(a, b)| TimeRange::new(a, b)),
            punch: punch.map(|(a, b)| TimeRange::new(a, b)),
        };
        // 120 bpm: a quarter note is half a second.
        let events = [
            (2.5, [0x90, 60, 100], 3),
            (2.75, [0x80, 64, 0], 3),
            (3.0, [0xb0, 1, 50], 3),
            (3.5, [0x90, 60, 0], 3),
            (3.75, [0x90, 62, 90], 3),
        ];
        let it = place_midi_recording(&events, pass(None, None), 4.0, &tl).unwrap();
        assert_eq!((it.position, it.length), (2.0, 2.0));
        let Source::Raw(c) = &it.take().unwrap().source else { panic!() };
        let src = MidiSource::parse(c).unwrap();
        assert_eq!(src.length_qn(), 4.0);
        let notes = src.notes();
        assert_eq!(notes.len(), 2);
        assert_eq!((notes[0].start_qn, notes[0].end_qn, notes[0].key), (1.0, 3.0, 60));
        assert_eq!((notes[1].start_qn, notes[1].end_qn, notes[1].key), (3.5, 4.0, 62));
        assert!(!src.events.iter().any(|e| e.bytes[1] == 64), "orphan note-off dropped");
        assert!(place_midi_recording(&[], pass(None, None), 4.0, &tl).is_none());

        // Wrapped around a loop from 1 to 3 s: the item spans the loop, events stay in place.
        let looped = [(2.5, [0x90, 60, 100], 3), (1.5, [0x80, 60, 0], 3)];
        let it = place_midi_recording(&looped, pass(Some((1.0, 3.0)), None), 1.75, &tl).unwrap();
        assert_eq!((it.position, it.length), (1.0, 2.0));

        // Punch from 3 to 3.6 s keeps only what was played there.
        let it = place_midi_recording(&events, pass(None, Some((3.0, 3.6))), 4.0, &tl).unwrap();
        assert!((it.position - 3.0).abs() < 1e-9 && (it.length - 0.6).abs() < 1e-9);
        let Source::Raw(c) = &it.take().unwrap().source else { panic!() };
        let src = MidiSource::parse(c).unwrap();
        assert!(src.notes().is_empty());
        assert_eq!(src.events[0].bytes, [0xb0, 1, 50]);
    }
}
