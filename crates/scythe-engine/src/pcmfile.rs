//! The files of REAPER's PCM sinks: WAV with the chunks REAPER adds (Broadcast Wave `bext`,
//! markers as `cue `/`smpl`/`adtl`, ACID tempo, render metadata) and its Wave64, RF64 and BW64
//! forms, AIFF and CAF. Samples are streamed; the header is rewritten in place at the end.

use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use scythe_core::render_settings::{LargeWav, MarkerExport};
use scythe_core::{Marker, TimeRange};

/// Sony Wave64 chunk ids: a FOURCC followed by this GUID tail (`riff` has its own).
const W64_TAIL: [u8; 12] = [0xf3, 0xac, 0xd3, 0x11, 0x8c, 0xd1, 0x00, 0xc0, 0x4f, 0x8e, 0xdb, 0x8a];
const W64_RIFF: [u8; 16] =
    [b'r', b'i', b'f', b'f', 0x2e, 0x91, 0xcf, 0x11, 0xa5, 0xd6, 0x28, 0xdb, 0x04, 0xc1, 0x00, 0x00];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Wav(LargeWav),
    Aiff,
    Caf,
}

/// How samples are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sample {
    /// Integers of this many bits (8-bit WAV and CAF are unsigned, AIFF's signed).
    Int(u16),
    Float32,
    Float64,
    /// G.711 u-law in 8 bits.
    ULaw,
    /// Blocks written by the caller: the `fmt ` tag, bits, block size and frames per block.
    Adpcm {
        tag: u16,
        bits: u16,
        align: u16,
        frames_per_block: u16,
    },
}

impl Sample {
    fn bytes(self) -> u16 {
        match self {
            Self::Int(bits) => bits / 8,
            Self::Float32 => 4,
            Self::Float64 => 8,
            Self::ULaw => 1,
            Self::Adpcm { .. } => 0,
        }
    }
}

/// A chunk written as is: its id and body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub id: [u8; 4],
    pub body: Vec<u8>,
}

impl Chunk {
    pub fn new(id: &[u8; 4], body: Vec<u8>) -> Self {
        Self { id: *id, body }
    }
}

/// A marker or region in render frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cue {
    pub start: u64,
    /// The first frame after a region.
    pub end: Option<u64>,
    pub name: String,
}

/// What besides samples goes into the file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extras {
    /// Chunks between `fmt ` and the samples, in order (WAV: `bext`, `acid`, metadata; AIFF and
    /// CAF: their own).
    pub head: Vec<Chunk>,
    pub cues: Vec<Cue>,
    /// Chunks after the samples and markers (WAV: `LIST`/`INFO`).
    pub tail: Vec<Chunk>,
}

pub struct PcmWriter {
    out: BufWriter<File>,
    container: Container,
    sample: Sample,
    channels: u16,
    rate: u32,
    extras: Extras,
    /// Where the samples start, and how much the header took when written.
    data_at: u64,
    data_len: u64,
    frames: u64,
}

impl PcmWriter {
    pub fn create(
        path: &Path,
        container: Container,
        sample: Sample,
        channels: u16,
        rate: u32,
        extras: Extras,
    ) -> io::Result<Self> {
        let mut w = Self {
            out: BufWriter::with_capacity(1 << 16, File::create(path)?),
            container,
            sample,
            channels,
            rate,
            extras,
            data_at: 0,
            data_len: 0,
            frames: 0,
        };
        let header = w.header(container);
        w.out.write_all(&header)?;
        w.data_at = header.len() as u64;
        Ok(w)
    }

    /// An integer sample already scaled to the bit depth.
    pub fn int(&mut self, v: i32) -> io::Result<()> {
        let Sample::Int(bits) = self.sample else { unreachable!("not an integer format") };
        let be = self.container == Container::Aiff;
        match bits {
            8 if be => self.out.write_all(&[v as i8 as u8])?,
            8 => self.out.write_all(&[(v + 128) as u8])?,
            16 if be => self.out.write_all(&(v as i16).to_be_bytes())?,
            16 => self.out.write_all(&(v as i16).to_le_bytes())?,
            24 if be => self.out.write_all(&v.to_be_bytes()[1..])?,
            24 => self.out.write_all(&v.to_le_bytes()[..3])?,
            _ if be => self.out.write_all(&v.to_be_bytes())?,
            _ => self.out.write_all(&v.to_le_bytes())?,
        }
        self.data_len += bits as u64 / 8;
        Ok(())
    }

    /// A sample in the file's floating-point or u-law format.
    pub fn float(&mut self, s: f64) -> io::Result<()> {
        match self.sample {
            Sample::Float32 => self.out.write_all(&(s as f32).to_le_bytes())?,
            Sample::Float64 => self.out.write_all(&s.to_le_bytes())?,
            Sample::ULaw => self.out.write_all(&[ulaw(s)])?,
            _ => unreachable!("not a floating-point format"),
        }
        self.data_len += self.sample.bytes() as u64;
        Ok(())
    }

    /// Encoded blocks holding `frames` frames.
    pub fn raw(&mut self, data: &[u8], frames: u64) -> io::Result<()> {
        self.out.write_all(data)?;
        self.data_len += data.len() as u64;
        self.frames += frames;
        Ok(())
    }

    pub fn finalize(mut self) -> io::Result<()> {
        if !matches!(self.sample, Sample::Adpcm { .. }) {
            self.frames = self.data_len / (self.sample.bytes() as u64 * self.channels.max(1) as u64).max(1);
        }
        let container = match self.container {
            Container::Wav(large) => Container::Wav(self.grown(large)),
            c => c,
        };
        if self.data_len % 2 == 1 && container != Container::Caf {
            self.out.write_all(&[0])?;
        }
        let tail = self.tail(container);
        self.out.write_all(&tail)?;
        self.out.flush()?;
        let len = self.out.stream_position()?;
        let header = self.header_sized(container, len);
        debug_assert_eq!(header.len() as u64, self.data_at, "the header keeps its size");
        let file = self.out.get_mut();
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header)?;
        file.flush()
    }

    /// Plain WAV files past 4 GB turn into the form their room was left for.
    fn grown(&self, large: LargeWav) -> LargeWav {
        let len = self.data_at + self.data_len + (self.data_len & 1) + self.tail(Container::Wav(large)).len() as u64;
        if len - 8 <= u32::MAX as u64 {
            return large;
        }
        let grown = match large {
            LargeWav::AutoWave64 => LargeWav::Wave64,
            LargeWav::AutoRf64 => LargeWav::Rf64,
            LargeWav::AutoBw64 => LargeWav::Bw64,
            other => other,
        };
        // The room left is only sized for `fmt `, `fact` and `bext`; other chunks can make the
        // grown header longer than the room, and the file then stays as it is.
        if self.header(Container::Wav(grown)).len() as u64 == self.data_at { grown } else { large }
    }

    fn header(&self, container: Container) -> Vec<u8> {
        self.header_sized(container, 0)
    }

    /// The header for a file of `len` bytes.
    fn header_sized(&self, container: Container, len: u64) -> Vec<u8> {
        match container {
            Container::Wav(LargeWav::Wave64) => self.w64_header(len),
            Container::Wav(large) => self.wav_header(large, len),
            Container::Aiff => self.aiff_header(len),
            Container::Caf => self.caf_header(),
        }
    }

    fn fmt(&self) -> Vec<u8> {
        let (tag, bits, align, extra): (u16, u16, u16, Option<Vec<u8>>) = match self.sample {
            Sample::Int(bits) => (1, bits, self.channels * bits / 8, None),
            Sample::Float32 => (3, 32, self.channels * 4, Some(vec![])),
            Sample::Float64 => (3, 64, self.channels * 8, Some(vec![])),
            Sample::ULaw => (7, 8, self.channels, Some(vec![])),
            Sample::Adpcm { tag, bits, align, frames_per_block } => {
                (tag, bits, align, Some(frames_per_block.to_le_bytes().to_vec()))
            }
        };
        let byte_rate = match self.sample {
            Sample::Adpcm { bits, .. } => self.rate * bits as u32 * self.channels as u32 / 8,
            _ => self.rate * align as u32,
        };
        let mut fmt = Vec::with_capacity(20);
        fmt.extend_from_slice(&tag.to_le_bytes());
        fmt.extend_from_slice(&self.channels.to_le_bytes());
        fmt.extend_from_slice(&self.rate.to_le_bytes());
        fmt.extend_from_slice(&byte_rate.to_le_bytes());
        fmt.extend_from_slice(&align.to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
        if let Some(extra) = extra {
            fmt.extend_from_slice(&(extra.len() as u16).to_le_bytes());
            fmt.extend_from_slice(&extra);
        }
        fmt
    }

    fn has_fact(&self) -> bool {
        matches!(self.sample, Sample::Adpcm { .. })
    }

    fn has_bext(&self) -> bool {
        self.extras.head.iter().any(|c| &c.id == b"bext")
    }

    fn wav_header(&self, large: LargeWav, len: u64) -> Vec<u8> {
        let rf64 = matches!(large, LargeWav::Rf64 | LargeWav::Bw64);
        let big = len.saturating_sub(8) > u32::MAX as u64;
        let mut h = Vec::with_capacity(1024);
        h.extend_from_slice(match large {
            LargeWav::Rf64 => b"RF64",
            LargeWav::Bw64 => b"BW64",
            _ => b"RIFF",
        });
        h.extend_from_slice(&(if big { u32::MAX } else { len.saturating_sub(8) as u32 }).to_le_bytes());
        h.extend_from_slice(b"WAVE");
        if rf64 {
            let mut ds64 = Vec::with_capacity(28);
            ds64.extend_from_slice(&len.saturating_sub(8).to_le_bytes());
            ds64.extend_from_slice(&self.data_len.to_le_bytes());
            ds64.extend_from_slice(&self.frames.to_le_bytes());
            ds64.extend_from_slice(&0u32.to_le_bytes());
            riff_chunk(&mut h, b"ds64", &ds64);
        }
        riff_chunk(&mut h, b"fmt ", &self.fmt());
        if self.has_fact() {
            riff_chunk(&mut h, b"fact", &(self.frames as u32).to_le_bytes());
        }
        for c in &self.extras.head {
            riff_chunk(&mut h, &c.id, &c.body);
        }
        let junk = match large {
            LargeWav::AutoWave64 => Some(self.w64_room()),
            LargeWav::AutoRf64 | LargeWav::AutoBw64 => Some(28),
            _ => None,
        };
        if let Some(n) = junk {
            riff_chunk(&mut h, b"junk", &vec![0; n]);
        }
        h.extend_from_slice(b"data");
        h.extend_from_slice(
            &(if self.data_len > u32::MAX as u64 { u32::MAX } else { self.data_len as u32 }).to_le_bytes(),
        );
        h
    }

    /// The `junk` that makes room for the Wave64 header: the difference between the two forms of
    /// `fmt `, `fact` and `bext` (other chunks are left out of the count, as REAPER leaves them).
    fn w64_room(&self) -> usize {
        let fmt = self.fmt().len();
        let w64 = 40
            + (24 + fmt).next_multiple_of(8)
            + if self.has_fact() { 32 } else { 0 }
            + if self.has_bext() { 632 } else { 0 }
            + 24;
        let wav = 12
            + 8
            + fmt.next_multiple_of(2)
            + if self.has_fact() { 12 } else { 0 }
            + if self.has_bext() { 610 } else { 0 }
            + 8;
        w64 - wav - 8
    }

    fn w64_header(&self, len: u64) -> Vec<u8> {
        let mut h = Vec::with_capacity(1024);
        h.extend_from_slice(&W64_RIFF);
        h.extend_from_slice(&len.to_le_bytes());
        h.extend_from_slice(b"wave");
        h.extend_from_slice(&W64_TAIL);
        w64_chunk(&mut h, b"fmt ", &self.fmt());
        if self.has_fact() {
            w64_chunk(&mut h, b"fact", &self.frames.to_le_bytes());
        }
        // REAPER's Wave64 ACID chunk is not a well-formed chunk; it is left out.
        for c in self.extras.head.iter().filter(|c| &c.id != b"acid") {
            w64_chunk(&mut h, &c.id, &c.body);
        }
        h.extend_from_slice(b"data");
        h.extend_from_slice(&W64_TAIL);
        h.extend_from_slice(&(self.data_len + 24).to_le_bytes());
        h
    }

    fn aiff_header(&self, len: u64) -> Vec<u8> {
        let Sample::Int(bits) = self.sample else { unreachable!("AIFF stores integers") };
        let mut h = Vec::with_capacity(256);
        h.extend_from_slice(b"FORM");
        h.extend_from_slice(&(len.saturating_sub(8) as u32).to_be_bytes());
        h.extend_from_slice(b"AIFF");
        let mut comm = Vec::with_capacity(18);
        comm.extend_from_slice(&self.channels.to_be_bytes());
        comm.extend_from_slice(&(self.frames as u32).to_be_bytes());
        comm.extend_from_slice(&bits.to_be_bytes());
        comm.extend_from_slice(&extended(self.rate));
        aiff_chunk(&mut h, b"COMM", &comm);
        for c in &self.extras.head {
            aiff_chunk(&mut h, &c.id, &c.body);
        }
        h.extend_from_slice(b"SSND");
        h.extend_from_slice(&((self.data_len + 8) as u32).to_be_bytes());
        h.extend_from_slice(&[0; 8]);
        h
    }

    fn caf_header(&self) -> Vec<u8> {
        let mut h = Vec::with_capacity(256);
        h.extend_from_slice(b"caff\x00\x01\x00\x00");
        let mut desc = Vec::with_capacity(32);
        desc.extend_from_slice(&(self.rate as f64).to_be_bytes());
        desc.extend_from_slice(b"lpcm");
        let (flags, bits) = match self.sample {
            Sample::Int(bits) => (2u32, bits),
            Sample::Float32 => (3, 32),
            Sample::Float64 => (3, 64),
            _ => unreachable!("CAF stores PCM"),
        };
        desc.extend_from_slice(&flags.to_be_bytes());
        desc.extend_from_slice(&(self.channels as u32 * bits as u32 / 8).to_be_bytes());
        desc.extend_from_slice(&1u32.to_be_bytes());
        desc.extend_from_slice(&(self.channels as u32).to_be_bytes());
        desc.extend_from_slice(&(bits as u32).to_be_bytes());
        caf_chunk(&mut h, b"desc", &desc);
        for c in &self.extras.head {
            caf_chunk(&mut h, &c.id, &c.body);
        }
        h.extend_from_slice(b"data");
        h.extend_from_slice(&(self.data_len + 4).to_be_bytes());
        h.extend_from_slice(&[0; 4]);
        h
    }

    fn tail(&self, container: Container) -> Vec<u8> {
        let cues = &self.extras.cues;
        let mut t = Vec::new();
        match container {
            Container::Wav(LargeWav::Wave64) => {
                if !cues.is_empty() {
                    // Unlike the other chunks, REAPER counts the padding in this one's size.
                    let body = w64_cues(cues);
                    let padding = vec![0; (body.len() + 24).next_multiple_of(8) - 24 - body.len()];
                    w64_chunk(&mut t, b"cue ", &[body, padding].concat());
                }
            }
            Container::Wav(large) => {
                if !cues.is_empty() {
                    for c in wav_cues(cues) {
                        riff_chunk(&mut t, &c.id, &c.body);
                    }
                    if matches!(large, LargeWav::Rf64 | LargeWav::Bw64) {
                        riff_chunk(&mut t, b"r64m", &r64m(cues));
                    }
                }
                for c in &self.extras.tail {
                    riff_chunk(&mut t, &c.id, &c.body);
                }
            }
            Container::Aiff => {}
            Container::Caf => {
                for c in caf_cues(cues) {
                    caf_chunk(&mut t, &c.id, &c.body);
                }
            }
        }
        t
    }
}

fn riff_chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
}

fn aiff_chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
}

fn caf_chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(body.len() as u64).to_be_bytes());
    out.extend_from_slice(body);
}

fn w64_chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&W64_TAIL);
    out.extend_from_slice(&(body.len() as u64 + 24).to_le_bytes());
    out.extend_from_slice(body);
    out.resize(out.len().next_multiple_of(8), 0);
}

/// The 80-bit extended float of an AIFF sample rate.
fn extended(rate: u32) -> [u8; 10] {
    let mut out = [0u8; 10];
    if rate == 0 {
        return out;
    }
    let shift = rate.leading_zeros();
    let exponent = 16383 + 31 - shift as u16;
    out[..2].copy_from_slice(&exponent.to_be_bytes());
    out[2..6].copy_from_slice(&(rate << shift).to_be_bytes());
    out
}

/// REAPER's u-law: G.711 of the sample in 16-bit units plus 2, rounded down, negative values
/// taking the one's complement.
fn ulaw(s: f64) -> u8 {
    let i = (s * 32768.0 + 2.0).floor().clamp(-1e9, 1e9) as i64;
    let (sign, magnitude) = if i >= 0 { (0u8, i) } else { (0x80, -i - 1) };
    let v = (magnitude.min(32635) + 132) as u32;
    let exponent = (31 - v.leading_zeros() - 7) as u8;
    let mantissa = ((v >> (exponent + 3)) & 15) as u8;
    !(sign | exponent << 4 | mantissa)
}

/// The markers and regions of `range` a render embeds, in frames from its start: markers at
/// or after the start and before the end, regions wholly inside, in project order.
pub fn collect_cues(markers: &[Marker], export: MarkerExport, range: TimeRange, rate: u32) -> Vec<Cue> {
    let frame = |t: f64| (t * rate as f64).round() as i64;
    let start = frame(range.start);
    let total = (range.length() * rate as f64).round() as i64;
    let mut cues: Vec<(i64, Cue)> = markers
        .iter()
        .filter(|m| !export.hash_only || m.name.starts_with('#'))
        .filter_map(|m| {
            let at = frame(m.position) - start;
            match m.region_end {
                None if export.markers && (0..total).contains(&at) => {
                    Some((at, Cue { start: at as u64, end: None, name: m.name.clone() }))
                }
                Some(end) if export.regions => {
                    let end = frame(end) - start;
                    (at >= 0 && end <= total)
                        .then(|| (at, Cue { start: at as u64, end: Some(end.max(at) as u64), name: m.name.clone() }))
                }
                _ => None,
            }
        })
        .collect();
    cues.sort_by_key(|(at, _)| *at);
    cues.into_iter().map(|(_, c)| c).collect()
}

/// `cue `, `smpl` (for regions) and `LIST`/`adtl` with a `labl` per cue.
fn wav_cues(cues: &[Cue]) -> Vec<Chunk> {
    let mut cue = (cues.len() as u32).to_le_bytes().to_vec();
    let mut loops = Vec::new();
    let mut labels = b"adtl".to_vec();
    for (i, c) in cues.iter().enumerate() {
        let id = i as u32 + 1;
        for v in [id, c.start as u32] {
            cue.extend_from_slice(&v.to_le_bytes());
        }
        cue.extend_from_slice(b"data");
        for v in [0u32, 0, c.start as u32] {
            cue.extend_from_slice(&v.to_le_bytes());
        }
        if let Some(end) = c.end {
            for v in [id, 0, c.start as u32, (end.saturating_sub(1)).max(c.start) as u32, 0, 0] {
                loops.extend_from_slice(&v.to_le_bytes());
            }
        }
        let mut labl = id.to_le_bytes().to_vec();
        labl.extend_from_slice(c.name.as_bytes());
        labl.push(0);
        riff_chunk(&mut labels, b"labl", &labl);
    }
    let mut out = vec![Chunk::new(b"cue ", cue)];
    if !loops.is_empty() {
        let mut smpl = vec![0u8; 36];
        smpl[28..32].copy_from_slice(&((loops.len() / 24) as u32).to_le_bytes());
        smpl.extend_from_slice(&loops);
        out.push(Chunk::new(b"smpl", smpl));
    }
    out.push(Chunk::new(b"LIST", labels));
    out
}

/// RF64's marker chunk: 320-byte entries, a region adding one for its end.
fn r64m(cues: &[Cue]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut entry = |flags: u32, at: u64, name: &str, id: u32| {
        let mut e = [0u8; 320];
        e[..4].copy_from_slice(&flags.to_le_bytes());
        e[4..12].copy_from_slice(&at.to_le_bytes());
        let name = name.as_bytes();
        let n = name.len().min(255);
        e[28..28 + n].copy_from_slice(&name[..n]);
        e[284..288].copy_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&e);
    };
    for (i, c) in cues.iter().enumerate() {
        entry(0x11, c.start, &c.name, i as u32 + 1);
        if let Some(end) = c.end {
            entry(0, end, "", 0xdead_beef);
        }
    }
    out
}

/// Wave64's own `cue ` chunk: 64-bit positions and lengths, names in UTF-16.
fn w64_cues(cues: &[Cue]) -> Vec<u8> {
    let mut out = (cues.len() as u32).to_le_bytes().to_vec();
    for (i, c) in cues.iter().enumerate() {
        let name: Vec<u8> = c.name.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
        let len = c.end.map_or(u64::MAX, |e| e - c.start);
        for v in [i as u64 + 1, c.start, len, name.len() as u64] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&name);
    }
    out
}

/// CAF `mark`, `regn` and `strg`: markers numbered first, then each region and its two
/// markers. REAPER stores frame positions as integers where CAF has a float.
fn caf_cues(cues: &[Cue]) -> Vec<Chunk> {
    let mut strings: Vec<(u32, &str)> = Vec::new();
    let marker = |out: &mut Vec<u8>, at: u64, id: u32| {
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&at.to_be_bytes());
        out.extend_from_slice(&id.to_be_bytes());
        out.extend_from_slice(&[0xff; 8]);
        out.extend_from_slice(&0u32.to_be_bytes());
    };
    let mut id = 0u32;
    let mut marks = Vec::new();
    let mut count = 0u32;
    for c in cues.iter().filter(|c| c.end.is_none()) {
        id += 1;
        marker(&mut marks, c.start, id);
        strings.push((id, &c.name));
        count += 1;
    }
    let mut regions = Vec::new();
    let mut region_count = 0u32;
    for c in cues.iter().filter(|c| c.end.is_some()) {
        id += 1;
        regions.extend_from_slice(&id.to_be_bytes());
        regions.extend_from_slice(&0u32.to_be_bytes());
        regions.extend_from_slice(&2u32.to_be_bytes());
        strings.push((id, &c.name));
        for at in [c.start, c.end.unwrap_or(c.start)] {
            id += 1;
            marker(&mut regions, at, id);
            strings.push((id, &c.name));
        }
        region_count += 1;
    }
    let mut out = Vec::new();
    if count > 0 {
        let mut body = 0u32.to_be_bytes().to_vec();
        body.extend_from_slice(&count.to_be_bytes());
        body.extend_from_slice(&marks);
        out.push(Chunk::new(b"mark", body));
    }
    if region_count > 0 {
        let mut body = 0u32.to_be_bytes().to_vec();
        body.extend_from_slice(&region_count.to_be_bytes());
        body.extend_from_slice(&regions);
        out.push(Chunk::new(b"regn", body));
    }
    if !strings.is_empty() {
        let mut body = (strings.len() as u32).to_be_bytes().to_vec();
        let mut text = Vec::new();
        for (id, s) in &strings {
            body.extend_from_slice(&id.to_be_bytes());
            body.extend_from_slice(&(text.len() as u64).to_be_bytes());
            text.extend_from_slice(s.as_bytes());
            text.push(0);
        }
        body.extend_from_slice(&text);
        out.push(Chunk::new(b"strg", body));
    }
    out
}

/// What a render embeds besides its samples.
#[derive(Debug, Clone, Default)]
pub struct Embed<'a> {
    pub project: Option<&'a Path>,
    /// `<RENDER_METADATA` tags, when the render embeds them.
    pub metadata: &'a [(String, String)],
    /// The render's first frame on the project timeline (`bext` time reference).
    pub start_frame: u64,
    /// Project tempo at the start, and the render's length in beats.
    pub bpm: f64,
    pub beats: f64,
    pub cues: Vec<Cue>,
    /// Local date and time of the render, as `bext` writes them by default.
    pub date: String,
    pub time: String,
}

impl<'a> Embed<'a> {
    /// The tags of one scheme (`BWF`, `IXML` …) without its prefix, sorted as REAPER keeps them.
    fn tags(&self, scheme: &str) -> std::vec::IntoIter<(&'a str, &'a str)> {
        let metadata: &'a [(String, String)] = self.metadata;
        let mut tags: Vec<(&str, &str)> = metadata
            .iter()
            .filter_map(|(k, v)| Some((k.strip_prefix(scheme)?.strip_prefix(':')?, v.as_str())))
            .collect();
        tags.sort_by(|a, b| a.0.cmp(b.0));
        tags.into_iter()
    }

    fn tag(&self, scheme: &str, key: &str) -> Option<&'a str> {
        self.tags(scheme).find(|(k, _)| *k == key).map(|(_, v)| v)
    }

    /// The beat count when the render is a whole number of beats.
    fn whole_beats(&self) -> Option<u32> {
        let b = self.beats.round();
        ((self.beats - b).abs() < 1e-6 && b >= 1.0).then_some(b as u32)
    }

    fn acid(&self) -> Chunk {
        let mut b = Vec::with_capacity(24);
        // Flags, root note, two unknowns and the beat count (REAPER leaves it 0), then 4/4
        // whatever the project's time signature.
        b.extend_from_slice(&8u32.to_le_bytes());
        b.extend_from_slice(&[0; 12]);
        b.extend_from_slice(&4u16.to_le_bytes());
        b.extend_from_slice(&4u16.to_le_bytes());
        b.extend_from_slice(&(self.bpm as f32).to_le_bytes());
        Chunk::new(b"acid", b)
    }

    fn xmp(&self) -> Option<String> {
        let tags: Vec<(&str, &str)> = self.tags("XMP").collect();
        if tags.is_empty() {
            return None;
        }
        let name = |key: &str| {
            let (ns, name) = key.split_once('/').unwrap_or(("dc", key));
            (if ns == "dm" { "xmpDM" } else { ns }.to_owned(), name.to_owned())
        };
        let alt = |key: &str| matches!(key, "dc/title" | "dc/description" | "dc/rights");
        let mut s = String::from(concat!(
            "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\">",
            "<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description ",
            "xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:xmpDM=\"http://ns.adobe.com/xmp/1.0/DynamicMedia/\"",
        ));
        for (k, v) in tags.iter().filter(|(k, _)| !alt(k)) {
            let (ns, n) = name(k);
            s += &format!(" {ns}:{n}=\"{}\"", xml_escape(v));
        }
        s.push('>');
        for (k, v) in tags.iter().filter(|(k, _)| alt(k)) {
            let (ns, n) = name(k);
            s += &format!(
                "<{ns}:{n}><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></{ns}:{n}>",
                xml_escape(v)
            );
        }
        s += "</rdf:Description></rdf:RDF></x:xmpmeta><?xpacket end=\"w\"?>";
        Some(s)
    }

    fn id3(&self) -> Option<Vec<u8>> {
        let mut frames = Vec::new();
        for (key, value) in self.tags("ID3") {
            let (id, desc) = key.split_once(':').unwrap_or((key, ""));
            if id.len() != 4 || !(id.starts_with('T') || id == "COMM") {
                continue;
            }
            let mut body = vec![3u8];
            match id {
                "COMM" => body.extend_from_slice(b"XXX\0"),
                "TXXX" => {
                    body.extend_from_slice(desc.as_bytes());
                    body.push(0);
                }
                _ => {}
            }
            body.extend_from_slice(value.as_bytes());
            frames.extend_from_slice(id.as_bytes());
            frames.extend_from_slice(&synchsafe(body.len() as u32));
            frames.extend_from_slice(&[0, 0]);
            frames.extend_from_slice(&body);
        }
        if frames.is_empty() {
            return None;
        }
        let mut tag = b"ID3\x04\x00\x00".to_vec();
        tag.extend_from_slice(&synchsafe(frames.len() as u32));
        tag.extend_from_slice(&frames);
        Some(padded(tag))
    }

    fn cart(&self) -> Option<Vec<u8>> {
        let tags: Vec<(&str, &str)> = self.tags("CART").collect();
        if tags.is_empty() {
            return None;
        }
        let mut b = vec![0u8; 2048];
        b[..4].copy_from_slice(b"0101");
        let fields: [(&str, usize, usize); 16] = [
            ("Title", 4, 64),
            ("Artist", 68, 64),
            ("CutID", 132, 64),
            ("ClientID", 196, 64),
            ("Category", 260, 64),
            ("Classification", 324, 64),
            ("OutCue", 388, 64),
            ("StartDate", 452, 10),
            ("StartTime", 462, 8),
            ("EndDate", 470, 10),
            ("EndTime", 480, 8),
            ("ProducerAppID", 488, 64),
            ("ProducerAppVersion", 552, 64),
            ("UserDef", 616, 64),
            ("URL", 1024, 1024),
            ("LevelReference", 680, 4),
        ];
        for (key, value) in &tags {
            let Some(&(_, at, len)) = fields.iter().find(|f| f.0 == *key) else { continue };
            if *key == "LevelReference" {
                b[at..at + 4].copy_from_slice(&value.trim().parse::<i32>().unwrap_or(0).to_le_bytes());
            } else {
                let v = value.as_bytes();
                let n = v.len().min(len);
                b[at..at + n].copy_from_slice(&v[..n]);
            }
        }
        if let Some((_, text)) = tags.iter().find(|(k, _)| *k == "TagText") {
            b.extend_from_slice(text.as_bytes());
            b.push(0);
            b = padded(b);
        }
        Some(b)
    }

    fn axml(&self) -> Option<String> {
        let isrc = self.tag("AXML", "ISRC")?;
        Some(format!(
            concat!(
                "<ebucore:ebuCoreMain xmlns:dc=\"http://purl.org/dc/elements/1.1/\" ",
                "xmlns=\"urn:ebu:metadata-schema:ebuCore_2012\"><ebucore:coreMetadata><ebucore:identifier ",
                "typeLabel=\"GUID\" typeDefinition=\"Globally Unique Identifier\" formatLabel=\"ISRC\" ",
                "formatDefinition=\"International Standard Recording Code\" ",
                "formatLink=\"http://www.ebu.ch/metadata/cs/ebu_IdentifierTypeCodeCS.xml#3.7\">",
                "<dc:identifier>ISRC:{}</dc:identifier></ebucore:identifier></ebucore:coreMetadata>",
                "</ebucore:ebuCoreMain>"
            ),
            xml_escape(isrc)
        ))
    }

    fn info(&self) -> Option<Chunk> {
        let mut b = b"INFO".to_vec();
        for (key, value) in self.tags("INFO").filter(|(k, _)| k.len() == 4) {
            let mut v = value.as_bytes().to_vec();
            v.push(0);
            riff_chunk(&mut b, &key.as_bytes().try_into().unwrap(), &padded(v));
        }
        (b.len() > 4).then(|| Chunk::new(b"LIST", b))
    }
}

/// The chunks of a WAV render. Without `BWF:` metadata, `bext` names REAPER as originator
/// over the start of the description (REAPER copies the description's buffer), dates the
/// file and holds its start; with it, the tags alone fill `bext` and an `iXML` copy.
pub fn wav_extras(o: &scythe_core::render_settings::WavOptions, e: &Embed) -> Extras {
    let rpp = e.project.filter(|_| o.project_in_description).map(|p| format!("RPP:{}", p.display()));
    let mut bwf: Vec<(&str, &str)> = e.tags("BWF").collect();
    if let Some(rpp) = rpp.as_deref().filter(|_| !bwf.is_empty() && !bwf.iter().any(|(k, _)| *k == "Description")) {
        bwf.push(("Description", rpp));
        bwf.sort_by(|a, b| a.0.cmp(b.0));
    }
    let field = |key: &str| bwf.iter().find(|(k, _)| *k == key).map_or("", |(_, v)| *v);
    let mut bext = vec![0u8; 602];
    let put = |b: &mut [u8], at: usize, len: usize, s: &str| {
        let n = s.len().min(len);
        b[at..at + n].copy_from_slice(&s.as_bytes()[..n]);
    };
    if bwf.is_empty() {
        put(&mut bext, 0, 256, rpp.as_deref().unwrap_or(""));
        let desc: Vec<u8> = bext[..32].to_vec();
        bext[256..288].copy_from_slice(&desc);
        bext[256..263].copy_from_slice(b"REAPER\0");
        put(&mut bext, 320, 10, &e.date);
        put(&mut bext, 330, 8, &e.time);
        bext[338..346].copy_from_slice(&e.start_frame.to_le_bytes());
    } else {
        put(&mut bext, 0, 256, field("Description"));
        put(&mut bext, 256, 32, field("Originator"));
        put(&mut bext, 288, 32, field("OriginatorReference"));
        put(&mut bext, 320, 10, field("OriginationDate"));
        put(&mut bext, 330, 8, field("OriginationTime"));
        let time_reference = field("TimeReference").trim().parse::<u64>().unwrap_or(0);
        bext[338..346].copy_from_slice(&time_reference.to_le_bytes());
    }
    bext[346..348].copy_from_slice(&1u16.to_le_bytes());

    let mut head = Vec::new();
    // `BWF:` metadata brings its `bext` even when the sink leaves it out.
    let bext = (o.bext || !bwf.is_empty()).then(|| Chunk::new(b"bext", bext));
    // REAPER's own `bext` comes before the ACID chunk, one made from metadata after it.
    if bwf.is_empty() {
        head.extend(bext);
        head.extend(o.tempo.then(|| e.acid()));
    } else {
        head.extend(o.tempo.then(|| e.acid()));
        head.extend(bext);
    }
    if !e.metadata.is_empty() {
        let ixml: Vec<(&str, &str)> = e.tags("IXML").filter(|(k, _)| !k.contains(':')).collect();
        if !bwf.is_empty() || !ixml.is_empty() {
            let mut x = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?><BWFXML>");
            let names = [
                ("Description", "BWF_DESCRIPTION"),
                ("OriginationDate", "BWF_ORIGINATION_DATE"),
                ("OriginationTime", "BWF_ORIGINATION_TIME"),
                ("Originator", "BWF_ORIGINATOR"),
                ("OriginatorReference", "BWF_ORIGINATOR_REFERENCE"),
            ];
            let bwf_xml: Vec<_> = names.iter().filter(|(k, _)| !field(k).is_empty()).collect();
            if !bwf_xml.is_empty() {
                x += "<BWF>";
                for (k, n) in bwf_xml {
                    x += &format!("<{n}>{}</{n}>", xml_escape(field(k)));
                }
                x += "</BWF>";
            }
            for (k, v) in ixml {
                x += &format!("<{k}>{}</{k}>", xml_escape(v));
            }
            x += "</BWFXML>";
            head.push(Chunk::new(b"iXML", text_body(&x)));
        }
        if let Some(x) = e.xmp() {
            head.push(Chunk::new(b"_PMX", text_body(&x)));
        }
        if let Some(x) = e.axml() {
            head.push(Chunk::new(b"axml", text_body(&x)));
        }
        if let Some(c) = e.cart() {
            head.push(Chunk::new(b"cart", c));
        }
        if let Some(c) = e.id3() {
            head.push(Chunk::new(b"id3 ", c));
        }
    }
    Extras { head, cues: e.cues.clone(), tail: e.info().into_iter().collect() }
}

/// AIFF's text chunks, XMP, ID3 and, when the render is a whole number of beats, Apple's
/// beat count.
pub fn aiff_extras(tempo: bool, e: &Embed) -> Extras {
    let mut head = Vec::new();
    for (key, value) in e.tags("IFF") {
        let id = match key {
            "NAME" => b"NAME",
            "AUTH" => b"AUTH",
            "ANNO" => b"ANNO",
            "COPY" => b"(c) ",
            _ => continue,
        };
        head.push(Chunk::new(id, padded(value.as_bytes().to_vec())));
    }
    if let Some(x) = e.xmp() {
        let mut body = b"XMP ".to_vec();
        body.extend_from_slice(x.as_bytes());
        body.push(0);
        head.push(Chunk::new(b"APPL", padded(body)));
    }
    if let Some(c) = e.id3() {
        head.push(Chunk::new(b"id3 ", c));
    }
    if tempo {
        // The room stays a `junk` chunk when the render is not a whole number of beats.
        let mut basc = vec![0u8; 84];
        match e.whole_beats() {
            Some(beats) => {
                basc[4..8].copy_from_slice(&beats.to_be_bytes());
                head.push(Chunk::new(b"basc", basc));
            }
            None => head.push(Chunk::new(b"junk", basc)),
        }
    }
    Extras { head, cues: Vec::new(), tail: Vec::new() }
}

/// CAF `info` chunks: the tempo (as `%.4f` in a 12-byte field) and `CAFINFO:` metadata.
pub fn caf_extras(tempo: bool, e: &Embed) -> Extras {
    let mut head = Vec::new();
    if tempo {
        let mut b = 1u32.to_be_bytes().to_vec();
        b.extend_from_slice(b"tempo\0");
        let mut v = format!("{:.4}", e.bpm).into_bytes();
        v.resize(v.len().max(11), 0);
        v.push(0);
        b.extend_from_slice(&v);
        head.push(Chunk::new(b"info", b));
    }
    let tags: Vec<(&str, &str)> = e.tags("CAFINFO").collect();
    if !tags.is_empty() {
        let mut b = (tags.len() as u32).to_be_bytes().to_vec();
        for (k, v) in tags {
            for s in [k, v] {
                b.extend_from_slice(s.as_bytes());
                b.push(0);
            }
        }
        head.push(Chunk::new(b"info", b));
    }
    Extras { head, cues: e.cues.clone(), tail: Vec::new() }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Text, a terminating zero and padding to an even length, all counted in the chunk.
fn text_body(s: &str) -> Vec<u8> {
    let mut b = s.as_bytes().to_vec();
    b.push(0);
    padded(b)
}

fn padded(mut b: Vec<u8>) -> Vec<u8> {
    if b.len() % 2 == 1 {
        b.push(0);
    }
    b
}

fn synchsafe(n: u32) -> [u8; 4] {
    [(n >> 21) as u8 & 0x7f, (n >> 14) as u8 & 0x7f, (n >> 7) as u8 & 0x7f, n as u8 & 0x7f]
}

/// Local date and time as `bext` stores them: `YYYY-MM-DD` and `HH-MM-SS`.
pub fn local_date_time() -> (String, String) {
    let now =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as libc::time_t;
    // SAFETY: localtime_r and localtime_s fill the zeroed struct from a valid time value.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        #[cfg(unix)]
        libc::localtime_r(&now, &mut tm);
        #[cfg(windows)]
        libc::localtime_s(&mut tm, &now);
        tm
    };
    (
        format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday),
        format!("{:02}-{:02}-{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aiff_rates() {
        assert_eq!(extended(44100), [0x40, 0x0e, 0xac, 0x44, 0, 0, 0, 0, 0, 0]);
        assert_eq!(extended(22050), [0x40, 0x0d, 0xac, 0x44, 0, 0, 0, 0, 0, 0]);
        assert_eq!(extended(96000), [0x40, 0x0f, 0xbb, 0x80, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn ulaw_follows_reaper() {
        // Steps of REAPER's 8-bit u-law render of a ramp: 330 is the first sample of code 226,
        // and -1790 lands on a step but takes the smaller magnitude.
        assert_eq!((ulaw(329.99 / 32768.0), ulaw(330.0 / 32768.0)), (227, 226));
        assert_eq!(ulaw(-1790.0 / 32768.0), 66);
        assert_eq!(ulaw(-1.2), 0);
        assert_eq!(ulaw(0.0), 255);
        assert_eq!(ulaw(-5.796 / 32768.0), 127);
        assert_eq!(ulaw(2.229 / 32768.0), 254);
    }
}
