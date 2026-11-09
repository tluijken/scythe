use std::path::Path;

use scythe_core::recording::{RecordPass, place_midi_recording, place_recording};
use scythe_core::{Item, TimeRange, TrackId};
use scythe_engine::midi_io::{LiveMidi, RecInput, midi_input_channel};
use scythe_engine::record::{RecordTarget, Recording};

use crate::{Selection, Session};

pub struct ActiveRecording {
    /// Audio input, when an armed track records audio.
    pub session: Option<Recording>,
    pub pass: RecordPass,
    /// Every armed track.
    pub tracks: Vec<TrackId>,
    /// Tracks recording audio, in the order of the session's levels.
    pub(crate) audio_tracks: Vec<TrackId>,
    /// Tracks recording MIDI, with the channel they listen to (`None` for all).
    pub(crate) midi_tracks: Vec<(TrackId, RecInput)>,
    /// MIDI received so far, with the project frame it arrived at.
    pub(crate) midi: Vec<(i64, LiveMidi)>,
    /// Seconds recorded before the playhead reached the start: count-in and pre-roll.
    pub(crate) lead_in: f64,
    /// Where a pre-roll started the playhead, before the start.
    pub(crate) preroll_from: Option<f64>,
}

impl Session {
    pub fn position(&self) -> f64 {
        if self.engine.transport().is_playing() {
            self.engine.transport().position_seconds()
        } else {
            self.project.cursor
        }
    }

    pub fn is_playing(&self) -> bool {
        self.engine.transport().is_playing()
    }

    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    /// Starts recording every armed track from the edit cursor (or the playhead while playing),
    /// or stops and adds the takes.
    pub fn toggle_record(&mut self) {
        if let Some(rec) = self.recording.take() {
            self.finish_recording(rec);
            return;
        }

        let armed: Vec<_> = self.project.tracks.iter().filter(|t| t.armed).cloned().collect();
        if armed.is_empty() {
            self.notify("Arm a track to record");
            return;
        }
        let (midi_armed, audio_armed): (Vec<_>, Vec<_>) =
            armed.iter().partition(|t| midi_input_channel(t.rec_input).is_some());
        let dir = match self.project.path.as_deref().and_then(Path::parent) {
            Some(d) => d.to_owned(),
            None => crate::data_dir().join("recordings"),
        };
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let targets: Vec<RecordTarget> = audio_armed
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let base: String = if t.name.is_empty() { format!("Track {}", i + 1) } else { t.name.clone() };
                let safe: String =
                    base.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' }).collect();
                RecordTarget {
                    track: t.id,
                    channels: RecordTarget::channels_for_input(t.rec_input),
                    path: dir.join(format!("{}-{stamp}.wav", safe.trim())),
                }
            })
            .collect();
        let session = if targets.is_empty() {
            None
        } else {
            match Recording::start_on(self.engine.devices(), targets, self.engine.sample_rate()) {
                Ok(s) => Some(s),
                Err(e) => {
                    self.notify(format!("Cannot record: {e}"));
                    return;
                }
            }
        };
        if !midi_armed.is_empty() {
            self.sync();
            let keyboard_only = midi_armed.iter().all(|t| {
                RecInput::from_rec_input(t.rec_input)
                    .is_some_and(|i| i.device == scythe_core::midi_route::MidiDevice::VirtualKeyboard)
            });
            if self.engine.midi_input_names().is_empty() && !keyboard_only {
                self.notify("Cannot record MIDI: no MIDI input is open");
                if session.is_none() {
                    return;
                }
            }
            // Whatever was played before recording started is not part of the take.
            self.engine.take_recorded_midi();
        }
        // Recording during playback punches in at the playhead.
        let playing = self.is_playing();
        let start = if playing { self.engine.transport().position_seconds() } else { self.project.cursor };
        let selection = self.project.time_selection.filter(|r| !r.is_empty());
        let pass = RecordPass {
            start,
            looped: selection.filter(|_| self.project.loop_enabled),
            punch: selection.filter(|_| self.auto_punch),
        };
        let (lead_in, preroll_from) = match self.counting_in() {
            Some((before, _, _)) => (-before, None),
            None if playing => (0.0, None),
            None => {
                let lead_in = self.start_playback(start, true, false);
                let from = self.engine.transport().position_seconds();
                (lead_in, (from < start).then_some(from))
            }
        };
        self.engine.set_recording(true);
        let from = session.as_ref().map(|s| s.device.clone());
        let midi = self.engine.midi_input_names().join(", ");
        self.notify(match (from, midi.is_empty()) {
            (Some(d), true) => format!("Recording from {d}"),
            (Some(d), false) => format!("Recording from {d} and {midi}"),
            (None, _) => format!("Recording MIDI from {midi}"),
        });
        self.recording = Some(ActiveRecording {
            session,
            pass,
            tracks: armed.iter().map(|t| t.id).collect(),
            audio_tracks: audio_armed.iter().map(|t| t.id).collect(),
            midi_tracks: midi_armed
                .iter()
                .filter_map(|t| Some((t.id, RecInput::from_rec_input(t.rec_input)?)))
                .collect(),
            midi: Vec::new(),
            lead_in,
            preroll_from,
        });
    }

    fn finish_recording(&mut self, mut rec: ActiveRecording) {
        let stop = self.engine.transport().position_seconds();
        self.engine.stop();
        self.engine.set_recording(false);
        rec.midi.extend(self.engine.take_recorded_midi());
        if let Some(from) = rec.preroll_from {
            // What was played along to the pre-roll is not part of the take, as in REAPER.
            let (sr, start) = (self.engine.sample_rate() as f64, rec.pass.start);
            let looped = rec.pass.looped.filter(|r| r.start < start);
            rec.midi.retain(|(frame, _)| {
                let t = *frame as f64 / sr;
                !(from - 1e-9..start).contains(&t) || looped.is_some_and(|r| t >= r.start)
            });
        }
        let sr = self.engine.sample_rate() as f64;
        let timeline = self.project.tempo.timeline();
        let mut items: Vec<(TrackId, Item)> = Vec::new();
        for &(track, input) in &rec.midi_tracks {
            let events: Vec<(f64, [u8; 3], u8)> = rec
                .midi
                .iter()
                .filter(|(_, m)| input.hears(m.device, m.channel()))
                .map(|(frame, m)| (*frame as f64 / sr, m.bytes, m.len))
                .collect();
            if let Some(item) = place_midi_recording(&events, rec.pass, stop, &timeline) {
                items.push((track, item));
            }
        }
        let midi_count = items.len();
        let mut note = String::new();
        let mut failed = false;
        if let Some(session) = rec.session {
            let overruns = session.overruns();
            match session.finish() {
                Ok(takes) => {
                    // What was played along to left the speakers `output_latency` late and
                    // reached the file `input_latency` after that.
                    let output_latency = self.engine.transport().output_latency();
                    for take in &takes {
                        let offset = take.input_latency + output_latency + rec.lead_in;
                        let len = take.frames as f64 / take.sample_rate as f64;
                        if let Some(item) = place_recording(take.path.clone(), len, offset, rec.pass) {
                            items.push((take.track, item));
                        }
                    }
                    if overruns > 0 {
                        note = format!(" ({overruns} samples dropped)");
                    }
                }
                Err(e) => {
                    self.notify(format!("Recording failed: {e}"));
                    failed = true;
                }
            }
        }
        let first = items.first().map(|(t, it)| Selection { track: Some(*t), item: Some(it.id) });
        let count = items.len();
        if count > 0 {
            self.edit("Record", |p| {
                for (track, item) in items {
                    if let Some(t) = p.track_mut(track) {
                        t.items.push(item);
                    }
                }
            });
            self.select = first;
            let midi = if midi_count > 0 { format!(", {midi_count} MIDI") } else { String::new() };
            self.notify(format!("Recorded {count} take(s){midi}{note}"));
            self.request_media();
        } else if !failed {
            self.notify("Nothing was recorded");
        }
        self.engine.seek(self.project.cursor);
    }

    /// Play/pause: pausing leaves the edit cursor where playback stopped.
    pub fn toggle_play(&mut self) {
        self.live.driving = false;
        if self.is_playing() {
            self.project.cursor = self.engine.transport().position_seconds();
            self.engine.stop();
        } else {
            self.start_playback(self.project.cursor, false, false);
        }
    }

    /// Plays (or starts the transport for recording) from `at` as REAPER does: pre-roll as the
    /// preferences ask, then the project's count-in, or a count-in in any case with
    /// `force_count_in`. Answers the seconds until the playhead reaches `at`.
    pub fn start_playback(&mut self, at: f64, recording: bool, force_count_in: bool) -> f64 {
        let start = self.project.metronome.start(&self.prefs, &self.project.tempo, at, recording, force_count_in);
        self.engine.seek(start.from);
        match &start.count_in {
            Some(count_in) => self.engine.play_counting_in(count_in),
            None => self.engine.play(),
        }
        start.until(at)
    }

    /// While counting in: how far before the start playback is (negative seconds), and the
    /// beat sounding (from 1) of how many.
    pub fn counting_in(&self) -> Option<(f64, u32, u32)> {
        self.engine.transport().count_in()
    }

    /// Stop returns the play cursor to the edit cursor, as in REAPER.
    pub fn stop(&mut self) {
        self.live.driving = false;
        if self.is_recording() {
            self.toggle_record();
            return;
        }
        self.engine.stop();
        self.engine.seek(self.project.cursor);
    }

    pub fn set_cursor(&mut self, seconds: f64) {
        self.project.cursor = seconds.max(0.0);
        self.engine.seek(self.project.cursor);
    }

    pub fn toggle_loop(&mut self) {
        let enabled = !self.project.loop_enabled;
        self.edit(if enabled { "Enable repeat" } else { "Disable repeat" }, |p| p.loop_enabled = enabled);
    }

    pub fn set_loop_range(&mut self, range: Option<TimeRange>) {
        self.gesture("Set loop points", |p| p.time_selection = range);
    }
}
