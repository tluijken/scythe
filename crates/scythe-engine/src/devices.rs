//! Picking the audio host and devices by name, for setups that do not want the defaults (a
//! headless machine with an audio interface beside the onboard card).

use cpal::traits::{DeviceTrait, HostTrait};

/// Which audio devices to open. Unset fields take the host's defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceRequest {
    /// The audio host by name (`ALSA`, `JACK`, `PipeWire`…), case-insensitive.
    pub host: Option<String>,
    /// The output device: its exact name, else the first whose name contains this.
    pub output: Option<String>,
    /// The input device for monitoring and recording, matched like `output`.
    pub input: Option<String>,
    pub sample_rate: Option<u32>,
    /// Frames per callback.
    pub buffer_size: Option<u32>,
    /// Output channels to open at least, so hardware outputs beyond the first pair are there.
    pub output_channels: Option<u16>,
    /// Never open an input: no monitoring, no recording.
    pub no_input: bool,
}

/// One device a host offers, as `list` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceEntry {
    pub host: String,
    pub name: String,
    pub input: bool,
    pub output: bool,
}

pub fn host(request: &DeviceRequest) -> Result<cpal::Host, String> {
    let Some(name) = request.host.as_deref() else { return Ok(cpal::default_host()) };
    let id = cpal::available_hosts()
        .into_iter()
        .find(|id| id.name().eq_ignore_ascii_case(name))
        .ok_or_else(|| format!("no audio host named {name}"))?;
    cpal::host_from_id(id).map_err(|e| e.to_string())
}

pub fn device_name(device: &cpal::Device) -> String {
    device.description().map(|d| d.name().to_owned()).unwrap_or_else(|_| "Unnamed device".into())
}

fn pick(devices: impl Iterator<Item = cpal::Device>, wanted: &str) -> Option<cpal::Device> {
    let named: Vec<(String, cpal::Device)> = devices.map(|d| (device_name(&d), d)).collect();
    let lower = wanted.to_lowercase();
    let exact = named.iter().position(|(n, _)| n == wanted);
    let partial = || named.iter().position(|(n, _)| n.to_lowercase().contains(&lower));
    exact.or_else(partial).map(|i| named[i].1.clone())
}

pub fn output_device(request: &DeviceRequest) -> Result<cpal::Device, String> {
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
        let mut add = |name: String, input: bool| match out.iter_mut().find(|e| e.host == id.name() && e.name == name) {
            Some(e) if input => e.input = true,
            Some(e) => e.output = true,
            None => out.push(DeviceEntry { host: id.name().to_owned(), name, input, output: !input }),
        };
        for d in host.output_devices().into_iter().flatten() {
            add(device_name(&d), false);
        }
        for d in host.input_devices().into_iter().flatten() {
            add(device_name(&d), true);
        }
    }
    out
}

/// One side of the devices `request` opens, for naming their channels: the device's name and
/// how many channels its default configuration has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceChannels {
    pub name: String,
    pub channels: u16,
}

/// The output and input devices `request` opens, with their channel counts (`None` for a side
/// that cannot be opened).
pub fn channels(request: &DeviceRequest) -> (Option<DeviceChannels>, Option<DeviceChannels>) {
    let out = output_device(request).ok().and_then(|d| {
        let channels = d.default_output_config().ok()?.channels();
        Some(DeviceChannels { name: device_name(&d), channels: channels.max(request.output_channels.unwrap_or(0)) })
    });
    let inp = input_device(request).ok().and_then(|d| {
        let channels = d.default_input_config().ok()?.channels();
        Some(DeviceChannels { name: device_name(&d), channels })
    });
    (out, inp)
}

/// The name of a pair of device channels (counted from 0) in ReaInsert's lists: the
/// channel numbers and the device.
pub fn pair_name(first: usize, output: bool, device: Option<&str>) -> String {
    let side = if output { "Out" } else { "In" };
    let pair = format!("{side} {} / {side} {}", first + 1, first + 2);
    match device {
        Some(d) if !d.is_empty() => format!("{pair} · {d}"),
        _ => pair,
    }
}
