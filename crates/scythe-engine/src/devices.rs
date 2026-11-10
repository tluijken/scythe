//! Picking the audio host and devices by name, for setups that do not want the defaults (a
//! headless machine with an audio interface beside the onboard card), and what those devices
//! offer: channel counts, rates, block sizes and channel names.

use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{SampleFormat, StreamConfig};

/// The audio system that opens no device and runs on a silent clock, REAPER's "Dummy Audio".
pub const DUMMY_HOST: &str = "Dummy";

/// cpal enumerates at most this many channels; a device reporting it is a plugin (`plughw`,
/// `default`, JACK) that takes any count, not hardware with a fixed set.
const CHANNEL_CAP: u16 = 64;

/// Which audio devices to open. Unset fields take the host's defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceRequest {
    /// The audio host by name (`ALSA`, `JACK`, [`DUMMY_HOST`]…), case-insensitive.
    pub host: Option<String>,
    /// The output device: its id (`hw:CARD=USB,DEV=0`), its exact name, else the first whose
    /// name contains this.
    pub output: Option<String>,
    /// The input device for monitoring and recording, matched like `output`.
    pub input: Option<String>,
    pub sample_rate: Option<u32>,
    /// Frames per callback.
    pub buffer_size: Option<u32>,
    /// Output channels to open (REAPER's "Output channels"); unset opens every channel of a
    /// device with a fixed set, else its default (a stereo pair).
    pub output_channels: Option<u16>,
    /// Input channels to open, chosen like `output_channels`.
    pub input_channels: Option<u16>,
    /// Bits per sample to ask for (REAPER's ALSA "Bit depth": 16, 24 or 32 bit integer); the
    /// device's own format when it does not offer it.
    pub bits: Option<u16>,
    /// JACK: start the server when it is not running, with this command (empty: as libjack
    /// starts it, from `~/.jackdrc`).
    pub jack_start: Option<String>,
    /// JACK: leave Scythe's ports unconnected instead of connecting them to the system ports.
    pub jack_no_connect: bool,
    /// Never open an input: no monitoring, no recording.
    pub no_input: bool,
}

impl DeviceRequest {
    /// Whether this asks for REAPER's "Dummy Audio": no device, a silent clock.
    pub fn is_dummy(&self) -> bool {
        self.host.as_deref().is_some_and(|h| h.eq_ignore_ascii_case(DUMMY_HOST))
    }
}

/// One device a host offers, as `list` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceEntry {
    pub host: String,
    /// What the host calls it (`hw:CARD=USB,DEV=0` on ALSA), stable across restarts.
    pub id: String,
    pub name: String,
    pub input: bool,
    pub output: bool,
}

/// What one side of a device can be opened with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// Channel counts it can be opened with, ascending.
    pub channels: Vec<u16>,
    /// The common rates it supports, ascending.
    pub sample_rates: Vec<u32>,
    /// Frames per callback it accepts, when it says.
    pub buffer_sizes: Option<(u32, u32)>,
    /// The channel count opened when none is asked for.
    pub default_channels: u16,
}

/// Rates REAPER's device page offers.
pub const COMMON_RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];
/// Block sizes REAPER's device page offers.
pub const COMMON_BLOCK_SIZES: [u32; 8] = [16, 32, 64, 128, 256, 512, 1024, 2048];

pub fn host(request: &DeviceRequest) -> Result<cpal::Host, String> {
    let Some(name) = request.host.as_deref() else { return Ok(cpal::default_host()) };
    let id = cpal::available_hosts()
        .into_iter()
        .find(|id| id.name().eq_ignore_ascii_case(name))
        .ok_or_else(|| format!("no audio host named {name}"))?;
    cpal::host_from_id(id).map_err(|e| e.to_string())
}

/// The audio systems to choose from: every host cpal can use here, then [`DUMMY_HOST`].
pub fn hosts() -> Vec<String> {
    let mut out: Vec<String> = cpal::available_hosts().into_iter().map(|id| id.name().to_owned()).collect();
    out.push(DUMMY_HOST.to_owned());
    out
}

/// The host a request ends up on, by name.
pub fn host_name(request: &DeviceRequest) -> String {
    if request.is_dummy() {
        return DUMMY_HOST.to_owned();
    }
    host(request).map(|h| h.id().name().to_owned()).unwrap_or_else(|_| request.host.clone().unwrap_or_default())
}

pub fn device_name(device: &cpal::Device) -> String {
    device.description().map(|d| d.name().to_owned()).unwrap_or_else(|_| "Unnamed device".into())
}

/// The id the host gives `device`, its name when it gives none.
pub fn device_id(device: &cpal::Device) -> String {
    device.id().map(|id| id.id().to_owned()).unwrap_or_else(|_| device_name(device))
}

fn pick(devices: impl Iterator<Item = cpal::Device>, wanted: &str) -> Option<cpal::Device> {
    let named: Vec<(String, String, cpal::Device)> = devices.map(|d| (device_id(&d), device_name(&d), d)).collect();
    let lower = wanted.to_lowercase();
    let by_id = named.iter().position(|(id, _, _)| id == wanted);
    let exact = || named.iter().position(|(_, n, _)| n == wanted);
    let partial = || named.iter().position(|(_, n, _)| n.to_lowercase().contains(&lower));
    by_id.or_else(exact).or_else(partial).map(|i| named[i].2.clone())
}

pub fn output_device(request: &DeviceRequest) -> Result<cpal::Device, String> {
    if request.is_dummy() {
        return Err("the dummy audio system opens no device".into());
    }
    start_jack(request)?;
    let host = host(request)?;
    match request.output.as_deref() {
        Some(name) => pick(host.output_devices().map_err(|e| e.to_string())?, name)
            .ok_or_else(|| format!("no output device named {name}")),
        None => host.default_output_device().ok_or_else(|| "no default output device".into()),
    }
}

pub fn input_device(request: &DeviceRequest) -> Result<cpal::Device, String> {
    if request.no_input {
        return Err("audio input is turned off".into());
    }
    if request.is_dummy() {
        return Err("the dummy audio system has no inputs".into());
    }
    start_jack(request)?;
    let host = host(request)?;
    match request.input.as_deref() {
        Some(name) => pick(host.input_devices().map_err(|e| e.to_string())?, name)
            .ok_or_else(|| format!("no input device named {name}")),
        None => host.default_input_device().ok_or_else(|| "no audio input device".into()),
    }
}

/// Every device of every available host.
pub fn list() -> Vec<DeviceEntry> {
    let mut out: Vec<DeviceEntry> = Vec::new();
    for id in cpal::available_hosts() {
        let Ok(host) = cpal::host_from_id(id) else { continue };
        let mut add = |d: &cpal::Device, input: bool| {
            let (dev_id, name) = (device_id(d), device_name(d));
            match out.iter_mut().find(|e| e.host == id.name() && e.id == dev_id) {
                Some(e) if input => e.input = true,
                Some(e) => e.output = true,
                None => out.push(DeviceEntry { host: id.name().to_owned(), id: dev_id, name, input, output: !input }),
            }
        };
        for d in host.output_devices().into_iter().flatten() {
            add(&d, false);
        }
        for d in host.input_devices().into_iter().flatten() {
            add(&d, true);
        }
    }
    out
}

/// What the output (or, with `input`, the input) device `request` names can be opened with.
pub fn capabilities(request: &DeviceRequest, input: bool) -> Result<Capabilities, String> {
    let device = if input { input_device(request)? } else { output_device(request)? };
    let (default, ranges) = if input {
        let d = device.default_input_config().map_err(|e| e.to_string())?;
        (d, device.supported_input_configs().map_err(|e| e.to_string())?.collect::<Vec<_>>())
    } else {
        let d = device.default_output_config().map_err(|e| e.to_string())?;
        (d, device.supported_output_configs().map_err(|e| e.to_string())?.collect::<Vec<_>>())
    };
    let ranges: Vec<_> = ranges.into_iter().filter(|r| r.sample_format() == default.sample_format()).collect();
    let mut channels: Vec<u16> = ranges.iter().map(|r| r.channels()).collect();
    channels.sort_unstable();
    channels.dedup();
    let sample_rates = COMMON_RATES
        .into_iter()
        .filter(|rate| ranges.iter().any(|r| (r.min_sample_rate()..=r.max_sample_rate()).contains(rate)))
        .collect();
    let buffer_sizes = ranges.iter().find_map(|r| match r.buffer_size() {
        cpal::SupportedBufferSize::Range { min, max } => Some((*min, *max)),
        cpal::SupportedBufferSize::Unknown => None,
    });
    Ok(Capabilities { channels, sample_rates, buffer_sizes, default_channels: default.channels() })
}

/// How many channels to open: what was asked for (the fewest the device offers that are at
/// least that many, else its most), or with nothing asked every channel of a device with a
/// fixed set and the default count of one taking any.
pub fn stream_channels(supported: &[u16], default: u16, wanted: Option<u16>) -> u16 {
    let most = supported.iter().copied().max();
    match wanted {
        Some(n) => supported.iter().copied().filter(|&c| c >= n).min().or(most).unwrap_or(n),
        None => most.filter(|&m| m < CHANNEL_CAP).unwrap_or(default),
    }
}

/// The configuration to open `device`'s input with at `rate`: the format `request.bits` asks
/// for when the input offers it, else its default, and the channel count
/// `request.input_channels` picks (see [`stream_channels`]).
pub fn input_config(
    device: &cpal::Device,
    request: &DeviceRequest,
    rate: u32,
) -> Result<(StreamConfig, SampleFormat), String> {
    let wanted = request.input_channels;
    let default = device.default_input_config().map_err(|e| e.to_string())?;
    let offered: Vec<(SampleFormat, u32, u32)> = device
        .supported_input_configs()
        .map(|c| c.map(|c| (c.sample_format(), c.min_sample_rate(), c.max_sample_rate())).collect())
        .unwrap_or_default();
    let format = format_for(request.bits, &offered, rate).unwrap_or(default.sample_format());
    let mut supported: Vec<u16> = device
        .supported_input_configs()
        .map(|configs| {
            configs
                .filter(|c| c.sample_format() == format && (c.min_sample_rate()..=c.max_sample_rate()).contains(&rate))
                .map(|c| c.channels())
                .collect()
        })
        .unwrap_or_default();
    supported.sort_unstable();
    supported.dedup();
    if supported.is_empty() {
        return Err(format!("the input does not run at {rate} Hz"));
    }
    let mut config = default.config();
    config.sample_rate = rate;
    config.channels = stream_channels(&supported, config.channels, wanted);
    Ok((config, format))
}

/// The integer format for REAPER's bit depth (16, 24, 32) among `offered` (format, lowest and
/// highest rate), when one runs at `rate`.
pub fn format_for(bits: Option<u16>, offered: &[(SampleFormat, u32, u32)], rate: u32) -> Option<SampleFormat> {
    let format = match bits? {
        16 => SampleFormat::I16,
        24 => SampleFormat::I24,
        32 => SampleFormat::I32,
        _ => return None,
    };
    offered.iter().any(|(f, lo, hi)| *f == format && (*lo..=*hi).contains(&rate)).then_some(format)
}

/// Starts the JACK server as REAPER's "Auto-start jackd" does, when the request is for JACK,
/// asks for it, and no server runs: with the launch command, else as libjack starts one.
#[cfg(target_os = "linux")]
fn start_jack(request: &DeviceRequest) -> Result<(), String> {
    let Some(command) = request.jack_start.as_deref() else { return Ok(()) };
    let jack = request.host.as_deref().is_some_and(|h| h.eq_ignore_ascii_case("JACK"));
    let running = || jack::Client::new("scythe_probe", jack::ClientOptions::NO_START_SERVER).is_ok();
    if !jack || running() {
        return Ok(());
    }
    if command.trim().is_empty() {
        return jack::Client::new("scythe_probe", jack::ClientOptions::empty())
            .map(drop)
            .map_err(|e| format!("starting the JACK server: {e}"));
    }
    std::process::Command::new("sh")
        .args(["-c", command])
        .stdin(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("{command}: {e}"))?;
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if running() {
            return Ok(());
        }
    }
    Err(format!("the JACK server did not start with {command}"))
}

#[cfg(not(target_os = "linux"))]
fn start_jack(_request: &DeviceRequest) -> Result<(), String> {
    Ok(())
}

/// Disconnects every port of JACK clients named `client` (REAPER's auto-connect turned off),
/// which the audio backend connects to the system ports as it opens them.
#[cfg(target_os = "linux")]
pub fn jack_disconnect(client: &str) {
    let Ok((jack, _)) = jack::Client::new("scythe_patch", jack::ClientOptions::NO_START_SERVER) else { return };
    for port in jack.ports(Some(&format!("^{client}:")), None, jack::PortFlags::empty()) {
        let Some(p) = jack.port_by_name(&port) else { continue };
        for other in p.get_connections() {
            let _ = jack.disconnect_ports_by_name(&port, &other);
            let _ = jack.disconnect_ports_by_name(&other, &port);
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub fn jack_disconnect(_client: &str) {}

/// The names REAPER gives a device's channels: `Input 3`, `Output 12`; on JACK the system port
/// each is connected to comes first, `system:capture_3 (Input 3)`.
pub fn channel_names(host: &str, input: bool, count: usize) -> Vec<String> {
    let side = if input { "Input" } else { "Output" };
    let ports = if host.eq_ignore_ascii_case("JACK") { jack_system_ports(input) } else { Vec::new() };
    (0..count)
        .map(|i| match ports.get(i) {
            Some(port) => format!("{port} ({side} {})", i + 1),
            None => format!("{side} {}", i + 1),
        })
        .collect()
}

/// The JACK system ports in the order the host connects to them, empty without a server.
#[cfg(target_os = "linux")]
fn jack_system_ports(input: bool) -> Vec<String> {
    let Ok((client, _)) = jack::Client::new("scythe_ports", jack::ClientOptions::NO_START_SERVER) else {
        return Vec::new();
    };
    let pattern = if input { "system:capture_.*" } else { "system:playback_.*" };
    client.ports(Some(pattern), None, jack::PortFlags::empty())
}

#[cfg(not(target_os = "linux"))]
fn jack_system_ports(_input: bool) -> Vec<String> {
    Vec::new()
}

/// What REAPER calls the device in its channel alias sections (`[alias_in_<ident>]`): `ALSA
/// <device id>`, `JackIn`/`JackOut` on JACK.
pub fn ident(host: &str, device_id: &str, input: bool) -> String {
    if host.eq_ignore_ascii_case("JACK") {
        return if input { "JackIn" } else { "JackOut" }.to_owned();
    }
    if host.eq_ignore_ascii_case(DUMMY_HOST) {
        return "Dummy Audio".to_owned();
    }
    format!("{host} {device_id}")
}

/// The name of a pair of channels (counted from 0) in ReaInsert's lists: the two channel
/// names and the device.
pub fn pair_name(first: usize, names: &[String], output: bool, device: Option<&str>) -> String {
    let side = if output { "Out" } else { "In" };
    let name = |i: usize| names.get(i).cloned().unwrap_or_else(|| format!("{side} {}", i + 1));
    let pair = format!("{} / {}", name(first), name(first + 1));
    match device {
        Some(d) if !d.is_empty() => format!("{pair} · {d}"),
        _ => pair,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_counts_open_a_fixed_device_whole_and_a_plugin_as_a_pair() {
        let interface: Vec<u16> = vec![18];
        assert_eq!(stream_channels(&interface, 18, None), 18);
        let plugin: Vec<u16> = (1..=64).collect();
        assert_eq!(stream_channels(&plugin, 2, None), 2);
        assert_eq!(stream_channels(&plugin, 2, Some(20)), 20);
        assert_eq!(stream_channels(&[2, 8, 10], 2, Some(9)), 10);
        assert_eq!(stream_channels(&[2, 8], 2, Some(12)), 8);
        assert_eq!(stream_channels(&[], 2, Some(6)), 6);
        assert_eq!(stream_channels(&[], 2, None), 2);
    }

    #[test]
    fn channels_are_named_as_reaper_names_them() {
        assert_eq!(channel_names("ALSA", true, 3), ["Input 1", "Input 2", "Input 3"]);
        assert_eq!(channel_names("ALSA", false, 1), ["Output 1"]);
        assert_eq!(ident("ALSA", "null", true), "ALSA null");
        assert_eq!(ident("JACK", "input", true), "JackIn");
        let names = channel_names("ALSA", false, 4);
        assert_eq!(pair_name(2, &names, true, Some("USB")), "Output 3 / Output 4 · USB");
        assert_eq!(pair_name(4, &names, false, None), "In 5 / In 6");
    }

    #[test]
    fn the_dummy_system_opens_nothing() {
        let r = DeviceRequest { host: Some("dummy".into()), ..Default::default() };
        assert!(r.is_dummy());
        assert!(output_device(&r).is_err());
        assert!(input_device(&r).is_err());
        assert_eq!(host_name(&r), DUMMY_HOST);
        assert_eq!(hosts().last().map(String::as_str), Some(DUMMY_HOST));
    }
}
