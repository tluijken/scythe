//! The project metronome as REAPER stores it: the `<METRONOME flags count-in` chunk and the
//! `METRONOMEOUT` lines after it.
//!
//! REAPER 7.73 keeps these in the project (`projmetroen`, `projmetrocountin`; the bits match the
//! checkboxes of Options > Metronome/pre-roll settings), while pre-roll (`preroll`, `prerollmeas`)
//! and the default metronome output (`metronome_defout`) are global preferences in reaper.ini.

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

/// Longest count-in accepted, in measures.
pub const MAX_COUNT_IN: f64 = 16.0;

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
    /// default: the master's outputs.
    pub outputs: Vec<i64>,
}

impl Default for Metronome {
    fn default() -> Self {
        Self { flags: RUN_ON_PLAY | RUN_ON_RECORD, count_in_measures: 2.0, volume: 0.25, outputs: Vec::new() }
    }
}

/// How a count-in clicks: `beats` beats of `beat` seconds, accented every `beats_per_bar`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CountIn {
    pub beats: u32,
    pub beat: f64,
    pub beats_per_bar: u32,
}

impl CountIn {
    pub fn seconds(&self) -> f64 {
        self.beats as f64 * self.beat
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

    /// Whether starting playback (or recording) counts in first.
    pub fn counts_in(&self, recording: bool) -> bool {
        self.has(if recording { COUNT_IN_RECORD } else { COUNT_IN_PLAY }) && self.count_in_measures > 0.0
    }

    /// The count-in before `at`: the configured measures at the tempo and time signature in
    /// force there, rounded to whole beats (at least one).
    pub fn count_in_at(&self, tempo: &TempoMap, at: f64) -> CountIn {
        let (num, denom) = tempo.time_sig_at(at);
        let num = num.max(1);
        let beat = 60.0 / tempo.bpm_at(at).max(1.0) * 4.0 / denom.max(1) as f64;
        let measures = self.count_in_measures.clamp(0.0, MAX_COUNT_IN);
        let beats = ((measures * num as f64).round() as u32).max(1);
        CountIn { beats, beat, beats_per_bar: num }
    }

    /// The outputs as (first device output from 0, mono), leaving out ReaRoute channels.
    pub fn device_outputs(&self) -> Vec<(usize, bool)> {
        self.outputs
            .iter()
            .filter(|&&o| o >= 0 && o & crate::HwOut::REAROUTE == 0)
            .map(|&o| ((o & 1023) as usize, o & crate::HwOut::MONO != 0))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_a_new_reaper_project() {
        let m = Metronome::default();
        assert_eq!((m.flags, m.count_in_measures, m.volume), (6, 2.0, 0.25));
        assert!(!m.enabled() && !m.clicks(false) && !m.counts_in(false));
    }

    #[test]
    fn count_in_follows_the_tempo_where_playback_starts() {
        let mut tempo = TempoMap::constant(120.0, 4, 4);
        tempo.points =
            vec![crate::tempo::TempoPoint { time_sig: Some((6, 8)), ..crate::tempo::TempoPoint::new(10.0, 90.0) }];
        let m = Metronome { count_in_measures: 1.0, ..Metronome::default() };
        assert_eq!(m.count_in_at(&tempo, 0.0), CountIn { beats: 4, beat: 0.5, beats_per_bar: 4 });
        let late = m.count_in_at(&tempo, 20.0);
        assert_eq!((late.beats, late.beats_per_bar), (6, 6));
        assert!((late.beat - 60.0 / 90.0 / 2.0).abs() < 1e-12);
        assert_eq!(Metronome { count_in_measures: 0.5, ..m }.count_in_at(&tempo, 0.0).beats, 2);
    }

    #[test]
    fn outputs_decode_like_hardware_outputs() {
        let m = Metronome { outputs: vec![2, 1028, 512], ..Metronome::default() };
        assert_eq!(m.device_outputs(), [(2, false), (4, true)]);
    }
}
