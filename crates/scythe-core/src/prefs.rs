//! The preferences REAPER keeps in reaper.ini rather than in the project, under the same keys
//! in a `[REAPER]` section, so a value can be copied over from REAPER's own file.

use crate::metronome::COUNT_IN_AT_MEASURE;
use crate::surface::Surface;

#[derive(Debug, Clone, PartialEq)]
pub struct Preferences {
    /// `preroll`: bits as `metronome::PREROLL_*` and `COUNT_IN_AT_MEASURE`.
    pub preroll: u32,
    /// `prerollmeas`: pre-roll length in measures.
    pub preroll_measures: f64,
    /// `metronome_defout`: where the click plays when the project names no output, encoded as
    /// a `METRONOMEOUT` value; -1 plays it with the master.
    pub metronome_default_output: i64,
    /// `csurf_cnt` and `csurf_<i>`: the control surfaces.
    pub surfaces: Vec<Surface>,
    /// MIDI device settings, each a mask over device numbers 0-63 stored as two 32-bit words
    /// (`<key>` and `<key>_h`).
    pub midi: MidiPrefs,
    /// `projecttabs` and `projecttab<n>`: the open project tabs' files, in tab order; an
    /// unsaved tab is an empty string.
    pub project_tabs: Vec<String>,
    /// `lastproject`: the file of the active tab.
    pub last_project: Option<String>,
    /// `autoxfade`: editing and recording flags; Scythe reads the recording ones
    /// (`AUTOXFADE_*`) and keeps the others. `None` until set: REAPER's default.
    pub autoxfade: Option<u32>,
    /// `midicctouchtimeout`: touch-replace recording stops replacing a controller this many
    /// milliseconds after its last message. `None` until set: REAPER 7.73's 1000.
    pub midi_cc_touch_timeout: Option<u32>,
    /// The audio device page (Preferences > Audio > Device) under REAPER's Linux keys.
    pub audio: AudioDevicePrefs,
    /// `useinnc`: bit 1 names inputs by their aliases, bit 2 outputs.
    pub channel_aliasing: u32,
    /// `allstereopairs`: stereo pairs start on every channel (Input 2 / Input 3), not only
    /// odd ones. `None` until set: REAPER's default, on.
    pub all_stereo_pairs: Option<bool>,
}

impl Preferences {
    /// The touch-replace controller timeout in seconds.
    pub fn cc_touch_timeout(&self) -> f64 {
        self.midi_cc_touch_timeout.unwrap_or(1000) as f64 / 1000.0
    }
}

/// The audio system REAPER on Linux opens (`linux_audio_mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSystem {
    Jack,
    Alsa,
    Dummy,
    PulseAudio,
}

impl AudioSystem {
    pub const ALL: [AudioSystem; 4] = [Self::Jack, Self::Alsa, Self::Dummy, Self::PulseAudio];

    pub fn code(self) -> u32 {
        match self {
            Self::Jack => 0,
            Self::Alsa => 1,
            Self::Dummy => 2,
            Self::PulseAudio => 3,
        }
    }

    pub fn from_code(code: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.code() == code)
    }

    /// The name on REAPER's device page.
    pub fn label(self) -> &'static str {
        match self {
            Self::Jack => "JACK",
            Self::Alsa => "ALSA",
            Self::Dummy => "Dummy Audio",
            Self::PulseAudio => "PulseAudio",
        }
    }

    /// The audio host that plays it (as the engine names hosts).
    pub fn host(self) -> &'static str {
        match self {
            Self::Jack => "JACK",
            Self::Alsa => "ALSA",
            Self::Dummy => "Dummy",
            Self::PulseAudio => "PulseAudio",
        }
    }

    pub fn from_host(host: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.host().eq_ignore_ascii_case(host) || s.label().eq_ignore_ascii_case(host))
    }
}

/// Preferences > Audio > Device, as REAPER 7.73 on Linux keeps it in reaper.ini. Unset values
/// were never chosen: the system default device, at its own rate and block size.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioDevicePrefs {
    /// `linux_audio_mode`. REAPER leaves the key out for JACK, its default; Scythe writes 0 so
    /// that a missing key still means "the system default".
    pub system: Option<AudioSystem>,
    /// `alsa_indev`: the ALSA input device (`hw:CARD=USB,DEV=0`), empty for the default.
    pub input_device: Option<String>,
    /// `alsa_outdev`.
    pub output_device: Option<String>,
    /// `linux_audio_nch_in`: input channels to open.
    pub input_channels: Option<u32>,
    /// `linux_audio_nch_out`.
    pub output_channels: Option<u32>,
    /// `linux_audio_srate`.
    pub sample_rate: Option<u32>,
    /// `linux_audio_bsize`: frames per block.
    pub block_size: Option<u32>,
    /// `linux_audio_srateor`: "Allow projects to override device sample rate", on by default.
    pub project_rate_override: Option<bool>,
    /// `linux_audio_bits`: ALSA's bit depth (16, 24, 32).
    pub bits: Option<u32>,
    /// `linux_audio_bufs`: ALSA's periods, 2-8 (REAPER's default 3).
    pub periods: Option<u32>,
    /// `alsa_rtprio`: the audio thread's real-time priority with ALSA, 0 for none.
    pub rt_priority: Option<u32>,
    /// `jack_options`: bit 1 leaves the JACK server alone (no auto-start), bit 2 leaves audio
    /// ports unconnected, bit 4 MIDI ports. REAPER's default (no key) is 1.
    pub jack_options: Option<u32>,
    /// `jack_launchcmd`: the command that starts jackd; empty lets libjack start it.
    pub jack_launch: Option<String>,
    /// `linux_disable_pm`: hold `/dev/cpu_dma_latency` at 0 while audio runs.
    pub disable_power_management: Option<bool>,
    /// `linux_mlockall`: lock the process memory.
    pub lock_memory: Option<bool>,
    /// `audiocloseinactive_linux`: bits of `CLOSE_*`.
    pub close_inactive: Option<u32>,
    /// `audioclosestop`: close the device whenever stopped, even while active.
    pub close_stopped: Option<bool>,
}

/// `audiocloseinactive_linux` bit: close the device while stopped and Scythe is not the active
/// application.
pub const CLOSE_INACTIVE: u32 = 1;
/// ... also while tracks are record armed.
pub const CLOSE_INACTIVE_ARMED: u32 = 2;
/// `jack_options` bit: do not start the JACK server.
pub const JACK_NO_START: u32 = 1;
/// `jack_options` bit: do not connect the audio ports to the system ports.
pub const JACK_NO_CONNECT: u32 = 2;

impl AudioDevicePrefs {
    pub fn project_rate_override(&self) -> bool {
        self.project_rate_override.unwrap_or(true)
    }

    /// Takes `key` when it is one of the device page's; false for any other key.
    fn read(&mut self, key: &str, value: &str) -> bool {
        let number = || value.parse::<i64>().ok().filter(|v| *v >= 0).map(|v| v as u32);
        let text = || Some(value.to_owned()).filter(|v| !v.is_empty());
        match key {
            "linux_audio_mode" => self.system = number().and_then(AudioSystem::from_code),
            "alsa_indev" => self.input_device = text(),
            "alsa_outdev" => self.output_device = text(),
            "linux_audio_nch_in" => self.input_channels = number(),
            "linux_audio_nch_out" => self.output_channels = number(),
            "linux_audio_srate" => self.sample_rate = number().filter(|r| *r > 0),
            "linux_audio_bsize" => self.block_size = number().filter(|b| *b > 0),
            "linux_audio_srateor" => self.project_rate_override = number().map(|v| v != 0),
            "linux_audio_bits" => self.bits = number(),
            "linux_audio_bufs" => self.periods = number(),
            "alsa_rtprio" => self.rt_priority = number(),
            "jack_options" => self.jack_options = number(),
            "jack_launchcmd" => self.jack_launch = Some(value.to_owned()),
            "linux_disable_pm" => self.disable_power_management = number().map(|v| v != 0),
            "linux_mlockall" => self.lock_memory = number().map(|v| v != 0),
            "audiocloseinactive_linux" => self.close_inactive = number(),
            "audioclosestop" => self.close_stopped = number().map(|v| v != 0),
            _ => return false,
        }
        true
    }

    fn values(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if let Some(system) = self.system {
            out.push(("linux_audio_mode".to_owned(), system.code().to_string()));
            let device = |d: &Option<String>| d.clone().unwrap_or_default();
            out.push(("alsa_indev".to_owned(), device(&self.input_device)));
            out.push(("alsa_outdev".to_owned(), device(&self.output_device)));
        }
        let numbers = [
            ("linux_audio_nch_in", self.input_channels),
            ("linux_audio_nch_out", self.output_channels),
            ("linux_audio_srate", self.sample_rate),
            ("linux_audio_bsize", self.block_size),
            ("linux_audio_srateor", self.project_rate_override.map(u32::from)),
            ("linux_audio_bits", self.bits),
            ("linux_audio_bufs", self.periods),
            ("alsa_rtprio", self.rt_priority),
            ("jack_options", self.jack_options),
            ("linux_disable_pm", self.disable_power_management.map(u32::from)),
            ("linux_mlockall", self.lock_memory.map(u32::from)),
            ("audiocloseinactive_linux", self.close_inactive),
            ("audioclosestop", self.close_stopped.map(u32::from)),
        ];
        out.extend(numbers.iter().filter_map(|(k, v)| v.map(|v| (k.to_string(), v.to_string()))));
        if let Some(cmd) = &self.jack_launch {
            out.push(("jack_launchcmd".to_owned(), cmd.clone()));
        }
        out
    }
}

/// `useinnc` bit: inputs go by their aliases.
pub const ALIAS_INPUTS: u32 = 1;
/// `useinnc` bit: outputs go by their aliases.
pub const ALIAS_OUTPUTS: u32 = 2;

/// A device's channel names and mapping, as REAPER keeps them in an `[alias_in_<device>]` or
/// `[alias_out_<device>]` section of reaper.ini and in `.ReaperChanMap` files
/// (`[reaper_chanmap]`): per channel slot the hardware channel it plays (`ch<i>`, -1 for none)
/// and its alias (`name<i>`, empty for the hardware channel's name). There are `map_size`
/// slots, as many as tracks see, whatever the device has.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelAliases {
    pub channels: Vec<(i32, String)>,
    /// `map_hwnch`: how many channels the device had when the map was made.
    pub hardware_channels: usize,
}

/// The section of a `.ReaperChanMap` file.
pub const CHANMAP_SECTION: &str = "reaper_chanmap";

impl ChannelAliases {
    /// The section name for a device REAPER identifies as `ident` (`ALSA null`, `JackIn`):
    /// characters other than letters and digits become `_`.
    pub fn section(ident: &str, input: bool) -> String {
        let clean: String = ident.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
        format!("alias_{}_{clean}", if input { "in" } else { "out" })
    }

    /// Each channel plays its own hardware channel, under its own name.
    pub fn identity(hardware: usize) -> Self {
        Self { channels: (0..hardware as i32).map(|c| (c, String::new())).collect(), hardware_channels: hardware }
    }

    pub fn read(ini: &crate::ini::Ini, section: &str) -> Option<Self> {
        let s = ini.section(section)?;
        let number = |k: &str| s.get(k).and_then(|v| v.trim().parse::<i64>().ok());
        let size = number("map_size").unwrap_or(0).clamp(0, 1024) as usize;
        let channels = (0..size)
            .map(|i| {
                let hw = number(&format!("ch{i}")).unwrap_or(i as i64).clamp(-1, 1023) as i32;
                (hw, s.get(&format!("name{i}")).unwrap_or("").to_owned())
            })
            .collect();
        let hardware = number("map_hwnch").map_or(size, |n| n.max(0) as usize);
        Some(Self { channels, hardware_channels: hardware })
    }

    /// Replaces `section` in `ini` with these channels, keys sorted as REAPER sorts them.
    pub fn write(&self, ini: &mut crate::ini::Ini, section: &str) {
        let mut entries: Vec<(String, String)> = Vec::new();
        for (i, (hw, name)) in self.channels.iter().enumerate() {
            entries.push((format!("ch{i}"), hw.to_string()));
            entries.push((format!("name{i}"), name.clone()));
        }
        entries.push(("map_hwnch".into(), self.hardware_channels.to_string()));
        entries.push(("map_size".into(), self.channels.len().to_string()));
        entries.sort();
        ini.section_mut(section).entries = entries;
    }

    /// A `.ReaperChanMap` file's text.
    pub fn to_chanmap(&self) -> String {
        let mut ini = crate::ini::Ini::default();
        self.write(&mut ini, CHANMAP_SECTION);
        ini.write()
    }

    pub fn from_chanmap(text: &str) -> Option<Self> {
        Self::read(&crate::ini::Ini::parse(text), CHANMAP_SECTION)
    }

    /// The alias of channel slot `i`, when it has one.
    pub fn alias(&self, i: usize) -> Option<&str> {
        self.channels.get(i).map(|(_, a)| a.as_str()).filter(|a| !a.is_empty())
    }

    /// The hardware channel slot `i` plays, `None` when it is not connected.
    pub fn hardware(&self, i: usize) -> Option<usize> {
        self.channels.get(i).and_then(|(hw, _)| usize::try_from(*hw).ok())
    }

    /// Sets slot `i`'s alias (empty clears it), growing the map to `hardware` channels.
    pub fn set_alias(&mut self, i: usize, alias: &str, hardware: usize) {
        self.grow(i + 1, hardware);
        self.channels[i].1 = alias.trim().to_owned();
    }

    /// Points slot `i` at hardware channel `hw` (`None`: not connected).
    pub fn set_hardware(&mut self, i: usize, hw: Option<usize>, hardware: usize) {
        self.grow(i + 1, hardware);
        self.channels[i].0 = hw.map_or(-1, |c| c as i32);
    }

    /// REAPER's Add: one more slot, not connected.
    pub fn add_slot(&mut self, hardware: usize) {
        self.grow(0, hardware);
        self.channels.push((-1, String::new()));
    }

    /// REAPER's Remove: drops slot `i`.
    pub fn remove_slot(&mut self, i: usize) {
        if i < self.channels.len() {
            self.channels.remove(i);
        }
    }

    fn grow(&mut self, len: usize, hardware: usize) {
        if self.channels.is_empty() {
            *self = Self::identity(hardware);
        }
        while self.channels.len() < len {
            self.channels.push((self.channels.len() as i32, String::new()));
        }
        self.hardware_channels = self.hardware_channels.max(hardware);
    }

    /// The hardware channel of every slot, when that is not simply each its own.
    pub fn remap(&self, hardware: usize) -> Option<Vec<i32>> {
        let map: Vec<i32> = self.channels.iter().map(|(hw, _)| *hw).collect();
        let identity = map.len() == hardware && map.iter().enumerate().all(|(i, hw)| *hw == i as i32);
        (!identity && !map.is_empty()).then_some(map)
    }

    /// The names the slots go by: the alias, else the name of the hardware channel played,
    /// `(not connected)` for none.
    pub fn names(&self, hardware: &[String]) -> Vec<String> {
        (0..self.channels.len())
            .map(|i| match (self.alias(i), self.hardware(i)) {
                (Some(a), _) => a.to_owned(),
                (None, Some(hw)) => hardware.get(hw).cloned().unwrap_or_else(|| format!("{}", hw + 1)),
                (None, None) => "(not connected)".to_owned(),
            })
            .collect()
    }
}

/// `autoxfade` bit: recording on a fixed-lane track puts each pass in a new lane.
pub const AUTOXFADE_RECORD_LANES: u32 = 8;
/// `autoxfade` bit: loop recording adds takes even when new recording adds lanes.
pub const AUTOXFADE_LOOP_TAKES: u32 = 16;
/// `autoxfade` bit: newly recorded lanes play exclusively (else they play in layers).
pub const AUTOXFADE_LANES_EXCLUSIVE: u32 = 256;
/// REAPER 7.73's `autoxfade` when reaper.ini has none.
pub const AUTOXFADE_DEFAULT: u32 = 385;

/// Which MIDI devices are enabled and which outputs get clock, as reaper.ini keeps them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MidiPrefs {
    /// `midiins`: enabled inputs; `None` (no key) enables every input.
    pub inputs: Option<u64>,
    /// `midiouts`: enabled outputs; `None` enables every output.
    pub outputs: Option<u64>,
    /// `midiouts_clock`: outputs that get MIDI clock.
    pub clock: u64,
    /// `midiouts_clock_nospp`: clock outputs that get no song position pointer.
    pub clock_no_spp: u64,
    /// `midiouts_noreset`: outputs spared the reset messages at play.
    pub no_reset: u64,
    /// `midiins_cs`: inputs whose messages reach learned controls and action shortcuts;
    /// `None` (no key) lets every enabled input do so.
    pub control: Option<u64>,
}

impl MidiPrefs {
    pub fn input_enabled(&self, n: u8) -> bool {
        n >= 64 || self.inputs.is_none_or(|m| m >> n & 1 != 0)
    }

    /// Whether input `n` drives learned controls and action shortcuts.
    pub fn control_enabled(&self, n: u8) -> bool {
        self.input_enabled(n) && (n >= 64 || self.control.is_none_or(|m| m >> n & 1 != 0))
    }

    pub fn output_enabled(&self, n: u8) -> bool {
        n >= 64 || self.outputs.is_none_or(|m| m >> n & 1 != 0)
    }

    pub fn bit(mask: u64, n: u8) -> bool {
        n < 64 && mask >> n & 1 != 0
    }

    pub fn with(mask: u64, n: u8, on: bool) -> u64 {
        if n >= 64 {
            return mask;
        }
        if on { mask | 1 << n } else { mask & !(1 << n) }
    }
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            preroll: 0,
            preroll_measures: 2.0,
            metronome_default_output: -1,
            surfaces: Vec::new(),
            midi: MidiPrefs::default(),
            project_tabs: Vec::new(),
            last_project: None,
            autoxfade: None,
            midi_cc_touch_timeout: None,
            audio: AudioDevicePrefs::default(),
            channel_aliasing: 0,
            all_stereo_pairs: None,
        }
    }
}

/// The reaper.ini keys of the MIDI masks, low word first.
const MIDI_KEYS: [&str; 6] =
    ["midiins", "midiouts", "midiouts_clock", "midiouts_clock_nospp", "midiouts_noreset", "midiins_cs"];

const SECTION: &str = "[REAPER]";

/// The index of a `csurf_<i>` key.
fn tab_index(key: &str) -> Option<usize> {
    key.strip_prefix("projecttab").and_then(|i| i.parse::<usize>().ok()).filter(|i| *i > 0)
}

fn surface_index(key: &str) -> Option<usize> {
    key.strip_prefix("csurf_").and_then(|i| i.parse().ok())
}

impl Preferences {
    /// Reads the keys it knows from the `[REAPER]` section; the others keep their defaults.
    pub fn parse(text: &str) -> Self {
        let mut prefs = Self::default();
        let mut in_section = false;
        let mut surface_count = 0;
        let mut surfaces = Vec::new();
        let (mut tab_count, mut tabs) = (0, Vec::new());
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_section = line.eq_ignore_ascii_case(SECTION);
                continue;
            }
            let Some((key, value)) = line.split_once('=').filter(|_| in_section) else { continue };
            let value = value.trim();
            match key.trim() {
                "preroll" => prefs.preroll = value.parse().unwrap_or(prefs.preroll),
                "prerollmeas" => {
                    prefs.preroll_measures = value
                        .parse()
                        .ok()
                        .filter(|m: &f64| m.is_finite() && *m >= 0.0)
                        .unwrap_or(prefs.preroll_measures)
                }
                "metronome_defout" => {
                    prefs.metronome_default_output = value.parse().unwrap_or(prefs.metronome_default_output)
                }
                "autoxfade" => prefs.autoxfade = value.parse::<i64>().ok().map(|v| v as u32),
                "midicctouchtimeout" => {
                    prefs.midi_cc_touch_timeout = value.parse::<i64>().ok().map(|v| v.max(0) as u32)
                }
                "useinnc" => prefs.channel_aliasing = value.parse::<i64>().map_or(0, |v| v as u32),
                "allstereopairs" => prefs.all_stereo_pairs = value.parse::<i64>().ok().map(|v| v != 0),
                key if prefs.audio.read(key, value) => {}
                "csurf_cnt" => surface_count = value.parse().unwrap_or(0),
                "projecttabs" => tab_count = value.parse().unwrap_or(0),
                "lastproject" => prefs.last_project = Some(value.to_owned()).filter(|v| !v.is_empty()),
                key => {
                    if let Some(i) = tab_index(key) {
                        tabs.push((i, value.to_owned()));
                        continue;
                    }
                    if let Some(i) = surface_index(key) {
                        surfaces.push((i, Surface::parse(value)));
                        continue;
                    }
                    let (base, high) = key.strip_suffix("_h").map_or((key, false), |b| (b, true));
                    let Some(i) = MIDI_KEYS.iter().position(|k| *k == base) else { continue };
                    let Ok(word) = value.parse::<i64>() else { continue };
                    let word = (word as u32 as u64) << if high { 32 } else { 0 };
                    let keep = if high { 0xffff_ffff } else { 0xffff_ffff << 32 };
                    let m = &mut prefs.midi;
                    match i {
                        0 => m.inputs = Some(m.inputs.unwrap_or(0) & keep | word),
                        1 => m.outputs = Some(m.outputs.unwrap_or(0) & keep | word),
                        2 => m.clock = m.clock & keep | word,
                        3 => m.clock_no_spp = m.clock_no_spp & keep | word,
                        4 => m.no_reset = m.no_reset & keep | word,
                        _ => m.control = Some(m.control.unwrap_or(0) & keep | word),
                    }
                }
            }
        }
        surfaces.sort_by_key(|(i, _)| *i);
        prefs.surfaces = surfaces.into_iter().filter(|(i, _)| *i < surface_count).map(|(_, s)| s).collect();
        tabs.sort_by_key(|(i, _)| *i);
        prefs.project_tabs = tabs.into_iter().filter(|(i, _)| *i <= tab_count).map(|(_, t)| t).collect();
        prefs
    }

    /// `text` with these values in its `[REAPER]` section, everything else left as it was.
    pub fn write_into(&self, text: &str) -> String {
        let mut values = vec![
            ("preroll".to_owned(), self.preroll.to_string()),
            ("prerollmeas".to_owned(), format!("{:.14}", self.preroll_measures)),
            ("metronome_defout".to_owned(), self.metronome_default_output.to_string()),
        ];
        if let Some(v) = self.autoxfade {
            values.push(("autoxfade".to_owned(), v.to_string()));
        }
        if let Some(v) = self.midi_cc_touch_timeout {
            values.push(("midicctouchtimeout".to_owned(), v.to_string()));
        }
        if self.channel_aliasing != 0 || text.contains("useinnc=") {
            values.push(("useinnc".to_owned(), self.channel_aliasing.to_string()));
        }
        if let Some(on) = self.all_stereo_pairs {
            values.push(("allstereopairs".to_owned(), u8::from(on).to_string()));
        }
        values.extend(self.audio.values());
        let has_surfaces = !self.surfaces.is_empty() || text.lines().any(|l| l.trim_start().starts_with("csurf_cnt="));
        if has_surfaces {
            values.push(("csurf_cnt".to_owned(), self.surfaces.len().to_string()));
        }
        values.extend(self.surfaces.iter().enumerate().map(|(i, s)| (format!("csurf_{i}"), s.line())));
        if !self.project_tabs.is_empty() || text.lines().any(|l| l.trim_start().starts_with("projecttabs=")) {
            values.push(("projecttabs".to_owned(), self.project_tabs.len().to_string()));
            values
                .extend(self.project_tabs.iter().enumerate().map(|(i, t)| (format!("projecttab{}", i + 1), t.clone())));
        }
        if let Some(last) = &self.last_project {
            values.push(("lastproject".to_owned(), last.clone()));
        }
        let stale = |key: &str| {
            surface_index(key).is_some_and(|i| i >= self.surfaces.len())
                || tab_index(key).is_some_and(|i| i > self.project_tabs.len())
        };
        let m = &self.midi;
        let masks = [Some(m.inputs), Some(m.outputs), None, None, None, Some(m.control)];
        let plain = [0, 0, m.clock, m.clock_no_spp, m.no_reset, 0];
        for (i, key) in MIDI_KEYS.iter().enumerate() {
            let mask = match masks[i] {
                Some(None) => continue,
                Some(Some(v)) => v,
                None if plain[i] == 0 && !text.contains(key) => continue,
                None => plain[i],
            };
            values.push((key.to_string(), (mask as u32 as i32).to_string()));
            values.push((format!("{key}_h"), ((mask >> 32) as u32 as i32).to_string()));
        }
        let mut written = vec![false; values.len()];
        let mut out = Vec::new();
        let mut in_section = false;
        let mut has_section = false;
        let flush = |out: &mut Vec<String>, written: &mut [bool]| {
            for (i, (k, v)) in values.iter().enumerate() {
                if !written[i] {
                    out.push(format!("{k}={v}"));
                    written[i] = true;
                }
            }
        };
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                if in_section {
                    flush(&mut out, &mut written);
                }
                in_section = trimmed.eq_ignore_ascii_case(SECTION);
                has_section |= in_section;
            } else if in_section && let Some((key, _)) = trimmed.split_once('=') {
                let key = key.trim();
                if let Some(i) = values.iter().position(|(n, _)| n == key) {
                    out.push(format!("{}={}", values[i].0, values[i].1));
                    written[i] = true;
                    continue;
                }
                if stale(key) {
                    continue;
                }
            }
            out.push(line.to_owned());
        }
        if !has_section {
            out.push(SECTION.to_owned());
        }
        if !has_section || in_section {
            flush(&mut out, &mut written);
        }
        out.join("\n") + "\n"
    }

    pub fn autoxfade_bit(&self, bit: u32) -> bool {
        self.autoxfade.unwrap_or(AUTOXFADE_DEFAULT) & bit != 0
    }

    pub fn set_autoxfade_bit(&mut self, bit: u32, on: bool) {
        let v = self.autoxfade.unwrap_or(AUTOXFADE_DEFAULT);
        self.autoxfade = Some(if on { v | bit } else { v & !bit });
    }

    pub fn count_in_at_measure(&self) -> bool {
        self.preroll & COUNT_IN_AT_MEASURE != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::{MidiKind, MidiSurface};

    #[test]
    fn reads_and_writes_reaper_ini_keys() {
        let text =
            "[audioconfig]\npreroll=99\n[REAPER]\nafxcfg=0\nprerollmeas=1.50000000000000\npreroll=9\n[other]\nx=1\n";
        let p = Preferences::parse(text);
        assert_eq!(p, Preferences { preroll: 9, preroll_measures: 1.5, ..Default::default() });
        assert!(p.count_in_at_measure());
        let changed = Preferences { metronome_default_output: 1026, ..p }.write_into(text);
        assert_eq!(
            changed,
            "[audioconfig]\npreroll=99\n[REAPER]\nafxcfg=0\nprerollmeas=1.50000000000000\npreroll=9\n\
             metronome_defout=1026\n[other]\nx=1\n"
        );
        assert_eq!(Preferences::parse(&changed).metronome_default_output, 1026);
        let fresh = Preferences::default().write_into("");
        assert_eq!(fresh, "[REAPER]\npreroll=0\nprerollmeas=2.00000000000000\nmetronome_defout=-1\n");
        assert_eq!(Preferences::parse(&fresh), Preferences::default());
        assert_eq!(Preferences::parse("[REAPER]\nprerollmeas=-2\npreroll=x\n"), Preferences::default());
    }

    #[test]
    fn audio_device_keys_read_as_reaper_writes_them() {
        // REAPER 7.73's reaper.ini after choosing ALSA with 18 inputs and 20 outputs.
        let text = "[reaper]\nalsa_indev=null\nalsa_outdev=hw:CARD=USB,DEV=0\nalsa_rtprio=0\nlinux_audio_bits=32\n\
                    linux_audio_bsize=512\nlinux_audio_bufs=3\nlinux_audio_mode=1\nlinux_audio_nch_in=18\n\
                    linux_audio_nch_out=20\nlinux_audio_srate=44100\nlinux_audio_srateor=1\nuseinnc=3\n\
                    allstereopairs=0\njack_options=2\njack_launchcmd=jackd -d dummy\naudiocloseinactive_linux=3\n\
                    audioclosestop=1\nlinux_disable_pm=1\nlinux_mlockall=0\nalsa_rtprio=40\n";
        let p = Preferences::parse(text);
        let a = &p.audio;
        assert_eq!((a.bits, a.periods, a.rt_priority), (Some(32), Some(3), Some(40)));
        assert_eq!((a.jack_options, a.jack_launch.as_deref()), (Some(JACK_NO_CONNECT), Some("jackd -d dummy")));
        assert_eq!((a.close_inactive, a.close_stopped), (Some(CLOSE_INACTIVE | CLOSE_INACTIVE_ARMED), Some(true)));
        assert_eq!((a.disable_power_management, a.lock_memory), (Some(true), Some(false)));
        assert_eq!(a.system, Some(AudioSystem::Alsa));
        assert_eq!((a.input_device.as_deref(), a.output_device.as_deref()), (Some("null"), Some("hw:CARD=USB,DEV=0")));
        assert_eq!((a.input_channels, a.output_channels), (Some(18), Some(20)));
        assert_eq!((a.sample_rate, a.block_size), (Some(44100), Some(512)));
        assert!(a.project_rate_override());
        assert_eq!(p.channel_aliasing, ALIAS_INPUTS | ALIAS_OUTPUTS);
        assert_eq!(p.all_stereo_pairs, Some(false));
        let written = p.write_into(text);
        assert_eq!(Preferences::parse(&written), p);
        assert!(written.contains("linux_audio_bufs=3"), "keys Scythe does not use stay");
        // REAPER leaves `linux_audio_mode` out for JACK; Scythe writes it so the choice sticks.
        let jack = Preferences {
            audio: AudioDevicePrefs { system: Some(AudioSystem::Jack), ..Default::default() },
            ..Default::default()
        };
        let written = jack.write_into("");
        assert!(written.contains("linux_audio_mode=0\n"), "{written}");
        assert_eq!(Preferences::parse(&written).audio.system, Some(AudioSystem::Jack));
        assert_eq!(Preferences::parse("[REAPER]\n").audio, AudioDevicePrefs::default());
    }

    #[test]
    fn channel_aliases_use_reapers_sections() {
        // As REAPER 7.73 wrote it after naming input 2 "Snare" on the ALSA device `null`.
        let mut text = String::from("[alias_in_ALSA_null]\n");
        for i in 0..3 {
            text.push_str(&format!("ch{i}={i}\n"));
        }
        text.push_str("map_hwnch=3\nmap_size=3\nname0=\nname1=Snare\nname2=\n");
        let ini = crate::ini::Ini::parse(&text);
        let section = ChannelAliases::section("ALSA null", true);
        assert_eq!(section, "alias_in_ALSA_null");
        let aliases = ChannelAliases::read(&ini, &section).unwrap();
        assert_eq!((aliases.alias(0), aliases.alias(1)), (None, Some("Snare")));
        let mut again = crate::ini::Ini::default();
        aliases.write(&mut again, &section);
        assert_eq!(ChannelAliases::read(&again, &section), Some(aliases.clone()));
        let mut grown = aliases;
        grown.set_alias(4, " Kick ", 6);
        assert_eq!(grown.channels.len(), 5);
        assert_eq!(grown.alias(4), Some("Kick"));
        assert_eq!(ChannelAliases::section("ALSA hw:CARD=USB,DEV=0", false), "alias_out_ALSA_hw_CARD_USB_DEV_0");
    }

    #[test]
    fn channel_remapping_reads_as_reaper_shows_it() {
        // REAPER 7.73 after pointing input 1 at hardware input 3 and adding a slot (Add): 19
        // inputs, the first named "Input 3", the last "(not connected)".
        let mut text = String::from("[reaper_chanmap]\nch0=2\n");
        for i in 1..18 {
            text.push_str(&format!("ch{i}={i}\n"));
        }
        text.push_str("ch18=-1\nmap_hwnch=18\nmap_size=19\nname1=Snare\n");
        let map = ChannelAliases::from_chanmap(&text).unwrap();
        let hardware: Vec<String> = (1..=18).map(|i| format!("Input {i}")).collect();
        let names = map.names(&hardware);
        assert_eq!(names.len(), 19);
        assert_eq!((names[0].as_str(), names[1].as_str(), names[2].as_str()), ("Input 3", "Snare", "Input 3"));
        assert_eq!(names[18], "(not connected)");
        let remap = map.remap(18).unwrap();
        assert_eq!((remap[0], remap[1], remap[18]), (2, 1, -1));
        assert_eq!(ChannelAliases::identity(18).remap(18), None);
        assert_eq!(ChannelAliases::from_chanmap(&map.to_chanmap()), Some(map.clone()));
        let mut fewer = map;
        fewer.remove_slot(18);
        fewer.add_slot(18);
        assert_eq!(fewer.channels.len(), 19);
    }

    #[test]
    fn control_surfaces_round_trip() {
        // As REAPER 7.73 keeps them in reaper.ini (its section is `[reaper]`, keys sorted).
        let text = "[reaper]\ncsurf_0=OSC \"TouchOSC\" 3 9123 \"127.0.0.1\" 9124 1024 10 \"\"\n\
                    csurf_1=MCU 0 0 2 3 0\ncsurf_2=BCF2000 0 1\ncsurf_cnt=3\nerrnowarn=5\n";
        let p = Preferences::parse(text);
        assert_eq!(p.surfaces.len(), 3);
        assert_eq!(p.surfaces[2].line(), "BCF2000 0 1");
        let again = p.write_into(text);
        assert_eq!(Preferences::parse(&again), p);
        for line in text.lines().filter(|l| l.starts_with("csurf")) {
            assert!(again.lines().any(|l| l == line), "{line} in\n{again}");
        }
        let mut fewer = p.clone();
        fewer.surfaces.remove(0);
        fewer.surfaces[0] = Surface::Midi(MidiSurface::new(MidiKind::McuExtender, 4, 5));
        let written = fewer.write_into(&again);
        assert!(written.contains("csurf_0=MCUEX 0 0 4 5 0\ncsurf_1=BCF2000 0 1\ncsurf_cnt=2\n"), "{written}");
        assert!(!written.contains("csurf_2"));
        assert_eq!(Preferences::parse(&written), fewer);
        let none = Preferences { surfaces: Vec::new(), ..fewer }.write_into(&written);
        assert!(none.contains("csurf_cnt=0") && !none.contains("csurf_0"), "{none}");
        assert!(Preferences::parse("[REAPER]\ncsurf_0=MCU 0 0 0 0 0\n").surfaces.is_empty());
    }

    #[test]
    fn midi_device_masks() {
        let text = "[REAPER]\nmidiins=5\nmidiins_h=1\nmidiouts_clock=2\n";
        let p = Preferences::parse(text);
        assert_eq!(p.midi.inputs, Some(5 | 1 << 32));
        assert!(p.midi.input_enabled(0) && !p.midi.input_enabled(1) && p.midi.input_enabled(32));
        assert!(p.midi.output_enabled(9), "no midiouts key enables every output");
        assert_eq!(p.midi.clock, 2);
        let mut q = p.clone();
        q.midi.inputs = Some(u64::MAX);
        let written = q.write_into(text);
        assert!(written.contains("midiins=-1\nmidiins_h=-1\n"), "{written}");
        assert_eq!(Preferences::parse(&written).midi, q.midi);
        assert!(!Preferences::default().write_into("").contains("midi"));
    }

    #[test]
    fn project_tabs_round_trip() {
        let text =
            "[REAPER]\nlastproject=/a/b.RPP\nprojecttab1=/a/a.RPP\nprojecttab2=/a/b.RPP\nprojecttab3=\nprojecttabs=3\n";
        let p = Preferences::parse(text);
        assert_eq!(p.project_tabs, ["/a/a.RPP", "/a/b.RPP", ""]);
        assert_eq!(p.last_project.as_deref(), Some("/a/b.RPP"));
        let one = Preferences { project_tabs: vec!["/a/c.RPP".into()], last_project: Some("/a/c.RPP".into()), ..p };
        let written = one.write_into(text);
        assert!(!written.contains("projecttab2") && !written.contains("projecttab3="));
        assert_eq!(Preferences::parse(&written).project_tabs, ["/a/c.RPP"]);
    }
}
