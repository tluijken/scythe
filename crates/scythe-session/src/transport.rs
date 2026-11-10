use std::path::{Path, PathBuf};

use scythe_core::prefs::{AUTOXFADE_LOOP_TAKES, AUTOXFADE_RECORD_LANES};
use scythe_core::recording::{
    InputFormat, OutputFormat, ProjectRecordMode, RecordInto, RecordMode, RecordPass, add_recording, last_record_pass,
    merge_midi_recording, place_midi_recording, record_file_name, record_path, record_segments,
};
use scythe_core::{Item, ItemId, Take, TimeRange, Track, TrackId};
use scythe_engine::midi_io::{LiveMidi, RecInput, midi_input_channel};
use scythe_engine::record::{OutputRecording, OutputTarget, RecordTarget, Recording};

use crate::{Selection, Session};

pub struct ActiveRecording {
    /// Audio input, when an armed track records audio.
    pub session: Option<Recording>,
    /// Track outputs, when an armed track records its output.
    pub(crate) output: Option<OutputRecording>,
    pub pass: RecordPass,
    /// Every armed track.
    pub tracks: Vec<TrackId>,
    /// Tracks recording audio, in the order of the session's levels.
    pub(crate) audio_tracks: Vec<TrackId>,
    /// Tracks recording MIDI, with the channel they listen to (`None` for all).
    pub(crate) midi_tracks: Vec<(TrackId, RecInput)>,
    /// Tracks recording the MIDI their effects put out ("Output (MIDI)").
    pub(crate) midi_output_tracks: Vec<TrackId>,
    /// MIDI received so far, with the project frame it arrived at.
    pub(crate) midi: Vec<(i64, LiveMidi)>,
    /// Seconds recorded before the playhead reached the start: count-in and pre-roll.
    pub(crate) lead_in: f64,
    /// Where a pre-roll started the playhead, before the start.
    pub(crate) preroll_from: Option<f64>,
    /// What each track keeps: everything (`None`) or what falls inside these ranges.
    pub(crate) punch: Vec<(TrackId, Option<Vec<TimeRange>>)>,
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
    /// or stops and adds the takes. Each track records what its record mode says; the project's
    /// record mode punches in to the time selection or the selected items
    /// ([`Session::record_items`]).
    pub fn toggle_record(&mut self) {
        if let Some(rec) = self.recording.take() {
            self.finish_recording(rec);
            return;
        }

        let armed: Vec<Track> = self.project.tracks.iter().filter(|t| t.armed).cloned().collect();
        if armed.is_empty() {
            self.notify("Arm a track to record");
            return;
        }
        let is_midi = |t: &Track| {
            midi_input_channel(t.rec_input).is_some()
                && !t.rec_mode.is_output()
                && !matches!(
                    t.rec_mode,
                    RecordMode::InputForced(InputFormat::Mono | InputFormat::Stereo | InputFormat::Multichannel)
                )
        };
        let records =
            |t: &&Track| !matches!(t.rec_mode, RecordMode::Disabled | RecordMode::MidiOutput | RecordMode::Other(_));
        let midi_armed: Vec<&Track> = armed.iter().filter(records).filter(|t| is_midi(t)).collect();
        let output_armed: Vec<&Track> = armed.iter().filter(|t| t.rec_mode.is_output()).collect();
        let audio_armed: Vec<&Track> = armed
            .iter()
            .filter(records)
            .filter(|t| {
                !is_midi(t)
                    && !t.rec_mode.is_output()
                    && !t.rec_mode.is_midi_overdub()
                    && t.rec_mode != RecordMode::InputForced(InputFormat::Midi)
            })
            .collect();
        let midi_output: Vec<TrackId> =
            armed.iter().filter(|t| t.rec_mode == RecordMode::MidiOutput).map(|t| t.id).collect();
        self.engine.start_midi_output_recording(&midi_output);
        let dir = self.record_dir();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let number = |t: &Track| self.project.tracks.iter().position(|x| x.id == t.id).map_or(0, |i| i + 1);
        let mut taken = Vec::new();
        let mut file_for = |t: &Track| {
            let path = unique_path(&dir, &record_file_name(number(t), &t.name, now), &taken);
            taken.push(path.clone());
            path
        };
        let targets: Vec<RecordTarget> = audio_armed
            .iter()
            .map(|t| RecordTarget { track: t.id, channels: input_channels(t), path: file_for(t) })
            .collect();
        let outputs: Vec<OutputTarget> = output_armed
            .iter()
            .map(|t| {
                let RecordMode::Output { format, compensated } = t.rec_mode else { unreachable!() };
                let channels = match format {
                    OutputFormat::Mono => 1,
                    OutputFormat::Stereo => 2,
                    OutputFormat::Multichannel => t.channels.max(2) as usize,
                };
                OutputTarget { track: t.id, point: t.rec_tap, compensated, channels, path: file_for(t) }
            })
            .collect();
        let session = if targets.is_empty() {
            None
        } else {
            let tap = self.engine.record_input().map_err(scythe_engine::record::RecordError::Device);
            match tap.and_then(|tap| Recording::start(tap, targets)) {
                Ok(s) => Some(s),
                Err(e) => {
                    self.notify(format!("Cannot record: {e}"));
                    return;
                }
            }
        };
        let output = if outputs.is_empty() {
            None
        } else {
            match self.engine.start_output_recording(outputs) {
                Ok(o) => Some(o),
                Err(e) => {
                    self.notify(format!("Cannot record the track output: {e}"));
                    return;
                }
            }
        };
        self.sync();
        if !midi_armed.is_empty() {
            let keyboard_only = midi_armed.iter().all(|t| {
                RecInput::from_rec_input(t.rec_input)
                    .is_some_and(|i| i.device == scythe_core::midi_route::MidiDevice::VirtualKeyboard)
            });
            if self.engine.midi_input_names().is_empty() && !keyboard_only {
                self.notify("Cannot record MIDI: no MIDI input is open");
                if session.is_none() && output.is_none() {
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
        let mode = ProjectRecordMode::of(&self.project);
        let pass = RecordPass {
            start,
            looped: selection.filter(|_| self.project.loop_enabled),
            punch: selection.filter(|_| mode == ProjectRecordMode::TimeSelectionPunch),
        };
        let punch = armed
            .iter()
            .map(|t| {
                let ranges = match mode {
                    ProjectRecordMode::Normal => None,
                    ProjectRecordMode::TimeSelectionPunch => Some(selection.into_iter().collect()),
                    ProjectRecordMode::ItemPunch => Some(
                        t.items
                            .iter()
                            .filter(|i| self.record_items.contains(&i.id))
                            .map(|i| TimeRange::new(i.position, i.end()))
                            .collect(),
                    ),
                };
                (t.id, ranges)
            })
            .collect();
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
        self.notify(match (from, midi.is_empty(), output.is_some()) {
            (Some(d), true, _) => format!("Recording from {d}"),
            (Some(d), false, _) => format!("Recording from {d} and {midi}"),
            (None, _, true) => "Recording track output".to_owned(),
            (None, true, false) if midi_armed.is_empty() => "Armed tracks only monitor".to_owned(),
            (None, _, false) => format!("Recording MIDI from {midi}"),
        });
        self.recording = Some(ActiveRecording {
            session,
            output,
            pass,
            tracks: armed.iter().map(|t| t.id).collect(),
            audio_tracks: audio_armed.iter().map(|t| t.id).collect(),
            midi_tracks: midi_armed
                .iter()
                .filter_map(|t| Some((t.id, RecInput::from_rec_input(t.rec_input)?)))
                .collect(),
            midi_output_tracks: midi_output,
            midi: Vec::new(),
            lead_in,
            preroll_from,
            punch,
        });
    }

    /// Sets the project's record mode (`RECMODE`).
    pub fn set_record_mode(&mut self, mode: ProjectRecordMode) {
        self.edit("Set record mode", |p| mode.set(p));
        self.notify(format!("Record mode: {}", mode.label().to_lowercase()));
    }

    /// Where recordings of this project go: its folder and the record path (`RECORD_PATH`).
    pub fn record_dir(&self) -> PathBuf {
        match self.project.path.as_deref().and_then(Path::parent) {
            Some(d) => d.join(record_path(&self.project)),
            None => crate::data_dir().join("recordings"),
        }
    }

    /// How a recording goes into `track`: a lane per pass when the preferences say new
    /// recording adds lanes (loop recording keeps takes if they ask for that), else takes in
    /// the lane recording goes into.
    fn record_into(&self, track: &Track, looped: bool) -> RecordInto {
        let lanes = self.prefs.autoxfade_bit(AUTOXFADE_RECORD_LANES)
            && !(looped && self.prefs.autoxfade_bit(AUTOXFADE_LOOP_TAKES));
        if track.fixed_lanes && lanes {
            return RecordInto::NewLanes;
        }
        let comp = track.comp_lane();
        let lane = match track.lane_rec.map(|r| r[0]) {
            Some(l) if l >= 0 => l as usize,
            _ if track.fixed_lanes => track.playing_lanes().into_iter().find(|&l| Some(l) != comp).unwrap_or(0),
            _ => 0,
        };
        RecordInto::Takes { lane }
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
        let punch_of = |track: TrackId| rec.punch.iter().find(|p| p.0 == track).and_then(|p| p.1.clone());
        let mut midi_items: Vec<(TrackId, Item)> = Vec::new();
        for &(track, input) in &rec.midi_tracks {
            let events: Vec<(f64, [u8; 3], u8)> = rec
                .midi
                .iter()
                .filter(|(_, m)| input.hears(m.device, m.channel()))
                .map(|(frame, m)| (*frame as f64 / sr, m.bytes, m.len))
                .collect();
            let mut pass = rec.pass;
            if let Some(ranges) = punch_of(track) {
                // MIDI keeps one range: the time selection, or the first selected item.
                match ranges.first() {
                    Some(r) => pass.punch = Some(*r),
                    None => continue,
                }
            }
            if let Some(item) = place_midi_recording(&events, pass, stop, &timeline) {
                midi_items.push((track, item));
            }
        }
        for &track in &rec.midi_output_tracks {
            let events: Vec<(f64, [u8; 3], u8)> = self
                .engine
                .take_midi_output(track, true)
                .into_iter()
                .map(|(frame, b)| (frame as f64 / sr, b, scythe_engine::midi_io::message_len(b[0]) as u8))
                .collect();
            let mut pass = rec.pass;
            if let Some(ranges) = punch_of(track) {
                match ranges.first() {
                    Some(r) => pass.punch = Some(*r),
                    None => continue,
                }
            }
            if let Some(item) = place_midi_recording(&events, pass, stop, &timeline) {
                midi_items.push((track, item));
            }
        }
        let mut files: Vec<(TrackId, PathBuf, f64, f64)> = Vec::new();
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
                        files.push((
                            take.track,
                            take.path.clone(),
                            take.frames as f64 / take.sample_rate as f64,
                            offset,
                        ));
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
        if let Some(output) = rec.output {
            match self.engine.finish_output_recording(output) {
                Ok(outs) => {
                    for o in outs {
                        // The graph stamps the first frame with its project time.
                        let Some(start) = o.start else { continue };
                        files.push((o.track, o.path, o.frames as f64 / o.sample_rate as f64, rec.pass.start - start));
                    }
                }
                Err(e) => {
                    self.notify(format!("Recording the track output failed: {e}"));
                    failed = true;
                }
            }
            self.sync();
        }
        let first_pass = last_record_pass(&self.project) + 1;
        let looped = rec.pass.looped.is_some();
        let mut placed: Vec<(TrackId, RecordInto, Vec<scythe_core::recording::Segment>, usize, PathBuf, f64)> =
            Vec::new();
        for (track, path, len, offset) in files {
            let punch = punch_of(track);
            if punch.as_ref().is_some_and(Vec::is_empty) {
                continue;
            }
            let (segments, passes) = record_segments(len, offset, rec.pass, punch.as_deref().unwrap_or(&[]));
            let Some(t) = self.project.track(track) else { continue };
            if !segments.is_empty() {
                placed.push((track, self.record_into(t, looped), segments, passes, path, len));
            }
        }
        let count = placed.len() + midi_items.len();
        let midi_count = midi_items.len();
        if count > 0 {
            let modes: Vec<(TrackId, RecordMode)> = self.project.tracks.iter().map(|t| (t.id, t.rec_mode)).collect();
            let cc_timeout = self.prefs.cc_touch_timeout();
            let mut first: Option<(TrackId, ItemId)> = None;
            self.edit("Record", |p| {
                for (track, into, segments, passes, path, len) in placed {
                    let Some(t) = p.track_mut(track) else { continue };
                    let take = |offset: f64| {
                        let mut tk = Take::from_file(path.clone());
                        tk.source_offset = offset;
                        tk
                    };
                    let ids = add_recording(t, &segments, passes, &take, into, first_pass, len);
                    if let (None, Some(&id)) = (first, ids.first()) {
                        first = Some((track, id));
                    }
                }
                for (track, item) in midi_items {
                    let mode = modes.iter().find(|m| m.0 == track).map_or(RecordMode::Input, |m| m.1);
                    let Some(t) = p.track_mut(track) else { continue };
                    let id = merge_midi_recording(t, item, mode, &timeline, cc_timeout);
                    first.get_or_insert((track, id));
                }
            });
            self.select = first.map(|(t, i)| Selection { track: Some(t), item: Some(i) });
            let midi = if midi_count > 0 { format!(", {midi_count} MIDI") } else { String::new() };
            self.notify(format!("Recorded {count} take(s){midi}{note}"));
            self.request_media();
        } else if !failed {
            self.notify("Nothing was recorded");
        }
        self.engine.seek(self.project.cursor);
    }

    /// Selects `track` in the session's eyes; tracks set to arm when selected (`AUTO_RECARM`)
    /// arm, and those that armed that way disarm when another track is selected.
    pub fn set_selected_track(&mut self, track: Option<TrackId>) {
        if self.selected_track == track {
            return;
        }
        self.selected_track = track;
        if self.is_recording() {
            return;
        }
        let mut changed = false;
        for t in self.project.tracks.iter_mut().filter(|t| t.auto_arm) {
            let arm = Some(t.id) == track;
            changed |= t.armed != arm;
            t.armed = arm;
        }
        if changed {
            self.sync();
        }
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

/// The input channels an audio recording writes, as the track's record mode forces them.
fn input_channels(t: &Track) -> Vec<usize> {
    let channels = RecordTarget::channels_for_input(t.rec_input);
    match t.rec_mode {
        RecordMode::InputForced(InputFormat::Mono) => channels[..1].to_vec(),
        RecordMode::InputForced(InputFormat::Stereo) if channels.len() == 1 => vec![channels[0], channels[0]],
        RecordMode::InputForced(InputFormat::Stereo) => channels[..2].to_vec(),
        _ => channels,
    }
}

/// `dir/name.wav`, or `name-001.wav` and on when that file exists or is taken already.
fn unique_path(dir: &Path, name: &str, taken: &[PathBuf]) -> PathBuf {
    let free = |p: &PathBuf| !p.exists() && !taken.contains(p);
    let plain = dir.join(format!("{name}.wav"));
    if free(&plain) {
        return plain;
    }
    (1..).map(|i| dir.join(format!("{name}-{i:03}.wav"))).find(free).unwrap_or(plain)
}
