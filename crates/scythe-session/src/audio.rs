//! The audio device the session plays on: chosen in the preferences as REAPER's Preferences >
//! Audio > Device page does, switched while the project stays open, and the names of its
//! channels with the aliases REAPER lets you give them.

use std::sync::Arc;

use scythe_core::ini::Ini;
use scythe_core::prefs::{
    ALIAS_INPUTS, ALIAS_OUTPUTS, AudioDevicePrefs, AudioSystem, CLOSE_INACTIVE, CLOSE_INACTIVE_ARMED, ChannelAliases,
    JACK_NO_CONNECT, JACK_NO_START,
};
use scythe_engine::{DeviceRequest, Engine, MediaPool};

use crate::Session;

/// The devices `prefs` choose (no choice is the system default).
pub fn request_from_prefs(prefs: &AudioDevicePrefs) -> DeviceRequest {
    let Some(system) = prefs.system else { return DeviceRequest::default() };
    let channels = |n: Option<u32>| n.map(|n| n.min(u16::MAX as u32) as u16);
    let mut request = DeviceRequest {
        host: Some(system.host().to_owned()),
        sample_rate: prefs.sample_rate,
        buffer_size: prefs.block_size,
        output_channels: channels(prefs.output_channels),
        input_channels: channels(prefs.input_channels),
        ..Default::default()
    };
    if system == AudioSystem::Alsa {
        request.output = prefs.output_device.clone();
        request.input = prefs.input_device.clone();
    }
    if system != AudioSystem::Jack {
        request.bits = prefs.bits.map(|b| b as u16);
    }
    match system {
        AudioSystem::Alsa => {}
        // REAPER opens its PulseAudio streams as 32-bit integer, its page offering no bit depth.
        AudioSystem::PulseAudio => request.bits = Some(prefs.bits.unwrap_or(32) as u16),
        AudioSystem::Jack => {
            request.sample_rate = None;
            let options = prefs.jack_options.unwrap_or(JACK_NO_START);
            request.jack_start = (options & JACK_NO_START == 0).then(|| prefs.jack_launch.clone().unwrap_or_default());
            request.jack_no_connect = options & JACK_NO_CONNECT != 0;
        }
        AudioSystem::Dummy => request.input_channels = Some(0),
    }
    request
}

/// What the preferences keep of `request`.
pub fn prefs_from_request(request: &DeviceRequest, keep: &AudioDevicePrefs) -> AudioDevicePrefs {
    let system = match request.host.as_deref() {
        Some(host) => AudioSystem::from_host(host),
        None => Some(AudioSystem::Alsa),
    };
    AudioDevicePrefs {
        system,
        input_device: request.input.clone(),
        output_device: request.output.clone(),
        input_channels: request.input_channels.map(u32::from),
        output_channels: request.output_channels.map(u32::from),
        sample_rate: request.sample_rate,
        block_size: request.buffer_size,
        bits: request.bits.map(u32::from).or(keep.bits),
        ..keep.clone()
    }
}

/// Opens `request`: the engine, and why the device could not be opened. As REAPER does, a
/// device that cannot be opened leaves the audio closed (Scythe's clock runs on silently) rather
/// than playing on another device; the engine looks for it again every few seconds.
pub fn start_engine(pool: Arc<MediaPool>, request: &DeviceRequest) -> (Engine, Option<String>) {
    let engine = Engine::start_with(pool, request);
    let error = engine.info().fallback_reason.clone();
    (engine, error)
}

impl Session {
    /// The devices the preferences choose, as this session would open them.
    pub fn preferred_devices(&self) -> DeviceRequest {
        DeviceRequest { no_input: self.no_input, ..request_from_prefs(&self.prefs.audio) }
    }

    /// Switches to the devices `request` names without closing the project: the transport
    /// stops, effects keep their state, and the media is decoded again when the rate changes.
    /// When they cannot be opened the previous device plays on and the reason comes back.
    /// With `save` the choice goes into the preferences for the next start.
    pub fn change_audio_device(&mut self, request: DeviceRequest, save: bool) -> Result<(), String> {
        if self.is_recording() {
            return Err("stop recording before changing the audio device".into());
        }
        let request = DeviceRequest { no_input: request.no_input || self.no_input, ..request };
        self.stop();
        self.engine.store_plugin_states(&mut self.project);
        let rate = self.engine.sample_rate();
        let pool = self.engine.pool().clone();
        // Many devices allow one stream at a time, so the old one closes before the new opens.
        self.engine.release_device();
        let engine = match Engine::open(pool.clone(), &request) {
            Ok(engine) => engine,
            Err(e) => {
                if let Err(again) = self.engine.reopen_device() {
                    self.notify(format!("The previous audio device did not come back: {again}"));
                }
                return Err(e);
            }
        };
        if engine.sample_rate() != rate {
            pool.clear();
        }
        let mut engine = engine;
        engine.set_default_click_output(self.prefs.metronome_default_output);
        engine.set_monitor_fx(self.engine.fx().monitor_fx.clone());
        engine.set_marker_actions(self.engine.marker_actions());
        let old = std::mem::replace(&mut self.engine, engine);
        drop(old);
        self.audio_closed = false;
        self.preview = None;
        self.reset_device_errors();
        self.apply_channel_maps();
        self.engine.seek(self.project.cursor);
        self.sync();
        if save {
            let audio = prefs_from_request(&request, &self.prefs.audio);
            self.change_preferences(|p| p.audio = audio);
        }
        Ok(())
    }

    /// Applies the device page's process settings: the audio thread's real-time priority (ALSA's
    /// "RT priority"), locking memory and holding the CPU out of deep idle states.
    pub(crate) fn apply_audio_options(&mut self) {
        let a = self.prefs.audio.clone();
        let alsa = a.system == Some(AudioSystem::Alsa);
        scythe_engine::realtime::set_priority(if alsa { a.rt_priority } else { None });
        if a.lock_memory == Some(true) && !self.memory_locked {
            match scythe_engine::realtime::lock_memory() {
                Ok(()) => self.memory_locked = true,
                Err(e) => self.notify(format!("Cannot lock the memory (mlockall): {e}")),
            }
        }
        let awake = a.disable_power_management == Some(true) && !self.audio_closed;
        if awake && self.cpu_awake.is_none() {
            match scythe_engine::realtime::hold_cpu_awake() {
                Ok(file) => self.cpu_awake = Some(file),
                Err(e) => {
                    self.notify(format!("Cannot disable power management: {e}"));
                    self.change_preferences(|p| p.audio.disable_power_management = Some(false));
                }
            }
        } else if !awake {
            self.cpu_awake = None;
        }
    }

    /// REAPER's "Close audio device when stopped and application is inactive" (and "... when
    /// stopped and active"): the window calls it every frame with whether it is the active
    /// application; the device closes while stopped and opens again on play or focus.
    pub fn idle_audio(&mut self, active: bool) {
        let a = &self.prefs.audio;
        let stopped = !self.engine.transport().is_playing() && !self.is_recording();
        let armed = self.project.tracks.iter().any(|t| t.armed);
        let bits = a.close_inactive.unwrap_or(0);
        let inactive = !active && bits & CLOSE_INACTIVE != 0 && (!armed || bits & CLOSE_INACTIVE_ARMED != 0);
        let close = stopped && (inactive || a.close_stopped == Some(true)) && !self.engine.info().host.is_empty();
        if close && !self.audio_closed {
            self.engine.close_device("audio device closed while stopped");
            self.audio_closed = true;
            self.apply_audio_options();
        } else if !close && self.audio_closed {
            self.audio_closed = false;
            if let Err(e) = self.engine.reopen_device() {
                self.notify(format!("The audio device did not open again: {e}"));
            }
            self.apply_audio_options();
            self.sync();
        }
    }

    /// REAPER's "Allow projects to override device sample rate": a project that asks for its own
    /// rate (`SAMPLERATE <rate> 1`) reopens the device at that rate. As in REAPER the device
    /// stays there when a project that asks for none is opened.
    pub(crate) fn follow_project_rate(&mut self) {
        let wanted = self.project.sample_rate.filter(|_| self.project.force_sample_rate);
        let Some(rate) = wanted.filter(|_| self.prefs.audio.project_rate_override()) else { return };
        let host = self.engine.info().host.clone();
        if rate == self.engine.sample_rate() || host.is_empty() || host.eq_ignore_ascii_case("JACK") {
            return;
        }
        let request = DeviceRequest { sample_rate: Some(rate), ..self.engine.devices().clone() };
        if let Err(e) = self.change_audio_device(request, false) {
            self.notify(format!("The project asks for {rate} Hz, which the device does not open at: {e}"));
        }
    }

    /// The input channels tracks see, by name: the device's (`Input 3`), or while input
    /// aliasing/remapping is on the mapped slots with their aliases.
    pub fn input_names(&self) -> Vec<String> {
        self.channel_names(true)
    }

    /// The output channels' names, like [`Session::input_names`].
    pub fn output_names(&self) -> Vec<String> {
        self.channel_names(false)
    }

    /// The current device's channel map and aliases, whether aliasing is on or not, for
    /// editing them: the device's own channels when none was made.
    pub fn channel_aliases(&self, input: bool) -> ChannelAliases {
        self.aliases
            .get(&self.alias_section(input))
            .cloned()
            .unwrap_or_else(|| ChannelAliases::identity(self.hardware_channels(input).len()))
    }

    /// Whether inputs (or outputs) go by their map and aliases (REAPER's "Input channel name
    /// aliasing/remapping").
    pub fn channel_aliasing(&self, input: bool) -> bool {
        self.prefs.channel_aliasing & if input { ALIAS_INPUTS } else { ALIAS_OUTPUTS } != 0
    }

    pub fn set_channel_aliasing(&mut self, input: bool, on: bool) {
        let bit = if input { ALIAS_INPUTS } else { ALIAS_OUTPUTS };
        self.change_preferences(|p| {
            p.channel_aliasing = if on { p.channel_aliasing | bit } else { p.channel_aliasing & !bit }
        });
        self.apply_channel_maps();
    }

    /// Names channel slot `index` of the current device (empty clears the alias) and turns
    /// aliasing on for that side, saved in the preferences as REAPER keeps it.
    pub fn set_channel_alias(&mut self, input: bool, index: usize, alias: &str) {
        let hardware = self.hardware_channels(input).len();
        let mut aliases = self.channel_aliases(input);
        aliases.set_alias(index, alias, hardware);
        self.set_channel_map(input, aliases);
    }

    /// Replaces the current device's channel map and aliases (REAPER's channel alias/mapping
    /// window), saves it and turns aliasing on for that side.
    pub fn set_channel_map(&mut self, input: bool, aliases: ChannelAliases) {
        let section = self.alias_section(input);
        let mut ini = self.preferences_ini();
        aliases.write(&mut ini, &section);
        self.aliases.insert(section, aliases);
        self.write_preferences_ini(&ini);
        if !self.channel_aliasing(input) {
            self.set_channel_aliasing(input, true);
        }
        self.apply_channel_maps();
        self.sync();
    }

    /// Hands the engine the input and output maps aliasing/remapping asks for.
    pub(crate) fn apply_channel_maps(&mut self) {
        let map = |s: &Self, input: bool| {
            let hardware = s.hardware_channels(input).len();
            s.channel_aliasing(input).then(|| s.channel_aliases(input).remap(hardware)).flatten()
        };
        let (input, output) = (map(self, true), map(self, false));
        self.engine.set_input_map(input);
        self.engine.set_output_map(output);
    }

    /// Reads the channel alias sections of the preferences file.
    pub(crate) fn load_channel_aliases(&mut self) {
        let ini = self.preferences_ini();
        self.aliases = ini
            .sections
            .iter()
            .filter(|s| s.name.starts_with("alias_"))
            .filter_map(|s| Some((s.name.clone(), ChannelAliases::read(&ini, &s.name)?)))
            .collect();
        self.apply_channel_maps();
    }

    fn hardware_channels(&self, input: bool) -> &[String] {
        let info = self.engine.info();
        if input { &info.inputs } else { &info.outputs }
    }

    fn channel_names(&self, input: bool) -> Vec<String> {
        let hardware = self.hardware_channels(input);
        match self.aliases.get(&self.alias_section(input)).filter(|_| self.channel_aliasing(input)) {
            Some(aliases) => aliases.names(hardware),
            None => hardware.to_vec(),
        }
    }

    fn alias_section(&self, input: bool) -> String {
        let info = self.engine.info();
        let id = if input { info.input.as_ref().map_or("", |(_, id)| id.as_str()) } else { info.device_id.as_str() };
        ChannelAliases::section(&scythe_engine::devices::ident(&info.host, id, input), input)
    }

    fn preferences_ini(&self) -> Ini {
        let text = self.prefs_file.as_deref().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
        Ini::parse(&text)
    }

    fn write_preferences_ini(&mut self, ini: &Ini) {
        let Some(path) = self.prefs_file.clone() else { return };
        let written =
            path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(&path, ini.write()));
        if let Err(e) = written {
            self.notify(format!("Cannot save preferences to {}: {e}", path.display()));
        }
    }
}
