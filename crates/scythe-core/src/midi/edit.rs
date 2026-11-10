//! Note editing as REAPER's MIDI editor does it: notes with their selection, mute and notation,
//! and the commands that work on them (quantize, humanize, transpose, legato, split, join and
//! the rest). Commands work on the selected notes, or on all notes when none is selected.

use super::scale::KeySnap;
use super::{LongEvent, MUTED, MidiEvent, MidiSource, SELECTED};

/// A note in ticks from the source start.
#[derive(Debug, Clone, PartialEq)]
pub struct EditNote {
    pub start: u64,
    pub end: u64,
    pub channel: u8,
    pub key: u8,
    pub velocity: u8,
    pub off_velocity: u8,
    pub selected: bool,
    pub muted: bool,
    /// The note's notation (articulation, ornaments, text) as REAPER keeps it: a `NOTE channel
    /// key …` text event at the note start, here without that prefix.
    pub notation: Option<String>,
}

impl EditNote {
    pub fn new(start: u64, end: u64, channel: u8, key: u8, velocity: u8) -> Self {
        Self {
            start,
            end: end.max(start + 1),
            channel: channel & 0x0f,
            key: key.min(127),
            velocity: velocity.clamp(1, 127),
            off_velocity: 0,
            selected: false,
            muted: false,
            notation: None,
        }
    }

    pub fn length(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }
}

const NOTATION: u8 = 0x0f;

/// The channel, key and rest of a `NOTE channel key …` notation event.
fn notation(x: &LongEvent) -> Option<(u8, u8, String)> {
    let text = match x.data.as_slice() {
        [0xff, NOTATION, rest @ ..] => std::str::from_utf8(rest).ok()?,
        _ => return None,
    };
    let mut parts = text.splitn(4, ' ');
    if parts.next()? != "NOTE" {
        return None;
    }
    let channel = parts.next()?.parse().ok()?;
    let key = parts.next()?.parse().ok()?;
    Some((channel, key, parts.next().unwrap_or("").to_owned()))
}

fn notation_event(tick: u64, n: &EditNote, text: &str) -> LongEvent {
    let mut data = vec![0xff, NOTATION];
    data.extend(format!("NOTE {} {} {}", n.channel, n.key, text).trim_end().as_bytes());
    LongEvent { tick, args: vec!["0".into()], data }
}

fn is_note_on(e: &MidiEvent) -> bool {
    e.bytes[0] & 0xf0 == 0x90 && e.bytes[2] > 0
}

fn is_note_off(e: &MidiEvent) -> bool {
    e.bytes[0] & 0xf0 == 0x80 || (e.bytes[0] & 0xf0 == 0x90 && e.bytes[2] == 0)
}

impl MidiSource {
    /// Every note, muted ones too, in start order. A note-on ends at the first later note-off
    /// of its channel and key; one still held at the end of the source ends there.
    pub fn edit_notes(&self) -> Vec<EditNote> {
        let mut used = vec![false; self.events.len()];
        let mut out = Vec::new();
        for (i, e) in self.events.iter().enumerate() {
            if !is_note_on(e) {
                continue;
            }
            let (channel, key) = (e.bytes[0] & 0x0f, e.bytes[1]);
            let off = (i + 1..self.events.len()).find(|&j| {
                let o = &self.events[j];
                !used[j] && is_note_off(o) && o.bytes[0] & 0x0f == channel && o.bytes[1] == key
            });
            if let Some(j) = off {
                used[j] = true;
            }
            let (end, off_velocity) = off.map_or((self.length_ticks.max(e.tick), 0), |j| {
                let o = &self.events[j];
                (o.tick, if o.bytes[0] & 0xf0 == 0x80 { o.bytes[2] } else { 0 })
            });
            out.push(EditNote {
                start: e.tick,
                end,
                channel,
                key,
                velocity: e.bytes[2],
                off_velocity,
                selected: e.flags & SELECTED != 0,
                muted: e.muted(),
                notation: None,
            });
        }
        for x in &self.long {
            if let Some((ch, key, text)) = notation(x)
                && let Some(n) = out.iter_mut().find(|n| n.start == x.tick && n.channel == ch && n.key == key)
            {
                n.notation = Some(text);
            }
        }
        out.sort_by_key(|n| (n.start, n.key, n.channel));
        out
    }

    /// The same source with its notes (and their notation events) replaced by `notes`. Other
    /// events stay; the source grows if a note ends past its end.
    pub fn with_edit_notes(&self, notes: &[EditNote]) -> MidiSource {
        let old = self.edit_notes();
        let note_end = notes.iter().map(|n| n.end).max().unwrap_or(0);
        let length = self.length_ticks.max(note_end);
        let mut events: Vec<MidiEvent> = self
            .events
            .iter()
            .filter(|e| !matches!(e.bytes[0] & 0xf0, 0x80 | 0x90))
            .map(|e| if self.is_end_marker(e) { MidiEvent { tick: length, ..e.clone() } } else { e.clone() })
            .collect();
        for n in notes {
            let flags = if n.selected { SELECTED } else { 0 } | if n.muted { MUTED } else { 0 };
            let (start, end) = (n.start, n.end.max(n.start + 1));
            let on = [0x90 | n.channel & 0x0f, n.key.min(127), n.velocity.clamp(1, 127)];
            events.push(MidiEvent { tick: start, bytes: on, len: 3, flags, shape: None });
            let off = [0x80 | n.channel & 0x0f, n.key.min(127), n.off_velocity.min(127)];
            events.push(MidiEvent { tick: end, bytes: off, len: 3, flags, shape: None });
        }
        let end_marker = |e: &MidiEvent| e.tick == length && e.bytes[0] & 0xf0 == 0xb0 && e.bytes[1] == 0x7b;
        // Note-offs before note-ons at the same tick, so a repeated key is not cut short; the end
        // marker after everything at the end.
        events.sort_by_key(|e| (e.tick, end_marker(e), is_note_on(e)));
        let attached = |x: &LongEvent| {
            notation(x).is_some_and(|(ch, k, _)| old.iter().any(|n| n.start == x.tick && n.channel == ch && n.key == k))
        };
        let mut long: Vec<LongEvent> = self.long.iter().filter(|x| !attached(x)).cloned().collect();
        long.extend(notes.iter().filter_map(|n| n.notation.as_deref().map(|t| notation_event(n.start, n, t))));
        long.sort_by_key(|x| x.tick);
        MidiSource { events, long, length_ticks: length, ..self.without_events() }
    }
}

/// The notes a command works on: the selected ones, or all when none is selected.
pub fn targets(notes: &[EditNote]) -> Vec<usize> {
    let any = notes.iter().any(|n| n.selected);
    (0..notes.len()).filter(|&i| !any || notes[i].selected).collect()
}

/// A grid in quarter notes, with REAPER's swing: every second line moves later by half of
/// `swing` (-1 to 1) times the grid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    pub qn: f64,
    pub swing: f64,
}

impl Grid {
    pub fn straight(qn: f64) -> Self {
        Self { qn, swing: 0.0 }
    }

    /// The grid line nearest `tick`, in ticks.
    pub fn nearest(&self, ppq: u32, tick: f64) -> f64 {
        let g = self.qn * ppq as f64;
        if g <= 0.0 {
            return tick;
        }
        let pair = (tick / (2.0 * g)).floor() * 2.0 * g;
        let swung = pair + g + self.swing.clamp(-1.0, 1.0) * g / 2.0;
        // A note halfway between two lines goes to the later one, as REAPER rounds.
        [pair + 2.0 * g, swung, pair]
            .into_iter()
            .min_by(|a, b| (a - tick).abs().total_cmp(&(b - tick).abs()))
            .unwrap_or(tick)
    }

    /// The grid lines strictly inside `from..to`, in ticks.
    pub fn lines_between(&self, ppq: u32, from: f64, to: f64) -> Vec<f64> {
        let g = self.qn * ppq as f64;
        if g <= 0.0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut pair = (from / (2.0 * g)).floor() * 2.0 * g;
        while pair < to {
            for line in [pair, pair + g + self.swing.clamp(-1.0, 1.0) * g / 2.0] {
                if line > from + 1e-9 && line < to - 1e-9 {
                    out.push(line);
                }
            }
            pair += 2.0 * g;
        }
        out
    }
}

/// What quantizing moves, as REAPER's quantize dialog offers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QuantizeWhat {
    /// Note starts, keeping lengths.
    #[default]
    Position,
    /// Starts and ends.
    PositionAndEnd,
    /// Ends only.
    End,
    /// Lengths, to whole grid steps.
    Length,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quantize {
    pub grid: Grid,
    /// 0 to 1: how far towards the grid line.
    pub strength: f64,
    pub what: QuantizeWhat,
    /// Only notes this far from a grid line, as fractions of the grid (`(0, 1)` for all).
    pub window: (f64, f64),
}

impl Quantize {
    pub fn to_grid(grid: Grid) -> Self {
        Self { grid, strength: 1.0, what: QuantizeWhat::Position, window: (0.0, 1.0) }
    }

    /// Where `tick` goes, or `None` when it lies outside the window.
    fn moved(&self, ppq: u32, tick: u64) -> Option<u64> {
        let t = tick as f64;
        let target = self.grid.nearest(ppq, t);
        let g = self.grid.qn * ppq as f64;
        let distance = (target - t).abs() / g.max(1e-9);
        if distance < self.window.0 - 1e-9 || distance > self.window.1 + 1e-9 {
            return None;
        }
        Some((t + (target - t) * self.strength.clamp(0.0, 1.0)).round().max(0.0) as u64)
    }
}

/// Quantizes the target notes. An end that lands on or before its start goes one grid step past
/// it. Notes left on top of each other (same channel, key and start) are merged, as REAPER does.
pub fn quantize(notes: &mut Vec<EditNote>, ppq: u32, q: &Quantize) {
    let g = ((q.grid.qn * ppq as f64).round() as u64).max(1);
    for i in targets(notes) {
        let n = &mut notes[i];
        match q.what {
            QuantizeWhat::Position => {
                if let Some(start) = q.moved(ppq, n.start) {
                    let len = n.length();
                    n.start = start;
                    n.end = start + len;
                }
            }
            QuantizeWhat::PositionAndEnd => {
                let start = q.moved(ppq, n.start).unwrap_or(n.start);
                let end = q.moved(ppq, n.end).unwrap_or(n.end);
                n.start = start;
                n.end = if end <= start { start + g } else { end };
            }
            QuantizeWhat::End => {
                if let Some(end) = q.moved(ppq, n.end) {
                    n.end = if end <= n.start { n.start + g } else { end };
                }
            }
            QuantizeWhat::Length => {
                let len = n.length() as f64;
                let target = ((len / g as f64).round().max(1.0)) * g as f64;
                n.end = n.start + (len + (target - len) * q.strength.clamp(0.0, 1.0)).round().max(1.0) as u64;
            }
        }
    }
    merge_overlaps(notes);
}

/// Drops notes that start where an earlier one of the same channel and key starts.
fn merge_overlaps(notes: &mut Vec<EditNote>) {
    notes.sort_by_key(|n| (n.start, n.key, n.channel));
    notes.dedup_by(|b, a| a.start == b.start && a.key == b.key && a.channel == b.channel);
}

/// A small deterministic generator, so a humanize can be repeated from its seed.
struct Rng(u64);

impl Rng {
    /// Uniform in -1..1.
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Humanize {
    /// Largest shift of a start, as a fraction of `grid_qn`.
    pub timing: f64,
    pub grid_qn: f64,
    /// Largest velocity change, as a fraction of the full range.
    pub velocity: f64,
    pub seed: u64,
}

/// Moves starts (keeping lengths) and changes velocities by random amounts up to the limits.
pub fn humanize(notes: &mut [EditNote], ppq: u32, h: &Humanize) {
    let mut rng = Rng(h.seed | 1);
    let span = h.timing.clamp(0.0, 1.0) * h.grid_qn * ppq as f64;
    for i in targets(notes) {
        let n = &mut notes[i];
        let shift = (rng.next() * span).round() as i64;
        let len = n.length();
        n.start = (n.start as i64 + shift).max(0) as u64;
        n.end = n.start + len;
        let dv = (rng.next() * h.velocity.clamp(0.0, 1.0) * 127.0).round() as i32;
        n.velocity = (n.velocity as i32 + dv).clamp(1, 127) as u8;
    }
    notes.sort_by_key(|n| (n.start, n.key, n.channel));
}

/// Moves the target notes by `semitones`; with key snap, by that many steps of the scale.
pub fn transpose(notes: &mut [EditNote], semitones: i32, snap: Option<&KeySnap>) {
    for i in targets(notes) {
        let n = &mut notes[i];
        let key = match snap {
            Some(s) => s.step(n.key, semitones),
            None => (n.key as i32 + semitones).clamp(0, 127) as u8,
        };
        n.key = key;
    }
}

/// Moves the target notes onto the nearest key of the scale (REAPER's "Force selected notes
/// into key signature").
pub fn force_into_key(notes: &mut [EditNote], snap: &KeySnap) {
    for i in targets(notes) {
        notes[i].key = snap.nearest(notes[i].key);
    }
}

/// Ends each target note where the next target note (by start) begins; the last one stays.
pub fn legato(notes: &mut [EditNote]) {
    let t = targets(notes);
    let mut starts: Vec<u64> = t.iter().map(|&i| notes[i].start).collect();
    starts.sort_unstable();
    starts.dedup();
    for i in t {
        if let Some(&next) = starts.iter().find(|&&s| s > notes[i].start) {
            notes[i].end = next;
        }
    }
}

/// REAPER's "Make notes legato, preserving relative note spacing" (40766): the first target
/// note keeps its start and the last its end; of the `n` starts, the `i`th (from 0) moves back
/// by `i / n` of the gap before it, and every other note then ends where the next begins.
/// Measured in REAPER 7.73 on all, every other and three middle notes of seven.
pub fn legato_relative(notes: &mut [EditNote]) {
    let mut t = targets(notes);
    t.sort_by_key(|&i| notes[i].start);
    let mut starts: Vec<u64> = t.iter().map(|&i| notes[i].start).collect();
    starts.dedup();
    let n = starts.len();
    if n < 2 {
        return;
    }
    let end_before = |s: u64| t.iter().filter(|&&i| notes[i].start < s).map(|&i| notes[i].end).max();
    let moved: Vec<u64> = starts
        .iter()
        .enumerate()
        .map(|(k, &s)| {
            let gap = end_before(s).map_or(0.0, |e| s as f64 - e as f64);
            (s as f64 - gap * k as f64 / n as f64).round().max(0.0) as u64
        })
        .collect();
    for &i in &t {
        let k = starts.iter().position(|&s| s == notes[i].start).unwrap_or(0);
        notes[i].start = moved[k];
        if let Some(&next) = moved.get(k + 1) {
            notes[i].end = next.max(moved[k] + 1);
        }
    }
}

/// Splits the target notes at every grid line inside them.
pub fn split_on_grid(notes: &mut Vec<EditNote>, ppq: u32, grid: Grid) {
    let t = targets(notes);
    let mut out = Vec::with_capacity(notes.len());
    for (i, n) in notes.iter().enumerate() {
        if !t.contains(&i) {
            out.push(n.clone());
            continue;
        }
        let mut start = n.start;
        for line in grid.lines_between(ppq, n.start as f64, n.end as f64) {
            let at = line.round() as u64;
            if at > start && at < n.end {
                out.push(EditNote { end: at, ..n.clone() });
                start = at;
            }
        }
        out.push(EditNote { start, notation: if start == n.start { n.notation.clone() } else { None }, ..n.clone() });
    }
    out.sort_by_key(|n| (n.start, n.key, n.channel));
    *notes = out;
}

/// Splits the target notes that span `tick`.
pub fn split_at(notes: &mut Vec<EditNote>, tick: u64) {
    let t = targets(notes);
    let mut extra = Vec::new();
    for i in t {
        let n = &mut notes[i];
        if n.start < tick && tick < n.end {
            extra.push(EditNote { start: tick, notation: None, ..n.clone() });
            n.end = tick;
        }
    }
    notes.extend(extra);
    notes.sort_by_key(|n| (n.start, n.key, n.channel));
}

/// Joins the target notes of each channel and key into one, from the first start to the last
/// end, with the first note's velocity.
pub fn join(notes: &mut Vec<EditNote>) {
    let t = targets(notes);
    let mut drop = vec![false; notes.len()];
    for &i in &t {
        if drop[i] {
            continue;
        }
        let same: Vec<usize> = t
            .iter()
            .copied()
            .filter(|&j| j != i && !drop[j] && notes[j].channel == notes[i].channel && notes[j].key == notes[i].key)
            .collect();
        for j in same {
            let end = notes[j].end;
            notes[i].end = notes[i].end.max(end);
            drop[j] = true;
        }
    }
    let mut k = 0;
    notes.retain(|_| {
        k += 1;
        !drop[k - 1]
    });
}

/// Sets the length of the target notes, in ticks.
pub fn set_length(notes: &mut [EditNote], ticks: u64) {
    for i in targets(notes) {
        notes[i].end = notes[i].start + ticks.max(1);
    }
}

/// Lengthens (or with a negative `ticks`, shortens) the target notes; a note that would vanish
/// stays as it is.
pub fn lengthen(notes: &mut [EditNote], ticks: i64) {
    for i in targets(notes) {
        let n = &mut notes[i];
        let end = n.end as i64 + ticks;
        if end > n.start as i64 {
            n.end = end as u64;
        }
    }
}

/// Moves the target notes by `ticks`, not before the source start.
pub fn shift(notes: &mut [EditNote], ticks: i64) {
    for i in targets(notes) {
        let n = &mut notes[i];
        let len = n.length();
        n.start = (n.start as i64 + ticks).max(0) as u64;
        n.end = n.start + len;
    }
}

/// Adds `delta` to the target notes' velocities, or sets them when `absolute`.
pub fn velocity(notes: &mut [EditNote], value: i32, absolute: bool) {
    for i in targets(notes) {
        let v = if absolute { value } else { notes[i].velocity as i32 + value };
        notes[i].velocity = v.clamp(1, 127) as u8;
    }
}

pub fn set_channel(notes: &mut [EditNote], channel: u8) {
    for i in targets(notes) {
        notes[i].channel = channel & 0x0f;
    }
}

/// Mutes or unmutes the target notes; `None` toggles each.
pub fn mute(notes: &mut [EditNote], muted: Option<bool>) {
    for i in targets(notes) {
        notes[i].muted = muted.unwrap_or(!notes[i].muted);
    }
}

/// Copies the target notes `semitones` away and selects only the copies (REAPER's "Duplicate
/// events one octave higher" with 12).
pub fn duplicate_transposed(notes: &mut Vec<EditNote>, semitones: i32) {
    let t = targets(notes);
    let copies: Vec<EditNote> = t
        .iter()
        .filter_map(|&i| {
            let key = notes[i].key as i32 + semitones;
            (0..=127).contains(&key).then(|| EditNote { key: key as u8, selected: true, ..notes[i].clone() })
        })
        .collect();
    for n in notes.iter_mut() {
        n.selected = false;
    }
    notes.extend(copies);
    notes.sort_by_key(|n| (n.start, n.key, n.channel));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpp::parse;

    fn notes(spec: &[(u64, u64, u8)]) -> Vec<EditNote> {
        spec.iter().map(|&(s, e, k)| EditNote { selected: true, ..EditNote::new(s, e, 0, k, 100) }).collect()
    }

    fn spans(n: &[EditNote]) -> Vec<(u64, u64, u8)> {
        n.iter().map(|n| (n.start, n.end, n.key)).collect()
    }

    /// The notes REAPER 7.73's MIDI editor actions were measured on (all selected, ppq 960).
    fn probe() -> Vec<EditNote> {
        notes(&[
            (10, 317, 61),
            (500, 814, 62),
            (1000, 1321, 63),
            (1700, 2028, 64),
            (2400, 2735, 61),
            (2900, 3242, 66),
            (3300, 3400, 66),
        ])
    }

    #[test]
    fn quantize_matches_reaper() {
        let sixteenth = Grid::straight(0.25);
        let mut n = probe();
        quantize(&mut n, 960, &Quantize::to_grid(sixteenth));
        let starts: Vec<u64> = n.iter().map(|n| n.start).collect();
        assert_eq!(starts, [0, 480, 960, 1680, 2400, 2880, 3360], "40469");
        assert_eq!(n[0].end, 307);
        let mut n = probe();
        quantize(&mut n, 960, &Quantize { what: QuantizeWhat::PositionAndEnd, ..Quantize::to_grid(sixteenth) });
        assert_eq!(
            spans(&n),
            [
                (0, 240, 61),
                (480, 720, 62),
                (960, 1440, 63),
                (1680, 1920, 64),
                (2400, 2640, 61),
                (2880, 3360, 66),
                (3360, 3600, 66)
            ],
            "40729"
        );
        let mut n = probe();
        quantize(&mut n, 960, &Quantize::to_grid(Grid { qn: 0.5, swing: 0.5 }));
        let starts: Vec<u64> = n.iter().map(|n| n.start).collect();
        assert_eq!(starts, [0, 600, 960, 1560, 2520, 2880, 3480], "40469 on a swing grid");
        let mut n = probe();
        quantize(&mut n, 960, &Quantize::to_grid(Grid::straight(1.0 / 3.0)));
        let starts: Vec<u64> = n.iter().map(|n| n.start).collect();
        assert_eq!(starts, [0, 640, 960, 1600, 2560, 2880, 3200], "triplet grid");
    }

    #[test]
    fn swing_eighty_percent() {
        let mut n = notes(&[(500, 600, 60), (560, 660, 60), (620, 720, 60), (700, 800, 60), (1500, 1600, 60)]);
        quantize(&mut n, 960, &Quantize::to_grid(Grid { qn: 0.5, swing: 0.8 }));
        assert_eq!(n.iter().map(|n| n.start).collect::<Vec<_>>(), [672, 1632], "notes landing together merge");
    }

    #[test]
    fn legato_keeping_relative_spacing_matches_reaper() {
        let mut n = probe();
        legato_relative(&mut n);
        assert_eq!(
            spans(&n),
            [
                (10, 474, 61),
                (474, 947, 62),
                (947, 1538, 63),
                (1538, 2187, 64),
                (2187, 2782, 61),
                (2782, 3250, 66),
                (3250, 3400, 66)
            ],
            "40766 on all"
        );
        let mut n = probe();
        for (i, x) in n.iter_mut().enumerate() {
            x.selected = (1..=3).contains(&i);
        }
        legato_relative(&mut n);
        assert_eq!(
            spans(&n)[..5],
            [(10, 317, 61), (500, 938, 62), (938, 1447, 63), (1447, 2028, 64), (2400, 2735, 61)],
            "40766 on three middle notes"
        );
        let mut n = probe();
        for (i, x) in n.iter_mut().enumerate() {
            x.selected = i % 2 == 0;
        }
        legato_relative(&mut n);
        let picked: Vec<_> = spans(&n).into_iter().step_by(2).take(3).collect();
        assert_eq!(picked, [(10, 829, 61), (829, 1861, 63), (1861, 2876, 61)], "40766 on every other note");
    }

    #[test]
    fn edits_match_reaper() {
        let mut n = probe();
        legato(&mut n);
        assert_eq!(n.iter().map(|n| n.end).collect::<Vec<_>>(), [500, 1000, 1700, 2400, 2900, 3300, 3400], "40765");
        let mut n = probe();
        for (i, x) in n.iter_mut().enumerate() {
            x.selected = i % 2 == 0;
        }
        legato(&mut n);
        assert_eq!(n.iter().map(|n| n.end).collect::<Vec<_>>(), [1000, 814, 2400, 2028, 3300, 3242, 3400]);
        let mut n = probe();
        join(&mut n);
        assert_eq!(
            spans(&n),
            [(10, 2735, 61), (500, 814, 62), (1000, 1321, 63), (1700, 2028, 64), (2900, 3400, 66)],
            "40456"
        );
        let mut n = probe();
        split_on_grid(&mut n, 960, Grid::straight(0.25));
        assert_eq!(n.len(), 14, "40641");
        assert_eq!(spans(&n)[..4], [(10, 240, 61), (240, 317, 61), (500, 720, 62), (720, 814, 62)]);
        let mut n = probe();
        set_length(&mut n, 240);
        assert_eq!(n[0].end, 250, "40633");
        let mut n = probe();
        lengthen(&mut n, -240);
        assert_eq!((n[0].end, n[6].end), (77, 3400), "40447");
        let mut n = probe();
        velocity(&mut n, 10, false);
        assert_eq!(n[0].velocity, 110, "40463");
        let mut n = probe();
        duplicate_transposed(&mut n, 12);
        assert_eq!(n.len(), 14);
        assert_eq!(n.iter().filter(|n| n.selected).map(|n| n.key).min(), Some(73), "40884");
    }

    #[test]
    fn notes_round_trip_with_flags_and_notation() {
        let c = parse(
            "<SOURCE MIDI\n  HASDATA 1 960 QN\n  e 0 90 3c 64\n  <X 0 0\n    /w9OT1RFIDAgNjAgdGV4dCBoaQ==\n  >\n  e 480 80 3c 00\n  Em 0 90 40 50\n  Em 480 80 40 00\n  E 960 b0 7b 00\n>\n",
        )
        .unwrap();
        let m = MidiSource::parse(&c).unwrap();
        let mut n = m.edit_notes();
        assert_eq!(n.len(), 2);
        assert!(n[0].selected && !n[0].muted && n[1].muted);
        assert_eq!(n[0].notation.as_deref(), Some("text hi"));
        assert_eq!(crate::rpp::write(&m.with_edit_notes(&n).write_chunk(&c)), crate::rpp::write(&c));
        n[0].start = 240;
        n[0].key = 62;
        let moved = m.with_edit_notes(&n);
        let back = moved.edit_notes();
        assert_eq!((back[0].start, back[0].key, back[0].notation.as_deref()), (240, 62, Some("text hi")));
        assert_eq!(moved.long.len(), 1, "the old notation event goes");
    }
}
