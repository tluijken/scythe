//! Settings of a headless session: a TOML file, overridden by command-line flags.
//!
//! ```toml
//! project = "/home/band/live/monitors.RPP"
//! control = "0.0.0.0:8770"
//! token = "change-me"
//! folders = ["/home/band/live"]
//! access_file = "/home/band/.config/scythe/access.json"
//! no_input = false
//!
//! [audio]
//! host = "ALSA"
//! output = "Scarlett 18i20"
//! input = "Scarlett 18i20"
//! sample_rate = 48000
//! buffer_size = 128
//! periods = 3
//! output_channels = 8
//! null = false
//! ```

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use scythe_engine::DeviceRequest;
use serde::Deserialize;

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub project: Option<PathBuf>,
    /// Where the control API listens, `"off"` for not at all.
    pub control: Option<String>,
    pub token: Option<String>,
    /// Folders the web UI's file browser offers (the home folder when none are given).
    #[serde(default)]
    pub folders: Vec<PathBuf>,
    /// Where access links are kept (`$XDG_CONFIG_HOME/scythe/access.json` by default).
    pub access_file: Option<PathBuf>,
    #[serde(default)]
    pub no_input: bool,
    #[serde(default)]
    pub audio: AudioConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioConfig {
    pub host: Option<String>,
    pub output: Option<String>,
    pub input: Option<String>,
    pub sample_rate: Option<u32>,
    pub buffer_size: Option<u32>,
    /// ALSA periods, 2-8 (REAPER's default 3).
    pub periods: Option<u32>,
    pub output_channels: Option<u16>,
    pub input_channels: Option<u16>,
    /// Run without opening any audio device (silent, for tests and dry runs).
    #[serde(default)]
    pub null: bool,
}

/// Everything a headless run needs, after the file and the flags are combined.
#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    pub project: Option<PathBuf>,
    pub control: Option<SocketAddr>,
    pub token: Option<String>,
    pub folders: Vec<PathBuf>,
    pub access_file: Option<PathBuf>,
    pub devices: DeviceRequest,
    pub null_audio: bool,
    pub list_devices: bool,
    /// `--list-devices` prints every device name, not only those the device page offers.
    pub list_all: bool,
    /// Write a systemd user unit running with these options instead of running.
    pub install_service: bool,
    /// Ask the running session how it is doing instead of running.
    pub status: bool,
}

/// `$XDG_CONFIG_HOME/scythe/headless.toml`.
pub fn default_path() -> Option<PathBuf> {
    scythe_core::paths::config_home().map(|c| c.join("scythe").join("headless.toml"))
}

pub fn read(path: &Path) -> Result<Config, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

pub const USAGE: &str = "\
usage: scythe-server [project.RPP] [options]

  --config FILE          settings file (default $XDG_CONFIG_HOME/scythe/headless.toml)
  --control ADDR         control API address (default 127.0.0.1:8770), `off` to disable
  --token TOKEN          the admin token clients need (also SCYTHE_CONTROL_TOKEN)
  --folder DIR           a folder the web UI may browse for projects (repeatable; default: home)
  --access-file FILE     where access links are kept (default $XDG_CONFIG_HOME/scythe/access.json)
  --audio-host NAME      audio host (ALSA, JACK, ...)
  --output-device NAME   output device, exact or part of its name
  --input-device NAME    input device for monitoring and recording
  --sample-rate HZ       device sample rate
  --buffer-size FRAMES   frames per audio callback
  --periods N            ALSA periods, the blocks the device ring holds (2-8, default 3)
  --output-channels N    output channels to open (default: all of an interface, else 2)
  --input-channels N     input channels to open, likewise
  --null-audio           open no audio device at all
  --no-input             never open an audio or MIDI input
  --list-devices         print the audio devices as the device page offers them, and exit
  --all                  with --list-devices: every device name the audio systems know
  --install-service      write a systemd user unit that runs with these options, and exit (Linux)
  --status               show how the running session is doing (device, xruns, clients)
  --version              print the version, and exit
  --help                 print this help, and exit
";

/// Flags followed by a value.
pub const VALUED: [&str; 13] = [
    "--config",
    "--control",
    "--token",
    "--folder",
    "--access-file",
    "--audio-host",
    "--output-device",
    "--input-device",
    "--sample-rate",
    "--buffer-size",
    "--periods",
    "--output-channels",
    "--input-channels",
];

/// The audio device flags among `args` (`--audio-host`, `--output-device`, `--sample-rate`…,
/// as `--flag value` or `--flag=value`), which override a settings file or saved preferences.
/// `null` is `--null-audio`.
pub fn audio_flags(args: &[String]) -> Result<AudioConfig, String> {
    let value = |name: &str| {
        let prefix = format!("{name}=");
        args.iter().enumerate().rev().find_map(|(i, a)| match a.strip_prefix(&prefix) {
            Some(v) => Some(v.to_owned()),
            None if a == name => args.get(i + 1).cloned(),
            None => None,
        })
    };
    let number = |name: &str| -> Result<Option<u32>, String> {
        value(name).map(|v| v.parse().map_err(|_| format!("{name} takes a number, not {v}"))).transpose()
    };
    let channels = |name: &str| -> Result<Option<u16>, String> {
        number(name)?.map(|n| u16::try_from(n).map_err(|_| format!("{name} is too large"))).transpose()
    };
    Ok(AudioConfig {
        host: value("--audio-host"),
        output: value("--output-device"),
        input: value("--input-device"),
        sample_rate: number("--sample-rate")?,
        buffer_size: number("--buffer-size")?,
        periods: number("--periods")?,
        output_channels: channels("--output-channels")?,
        input_channels: channels("--input-channels")?,
        null: args.iter().any(|a| a == "--null-audio"),
    })
}

/// Prints the audio devices as `--list-devices` does: those the device page offers (one per
/// sound card on ALSA, as REAPER lists them), or with `all` every device name the hosts know.
pub fn print_devices(all: bool) {
    let devices = if all { scythe_engine::devices::list_all() } else { scythe_engine::devices::list() };
    for line in device_lines(&devices) {
        println!("{line}");
    }
}

fn device_lines(devices: &[scythe_engine::devices::DeviceEntry]) -> Vec<String> {
    devices
        .iter()
        .map(|d| {
            let dir = match (d.input, d.output) {
                (true, true) => "in/out",
                (true, false) => "in",
                _ => "out",
            };
            let name = d.name.lines().next().unwrap_or_default();
            let name = if d.id == name { d.id.clone() } else { format!("{} ; {name}", d.id) };
            format!("{:<10} {:<7} {name}", d.host, dir)
        })
        .collect()
}

/// Combines the settings file with `args` (the arguments after the program name, `--headless`
/// already taken out).
pub fn options(args: &[String]) -> Result<Options, String> {
    let mut flags: Vec<(String, Option<String>)> = Vec::new();
    let mut positional: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if let Some((name, value)) = arg.split_once('=').filter(|(n, _)| n.starts_with("--")) {
            flags.push((name.to_owned(), Some(value.to_owned())));
        } else if VALUED.contains(&arg.as_str()) {
            let value = it.next().ok_or_else(|| format!("{arg} needs a value"))?;
            flags.push((arg.clone(), Some(value.clone())));
        } else if arg.starts_with("--") {
            flags.push((arg.clone(), None));
        } else {
            positional.push(arg.clone());
        }
    }
    let flag = |name: &str| flags.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v.clone());
    let value = |name: &str| flag(name).flatten();
    let config = match value("--config") {
        Some(path) => read(Path::new(&path))?,
        None => match default_path().filter(|p| p.is_file()) {
            Some(path) => read(&path)?,
            None => Config::default(),
        },
    };
    for (name, _) in &flags {
        let known = VALUED.contains(&name.as_str())
            || ["--null-audio", "--no-input", "--list-devices", "--all", "--headless", "--install-service", "--status"]
                .contains(&name.as_str());
        if !known {
            return Err(format!("unknown option {name}"));
        }
    }
    if positional.len() > 1 {
        return Err("give at most one project".into());
    }

    let control = match value("--control").or(config.control) {
        Some(c) if c == "off" || c == "none" => None,
        Some(c) => Some(c.parse::<SocketAddr>().map_err(|_| format!("{c} is not an address like 0.0.0.0:8770"))?),
        None => Some(crate::DEFAULT_ADDR.parse().expect("the default address parses")),
    };
    let token = value("--token")
        .or_else(|| std::env::var("SCYTHE_CONTROL_TOKEN").ok())
        .or(config.token)
        .filter(|t| !t.is_empty());
    let folders: Vec<PathBuf> =
        flags.iter().filter(|(n, _)| n == "--folder").filter_map(|(_, v)| v.clone().map(PathBuf::from)).collect();
    let folders = if folders.is_empty() { config.folders } else { folders };
    let access_file =
        value("--access-file").map(PathBuf::from).or(config.access_file).or_else(crate::access::default_file);
    let flags = audio_flags(args)?;
    let audio = config.audio;
    let devices = DeviceRequest {
        host: flags.host.or(audio.host),
        output: flags.output.or(audio.output),
        input: flags.input.or(audio.input),
        sample_rate: flags.sample_rate.or(audio.sample_rate),
        buffer_size: flags.buffer_size.or(audio.buffer_size),
        periods: flags.periods.or(audio.periods),
        output_channels: flags.output_channels.or(audio.output_channels),
        input_channels: flags.input_channels.or(audio.input_channels),
        no_input: flag("--no-input").is_some() || config.no_input,
        ..Default::default()
    };
    Ok(Options {
        project: positional.pop().map(PathBuf::from).or(config.project),
        control,
        token,
        folders,
        access_file,
        devices,
        null_audio: flag("--null-audio").is_some() || audio.null,
        list_devices: flag("--list-devices").is_some(),
        list_all: flag("--all").is_some(),
        install_service: flag("--install-service").is_some(),
        status: flag("--status").is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn flags_override_the_file() {
        let dir = std::env::temp_dir().join(format!("scythe-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("headless.toml");
        std::fs::write(
            &file,
            "project = \"/a.RPP\"\ncontrol = \"0.0.0.0:9000\"\n[audio]\noutput = \"USB\"\nsample_rate = 44100\n",
        )
        .unwrap();
        let o = options(&args(&["--config", file.to_str().unwrap(), "--sample-rate=96000", "b.RPP"])).unwrap();
        assert_eq!(o.project, Some(PathBuf::from("b.RPP")));
        assert_eq!(o.control, Some("0.0.0.0:9000".parse().unwrap()));
        assert_eq!(o.devices.output.as_deref(), Some("USB"));
        assert_eq!(o.devices.sample_rate, Some(96000));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn control_can_be_turned_off_and_bad_flags_are_refused() {
        let empty = std::env::temp_dir().join(format!("scythe-empty-{}.toml", std::process::id()));
        std::fs::write(&empty, "").unwrap();
        let config = format!("--config={}", empty.display());
        let o = options(&args(&[&config, "--control", "off", "--null-audio"])).unwrap();
        assert_eq!(o.control, None);
        assert!(o.null_audio);
        assert!(options(&args(&[&config, "--frobnicate"])).is_err());
        assert!(options(&args(&[&config, "--sample-rate", "fast"])).is_err());
        std::fs::remove_file(&empty).unwrap();
    }
}
