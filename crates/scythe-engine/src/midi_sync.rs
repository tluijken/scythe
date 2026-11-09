//! Synchronization with other gear: MIDI clock and song position out, the timecode generator
//! (LTC audio or MTC), and chasing incoming MTC, LTC or MIDI clock.
//!
//! Output follows what REAPER 7.73 sends, captured from its JACK MIDI ports: clock ticks on
//! the 1/24 quarter-note grid while playing and none while stopped; at play a reset (all
//! sound off, all notes off, pitch bend centred, sustain off on every channel, skipped for
//! outputs with `midiouts_noreset`), then Start from the project start or Continue elsewhere;
//! Stop and a song position pointer (sixteenths, rounded down) when stopping, pausing or
//! moving the playhead, and Continue again right away when it moved while playing.

use scythe_core::tempo::Timeline;

use crate::midi_io::SyncMsg;

/// A timecode frame rate as the generator stores it (`FRAMERATE rate flag`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimecodeRate {
    /// Frames per second.
    pub fps: f64,
    /// Frames per labelled second (24, 25 or 30).
    pub nominal: u32,
    /// Drop-frame labels (29.97 drop).
    pub drop: bool,
    /// The LTC drop-frame flag (bit 10), which REAPER also sets for 23.976.
    pub flag: bool,
}

impl TimecodeRate {
    /// From the generator's `FRAMERATE` fields: a non-zero flag makes it the NTSC rate
    /// (1000/1001 slower), with drop-frame labels at 30.
    pub fn from_rpp(rate: f64, flag: i64) -> Self {
        let nominal = (rate.round() as u32).clamp(1, 120);
        let ntsc = flag != 0 || (rate - rate.round()).abs() > 1e-6;
        let fps = if ntsc { nominal as f64 * 1000.0 / 1001.0 } else { nominal as f64 };
        Self { fps, nominal, drop: flag != 0 && nominal == 30, flag: flag != 0 }
    }

    /// The MTC rate code (bits 5-6 of the hours byte).
    pub fn mtc_code(self) -> u8 {
        match (self.nominal, self.drop) {
            (24, _) => 0,
            (25, _) => 1,
            (30, true) => 2,
            _ => 3,
        }
    }

    pub fn from_mtc_code(code: u8) -> Self {
        match code & 3 {
            0 => Self::from_rpp(24.0, 0),
            1 => Self::from_rpp(25.0, 0),
            2 => Self::from_rpp(30.0, 1),
            _ => Self::from_rpp(30.0, 0),
        }
    }

    /// The label of frame `n` counted from zero: hours, minutes, seconds, frames.
    ///
    /// REAPER's drop-frame labels skip one frame late at each minute: frame 1800 reads
    /// 00:01:00;00 and 1801 reads 00:01:00;03, where SMPTE has 00:01:00;02 and ;03.
    pub fn label(self, n: i64) -> [u32; 4] {
        let mut n = n.max(0);
        if self.drop {
            let (d, m) = (n / 17982, n % 17982);
            n += 18 * d + if m >= 3 { 2 * ((m - 3) / 1798) } else { 0 };
        }
        let nominal = self.nominal as i64;
        let s = n / nominal;
        [(s / 3600 % 24) as u32, (s / 60 % 60) as u32, (s % 60) as u32, (n % nominal) as u32]
    }

    /// The frame count of a label (standard drop-frame counting).
    pub fn frames(self, [h, m, s, f]: [u32; 4]) -> i64 {
        let nominal = self.nominal as i64;
        let minutes = h as i64 * 60 + m as i64;
        let mut n = (minutes * 60 + s as i64) * nominal + f as i64;
        if self.drop {
            n -= 2 * (minutes - minutes / 10);
        }
        n
    }
}

/// Level REAPER's generator writes LTC at.
pub const LTC_LEVEL: f32 = 0.31499;

/// The 80 bits of an LTC frame for frame `n`, as REAPER writes them: BCD time, the drop flag in
/// bit 10, bit 43 set, the polarity bit (27) evening out the ones, then the sync word.
pub fn ltc_bits(n: i64, rate: TimecodeRate) -> [bool; 80] {
    let [h, m, s, f] = rate.label(n);
    let mut b = [false; 80];
    let mut put = |lo: usize, len: usize, v: u32| {
        for j in 0..len {
            b[lo + j] = v >> j & 1 != 0;
        }
    };
    put(0, 4, f % 10);
    put(8, 2, f / 10);
    put(16, 4, s % 10);
    put(24, 3, s / 10);
    put(32, 4, m % 10);
    put(40, 3, m / 10);
    put(48, 4, h % 10);
    put(56, 2, h / 10);
    b[10] = rate.flag;
    b[43] = true;
    const SYNC: u16 = 0b1011_1111_1111_1100;
    for j in 0..16 {
        b[64 + j] = SYNC >> j & 1 != 0;
    }
    if b.iter().filter(|&&x| x).count() % 2 == 1 {
        b[27] = true;
    }
    b
}

/// The LTC waveform: biphase mark, low after every frame start, each transition a straight
/// ramp over the eighth of a bit before it.
#[derive(Debug)]
pub struct LtcWriter {
    rate: TimecodeRate,
    cache: Option<(i64, [bool; 80])>,
}

impl LtcWriter {
    pub fn new(rate: TimecodeRate) -> Self {
        Self { rate, cache: None }
    }

    fn bits(&mut self, n: i64) -> [bool; 80] {
        match self.cache {
            Some((c, b)) if c == n => b,
            _ => {
                let b = ltc_bits(n, self.rate);
                self.cache = Some((n, b));
                b
            }
        }
    }

    /// Whether the signal changes level at half-bit `k` (counted from timecode zero).
    fn toggles(&mut self, k: i64) -> bool {
        let (n, h) = (k.div_euclid(160), k.rem_euclid(160));
        h % 2 == 0 || self.bits(n)[h as usize / 2]
    }

    /// The level (+1 or -1) right after half-bit `k`'s transition point.
    fn level_after(&mut self, k: i64) -> f64 {
        let (n, h) = (k.div_euclid(160), k.rem_euclid(160));
        let bits = self.bits(n);
        let mut level = -1.0;
        for j in 0..=h {
            if j % 2 == 0 || bits[j as usize / 2] {
                level = -level;
            }
        }
        -level
    }

    /// The signal at timecode time `t` (seconds from 00:00:00:00).
    pub fn sample(&mut self, t: f64) -> f32 {
        let per_half = self.rate.fps * 160.0;
        let k = (t * per_half + 1e-9).floor() as i64;
        let level = self.level_after(k);
        let next = (k + 1) as f64 / per_half;
        let ramp = 1.0 / (self.rate.fps * 640.0);
        let v = if self.toggles(k + 1) && t > next - ramp {
            level - 2.0 * level * (t - (next - ramp)) / ramp
        } else {
            level
        };
        (v * LTC_LEVEL as f64) as f32
    }
}

/// MTC for the timecode generator: a full frame where playback starts or jumps, then quarter
/// frames, a quarter of a frame apart, each run of eight describing the frame its first one
/// falls on.
pub fn mtc_full_frame(n: i64, rate: TimecodeRate) -> [u8; 10] {
    let [h, m, s, f] = rate.label(n);
    [0xf0, 0x7f, 0x7f, 0x01, 0x01, (rate.mtc_code() << 5) | h as u8, m as u8, s as u8, f as u8, 0xf7]
}

/// Quarter frame `piece` (0-7) for the run that starts at frame `n`.
pub fn mtc_quarter_frame(piece: u8, n: i64, rate: TimecodeRate) -> [u8; 2] {
    let [h, m, s, f] = rate.label(n);
    let v = match piece & 7 {
        0 => f & 15,
        1 => f >> 4,
        2 => s & 15,
        3 => s >> 4,
        4 => m & 15,
        5 => m >> 4,
        6 => h & 15,
        _ => (h >> 4 & 1) | (rate.mtc_code() as u32) << 1,
    };
    [0xf1, (piece & 7) << 4 | v as u8]
}

/// MIDI clock out for the outputs that want it.
#[derive(Debug, Default)]
pub struct ClockOut {
    /// The open outputs and what each gets.
    pub ports: Vec<ClockPort>,
    playing: bool,
    /// Where the last played block ended, to notice jumps.
    end: Option<i64>,
    /// Song position last sent while stopped.
    spp: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockPort {
    pub port: u8,
    /// Clock, start, stop and continue (`midiouts_clock`).
    pub clock: bool,
    /// Song position pointers too (unless `midiouts_clock_nospp`).
    pub spp: bool,
    /// The reset at play (unless `midiouts_noreset`).
    pub reset: bool,
}

/// The reset REAPER sends on every channel when playback starts.
pub fn play_reset() -> impl Iterator<Item = [u8; 3]> {
    (0..16u8).flat_map(|c| [[0xb0 | c, 120, 0], [0xb0 | c, 123, 0], [0xe0 | c, 0, 64], [0xb0 | c, 64, 0]].into_iter())
}

fn spp_bytes(qn: f64) -> [u8; 3] {
    let v = ((qn * 4.0 + 1e-9).floor().max(0.0) as u32).min(0x3fff);
    [0xf2, (v & 0x7f) as u8, (v >> 7) as u8]
}

impl ClockOut {
    pub fn new(ports: Vec<ClockPort>) -> Self {
        Self { ports, ..Default::default() }
    }

    /// One output block. `segments` are the spans played (project frame, offset in the block,
    /// length); with none the transport rests at `position`. `emit` gets the block offset, the
    /// port and the message.
    pub fn block(
        &mut self,
        segments: &[(i64, usize, usize)],
        position: i64,
        timeline: &Timeline,
        sample_rate: u32,
        emit: &mut dyn FnMut(usize, u8, &[u8]),
    ) {
        if self.ports.is_empty() {
            return;
        }
        let sr = sample_rate as f64;
        let qn = |frame: i64| timeline.qn_at(frame as f64 / sr);
        if segments.is_empty() {
            if self.playing {
                self.playing = false;
                self.end = None;
                for p in self.ports.iter().filter(|p| p.clock) {
                    emit(0, p.port, &[0xfc]);
                }
                self.spp = None;
            }
            let spp = spp_bytes(qn(position));
            let value = spp[1] as u16 | (spp[2] as u16) << 7;
            if self.spp != Some(value) {
                self.spp = Some(value);
                for p in self.ports.iter().filter(|p| p.clock && p.spp) {
                    emit(0, p.port, &spp);
                }
            }
            return;
        }
        for &(pos, off, len) in segments {
            let jumped = self.playing && self.end != Some(pos);
            if !self.playing || jumped {
                if jumped {
                    for p in self.ports.iter().filter(|p| p.clock) {
                        emit(off, p.port, &[0xfc]);
                    }
                }
                let start = pos == 0;
                let spp = spp_bytes(qn(pos));
                let value = spp[1] as u16 | (spp[2] as u16) << 7;
                for p in &self.ports {
                    if !self.playing && p.reset {
                        for m in play_reset() {
                            emit(off, p.port, &m);
                        }
                    }
                    if !p.clock {
                        continue;
                    }
                    if !start && p.spp && (jumped || self.spp != Some(value)) {
                        emit(off, p.port, &spp);
                    }
                    emit(off, p.port, if start { &[0xfa] } else { &[0xfb] });
                }
                self.playing = true;
                self.spp = None;
            }
            let (q0, q1) = (qn(pos), qn(pos + len as i64));
            let mut k = (q0 * 24.0 - 1e-9).ceil() as i64;
            while (k as f64) < q1 * 24.0 - 1e-9 {
                let t = timeline.time_at_qn(k as f64 / 24.0);
                let at = ((t * sr).round() as i64 - pos).clamp(0, len as i64 - 1) as usize;
                for p in self.ports.iter().filter(|p| p.clock) {
                    emit(off + at, p.port, &[0xf8]);
                }
                k += 1;
            }
            self.end = Some(pos + len as i64);
        }
    }
}

/// Reads MTC: quarter frames build up a time, full frames locate.
#[derive(Debug, Default)]
pub struct MtcReader {
    pieces: [u8; 8],
    seen: u8,
    last_piece: Option<u8>,
    /// The latest time decoded (seconds) and the instant it applied to.
    pub time: Option<(f64, std::time::Instant)>,
    /// Whether quarter frames are running (forwards).
    pub running: bool,
    pub rate: Option<TimecodeRate>,
}

impl MtcReader {
    pub fn feed(&mut self, msg: SyncMsg, at: std::time::Instant) {
        match msg {
            SyncMsg::FullFrame([h, m, s, f]) => {
                let rate = TimecodeRate::from_mtc_code(h >> 5);
                self.rate = Some(rate);
                self.time = Some((rate.frames([(h & 31) as u32, m as u32, s as u32, f as u32]) as f64 / rate.fps, at));
                self.running = false;
                self.seen = 0;
                self.last_piece = None;
            }
            SyncMsg::QuarterFrame(v) => {
                let piece = v >> 4 & 7;
                let forwards = self.last_piece.is_none_or(|p| (p + 1) & 7 == piece);
                self.last_piece = Some(piece);
                if !forwards {
                    self.seen = 0;
                }
                self.pieces[piece as usize] = v & 15;
                self.seen |= 1 << piece;
                if piece == 7 && self.seen == 0xff {
                    let p = &self.pieces;
                    let rate = TimecodeRate::from_mtc_code(p[7] >> 1);
                    let label = [
                        ((p[7] & 1) << 4 | p[6]) as u32,
                        (p[5] << 4 | p[4]) as u32,
                        (p[3] << 4 | p[2]) as u32,
                        (p[1] << 4 | p[0]) as u32,
                    ];
                    // The run describes the frame its first piece fell on; by its last piece
                    // two frames have passed.
                    let frames = rate.frames(label) as f64 + 1.75;
                    self.rate = Some(rate);
                    self.time = Some((frames / rate.fps, at));
                    self.running = true;
                    self.seen = 0;
                }
            }
            _ => {}
        }
    }
}

/// Reads MIDI clock: start, stop, song position and the tempo the ticks imply.
#[derive(Debug, Default)]
pub struct ClockReader {
    /// Quarter notes from the song start, and whether the sender is playing.
    pub qn: f64,
    pub running: bool,
    /// Tempo from the spacing of recent ticks.
    pub bpm: Option<f64>,
    last_tick: Option<std::time::Instant>,
    spacing: Option<f64>,
}

impl ClockReader {
    pub fn feed(&mut self, msg: SyncMsg, at: std::time::Instant) {
        match msg {
            SyncMsg::Start => {
                self.qn = 0.0;
                self.running = true;
                self.last_tick = None;
            }
            SyncMsg::Continue => {
                self.running = true;
                self.last_tick = None;
            }
            SyncMsg::Stop => self.running = false,
            SyncMsg::SongPosition(v) => self.qn = v as f64 / 4.0,
            SyncMsg::Clock => {
                if self.running {
                    self.qn += 1.0 / 24.0;
                }
                if let Some(prev) = self.last_tick {
                    let dt = at.duration_since(prev).as_secs_f64();
                    if dt > 0.0 && dt < 0.25 {
                        let s = self.spacing.map_or(dt, |s| s + (dt - s) * 0.1);
                        self.spacing = Some(s);
                        self.bpm = Some(60.0 / (s * 24.0));
                    }
                }
                self.last_tick = Some(at);
            }
            _ => {}
        }
    }
}

/// Reads LTC from audio: finds the bit cells from the zero crossings and decodes a frame at
/// every sync word.
#[derive(Debug)]
pub struct LtcReader {
    sample_rate: f64,
    last_sign: bool,
    since_cross: f64,
    half: Option<f64>,
    pending_half: bool,
    bits: u128,
    /// The latest frame decoded: its label, frame rate guess and the sample count at its end.
    pub frame: Option<([u32; 4], bool, u64)>,
    samples: u64,
}

impl LtcReader {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate: sample_rate as f64,
            last_sign: false,
            since_cross: 0.0,
            half: None,
            pending_half: false,
            bits: 0,
            frame: None,
            samples: 0,
        }
    }

    pub fn feed(&mut self, input: &[f32]) {
        for &x in input {
            self.samples += 1;
            self.since_cross += 1.0;
            let sign = x > 0.0;
            if sign == self.last_sign || x.abs() < 1e-4 {
                continue;
            }
            self.last_sign = sign;
            let gap = std::mem::take(&mut self.since_cross);
            let half = *self.half.get_or_insert(gap);
            if gap > half * 1.5 {
                if gap > half * 2.6 {
                    self.half = None;
                    self.pending_half = false;
                    continue;
                }
                self.push_bit(false);
                self.pending_half = false;
                self.half = Some(half + (gap / 2.0 - half) * 0.1);
            } else if gap < half * 0.5 {
                self.half = Some(gap);
                self.pending_half = false;
            } else {
                self.half = Some(half + (gap - half) * 0.1);
                if self.pending_half {
                    self.push_bit(true);
                    self.pending_half = false;
                } else {
                    self.pending_half = true;
                }
            }
        }
        let _ = self.sample_rate;
    }

    fn push_bit(&mut self, bit: bool) {
        self.bits = self.bits >> 1 | (bit as u128) << 79;
        const SYNC: u128 = 0b1011_1111_1111_1100;
        if self.bits >> 64 & 0xffff == SYNC {
            let b = |lo: u32, len: u32| ((self.bits >> lo) & ((1 << len) - 1)) as u32;
            let label =
                [b(56, 2) * 10 + b(48, 4), b(40, 3) * 10 + b(32, 4), b(24, 3) * 10 + b(16, 4), b(8, 2) * 10 + b(0, 4)];
            self.frame = Some((label, self.bits >> 10 & 1 != 0, self.samples));
        }
    }
}

/// What the audio thread does about synchronization: which outputs get clock, and what it
/// chases.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncConfig {
    pub clock: Vec<ClockPort>,
    pub chase: Option<ChaseConfig>,
}

/// Where chased timecode comes from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChaseSource {
    /// MTC from one input device, or any.
    Mtc(Option<u8>),
    /// MIDI clock and song position from one input device, or any.
    Clock(Option<u8>),
    /// LTC on an audio input channel (0-based), at a frame rate.
    Ltc { channel: usize, rate: TimecodeRate },
}

/// External timecode synchronization as the project sets it (`SMPTESYNC`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChaseConfig {
    pub source: ChaseSource,
    /// Seconds subtracted from the incoming time.
    pub offset: f64,
    /// Drift, in seconds, that makes the playhead jump back in line.
    pub resync: f64,
    /// Seconds playback carries on after the timecode stops.
    pub freewheel: f64,
}

/// What chasing asks of the transport.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChaseAction {
    Play(f64),
    Seek(f64),
    Stop,
}

/// Follows incoming timecode: starts, moves and stops the transport.
#[derive(Debug)]
pub struct Chase {
    pub config: ChaseConfig,
    mtc: MtcReader,
    clock: ClockReader,
    ltc: LtcReader,
    /// When timecode last arrived.
    last: Option<std::time::Instant>,
    sample_rate: f64,
}

impl Chase {
    pub fn new(config: ChaseConfig, sample_rate: u32) -> Self {
        Self {
            config,
            mtc: MtcReader::default(),
            clock: ClockReader::default(),
            ltc: LtcReader::new(sample_rate),
            last: None,
            sample_rate: sample_rate as f64,
        }
    }

    pub fn feed(&mut self, input: &crate::midi_io::SyncInput) {
        match self.config.source {
            ChaseSource::Mtc(d) if d.is_none_or(|d| d == input.device) => {
                if matches!(input.msg, SyncMsg::QuarterFrame(_) | SyncMsg::FullFrame(_)) {
                    self.mtc.feed(input.msg, input.at);
                    self.last = Some(input.at);
                }
            }
            ChaseSource::Clock(d) if d.is_none_or(|d| d == input.device) => {
                self.clock.feed(input.msg, input.at);
                if matches!(input.msg, SyncMsg::Clock | SyncMsg::Start | SyncMsg::Continue) {
                    self.last = Some(input.at);
                }
            }
            _ => {}
        }
    }

    /// Audio from the chased input channel, the block ending at `now`.
    pub fn feed_audio(&mut self, samples: &[f32], now: std::time::Instant) {
        let before = self.ltc.frame.map(|f| f.2);
        self.ltc.feed(samples);
        if self.ltc.frame.map(|f| f.2) != before {
            self.last = Some(now);
        }
        let _ = self.sample_rate;
    }

    /// Where the timecode says the playhead is now, when it runs.
    fn target(&self, now: std::time::Instant) -> Option<f64> {
        let t = match self.config.source {
            ChaseSource::Mtc(_) => {
                let (t, at) = self.mtc.time.filter(|_| self.mtc.running)?;
                t + now.saturating_duration_since(at).as_secs_f64()
            }
            ChaseSource::Clock(_) => return self.clock.running.then_some(self.clock.qn),
            ChaseSource::Ltc { rate, .. } => {
                let last = self.last?;
                let (label, _, _) = self.ltc.frame?;
                (rate.frames(label) + 1) as f64 / rate.fps + now.saturating_duration_since(last).as_secs_f64()
            }
        };
        Some(t - self.config.offset)
    }

    /// What to do given the transport's position (seconds) and whether it plays. For MIDI
    /// clock the target is in quarter notes, turned into seconds by `qn_to_seconds`.
    pub fn decide(
        &mut self,
        now: std::time::Instant,
        position: f64,
        playing: bool,
        qn_to_seconds: impl Fn(f64) -> f64,
    ) -> Option<ChaseAction> {
        let fresh = self.last.is_some_and(|l| now.saturating_duration_since(l).as_secs_f64() <= self.config.freewheel);
        let target = self.target(now).filter(|_| fresh).map(|t| match self.config.source {
            ChaseSource::Clock(_) => qn_to_seconds(t),
            _ => t,
        });
        match (target, playing) {
            (Some(t), false) => Some(ChaseAction::Play(t.max(0.0))),
            (Some(t), true) if (t - position).abs() > self.config.resync => Some(ChaseAction::Seek(t.max(0.0))),
            (None, true) if self.last.is_some() => {
                self.last = None;
                self.mtc.running = false;
                Some(ChaseAction::Stop)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_and_labels() {
        let df = TimecodeRate::from_rpp(30.0, 1);
        assert!(df.drop && (df.fps - 29.97).abs() < 1e-3);
        assert_eq!(df.label(1799), [0, 0, 59, 29]);
        assert_eq!(df.label(1800), [0, 1, 0, 0], "REAPER's late skip");
        assert_eq!(df.label(1801), [0, 1, 0, 3]);
        assert_eq!(df.label(17982), [0, 10, 0, 0]);
        let r25 = TimecodeRate::from_rpp(25.0, 0);
        assert_eq!(r25.label(3600 * 25 + 7), [1, 0, 0, 7]);
        assert_eq!(r25.frames([1, 0, 0, 7]), 3600 * 25 + 7);
        assert_eq!(df.frames([0, 1, 0, 2]), 1800);
        let nd = TimecodeRate::from_rpp(29.97, 0);
        assert!(!nd.drop && !nd.flag && nd.nominal == 30);
    }

    #[test]
    fn ltc_frames_keep_polarity() {
        let rate = TimecodeRate::from_rpp(30.0, 0);
        for n in 0..200 {
            let b = ltc_bits(n, rate);
            assert_eq!(b.iter().filter(|&&x| x).count() % 2, 0);
            assert!(b[43]);
        }
        let mut w = LtcWriter::new(rate);
        let mut reader = LtcReader::new(48000);
        let audio: Vec<f32> = (0..48000).map(|i| w.sample(10.0 + i as f64 / 48000.0)).collect();
        reader.feed(&audio);
        let (label, _, _) = reader.frame.unwrap();
        assert_eq!(label, [0, 0, 10, 29]);
    }

    /// First samples of REAPER's 30 fps generator from 00:00:00:00 (falling into the first
    /// frame, then the ramp ending on the first bit boundary).
    #[test]
    fn ltc_matches_reaper_samples() {
        let mut w = LtcWriter::new(TimecodeRate::from_rpp(30.0, 0));
        let s: Vec<f32> = (16..21).map(|i| w.sample(i as f64 / 48000.0)).collect();
        let expect = [-0.31499, -0.31499, -0.18899, 0.063, 0.31499];
        for (a, b) in s.iter().zip(expect) {
            assert!((a - b).abs() < 1e-5, "{s:?}");
        }
    }

    #[test]
    fn mtc_messages() {
        let rate = TimecodeRate::from_rpp(25.0, 0);
        assert_eq!(mtc_full_frame(3600 * 25 + 5 * 25, rate), [0xf0, 0x7f, 0x7f, 1, 1, 0x21, 0, 5, 0, 0xf7]);
        let run: Vec<u8> = (0..8).map(|p| mtc_quarter_frame(p, 3600 * 25 + 2, rate)[1]).collect();
        assert_eq!(run, [0x02, 0x10, 0x20, 0x30, 0x40, 0x50, 0x61, 0x72]);
        let mut reader = MtcReader::default();
        let now = std::time::Instant::now();
        for v in run {
            reader.feed(SyncMsg::QuarterFrame(v), now);
        }
        let (t, _) = reader.time.unwrap();
        assert!((t - (3600.0 + 3.75 / 25.0)).abs() < 1e-9, "{t}");
        assert!(reader.running);
    }

    /// The sequence captured from REAPER at 120 BPM: play from 0, stop, move to 1.5 s, play,
    /// move to 3.3 s while playing.
    #[test]
    fn clock_follows_reaper() {
        let tl = scythe_core::tempo::TempoMap::constant(120.0, 4, 4).timeline();
        let mut clock = ClockOut::new(vec![ClockPort { port: 0, clock: true, spp: true, reset: false }]);
        let mut out: Vec<(i64, Vec<u8>)> = Vec::new();
        let run =
            |clock: &mut ClockOut, seg: &[(i64, usize, usize)], pos: i64, base: i64, out: &mut Vec<(i64, Vec<u8>)>| {
                clock.block(seg, pos, &tl, 48000, &mut |o, _, m| out.push((base + o as i64, m.to_vec())));
            };
        run(&mut clock, &[(0, 0, 512)], 0, 0, &mut out);
        assert_eq!(out[0], (0, vec![0xfa]));
        assert_eq!(out[1], (0, vec![0xf8]));
        for b in 1..94 {
            run(&mut clock, &[(b * 512, 0, 512)], 0, b * 512, &mut out);
        }
        let ticks: Vec<i64> = out.iter().filter(|m| m.1 == [0xf8]).map(|m| m.0).collect();
        assert!(ticks.windows(2).all(|w| w[1] - w[0] == 1000), "24 per quarter note at 120 BPM");
        out.clear();
        run(&mut clock, &[], 0, 0, &mut out);
        assert_eq!(out, [(0, vec![0xfc]), (0, vec![0xf2, 0, 0])]);
        out.clear();
        run(&mut clock, &[], 72000, 0, &mut out);
        assert_eq!(out, [(0, vec![0xf2, 12, 0])]);
        out.clear();
        run(&mut clock, &[(72000, 0, 512)], 72000, 0, &mut out);
        assert_eq!(out[..2], [(0, vec![0xfb]), (0, vec![0xf8])]);
        out.clear();
        run(&mut clock, &[(158400, 0, 512)], 158400, 0, &mut out);
        assert_eq!(out, [(0, vec![0xfc]), (0, vec![0xf2, 26, 0]), (0, vec![0xfb])], "the next tick is 600 frames on");
    }
}
