//! Standard MIDI Files (`.mid`): reading into a [`MidiSource`] and writing one back out.

use crate::midi::{MidiEvent, MidiSource};
use crate::rpp::{Chunk, Node};

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SmfError {
    #[error("not a standard MIDI file")]
    NotSmf,
    #[error("the file ends inside a {0}")]
    Truncated(&'static str),
    #[error("SMPTE time division is not supported")]
    Smpte,
}

/// What a MIDI file holds besides its events.
#[derive(Debug, Clone, PartialEq)]
pub struct SmfInfo {
    /// Track name (meta event 3) of the first track that has one.
    pub name: Option<String>,
    /// Initial tempo in beats per minute, when the file sets one.
    pub bpm: Option<f64>,
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize, what: &'static str) -> Result<&'a [u8], SmfError> {
        let out = self.data.get(self.at..self.at + n).ok_or(SmfError::Truncated(what))?;
        self.at += n;
        Ok(out)
    }

    fn byte(&mut self, what: &'static str) -> Result<u8, SmfError> {
        Ok(self.take(1, what)?[0])
    }

    fn varlen(&mut self, what: &'static str) -> Result<u64, SmfError> {
        let mut v = 0u64;
        for _ in 0..4 {
            let b = self.byte(what)?;
            v = (v << 7) | (b & 0x7f) as u64;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Ok(v)
    }
}

fn data_len(status: u8) -> usize {
    match status & 0xf0 {
        0xc0 | 0xd0 => 1,
        _ => 2,
    }
}

/// Reads every track of a format 0 or 1 file into one source at the file's resolution; system
/// exclusive and meta events are left out. The source ends at the last end-of-track.
pub fn read_smf(data: &[u8]) -> Result<(MidiSource, SmfInfo), SmfError> {
    let mut r = Reader { data, at: 0 };
    if r.take(4, "header")? != b"MThd" {
        return Err(SmfError::NotSmf);
    }
    let header_len = u32::from_be_bytes(r.take(4, "header")?.try_into().unwrap()) as usize;
    let header = r.take(header_len.max(6), "header")?;
    let ntracks = u16::from_be_bytes([header[2], header[3]]);
    let division = u16::from_be_bytes([header[4], header[5]]);
    if division & 0x8000 != 0 {
        return Err(SmfError::Smpte);
    }
    let mut info = SmfInfo { name: None, bpm: None };
    let mut events = Vec::new();
    let mut length = 0u64;
    for _ in 0..ntracks {
        if r.at + 8 > data.len() {
            break;
        }
        let id = r.take(4, "track")?;
        let len = u32::from_be_bytes(r.take(4, "track")?.try_into().unwrap()) as usize;
        let body = r.take(len.min(data.len() - r.at), "track")?;
        if id != b"MTrk" {
            continue;
        }
        let mut t = Reader { data: body, at: 0 };
        let mut tick = 0u64;
        let mut running = 0u8;
        while t.at < body.len() {
            tick += t.varlen("event")?;
            let mut status = t.byte("event")?;
            match status {
                0xff => {
                    let kind = t.byte("meta event")?;
                    let n = t.varlen("meta event")? as usize;
                    let payload = t.take(n, "meta event")?;
                    match kind {
                        0x03 if info.name.is_none() && !payload.is_empty() => {
                            info.name = Some(String::from_utf8_lossy(payload).trim().to_owned());
                        }
                        0x51 if info.bpm.is_none() && n == 3 => {
                            let us = u32::from_be_bytes([0, payload[0], payload[1], payload[2]]);
                            if us > 0 {
                                info.bpm = Some(60_000_000.0 / us as f64);
                            }
                        }
                        0x2f => break,
                        _ => {}
                    }
                }
                0xf0 | 0xf7 => {
                    let n = t.varlen("sysex")? as usize;
                    t.take(n, "sysex")?;
                }
                _ => {
                    let mut bytes = [0u8; 3];
                    let first = if status & 0x80 == 0 {
                        // Running status: this byte is already the first data byte.
                        let d = status;
                        status = running;
                        if status == 0 {
                            continue;
                        }
                        Some(d)
                    } else {
                        running = status;
                        None
                    };
                    bytes[0] = status;
                    let n = data_len(status);
                    for (i, b) in bytes[1..=n].iter_mut().enumerate() {
                        *b = match (i, first) {
                            (0, Some(d)) => d,
                            _ => t.byte("event")?,
                        };
                    }
                    events.push(MidiEvent { tick, bytes, len: n as u8 + 1 });
                }
            }
        }
        length = length.max(tick);
    }
    // Stable: events of the same tick keep their file order across tracks.
    events.sort_by_key(|e| e.tick);
    Ok((MidiSource { ticks_per_qn: division.max(1) as u32, events, length_ticks: length }, info))
}

fn put_varlen(out: &mut Vec<u8>, mut v: u64) {
    let mut stack = vec![(v & 0x7f) as u8];
    v >>= 7;
    while v > 0 {
        stack.push((v & 0x7f) as u8 | 0x80);
        v >>= 7;
    }
    out.extend(stack.iter().rev());
}

/// Writes a format 0 file with the source's events, an optional tempo and track name, and the
/// end of track at the source's end.
pub fn write_smf(src: &MidiSource, bpm: Option<f64>, name: Option<&str>) -> Vec<u8> {
    let mut track = Vec::new();
    if let Some(name) = name.filter(|n| !n.is_empty()) {
        track.extend([0, 0xff, 0x03]);
        put_varlen(&mut track, name.len() as u64);
        track.extend(name.as_bytes());
    }
    if let Some(bpm) = bpm.filter(|b| *b > 0.0) {
        let us = (60_000_000.0 / bpm).round() as u32;
        track.extend([0, 0xff, 0x51, 3]);
        track.extend(&us.to_be_bytes()[1..]);
    }
    let mut prev = 0;
    for e in &src.events {
        put_varlen(&mut track, e.tick - prev);
        track.extend(&e.bytes[..e.len as usize]);
        prev = e.tick;
    }
    put_varlen(&mut track, src.length_ticks.saturating_sub(prev));
    track.extend([0xff, 0x2f, 0]);

    let mut out = Vec::with_capacity(track.len() + 22);
    out.extend(b"MThd");
    out.extend(6u32.to_be_bytes());
    out.extend(0u16.to_be_bytes());
    out.extend(1u16.to_be_bytes());
    out.extend((src.ticks_per_qn.min(0x7fff) as u16).to_be_bytes());
    out.extend(b"MTrk");
    out.extend((track.len() as u32).to_be_bytes());
    out.extend(track);
    out
}

impl MidiSource {
    /// A new in-project `<SOURCE MIDI` chunk holding these events. REAPER marks the end of the
    /// source with a final event, so one is added when no event sits at the end.
    pub fn to_chunk(&self) -> Chunk {
        let mut c = Chunk::new("SOURCE", &["MIDI"]);
        c.children.push(Node::Line(format!("HASDATA 1 {} QN", self.ticks_per_qn)));
        let mut events = self.events.clone();
        if events.last().is_none_or(|e| e.tick < self.length_ticks) {
            events.push(MidiEvent { tick: self.length_ticks, bytes: [0xb0, 0x7b, 0], len: 3 });
        }
        let mut prev = 0;
        for e in &events {
            let bytes: Vec<String> = e.bytes[..e.len as usize].iter().map(|b| format!("{b:02x}")).collect();
            c.children.push(Node::Line(format!("E {} {}", e.tick - prev, bytes.join(" "))));
            prev = e.tick;
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_format_1_with_running_status() {
        let mut f = Vec::new();
        f.extend(b"MThd\0\0\0\x06\0\x01\0\x02\x01\xe0");
        // Tempo track: name, 100 bpm (600000 us), end at tick 0.
        let tempo = [0, 0xff, 3, 4, b'S', b'o', b'n', b'g', 0, 0xff, 0x51, 3, 0x09, 0x27, 0xc0, 0, 0xff, 0x2f, 0];
        f.extend(b"MTrk");
        f.extend((tempo.len() as u32).to_be_bytes());
        f.extend(tempo);
        // Note on C4, running-status note off (velocity 0) 480 ticks later, a CC, sysex, end.
        let notes =
            [0, 0x90, 60, 100, 0x83, 0x60, 60, 0, 0, 0xb0, 7, 90, 0, 0xf0, 2, 0x7e, 0xf7, 0x81, 0x40, 0xff, 0x2f, 0];
        f.extend(b"MTrk");
        f.extend((notes.len() as u32).to_be_bytes());
        f.extend(notes);
        let (src, info) = read_smf(&f).unwrap();
        assert_eq!(info, SmfInfo { name: Some("Song".into()), bpm: Some(100.0) });
        assert_eq!(src.ticks_per_qn, 480);
        assert_eq!(src.length_ticks, 672);
        let notes = src.notes();
        assert_eq!(notes.len(), 1);
        assert_eq!((notes[0].start_qn, notes[0].end_qn, notes[0].key, notes[0].velocity), (0.0, 1.0, 60, 100));
        assert_eq!(src.events[2].bytes, [0xb0, 7, 90]);
    }

    #[test]
    fn round_trips_through_a_file_and_a_chunk() {
        let src = MidiSource {
            ticks_per_qn: 960,
            events: vec![
                MidiEvent { tick: 0, bytes: [0xc0, 5, 0], len: 2 },
                MidiEvent { tick: 0, bytes: [0x99, 36, 110], len: 3 },
                MidiEvent { tick: 240, bytes: [0x89, 36, 0], len: 3 },
                MidiEvent { tick: 200_000, bytes: [0xe0, 0, 64], len: 3 },
            ],
            length_ticks: 230_400,
        };
        let bytes = write_smf(&src, Some(128.0), Some("Drums"));
        let (back, info) = read_smf(&bytes).unwrap();
        assert_eq!(back, src);
        assert_eq!(info.name.as_deref(), Some("Drums"));
        assert!((info.bpm.unwrap() - 128.0).abs() < 1e-3);
        let chunk = src.to_chunk();
        let parsed = MidiSource::parse(&chunk).unwrap();
        assert_eq!(parsed.length_ticks, src.length_ticks);
        assert_eq!(&parsed.events[..4], &src.events[..]);
    }

    #[test]
    fn rejects_other_files() {
        assert_eq!(read_smf(b"RIFF....").unwrap_err(), SmfError::NotSmf);
        assert_eq!(read_smf(b"MThd\0\0").unwrap_err(), SmfError::Truncated("header"));
    }
}
