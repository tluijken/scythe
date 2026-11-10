//! The audio device the session plays on: chosen in the preferences as REAPER's Preferences >
//! Audio > Device page does, switched while the project stays open, and the names of its
//! channels with the aliases REAPER lets you give them.

use std::sync::Arc;

use scythe_core::ini::Ini;
use scythe_core::prefs::{ALIAS_INPUTS, ALIAS_OUTPUTS, AudioDevicePrefs, AudioSystem, ChannelAliases};
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
    match system {
        AudioSystem::Alsa => {
            request.output = prefs.output_device.clone();
            request.input = prefs.input_device.clone();
        }
        // PulseAudio plays through ALSA's default device, which the sound server provides.
        AudioSystem::PulseAudio => request.host = Some(AudioSystem::Alsa.host().to_owned()),
        AudioSystem::Jack => request.sample_rate = None,
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
        project_rate_override: keep.project_rate_override,
    }
}

/// Opens `request`, else the system default: the engine, and the notice to show when the
/// chosen device could not be opened.
pub fn start_engine(pool: Arc<MediaPool>, request: &DeviceRequest) -> (Engine, Option<String>) {
    let fallback = DeviceRequest { no_input: request.no_input, ..Default::default() };
    if *request == fallback {
        return (Engine::start_with(pool, request), None);
    }
    match Engine::open(pool.clone(), request) {
        Ok(engine) => (engine, None),
        Err(e) => {
            let engine = Engine::start_with(pool, &fallback);
            (engine, Some(format!("The chosen audio device is unavailable ({e}); playing on the default device")))
        }
    }
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
        self.preview = None;
        self.reset_device_errors();
        self.engine.seek(self.project.cursor);
        self.sync();
        if save {
            let audio = prefs_from_request(&request, &self.prefs.audio);
            self.change_preferences(|p| p.audio = audio);
        }
        Ok(())
    }

    /// The input channels' names: the device's (`Input 3`), or their aliases while input
    /// aliasing is on.
    pub fn input_names(&self) -> Vec<String> {
        self.channel_names(true)
    }

    /// The output channels' names, like [`Session::input_names`].
    pub fn output_names(&self) -> Vec<String> {
        self.channel_names(false)
    }

    /// The names of the device's channels with every alias, whether aliasing is on or not, for
    /// editing them.
    pub fn channel_aliases(&self, input: bool) -> ChannelAliases {
        let ini = self.preferences_ini();
        ChannelAliases::read(&ini, &self.alias_section(input)).unwrap_or_default()
    }

    /// Whether inputs (or outputs) go by their aliases (REAPER's "Input channel name
    /// aliasing/remapping").
    pub fn channel_aliasing(&self, input: bool) -> bool {
        self.prefs.channel_aliasing & if input { ALIAS_INPUTS } else { ALIAS_OUTPUTS } != 0
    }

    pub fn set_channel_aliasing(&mut self, input: bool, on: bool) {
        let bit = if input { ALIAS_INPUTS } else { ALIAS_OUTPUTS };
        self.change_preferences(|p| {
            p.channel_aliasing = if on { p.channel_aliasing | bit } else { p.channel_aliasing & !bit }
        });
    }

    /// Names channel `index` of the current device (empty clears the alias) and turns
    /// aliasing on for that side, saved in the preferences as REAPER keeps it.
    pub fn set_channel_alias(&mut self, input: bool, index: usize, alias: &str) {
        let hardware = if input { self.engine.info().inputs.len() } else { self.engine.info().outputs.len() };
        let section = self.alias_section(input);
        let mut ini = self.preferences_ini();
        let mut aliases = ChannelAliases::read(&ini, &section).unwrap_or_default();
        aliases.set_alias(index, alias, hardware);
        aliases.write(&mut ini, &section);
        self.write_preferences_ini(&ini);
        if !self.channel_aliasing(input) {
            self.set_channel_aliasing(input, true);
        }
        self.sync();
    }

    fn channel_names(&self, input: bool) -> Vec<String> {
        let info = self.engine.info();
        let hardware = if input { &info.inputs } else { &info.outputs };
        if !self.channel_aliasing(input) {
            return hardware.clone();
        }
        let aliases = self.channel_aliases(input);
        hardware.iter().enumerate().map(|(i, n)| aliases.alias(i).map_or_else(|| n.clone(), str::to_owned)).collect()
    }

    fn alias_section(&self, input: bool) -> String {
        let info = self.engine.info();
        let id = if input { info.input.as_ref().map_or("", |(_, id)| id.as_str()) } else { info.device_id.as_str() };
        ChannelAliases::section(&scythe_engine::devices::ident(&info.host, id, input), input)
    }

    fn preferences_ini(&self) -> Ini {
        let text = self.prefs_file.as_deref().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
        let mut ini = Ini::parse(&text);
        for (section, aliases) in &self.unsaved_aliases {
            aliases.write(&mut ini, section);
        }
        ini
    }

    fn write_preferences_ini(&mut self, ini: &Ini) {
        let Some(path) = self.prefs_file.clone() else {
            for s in &ini.sections {
                if s.name.starts_with("alias_")
                    && let Some(a) = ChannelAliases::read(ini, &s.name)
                {
                    self.unsaved_aliases.insert(s.name.clone(), a);
                }
            }
            return;
        };
        let written =
            path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(&path, ini.write()));
        if let Err(e) = written {
            self.notify(format!("Cannot save preferences to {}: {e}", path.display()));
        }
    }
}
