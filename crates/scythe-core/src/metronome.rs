//! The project metronome as REAPER stores it: the `<METRONOME flags count-in` chunk and the
//! `METRONOMEOUT` lines after it.
//!
//! REAPER 7.73 keeps these in the project (`projmetroen`, `projmetrocountin`; the bits match the
//! checkboxes of Options > Metronome/pre-roll settings), while pre-roll (`preroll`, `prerollmeas`)
//! and the default metronome output (`metronome_defout`) are global preferences in reaper.ini
//! ([`crate::prefs::Preferences`]).
//!
//! How playback starts follows REAPER 7.73 as measured (`scripts/reaper-count-in.sh`): pre-roll
//! moves the start back first, then the count-in runs before that start, and only when the
//! metronome is enabled.

use crate::prefs::Preferences;
use crate::tempo::TempoMap;

/// The metronome sounds at all ("Enable metronome", action 40364).
pub const ENABLED: u32 = 1;
/// "Run metronome during playback".
pub const RUN_ON_PLAY: u32 = 2;
/// "Run metronome during recording".
pub const RUN_ON_RECORD: u32 = 4;
/// "Count-in before playback".
pub const COUNT_IN_PLAY: u32 = 8;
/// "Count-in before recording".
pub const COUNT_IN_RECORD: u32 = 16;

/// reaper.ini `preroll`: "Pre-roll before playback" (action 41818).
pub const PREROLL_PLAY: u32 = 1;
/// "Pre-roll before recording" (action 41819).
pub const PREROLL_RECORD: u32 = 2;
/// Set when "Start pre-roll at start of measure" is off: the dialog stores that checkbox
/// inverted, so it is on in a fresh reaper.ini.
pub const PREROLL_MID_MEASURE: u32 = 4;
/// "Start count-in at start of measure".
pub const COUNT_IN_AT_MEASURE: u32 = 8;

/// Longest count-in accepted, in measures.
pub const MAX_COUNT_IN: f64 = 16.0;

/// REAPER moves a start this far through its bar (in bars), or further, onto the next bar line.
const NEXT_BAR: f64 = 0.9;

#[derive(Debug, Clone, PartialEq)]
pub struct Metronome {
    /// The chunk's first field, bits as the constants above; others are kept as found.
    pub flags: u32,
    /// Count-in length in measures (the chunk's second field).
    pub count_in_measures: f64,
    /// Volume of the primary beat (`VOL`'s first value): REAPER's default is 0.25.
    pub volume: f64,
    /// Device outputs the click plays to, one per `METRONOMEOUT` line, encoded as a hardware
    /// output's `I_DSTCHAN` (first output from 0, plus 1024 for mono). Empty plays through the
    /// default metronome output, which is the master's outputs unless the preferences say so.
    pub outputs: Vec<i64>,
}

impl Default for Metronome {
    fn default() -> Self {
        Self { flags: RUN_ON_PLAY | RUN_ON_RECORD, count_in_measures: 2.0, volume: 0.25, outputs: Vec::new() }
    }
}

/// What sounds before the playhead moves: a count-in, and the part of a pre-roll that lies
/// before the project start. Clicks fall on the beats of the tempo where the playhead starts,
/// carried back from there, so bar lines land where the project's would.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CountIn {
    /// Seconds from its start until the playhead moves.
    pub length: f64,
    /// Seconds between clicks.
    pub beat: f64,
    /// Seconds into it of the first click.
    pub first: f64,
    /// Which beat of its bar the first click is, from 0.
    pub first_in_bar: u32,
    pub beats_per_bar: u32,
    /// Clicks sound only in the first `audible` seconds: a pre-roll before the project start
    /// clicks only when the metronome runs during playback (or recording).
    pub audible: f64,
    /// The first seconds of it that count in; the rest is a pre-roll before the project start.
    pub counting: f64,
}

impl CountIn {
    /// How many clicks fall inside it, sounding or not.
    pub fn beats(&self) -> u32 {
        if self.beat <= 0.0 || self.first >= self.length {
            return 0;
        }
        ((self.length - self.first) / self.beat - 1e-9).ceil().max(0.0) as u32
    }

    /// The count-in as REAPER 7.73 plays it on a device running `block` frames at a time: the
    /// counting part lengthened at its start to whole blocks, clicks staying on their beats. A
    /// pre-roll before the project start is not.
    pub fn padded(self, block: u32, sample_rate: u32) -> Self {
        let sr = sample_rate as f64;
        let frames = (self.counting * sr).round() as i64;
        let block = block as i64;
        if block <= 0 || frames <= 0 {
            return self;
        }
        let extra = (block - frames % block) % block;
        let pad = extra as f64 / sr;
        Self {
            length: self.length + pad,
            first: self.first + pad,
            audible: self.audible + pad,
            counting: self.counting + pad,
            ..self
        }
    }
}

/// Where playback starts and what comes before it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Start {
    /// Where the playhead starts moving: before the requested position when pre-rolling, never
    /// before the project start.
    pub from: f64,
    pub count_in: Option<CountIn>,
}

impl Start {
    /// Seconds from starting until the playhead reaches `at`.
    pub fn until(&self, at: f64) -> f64 {
        self.count_in.map_or(0.0, |c| c.length) + (at - self.from).max(0.0)
    }

    /// A recording's pre-roll to `at` as REAPER 7.73 plays it on a device running `block` frames
    /// at a time: lengthened at its start to whole blocks, so the recording starts on a block's
    /// first frame, a count-in's clicks staying on their beats. One that would then start before
    /// the project start is left as it is.
    pub fn padded_preroll(self, at: f64, block: u32, sample_rate: u32) -> Self {
        let sr = sample_rate as f64;
        let frames = ((at - self.from) * sr).round() as i64;
        let block = block as i64;
        if block <= 0 || frames <= 0 {
            return self;
        }
        let pad = ((block - frames % block) % block) as f64 / sr;
        if self.from - pad < 0.0 {
            return self;
        }
        let count_in = self.count_in.map(|c| CountIn { first: c.first + pad, ..c });
        Self { from: self.from - pad, count_in }
    }
}

impl Metronome {
    pub fn has(&self, flag: u32) -> bool {
        self.flags & flag != 0
    }

    pub fn set(&mut self, flag: u32, on: bool) {
        if on {
            self.flags |= flag;
        } else {
            self.flags &= !flag;
        }
    }

    pub fn enabled(&self) -> bool {
        self.has(ENABLED)
    }

    /// Whether the metronome clicks while the transport runs, recording or not.
    pub fn clicks(&self, recording: bool) -> bool {
        self.enabled() && self.has(if recording { RUN_ON_RECORD } else { RUN_ON_PLAY })
    }

    /// Whether starting playback (or recording) counts in first. REAPER skips the count-in
    /// altogether while the metronome is disabled, whatever the count-in checkboxes say.
    pub fn counts_in(&self, recording: bool) -> bool {
        self.enabled()
            && self.has(if recording { COUNT_IN_RECORD } else { COUNT_IN_PLAY })
            && self.count_in_measures > 0.0
    }

    /// How playback (or recording) from `at` starts: pre-roll as the preferences ask, then the
    /// project's count-in, or one in any case with `force_count_in` (a setlist's).
    pub fn start(
        &self,
        prefs: &Preferences,
        tempo: &TempoMap,
        at: f64,
        recording: bool,
        force_count_in: bool,
    ) -> Start {
        let preroll =
            prefs.preroll & if recording { PREROLL_RECORD } else { PREROLL_PLAY } != 0 && prefs.preroll_measures > 0.0;
        let start = if preroll {
            preroll_start(tempo, at, prefs.preroll_measures, prefs.preroll & PREROLL_MID_MEASURE == 0)
        } else {
            at
        };
        let from = start.max(0.0);
        let before_zero = from - start;
        let (num, denom) = tempo.time_sig_at(from);
        let num = num.max(1);
        let beat = 60.0 / tempo.bpm_at(from).max(1.0) * 4.0 / denom.max(1) as f64;
        let mut counting = 0.0;
        if force_count_in || self.counts_in(recording) {
            counting = self.count_in_measures.clamp(0.0, MAX_COUNT_IN) * num as f64 * beat;
            let into_bar = beats_into_bar(tempo, start);
            if prefs.preroll & COUNT_IN_AT_MEASURE != 0 && into_bar > 1e-9 {
                if into_bar < NEXT_BAR * num as f64 - 1e-9 {
                    counting += into_bar * beat;
                } else {
                    counting -= (num as f64 - into_bar) * beat;
                }
            }
        }
        let length = counting.max(0.0) + before_zero;
        if length <= 1e-9 {
            return Start { from, count_in: None };
        }
        let audible = counting.max(0.0) + if self.clicks(recording) { before_zero } else { 0.0 };
        // Beats counted from the bar line before `from`; the count-in ends at `from`.
        let at_from = beats_into_bar(tempo, from);
        let first_beat = (at_from - length / beat - 1e-9).ceil();
        let count_in = CountIn {
            length,
            beat,
            first: (length + (first_beat - at_from) * beat).max(0.0),
            first_in_bar: (first_beat as i64).rem_euclid(num as i64) as u32,
            beats_per_bar: num,
            audible,
            counting: counting.max(0.0),
        };
        Start { from, count_in: Some(count_in) }
    }

    /// The outputs as (first device output from 0, mono), leaving out ReaRoute channels.
    pub fn device_outputs(&self) -> Vec<(usize, bool)> {
        device_outputs(&self.outputs)
    }

    /// The outputs the click plays to: the project's own, else the preferences' default.
    pub fn outputs_or(&self, default_output: i64) -> Vec<(usize, bool)> {
        if self.outputs.is_empty() { device_outputs(&[default_output]) } else { self.device_outputs() }
    }
}

fn device_outputs(outputs: &[i64]) -> Vec<(usize, bool)> {
    outputs
        .iter()
        .filter(|&&o| o >= 0 && o & crate::HwOut::REAROUTE == 0)
        .map(|&o| ((o & 1023) as usize, o & crate::HwOut::MONO != 0))
        .collect()
}

/// Beats since the last bar line at `t`, in beats of that bar's time signature.
fn beats_into_bar(tempo: &TempoMap, t: f64) -> f64 {
    let bb = tempo.bar_beat(t);
    (bb.beat - 1) as f64 + bb.fraction
}

/// Where a pre-roll of `measures` before `at` starts: that many bars back by the project's bar
/// numbers, a fraction counted in beats of the bar at `at`, then on the bar line (the next one
/// when within a tenth of a bar of it) when `at_measure`. Before the project start it carries
/// the first tempo back.
pub fn preroll_start(tempo: &TempoMap, at: f64, measures: f64, at_measure: bool) -> f64 {
    let tl = tempo.timeline();
    let num = tempo.time_sig_at(at).0.max(1) as f64;
    let mut bar = tl.bar_beat(at).bar - measures.trunc() as i64;
    let mut beat = beats_into_bar(tempo, at) - measures.fract() * num;
    if beat < 0.0 {
        bar -= 1;
        beat += num;
    }
    if at_measure {
        if beat >= NEXT_BAR * num - 1e-9 {
            bar += 1;
        }
        beat = 0.0;
    }
    let bar_start = tl.bar_start(bar);
    let denom = tempo.time_sig_at(bar_start).1.max(1) as f64;
    tl.time_at_qn(tl.qn_at(bar_start) + beat * 4.0 / denom)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> Metronome {
        Metronome { flags: ENABLED | COUNT_IN_PLAY | COUNT_IN_RECORD, count_in_measures: 1.0, ..Metronome::default() }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn defaults_match_a_new_reaper_project() {
        let m = Metronome::default();
        assert_eq!((m.flags, m.count_in_measures, m.volume), (6, 2.0, 0.25));
        assert!(!m.enabled() && !m.clicks(false) && !m.counts_in(false));
    }

    #[test]
    fn a_disabled_metronome_does_not_count_in() {
        let tempo = TempoMap::constant(120.0, 4, 4);
        let prefs = Preferences::default();
        let off = Metronome { flags: COUNT_IN_PLAY, ..on() };
        assert!(!off.counts_in(false));
        assert_eq!(off.start(&prefs, &tempo, 4.0, false, false), Start { from: 4.0, count_in: None });
        let forced = off.start(&prefs, &tempo, 4.0, false, true).count_in.unwrap();
        assert!(close(forced.length, 2.0) && close(forced.audible, 2.0) && forced.beats() == 4);
        assert!(on().counts_in(false) && on().counts_in(true));
    }

    #[test]
    fn count_in_follows_the_tempo_where_playback_starts() {
        let mut tempo = TempoMap::constant(120.0, 4, 4);
        tempo.points =
            vec![crate::tempo::TempoPoint { time_sig: Some((6, 8)), ..crate::tempo::TempoPoint::new(10.0, 90.0) }];
        let prefs = Preferences::default();
        let c = on().start(&prefs, &tempo, 0.0, false, false).count_in.unwrap();
        assert_eq!((c.beats(), c.first_in_bar, c.beats_per_bar), (4, 0, 4));
        assert!(close(c.length, 2.0) && close(c.beat, 0.5) && close(c.first, 0.0));
        let late = on().start(&prefs, &tempo, 10.0, false, false).count_in.unwrap();
        assert_eq!((late.beats(), late.beats_per_bar), (6, 6));
        assert!(close(late.beat, 60.0 / 90.0 / 2.0));
        let half = Metronome { count_in_measures: 0.5, ..on() };
        assert_eq!(half.start(&prefs, &tempo, 0.0, false, false).count_in.unwrap().beats(), 2);
    }

    #[test]
    fn mid_bar_count_ins_click_on_the_project_beats() {
        let tempo = TempoMap::constant(120.0, 4, 4);
        let mut prefs = Preferences::default();
        // Beat 3 of bar 2, a quarter beat late: one bar back, clicks on the beats before.
        let c = on().start(&prefs, &tempo, 3.125, false, false).count_in.unwrap();
        assert!(close(c.length, 2.0) && close(c.first, 0.375), "{c:?}");
        assert_eq!((c.beats(), c.first_in_bar), (4, 3));
        prefs.preroll = COUNT_IN_AT_MEASURE;
        let c = on().start(&prefs, &tempo, 3.125, false, false).count_in.unwrap();
        assert!(close(c.length, 3.125) && close(c.first, 0.0), "back to the bar line: {c:?}");
        assert_eq!((c.beats(), c.first_in_bar), (7, 0));
        // Within a tenth of a bar of the next bar line it counts in to that line instead.
        let c = on().start(&prefs, &tempo, 3.9, false, false).count_in.unwrap();
        assert!(close(c.length, 1.9) && close(c.first, 0.0), "{c:?}");
        assert_eq!((c.beats(), c.first_in_bar), (4, 0));
    }

    #[test]
    fn preroll_starts_measures_back_on_a_bar_line() {
        let tempo = TempoMap::constant(120.0, 4, 4);
        assert!(close(preroll_start(&tempo, 9.0, 2.0, true), 4.0), "bar 5 beat 3 rolls from bar 3");
        assert!(close(preroll_start(&tempo, 9.0, 2.0, false), 5.0));
        assert!(close(preroll_start(&tempo, 9.0, 1.5, false), 6.0));
        assert!(close(preroll_start(&tempo, 9.0, 1.75, false), 5.5), "{}", preroll_start(&tempo, 9.0, 1.75, false));
        assert!(close(preroll_start(&tempo, 9.0, 1.75, true), 4.0));
        assert!(close(preroll_start(&tempo, 7.9, 1.0, true), 6.0), "late in a bar: to the next line");
        assert!(close(preroll_start(&tempo, 1.0, 2.0, true), -4.0), "before the project start");
    }

    #[test]
    fn preroll_before_the_project_start_waits_like_a_count_in() {
        let tempo = TempoMap::constant(120.0, 4, 4);
        let prefs = Preferences { preroll: PREROLL_PLAY | PREROLL_RECORD, ..Preferences::default() };
        let quiet = Metronome { flags: ENABLED | RUN_ON_RECORD, ..Metronome::default() };
        let s = quiet.start(&prefs, &tempo, 2.0, false, false);
        assert_eq!(s.from, 0.0);
        let c = s.count_in.unwrap();
        assert!(close(c.length, 2.0) && close(c.audible, 0.0) && close(s.until(2.0), 4.0), "{s:?}");
        let rec = quiet.start(&prefs, &tempo, 2.0, true, false).count_in.unwrap();
        assert!(close(rec.audible, 2.0), "it runs during recording");
        // The count-in comes before the pre-roll.
        let both = Metronome { count_in_measures: 1.0, ..on() }.start(&prefs, &tempo, 6.0, false, false);
        assert!(close(both.from, 2.0) && close(both.until(6.0), 6.0), "{both:?}");
        let off = Preferences { preroll: PREROLL_RECORD, ..prefs };
        assert_eq!(Metronome::default().start(&off, &tempo, 6.0, false, false).from, 6.0);
    }

    /// Starts measured in REAPER 7.73 on a JACK dummy device (`docs/verification.md`), at 120
    /// bpm in 4/4 with one measure of count-in: where the playhead started, how long it waited,
    /// when the first click sounded and which beat of its bar that was. The waits leave out
    /// REAPER's padding to whole blocks, which `padded` adds.
    #[test]
    fn starts_match_reaper() {
        let tempo = TempoMap::constant(120.0, 4, 4);
        let quiet = Metronome { flags: ENABLED | RUN_ON_PLAY, ..on() };
        let off = Metronome { flags: RUN_ON_PLAY | COUNT_IN_PLAY | COUNT_IN_RECORD, ..on() };
        #[rustfmt::skip]
        let cases = [
            // metronome, preroll, prerollmeas, start, from, wait, first click, its beat
            (&off, 0, 2.0, 4.0, 4.0, 0.0, 0.0, 0),
            (&on(), 0, 2.0, 4.0, 4.0, 2.0, 0.0, 0),
            (&on(), PREROLL_PLAY, 1.0, 6.0, 4.0, 2.0, 0.0, 0),
            (&quiet, PREROLL_PLAY, 1.0, 1.0, 0.0, 2.0, 0.0, 0),
            (&on(), PREROLL_PLAY, 1.0, 1.0, 0.0, 4.0, 0.0, 0),
            (&quiet, PREROLL_PLAY | PREROLL_MID_MEASURE, 1.0, 0.3, 0.0, 1.7, 0.2, 1),
            (&on(), PREROLL_PLAY | PREROLL_MID_MEASURE, 1.0, 0.3, 0.0, 3.7, 0.2, 1),
            (&on(), 0, 2.0, 3.125, 3.125, 2.0, 0.375, 3),
            (&on(), COUNT_IN_AT_MEASURE, 2.0, 3.125, 3.125, 3.125, 0.0, 0),
            (&on(), PREROLL_PLAY | PREROLL_MID_MEASURE | COUNT_IN_AT_MEASURE, 1.0, 9.25, 7.25, 3.25, 0.0, 0),
            (&quiet, PREROLL_PLAY, 1.0, 9.0, 6.0, 0.0, 0.0, 0),
            (&quiet, PREROLL_PLAY | PREROLL_MID_MEASURE, 1.0, 9.0, 7.0, 0.0, 0.0, 0),
            (&quiet, PREROLL_PLAY | PREROLL_MID_MEASURE, 1.75, 9.0, 5.5, 0.0, 0.0, 0),
            (&quiet, PREROLL_PLAY, 1.75, 9.0, 4.0, 0.0, 0.0, 0),
            (&on(), COUNT_IN_AT_MEASURE, 2.0, 3.9, 3.9, 1.9, 0.0, 0),
            (&on(), COUNT_IN_AT_MEASURE, 2.0, 3.85, 3.85, 1.85, 0.0, 0),
            (&on(), COUNT_IN_AT_MEASURE, 2.0, 3.8, 3.8, 1.8, 0.0, 0),
            (&on(), COUNT_IN_AT_MEASURE, 2.0, 3.75, 3.75, 3.75, 0.0, 0),
            (&on(), COUNT_IN_AT_MEASURE, 2.0, 3.7, 3.7, 3.7, 0.0, 0),
            (&quiet, PREROLL_PLAY, 1.0, 7.9, 6.0, 0.0, 0.0, 0),
            (&quiet, PREROLL_PLAY, 1.0, 7.8, 6.0, 0.0, 0.0, 0),
            (&quiet, PREROLL_PLAY, 1.0, 7.75, 4.0, 0.0, 0.0, 0),
        ];
        for (m, preroll, preroll_measures, at, from, wait, first, first_in_bar) in cases {
            let prefs = Preferences { preroll, preroll_measures, ..Preferences::default() };
            let s = m.start(&prefs, &tempo, at, false, false);
            let what = format!("preroll {preroll} x{preroll_measures} from {at}: {s:?}");
            assert!(close(s.from, from), "{what}");
            match s.count_in {
                None => assert_eq!(wait, 0.0, "{what}"),
                Some(c) => {
                    assert!(close(c.length, wait) && close(c.first, first), "{what}");
                    assert_eq!(c.first_in_bar, first_in_bar, "{what}");
                }
            }
        }
        let rec = Preferences { preroll: PREROLL_RECORD, preroll_measures: 1.0, ..Preferences::default() };
        let s = on().start(&rec, &tempo, 6.0, true, false);
        assert!(close(s.from, 4.0) && close(s.count_in.unwrap().length, 2.0), "{s:?}");
        let s = off.start(&Preferences::default(), &tempo, 4.0, true, false);
        assert_eq!(s.count_in, None, "nor before recording");
    }

    /// REAPER 7.73's count-in lengths on the JACK dummy device at 48 kHz, as `GetPlayPosition`
    /// showed them, with the clicks on the same beats before the playhead as unpadded.
    #[test]
    fn count_ins_are_padded_to_whole_blocks() {
        let tempo = TempoMap::constant(120.0, 4, 4);
        let at_measure = Preferences { preroll: COUNT_IN_AT_MEASURE, ..Preferences::default() };
        let before_zero = Preferences {
            preroll: PREROLL_PLAY | PREROLL_MID_MEASURE,
            preroll_measures: 1.0,
            ..Preferences::default()
        };
        let quiet = Metronome { flags: ENABLED | RUN_ON_PLAY, ..on() };
        let cases = [
            (on(), &at_measure, 4.0, 1024, 2.005333),
            (on(), &at_measure, 3.125, 1024, 3.136),
            (on(), &at_measure, 3.7, 1024, 3.712),
            (on(), &at_measure, 3.8, 1024, 1.813333),
            (on(), &at_measure, 3.9, 128, 1.901333),
            (on(), &at_measure, 3.85, 128, 1.850667),
            (on(), &at_measure, 3.125, 128, 3.125333),
            (on(), &at_measure, 3.75, 128, 3.752),
            (on(), &before_zero, 0.3, 1024, 2.005333 + 1.7),
            (quiet, &before_zero, 0.3, 1024, 1.7),
        ];
        for (m, prefs, at, block, length) in cases {
            let c = m.start(prefs, &tempo, at, false, false).count_in.unwrap();
            let p = c.padded(block, 48_000);
            let what = format!("{at} at {block}: {p:?}");
            assert!((p.length - length).abs() < 1e-6, "{what}");
            assert!(close(p.length - p.first, c.length - c.first), "{what}");
            assert!(close(p.audible - p.first, c.audible - c.first) && p.beats() == c.beats(), "{what}");
        }
        let c = on().start(&at_measure, &tempo, 3.9, false, false).count_in.unwrap();
        assert_eq!(c.padded(0, 48_000), c, "no block size yet");
    }

    /// Recording with pre-roll in REAPER 7.73 on the JACK dummy device at 48 kHz: the playhead
    /// started up to a block before the pre-roll's start, the clicks staying on the project's
    /// beats; in playback, at blocks of 128 frames and before the project start it did not.
    #[test]
    fn a_recording_pre_roll_is_padded_to_whole_blocks() {
        let tempo = TempoMap::constant(120.0, 4, 4);
        let rec = Preferences { preroll: PREROLL_RECORD, preroll_measures: 1.0, ..Preferences::default() };
        let mid = Preferences { preroll: PREROLL_RECORD | PREROLL_MID_MEASURE, ..rec.clone() };
        let quiet = Metronome { flags: ENABLED | RUN_ON_RECORD, ..on() };
        for (m, prefs, at, block, from) in [
            (&quiet, &rec, 6.0, 1024, 3.994667),
            (&quiet, &rec, 6.0, 512, 3.994667),
            (&quiet, &rec, 6.0, 128, 4.0),
            (&quiet, &rec, 7.0, 1024, 3.992),
            (&quiet, &mid, 7.3, 1024, 5.294667),
            (&quiet, &mid, 0.3, 1024, 0.0),
        ] {
            let s = m.start(prefs, &tempo, at, true, false).padded_preroll(at, block, 48_000);
            assert!((s.from - from).abs() < 1e-6, "{at} at {block}: {s:?}");
        }
        let s = on().start(&rec, &tempo, 6.0, true, false).padded_preroll(6.0, 1024, 48_000);
        let c = s.count_in.unwrap().padded(1024, 48_000);
        assert!((c.length - 2.005333).abs() < 1e-6 && (c.length - c.first - 1.994667).abs() < 1e-6, "{c:?}");
        let s = on().start(&rec, &tempo, 6.0, false, false).padded_preroll(6.0, 1024, 48_000);
        assert_eq!(s.from, 6.0, "no pre-roll in playback here");
    }

    #[test]
    fn outputs_decode_like_hardware_outputs() {
        let m = Metronome { outputs: vec![2, 1028, 512], ..Metronome::default() };
        assert_eq!(m.device_outputs(), [(2, false), (4, true)]);
        assert_eq!(m.outputs_or(6), [(2, false), (4, true)]);
        let none = Metronome::default();
        assert_eq!(none.outputs_or(-1), []);
        assert_eq!(none.outputs_or(1030), [(6, true)]);
    }
}
