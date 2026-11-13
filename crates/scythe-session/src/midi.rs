//! MIDI devices, the virtual keyboard and synchronization for a session: which ports to open,
//! what gets clock, and what the transport chases.

use scythe_core::midi_route::{MidiDevice, MidiInput, VIRTUAL_KEYBOARD};
use scythe_core::prefs::MidiPrefs;
use scythe_engine::midi_io::{DEVICE_CACHE_FILE, MidiDevices};
use scythe_engine::midi_sync::{ChaseConfig, ChaseSource, ClockPort, SyncConfig, TimecodeRate};

use crate::Session;

/// Project extension state (section, key) naming a MIDI clock chase source, which REAPER's
/// `SMPTESYNC` has no place for: "all" or a device number.
pub const CLOCK_CHASE: (&str, &str) = ("scythe", "chase_clock");

impl Session {
    fn device_cache_path(&self) -> Option<std::path::PathBuf> {
        Some(self.prefs_file.as_ref()?.with_file_name(DEVICE_CACHE_FILE))
    }

    /// Looks for MIDI ports again, keeping the numbers known devices had, and saves the map
    /// beside the preferences.
    pub fn rescan_midi_devices(&mut self) {
        let path = self.device_cache_path();
        let text = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
        let known = if self.midi_devices.inputs.is_empty() && self.midi_devices.outputs.is_empty() {
            MidiDevices::parse_cache(&text)
        } else {
            self.midi_devices.clone()
        };
        let mut map = known;
        map.merge_present(&scythe_engine::midi_io::input_names(), &scythe_engine::midi_io::output_names());
        self.midi_devices = map;
        scythe_engine::midi_io::share_devices(&self.midi_devices);
        self.save_midi_devices();
        self.sync();
    }

    /// Writes the device map (with its aliases) to its file, keeping the file's other sections.
    pub fn save_midi_devices(&mut self) {
        let Some(path) = self.device_cache_path() else { return };
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let new = self.midi_devices.write_cache(&text);
        if new != text
            && let Err(e) =
                path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&path, new))
        {
            self.notify(format!("Cannot save {}: {e}", path.display()));
        }
    }

    /// Renames a device in the map (`None` clears the alias).
    pub fn set_midi_alias(&mut self, output: bool, number: u8, alias: Option<String>) -> bool {
        let list = if output { &mut self.midi_devices.outputs } else { &mut self.midi_devices.inputs };
        let Some(port) = list.iter_mut().find(|p| p.number == number) else { return false };
        port.alias = alias.filter(|a| !a.trim().is_empty());
        self.save_midi_devices();
        self.changed();
        true
    }

    /// Present, enabled inputs a track or the chaser listens to, as (number, port name).
    pub(crate) fn midi_inputs_needed(&self) -> Vec<(u8, String)> {
        let midi = &self.prefs.midi;
        let mut wanted: Vec<MidiDevice> = self
            .project
            .tracks
            .iter()
            .filter(|t| t.armed)
            .filter_map(|t| MidiInput::from_rec_input(t.rec_input))
            .map(|i| i.device)
            .filter(|d| *d != MidiDevice::VirtualKeyboard)
            .collect();
        if let Some(ChaseSource::Mtc(d) | ChaseSource::Clock(d)) = self.chase_config().map(|c| c.source) {
            wanted.push(d.map_or(MidiDevice::All, MidiDevice::Port));
        }
        let control = self.control_inputs_wanted();
        self.midi_devices
            .inputs
            .iter()
            .filter(|p| p.present && midi.input_enabled(p.number))
            .filter(|p| wanted.iter().any(|d| d.hears(p.number)) || control && midi.control_enabled(p.number))
            .map(|p| (p.number, p.name.clone()))
            .collect()
    }

    /// Present, enabled outputs a track sends to or that get clock.
    pub(crate) fn midi_outputs_needed(&self) -> Vec<(u8, String)> {
        let midi = &self.prefs.midi;
        let used = |n: u8| {
            MidiPrefs::bit(midi.clock, n) || self.project.tracks.iter().any(|t| t.midi_out.is_some_and(|(d, _)| d == n))
        };
        self.midi_devices
            .outputs
            .iter()
            .filter(|p| p.present && midi.output_enabled(p.number) && used(p.number))
            .map(|p| (p.number, p.name.clone()))
            .collect()
    }

    /// What the audio thread does about synchronization: clock to the outputs that want it,
    /// and chasing when the project enables it.
    pub fn sync_config(&self) -> SyncConfig {
        let midi = &self.prefs.midi;
        let clock = self
            .midi_outputs_needed()
            .into_iter()
            .map(|(port, _)| ClockPort {
                port,
                clock: MidiPrefs::bit(midi.clock, port),
                spp: !MidiPrefs::bit(midi.clock_no_spp, port),
                reset: !MidiPrefs::bit(midi.no_reset, port),
            })
            .collect();
        SyncConfig { clock, chase: self.chase_config() }
    }

    fn chase_config(&self) -> Option<ChaseConfig> {
        let s = &self.project.timecode_sync;
        if !s.enabled() {
            return None;
        }
        let device = |v: i64| {
            let d = ((v >> 5) & 127) as u8;
            (d != 63).then_some(d)
        };
        let source = match self.project.ext_state(CLOCK_CHASE.0, CLOCK_CHASE.1) {
            Some(v) => ChaseSource::Clock(v.trim().parse::<u8>().ok()),
            None if s.input & scythe_core::midi_route::MIDI_INPUT_FLAG != 0 => ChaseSource::Mtc(device(s.input)),
            None => ChaseSource::Ltc {
                channel: (s.input.max(0) & 1023) as usize,
                rate: TimecodeRate::from_rpp(s.frame_rate, (s.frame_rate.fract() != 0.0) as i64),
            },
        };
        Some(ChaseConfig {
            source,
            offset: s.offset,
            resync: s.resync_ms.max(1) as f64 / 1000.0,
            freewheel: s.freewheel_ms.max(1) as f64 / 1000.0,
        })
    }

    /// Plays a message on the virtual MIDI keyboard (device 62): tracks armed with it as their
    /// input hear it, and it records like any MIDI input.
    pub fn virtual_keyboard(&mut self, bytes: [u8; 3]) {
        if let Some(steps) = &mut self.learn.step_input
            && bytes[0] & 0xf0 == 0x90
            && bytes[2] > 0
        {
            steps.push(bytes);
        }
        self.engine.virtual_keyboard(bytes);
    }

    /// Turns MIDI editor step input on or off; while on, [`Session::take_step_notes`] hands
    /// over the note-ons played since it was last called.
    pub fn set_step_input(&mut self, on: bool) {
        if on != self.learn.step_input.is_some() {
            self.learn.step_input = on.then(Vec::new);
            self.sync();
        }
    }

    pub fn take_step_notes(&mut self) -> Vec<[u8; 3]> {
        self.learn.step_input.as_mut().map(std::mem::take).unwrap_or_default()
    }

    /// Whether some armed track listens to the virtual keyboard.
    pub fn keyboard_in_use(&self) -> bool {
        self.project.tracks.iter().any(|t| {
            t.armed && MidiInput::from_rec_input(t.rec_input).is_some_and(|i| i.device.number() == VIRTUAL_KEYBOARD)
        })
    }
}
