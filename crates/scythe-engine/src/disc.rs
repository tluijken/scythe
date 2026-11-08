//! Audio CD images (cue sheet and `.bin`) and DDP 2.00 filesets, laid out from the project's
//! markers and regions the way REAPER 7.73's CD image and DDP sinks lay them out.

use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use scythe_core::render_settings::{CdImage, CdTracks};
use scythe_core::{Marker, TimeRange};

/// Both sinks write 44.1 kHz 16-bit stereo, whatever the project renders at.
pub const RATE: u32 = 44_100;
/// Samples per CD frame (1/75 s).
const FRAME: u64 = 588;
const MIN_TRACK: u64 = 4 * RATE as u64;

/// A position in render samples, as REAPER rounds marker times.
fn sample_at(seconds: f64) -> i64 {
    (seconds * RATE as f64 + 0.5).floor() as i64
}

/// `mm:ss:ff` of a position in samples, truncated to whole frames. REAPER works this out in
/// single precision, which puts some positions a frame early.
fn msf(samples: u64) -> String {
    msf_seconds(samples as f32 * (1.0f32 / RATE as f32))
}

fn msf_seconds(seconds: f32) -> String {
    let whole = seconds.floor();
    let frames = ((seconds - whole) * 75.0) as u64;
    let whole = whole as u64;
    format!("{:02}:{:02}:{:02}", whole / 60, whole % 60, frames)
}

#[derive(Debug, Clone, PartialEq)]
struct CdTrack {
    /// First sample in the image, after the lead-in.
    start: u64,
    /// Where the 4-second rule measures from: the start with the lead-in as asked for rather
    /// than padded to whole frames. A later track must start at least 4 s after this rounded
    /// up, so an exact 4 s gap fails when the lead-in is not a whole number of samples.
    measured: f64,
    title: Option<String>,
    /// Cue sheet keys and values, in marker order.
    info: Vec<(&'static str, String)>,
}

#[derive(Debug, Clone, PartialEq)]
struct CdLayout {
    /// Spans of the render (in samples) that go into the image, in order.
    spans: Vec<(u64, u64)>,
    tracks: Vec<CdTrack>,
    /// Silence before the audio, a whole number of frames.
    lead_in: u64,
}

/// `PERF=`, `WRIT=` and `ISRC=` markers set the cue sheet's performer, songwriter and ISRC of
/// the track they fall in.
fn cue_info(name: &str) -> Option<(&'static str, String)> {
    let key = match name.get(..5)?.to_ascii_uppercase().as_str() {
        "PERF=" => "PERFORMER",
        "WRIT=" => "SONGWRITER",
        "ISRC=" => "ISRC",
        _ => return None,
    };
    Some((key, name[5..].to_owned()))
}

fn add_info(track: &mut CdTrack, (key, value): (&'static str, String)) {
    // A track keeps the first of each key.
    if !track.info.iter().any(|(k, _)| *k == key) {
        track.info.push((key, value));
    }
}

fn cd_layout(markers: &[Marker], range: TimeRange, total: u64, opts: &CdImage) -> CdLayout {
    let exact_lead_in = opts.lead_in_ms as f64 * RATE as f64 / 1000.0;
    let lead_in = (exact_lead_in / FRAME as f64).ceil() as u64 * FRAME;
    let mut sorted: Vec<&Marker> = markers.iter().collect();
    sorted.sort_by(|a, b| a.position.total_cmp(&b.position));
    let points = sorted.iter().filter(|m| m.region_end.is_none());
    let new_track = |start: u64, title: Option<String>| CdTrack {
        start: start + lead_in,
        measured: if start == 0 { 0.0 } else { start as f64 + exact_lead_in },
        title,
        info: Vec::new(),
    };
    let long_enough =
        |at: u64, last: &CdTrack| (at as f64 + exact_lead_in).floor() >= last.measured.ceil() + MIN_TRACK as f64;
    match opts.tracks {
        CdTracks::Single => CdLayout { spans: vec![(0, total)], tracks: vec![new_track(0, None)], lead_in },
        CdTracks::Markers => {
            let title = |name: &str| {
                let name = if opts.hash_markers_only { name.strip_prefix('#').unwrap_or(name) } else { name };
                Some(name.to_owned())
            };
            let starts_track = |m: &Marker| !opts.hash_markers_only || m.name.starts_with('#');
            // The first track is named by the last marker at or before the start.
            let first = points
                .clone()
                .rfind(|m| m.position <= range.start && cue_info(&m.name).is_none() && starts_track(m))
                .and_then(|m| title(&m.name));
            let mut tracks = vec![new_track(0, first)];
            for m in points.filter(|m| m.position >= range.start) {
                let at = sample_at(m.position - range.start);
                if at < 0 || at as u64 >= total {
                    continue;
                }
                let at = at as u64;
                let current = tracks.last_mut().expect("there is always a first track");
                if let Some(info) = cue_info(&m.name) {
                    add_info(current, info);
                } else if !starts_track(m) || m.position == range.start {
                    continue;
                } else if long_enough(at, current) {
                    tracks.push(new_track(at, title(&m.name)));
                } else if current.title.is_none() {
                    current.title = title(&m.name);
                }
            }
            CdLayout { spans: vec![(0, total)], tracks, lead_in }
        }
        CdTracks::Regions => {
            let regions: Vec<(&Marker, u64, u64)> = sorted
                .iter()
                .filter_map(|r| {
                    let end = r.region_end?.min(range.end);
                    let s = sample_at(r.position.max(range.start) - range.start).max(0) as u64;
                    let e = (sample_at(end - range.start).max(0) as u64).min(total);
                    (e > s).then_some((*r, s, e))
                })
                .collect();
            let mut spans: Vec<(u64, u64)> = Vec::new();
            for &(_, s, e) in &regions {
                match spans.last_mut() {
                    Some(last) if s <= last.1 => last.1 = last.1.max(e),
                    _ => spans.push((s, e)),
                }
            }
            // Where a render sample lands in the image; one between spans lands at the next.
            let place = |at: u64| -> Option<u64> {
                let mut out = 0;
                for &(s, e) in &spans {
                    if at < e {
                        return Some(out + at.saturating_sub(s));
                    }
                    out += e - s;
                }
                None
            };
            let mut tracks: Vec<CdTrack> = Vec::new();
            let mut covered = 0;
            for &(_, s, e) in &regions {
                // An overlapping region starts its track where the previous one ends.
                let Some(at) = place(s.max(covered)) else { continue };
                covered = covered.max(e);
                if tracks.last().is_none_or(|t| long_enough(at, t)) {
                    tracks.push(new_track(at, None));
                }
            }
            // Titles follow the regions in order, whichever regions started the tracks.
            for (track, (region, _, _)) in tracks.iter_mut().zip(&regions) {
                track.title = (!region.name.is_empty()).then(|| region.name.clone());
            }
            // Cue entries go to the track numbered like the first region holding the marker.
            for m in points {
                let (Some(info), at) = (cue_info(&m.name), sample_at(m.position - range.start)) else { continue };
                let holder = regions.iter().position(|&(_, s, e)| at >= 0 && (s..e).contains(&(at as u64)));
                if let Some(track) = holder.and_then(|i| tracks.get_mut(i)) {
                    add_info(track, info);
                }
            }
            CdLayout { spans, tracks, lead_in }
        }
    }
}

/// Reads 16-bit stereo frames from `wav` and hands them to `sink` in blocks of whole frames.
fn for_each_block(wav: &Path, mut sink: impl FnMut(u64, &[u8]) -> io::Result<()>) -> io::Result<()> {
    let mut reader = hound::WavReader::open(wav).map_err(io::Error::other)?;
    let spec = reader.spec();
    if spec.channels != 2 || spec.bits_per_sample != 16 || spec.sample_format != hound::SampleFormat::Int {
        return Err(io::Error::other("disc images are written from 16-bit stereo"));
    }
    let mut buf = Vec::with_capacity(4 * 4096);
    let mut at = 0u64;
    let mut samples = reader.samples::<i16>();
    loop {
        buf.clear();
        for s in samples.by_ref().take(2 * 4096) {
            buf.extend_from_slice(&s.map_err(io::Error::other)?.to_le_bytes());
        }
        let whole = buf.len() / 4 * 4;
        if whole == 0 {
            return Ok(());
        }
        sink(at, &buf[..whole])?;
        at += whole as u64 / 4;
    }
}

fn wav_frames(wav: &Path) -> io::Result<u64> {
    Ok(hound::WavReader::open(wav).map_err(io::Error::other)?.duration() as u64)
}

fn write_silence(out: &mut impl Write, samples: u64) -> io::Result<()> {
    let zeros = [0u8; 4096];
    let mut left = samples * 4;
    while left > 0 {
        let n = left.min(zeros.len() as u64) as usize;
        out.write_all(&zeros[..n])?;
        left -= n as u64;
    }
    Ok(())
}

/// Writes `cue` and the `.bin` beside it from a 44.1 kHz 16-bit stereo render of `range`.
pub fn write_cd_image(markers: &[Marker], range: TimeRange, wav: &Path, opts: &CdImage, cue: &Path) -> io::Result<()> {
    let total = wav_frames(wav)?;
    let layout = cd_layout(markers, range, total, opts);
    let bin = cue.with_extension("bin");
    let mut out = BufWriter::new(File::create(&bin)?);
    write_silence(&mut out, layout.lead_in)?;
    let mut written = 0;
    for_each_block(wav, |at, block| {
        for &(s, e) in &layout.spans {
            let (from, to) = (s.max(at), e.min(at + block.len() as u64 / 4));
            if from < to {
                out.write_all(&block[(from - at) as usize * 4..(to - at) as usize * 4])?;
                written += to - from;
            }
        }
        Ok(())
    })?;
    write_silence(&mut out, written.next_multiple_of(FRAME) - written)?;
    out.flush()?;

    let name = bin.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let quote = |s: &str| s.replace('"', "'");
    let mut sheet = format!("FILE \"{name}\" BINARY\r\n");
    for (i, t) in layout.tracks.iter().enumerate() {
        sheet += &format!("  TRACK {:02} AUDIO\r\n", i + 1);
        if let Some(title) = &t.title {
            sheet += &format!("    TITLE \"{}\"\r\n", quote(title));
        }
        for (key, value) in &t.info {
            sheet += &format!("    {key} \"{}\"\r\n", quote(value));
        }
        if i == 0 && layout.lead_in > 0 {
            sheet += "    INDEX 00 00:00:00\r\n";
            // The first track's index is the lead-in as asked for, not as padded.
            sheet += &format!("    INDEX 01 {}\r\n", msf_seconds(opts.lead_in_ms as f32 / 1000.0));
        } else {
            sheet += &format!("    INDEX 01 {}\r\n", msf(t.start));
        }
    }
    std::fs::write(cue, sheet)
}

/// `#title|KEY=value|…`: the first field without `=` is the title.
fn ddp_fields(body: &str) -> Vec<(String, String)> {
    body.split('|')
        .enumerate()
        .filter_map(|(i, field)| match field.split_once('=') {
            Some((k, v)) => Some((k.to_ascii_uppercase(), v.to_owned())),
            None => (i == 0).then(|| ("TITLE".to_owned(), field.to_owned())),
        })
        .collect()
}

fn set(fields: &mut Vec<(String, String)>, key: &str, value: String) {
    match fields.iter_mut().find(|(k, _)| k == key) {
        Some(f) => f.1 = value,
        None => fields.push((key.to_owned(), value)),
    }
}

fn get<'a>(fields: &'a [(String, String)], key: &str) -> Option<&'a str> {
    fields.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

#[derive(Debug, Clone, PartialEq)]
struct DdpTrack {
    /// Frame positions of INDEX 0 (when set) and INDEX 1.
    index0: Option<u64>,
    index1: u64,
    fields: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
struct DdpLayout {
    tracks: Vec<DdpTrack>,
    album: Vec<(String, String)>,
    /// Whether any `@` marker exists; CD-TEXT is written only then.
    has_album: bool,
    lead_out: u64,
}

/// Marker positions count from the project start even when the render starts later, as in
/// REAPER.
fn ddp_layout(markers: &[Marker], total: u64) -> Result<DdpLayout, String> {
    let mut sorted: Vec<&Marker> = markers.iter().filter(|m| m.region_end.is_none()).collect();
    sorted.sort_by(|a, b| a.position.total_cmp(&b.position));
    let length = total as f64 / RATE as f64;
    let frames = |rel: f64| (sample_at(rel).max(0) as u64).div_ceil(FRAME);
    let (mut album, mut has_album) = (Vec::new(), false);
    let mut tracks: Vec<DdpTrack> = Vec::new();
    let mut index0 = None;
    for m in sorted {
        let rel = m.position;
        let Some(kind) = m.name.chars().next() else { continue };
        let body = &m.name[kind.len_utf8()..];
        match kind {
            '@' => {
                has_album = true;
                for (k, v) in ddp_fields(body) {
                    let k = if matches!(k.as_str(), "CATALOG" | "EAN") { "UPC".to_owned() } else { k };
                    set(&mut album, &k, v);
                }
            }
            // Track and index markers well past the audio are left out.
            _ if rel > length + 10.0 => {}
            '!' => index0 = Some(frames(rel)),
            '#' => {
                let fields = ddp_fields(body);
                let at = frames(rel);
                let track = match tracks.last() {
                    // Red Book: the first track starts at INDEX 0 = 0 and INDEX 1 >= 2 s.
                    None => DdpTrack { index0: Some(0), index1: at.max(150), fields },
                    // At least 4 s from the previous INDEX 1, INDEX 1 a frame after INDEX 0.
                    Some(prev) => {
                        let earliest = prev.index1 + 300;
                        match index0.take() {
                            Some(i0) => {
                                let i0 = i0.max(earliest);
                                DdpTrack { index0: Some(i0), index1: at.max(i0 + 1), fields }
                            }
                            None => DdpTrack { index0: None, index1: at.max(earliest), fields },
                        }
                    }
                };
                index0 = None;
                tracks.push(track);
            }
            _ => {}
        }
    }
    let last = tracks.last().ok_or("DDP needs at least one track: a marker named #title")?;
    let lead_out = total.div_ceil(FRAME).max(last.index1 + 300);
    Ok(DdpLayout { tracks, album, has_album, lead_out })
}

fn pq_time(frames: u64) -> String {
    format!("{:02}{:02}{:02}", frames / 75 / 60, frames / 75 % 60, frames % 75)
}

fn pad(text: &str, width: usize) -> String {
    let mut s: String = text.chars().take(width).collect();
    while s.len() < width {
        s.push(' ');
    }
    s
}

fn pq_entry(track: &str, index: &str, frames: u64, isrc: &str, upc: &str) -> String {
    format!("VVVS{track}{index}  {}01  {}{}{}", pq_time(frames), pad(isrc, 12), pad(upc, 13), " ".repeat(19))
}

fn pq_descriptors(layout: &DdpLayout) -> String {
    let upc = get(&layout.album, "UPC").unwrap_or("");
    let mut pq = pq_entry("00", "00", 0, "", upc);
    for (n, t) in layout.tracks.iter().enumerate() {
        let n = format!("{:02}", n + 1);
        let mut isrc = get(&t.fields, "ISRC").unwrap_or("");
        if let Some(i0) = t.index0 {
            pq += &pq_entry(&n, "00", i0, isrc, "");
            isrc = "";
        }
        pq += &pq_entry(&n, "01", t.index1, isrc, "");
    }
    // REAPER lists the lead-out twice.
    let lead_out = pq_entry("AA", "01", layout.lead_out, "", upc);
    pq + &lead_out + &lead_out
}

/// CD-TEXT language codes (EBU Tech 3264) by REAPER's language names.
const LANGUAGES: &[(&str, u8)] = &[
    ("Albanian", 0x01),
    ("Breton", 0x02),
    ("Catalan", 0x03),
    ("Croatian", 0x04),
    ("Welsh", 0x05),
    ("Czech", 0x06),
    ("Danish", 0x07),
    ("German", 0x08),
    ("English", 0x09),
    ("Spanish", 0x0a),
    ("Esperanto", 0x0b),
    ("Estonian", 0x0c),
    ("Basque", 0x0d),
    ("Faroese", 0x0e),
    ("French", 0x0f),
    ("Frisian", 0x10),
    ("Irish", 0x11),
    ("Gaelic", 0x12),
    ("Galician", 0x13),
    ("Icelandic", 0x14),
    ("Italian", 0x15),
    ("Lappish", 0x16),
    ("Latvian", 0x18),
    ("Luxembourgian", 0x19),
    ("Lithuanian", 0x1a),
    ("Hungarian", 0x1b),
    ("Maltese", 0x1c),
    ("Dutch", 0x1d),
    ("Norwegian", 0x1e),
    ("Occitan", 0x1f),
    ("Polish", 0x20),
    ("Portugese", 0x21),
    ("Romanian", 0x22),
    ("Romansh", 0x23),
    ("Serbian", 0x24),
    ("Slovak", 0x25),
    ("Slovenian", 0x26),
    ("Finnish", 0x27),
    ("Swedish", 0x28),
    ("Turkish", 0x29),
    ("Flemish", 0x2a),
    ("Wallon", 0x2b),
    ("Zulu", 0x45),
    ("Vietnamese", 0x46),
    ("Uzbek", 0x47),
    ("Urdu", 0x48),
    ("Ukrainian", 0x49),
    ("Thai", 0x4a),
    ("Telugu", 0x4b),
    ("Tatar", 0x4c),
    ("Tamil", 0x4d),
    ("Tadzhik", 0x4e),
    ("Swahili", 0x4f),
    ("SrananTongo", 0x50),
    ("Somali", 0x51),
    ("Sinhalese", 0x52),
    ("Shona", 0x53),
    ("Serbo-croat", 0x54),
    ("Ruthenian", 0x55),
    ("Russian", 0x56),
    ("Quechua", 0x57),
    ("Pushtu", 0x58),
    ("Punjabi", 0x59),
    ("Persian", 0x5a),
    ("Papamiento", 0x5b),
    ("Oriya", 0x5c),
    ("Nepali", 0x5d),
    ("Ndebele", 0x5e),
    ("Marathi", 0x5f),
    ("Moldavian", 0x60),
    ("Malaysian", 0x61),
    ("Malagasay", 0x62),
    ("Macedonian", 0x63),
    ("Laotian", 0x64),
    ("Korean", 0x65),
    ("Khmer", 0x66),
    ("Kazakh", 0x67),
    ("Kannada", 0x68),
    ("Japanese", 0x69),
    ("Indonesian", 0x6a),
    ("Hindi", 0x6b),
    ("Hebrew", 0x6c),
    ("Hausa", 0x6d),
    ("Gurani", 0x6e),
    ("Gujurati", 0x6f),
    ("Greek", 0x70),
    ("Georgian", 0x71),
    ("Fulani", 0x72),
    ("Dari", 0x73),
    ("Churash", 0x74),
    ("Chinese", 0x75),
    ("Burmese", 0x76),
    ("Bulgarian", 0x77),
    ("Bengali", 0x78),
    ("Bielorussian", 0x79),
    ("Bambora", 0x7a),
    ("Azerbaijani", 0x7b),
    ("Assamese", 0x7c),
    ("Armenian", 0x7d),
    ("Arabic", 0x7e),
    ("Amharic", 0x7f),
];

/// REAPER's genre names; a genre's CD-TEXT code is its place in this list plus 2.
const GENRES: &[&str] = &[
    "Adult Contemporary",
    "Alternative Rock",
    "Childrens Music",
    "Classical",
    "Contemporary Christian",
    "Country",
    "Dance",
    "Easy Listening",
    "Erotic",
    "Folk",
    "Gospel",
    "Hip Hop",
    "Jazz",
    "Latin",
    "Musical",
    "New Age",
    "Opera",
    "Operetta",
    "Pop",
    "Rap",
    "Reggae",
    "Rock Music",
    "Rhythm & Blues",
    "Sound Effects",
    "Soundtrack",
    "Spoken Word",
    "World Music",
];

fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    !crc
}

/// CD-TEXT packs: 18 bytes each, a header naming the pack type, the track whose text the pack
/// starts in, the sequence number and the character position, twelve bytes of text and a CRC.
struct Packs {
    data: Vec<u8>,
    count: u8,
    per_type: [u8; 16],
}

impl Packs {
    fn push(&mut self, kind: u8, track: u8, position: u8, text: &[u8]) {
        let mut pack = [0u8; 18];
        pack[..4].copy_from_slice(&[kind, track, self.count, position.min(15)]);
        pack[4..4 + text.len()].copy_from_slice(text);
        let crc = crc16(&pack[..16]);
        pack[16..].copy_from_slice(&crc.to_be_bytes());
        self.data.extend_from_slice(&pack);
        self.count += 1;
        self.per_type[(kind - 0x80) as usize] += 1;
    }

    /// Strings for the album (track 0) and each track, each ending in NUL, cut into packs.
    /// `binary` leading bytes of the album string are not counted as characters.
    fn texts(&mut self, kind: u8, strings: &[Vec<u8>], binary: usize) {
        let mut stream: Vec<(u8, u8, u8)> = Vec::new();
        for (track, s) in strings.iter().enumerate() {
            let skip = if track == 0 { binary } else { 0 };
            for (pos, &b) in s.iter().chain(&[0]).enumerate() {
                stream.push((b, track as u8, pos.saturating_sub(skip).min(255) as u8));
            }
        }
        for chunk in stream.chunks(12) {
            let text: Vec<u8> = chunk.iter().map(|c| c.0).collect();
            self.push(kind, chunk[0].1, chunk[0].2, &text);
        }
    }
}

fn cd_text(layout: &DdpLayout) -> Option<Vec<u8>> {
    if !layout.has_album {
        return None;
    }
    let mut packs = Packs { data: Vec::new(), count: 0, per_type: [0; 16] };
    let album = &layout.album;
    let value = |fields: &[(String, String)], key: &str| get(fields, key).unwrap_or("").as_bytes().to_vec();
    // Titles are always written; other kinds only when the album sets them.
    for (kind, key) in [
        (0x80, "TITLE"),
        (0x81, "PERFORMER"),
        (0x82, "SONGWRITER"),
        (0x83, "COMPOSER"),
        (0x84, "ARRANGER"),
        (0x85, "MESSAGE"),
    ] {
        if kind != 0x80 && get(album, key).is_none_or(str::is_empty) {
            continue;
        }
        let strings: Vec<Vec<u8>> =
            std::iter::once(value(album, key)).chain(layout.tracks.iter().map(|t| value(&t.fields, key))).collect();
        packs.texts(kind, &strings, 0);
    }
    if let Some(id) = get(album, "IDENTIFICATION").filter(|v| !v.is_empty()) {
        packs.texts(0x86, &[id.as_bytes().to_vec()], 0);
    }
    if let Some(genre) = get(album, "GENRE").filter(|v| !v.is_empty()) {
        let code = GENRES.iter().position(|g| g.eq_ignore_ascii_case(genre)).map_or(0, |i| i as u16 + 2);
        let mut text = code.to_be_bytes().to_vec();
        text.extend_from_slice(genre.as_bytes());
        packs.texts(0x87, &[text], 2);
    }
    let msf = |f: u64| [(f / 75 / 60) as u8, (f / 75 % 60) as u8, (f % 75) as u8];
    let last = layout.tracks.len() as u8;
    let mut toc = vec![1, last, 0];
    toc.extend_from_slice(&msf(layout.lead_out));
    packs.push(0x88, 0, 0, &toc);
    for (i, chunk) in layout.tracks.chunks(4).enumerate() {
        let text: Vec<u8> = chunk.iter().flat_map(|t| msf(t.index1)).collect();
        packs.push(0x88, (i * 4 + 1) as u8, 0, &text);
    }
    let language = get(album, "LANGUAGE")
        .and_then(|l| LANGUAGES.iter().find(|(name, _)| name.eq_ignore_ascii_case(l)))
        .map_or(0, |(_, code)| *code);
    // Size information: three packs, counted in themselves.
    let mut counts = packs.per_type;
    counts[15] = 3;
    let last_seq = packs.count + 2;
    let mut info = vec![0, 1, last, 0];
    info.extend_from_slice(&counts);
    info.extend_from_slice(&[last_seq, 0, 0, 0, 0, 0, 0, 0]);
    info.extend_from_slice(&[language, 0, 0, 0, 0, 0, 0, 0]);
    for (i, chunk) in info.chunks(12).enumerate() {
        packs.push(0x8f, i as u8, 0, chunk);
    }
    Some(packs.data)
}

fn ms_entry(kind: &str, length: u64, start: Option<u64>, descriptor: &str, file: &str) -> String {
    let mut e = format!("VVVM{kind}        {length:08}");
    e += &match start {
        Some(s) => format!("{s:08}"),
        None => " ".repeat(8),
    };
    e += &pad(descriptor, 8);
    e += if start.is_some() { "DA70     150" } else { "            " };
    e += &" ".repeat(71 - e.len());
    e += "017";
    e += file;
    pad(&e, 128)
}

/// Writes a DDP fileset into the directory of `dat`: DDPID, DDPMS, PQDESCR, CDTEXT.BIN (with
/// album metadata), the audio as `dat` with its name in capitals, and checksum.md5.
pub fn write_ddp(markers: &[Marker], wav: &Path, dat: &Path) -> io::Result<()> {
    let total = wav_frames(wav)?;
    let layout = ddp_layout(markers, total).map_err(io::Error::other)?;
    let dir = dat.parent().map(Path::to_owned).unwrap_or_default();
    let dat_name = dat.file_name().map(|n| n.to_string_lossy().to_uppercase()).unwrap_or_default();
    let mut audio = BufWriter::new(File::create(dir.join(&dat_name))?);
    for_each_block(wav, |_, block| audio.write_all(block))?;
    write_silence(&mut audio, layout.lead_out * FRAME - total)?;
    audio.flush()?;
    drop(audio);

    let upc = get(&layout.album, "UPC").unwrap_or("");
    let ddpid = pad(&format!("DDP 2.00{}", pad(upc, 13)), 87) + &pad("CD", 41);
    let pq = pq_descriptors(&layout);
    let text = cd_text(&layout);
    let mut ms = ms_entry("S0", pq.len() as u64, None, "PQ DESCR", "PQDESCR");
    ms += &ms_entry("D0", layout.lead_out, Some(0), "", &dat_name);
    if let Some(t) = &text {
        ms += &ms_entry("S0", t.len() as u64, None, "CDTEXT", "CDTEXT.BIN");
    }
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for (name, bytes) in [("DDPID", ddpid.into_bytes()), ("DDPMS", ms.into_bytes()), ("PQDESCR", pq.into_bytes())]
        .into_iter()
        .chain(text.map(|t| ("CDTEXT.BIN", t)))
    {
        let path = dir.join(name);
        std::fs::write(&path, bytes)?;
        files.push((name.to_owned(), path));
    }
    files.push((dat_name.clone(), dir.join(&dat_name)));
    let mut sums = String::new();
    for (name, path) in files {
        let mut md5 = Md5::new();
        let mut f = File::open(path)?;
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            md5.update(&buf[..n]);
        }
        let hex: String = md5.finish().iter().map(|b| format!("{b:02x}")).collect();
        sums += &format!("{hex} *{name}\n");
    }
    std::fs::write(dir.join("checksum.md5"), sums)
}

/// MD5 (RFC 1321), for the fileset's checksum list.
struct Md5 {
    state: [u32; 4],
    buffer: Vec<u8>,
    length: u64,
}

impl Md5 {
    fn new() -> Self {
        Self { state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476], buffer: Vec::new(), length: 0 }
    }

    fn update(&mut self, data: &[u8]) {
        self.length += data.len() as u64;
        self.buffer.extend_from_slice(data);
        let whole = self.buffer.len() / 64 * 64;
        for block in self.buffer[..whole].as_chunks::<64>().0 {
            Self::compress(&mut self.state, block);
        }
        self.buffer.drain(..whole);
    }

    fn compress(state: &mut [u32; 4], block: &[u8; 64]) {
        const S: [u32; 16] = [7, 12, 17, 22, 5, 9, 14, 20, 4, 11, 16, 23, 6, 10, 15, 21];
        let m: Vec<u32> = block.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)).collect();
        let [mut a, mut b, mut c, mut d] = *state;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let k = ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32;
            let rotated = a.wrapping_add(f).wrapping_add(k).wrapping_add(m[g]).rotate_left(S[i / 16 * 4 + i % 4]);
            (a, d, c) = (d, c, b);
            b = b.wrapping_add(rotated);
        }
        for (s, v) in state.iter_mut().zip([a, b, c, d]) {
            *s = s.wrapping_add(v);
        }
    }

    fn finish(mut self) -> [u8; 16] {
        let bits = self.length.wrapping_mul(8);
        let mut tail = vec![0x80u8];
        tail.resize((55usize.wrapping_sub(self.buffer.len()) % 64) + 1, 0);
        tail.extend_from_slice(&bits.to_le_bytes());
        self.update(&tail);
        let mut out = [0u8; 16];
        for (o, s) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.state) {
            *o = s.to_le_bytes();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker(position: f64, name: &str) -> Marker {
        Marker { index: 0, position, name: name.into(), color: None, region_end: None, lines: Default::default() }
    }

    fn region(position: f64, end: f64, name: &str) -> Marker {
        Marker { region_end: Some(end), ..marker(position, name) }
    }

    const CD: CdImage = CdImage { tracks: CdTracks::Markers, lead_in_ms: 0, hash_markers_only: false };

    #[test]
    fn md5_matches_the_reference_digests() {
        let hex = |data: &[u8]| {
            let mut m = Md5::new();
            m.update(data);
            m.finish().iter().map(|b| format!("{b:02x}")).collect::<String>()
        };
        assert_eq!(hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(b"The quick brown fox jumps over the lazy dog"), "9e107d9d372bb6826bd81d3542a419d6");
        assert_eq!(hex(&[b'a'; 1000]), "cabe45dcc9ae5b66ba86600cca6b8ba8");
    }

    #[test]
    fn markers_start_tracks_at_least_four_seconds_apart() {
        // REAPER 7.73's tracks for these markers over an 18 s render.
        let markers = [
            marker(0.5, "PERF=Early"),
            marker(2.0, "A"),
            marker(5.9, "B"),
            marker(10.0, "C"),
            marker(12.0, "PERF=Someone"),
            marker(12.5, "isrc=XX1234567890"),
            marker(13.0, "WRIT=Writer"),
            marker(14.5, "D"),
            marker(17.0, "E"),
        ];
        let layout = cd_layout(&markers, TimeRange::new(0.0, 18.0), 18 * 44_100, &CD);
        let starts: Vec<String> = layout.tracks.iter().map(|t| msf(t.start)).collect();
        assert_eq!(starts, ["00:00:00", "00:05:67", "00:10:00", "00:14:37"]);
        assert_eq!(layout.tracks[0].title.as_deref(), Some("A"));
        assert_eq!(layout.tracks[0].info, [("PERFORMER", "Early".to_owned())]);
        let keys: Vec<&str> = layout.tracks[2].info.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, ["PERFORMER", "ISRC", "SONGWRITER"]);
        // Rendering from 1 s: the marker at the start names the first track.
        let layout = cd_layout(&markers, TimeRange::new(1.0, 17.3), 16 * 44_100, &CD);
        assert_eq!(layout.tracks.len(), 4);
        assert!(layout.tracks[0].info.is_empty());
    }

    #[test]
    fn regions_are_joined_and_overlaps_start_late() {
        let markers = [
            region(1.0, 6.5, "#Reg A"),
            region(5.0, 9.0, "Overlap"),
            region(10.0, 12.0, "Reg C"),
            region(15.0, 20.0, "Past end"),
        ];
        let opts = CdImage { tracks: CdTracks::Regions, ..CD };
        let layout = cd_layout(&markers, TimeRange::new(0.0, 18.0), 18 * 44_100, &opts);
        assert_eq!(layout.spans, [(44_100, 396_900), (441_000, 529_200), (661_500, 793_800)]);
        let tracks: Vec<(String, Option<&str>)> =
            layout.tracks.iter().map(|t| (msf(t.start), t.title.as_deref())).collect();
        assert_eq!(
            tracks,
            [
                ("00:00:00".into(), Some("#Reg A")),
                ("00:05:37".into(), Some("Overlap")),
                ("00:10:00".into(), Some("Reg C"))
            ]
        );
    }

    #[test]
    fn ddp_tracks_follow_the_red_book() {
        // REAPER 7.73's PQ descriptors for these markers over an 18 s render.
        let markers = [
            marker(0.0, "#First|ISRC=USABC1234567|PERFORMER=Jane"),
            marker(5.3, "#TITLE=Second track|ISRC=USABC1234568"),
            marker(7.1, "!"),
            marker(8.2, "!"),
            marker(10.0, "#Third"),
            marker(11.0, "plain marker"),
            marker(12.5, "#Fourth"),
            marker(16.0, "#Fifth \"quoted\""),
            marker(17.5, "@My album|PERFORMER=Band|EAN=0123456789012|GENRE=Pop|LANGUAGE=English"),
        ];
        let layout = ddp_layout(&markers, 18 * 44_100).unwrap();
        let pq = pq_descriptors(&layout);
        let entries: Vec<&str> = (0..pq.len() / 64).map(|i| pq[i * 64..i * 64 + 18].trim_end()).collect();
        assert_eq!(
            entries,
            [
                "VVVS0000  00000001",
                "VVVS0100  00000001",
                "VVVS0101  00020001",
                "VVVS0201  00060001",
                "VVVS0300  00100001",
                "VVVS0301  00100101",
                "VVVS0401  00140101",
                "VVVS0501  00180101",
                "VVVSAA01  00220101",
                "VVVSAA01  00220101",
            ]
        );
        assert_eq!(&pq[64 + 20..64 + 32], "USABC1234567");
        assert_eq!(&pq[32..45], "0123456789012");
        let text = cd_text(&layout).unwrap();
        assert_eq!(text.len(), 14 * 18);
        assert_eq!(&text[..18], b"\x80\x00\x00\x00My album\x00Fir\xe5\x3e");
    }

    #[test]
    fn ddp_needs_a_track() {
        assert!(ddp_layout(&[marker(1.0, "@Album")], 5 * 44_100).is_err());
    }
}
