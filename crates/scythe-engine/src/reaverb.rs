//! Convolution reverb that reads the settings ReaVerb stores in a project: dry and wet gains and
//! the impulse response its modules build: the file loader's file, the echo generator's taps
//! and the reverse module. Behaviour was matched against REAPER renders (wet = IR scaled by
//! the wet gain, no normalisation, no added latency).
//!
//! Generators add into the impulse response, which is as long as the longest of them; the
//! echo generator (`ECHOGEN`: start ms, length ms, spacing ms, first gain, last gain, jitter,
//! stereo) puts a tap every spacing from the start while less than the length has passed,
//! each at frame `trunc(start × rate / 1000) + trunc(t × rate / 1000)` with a gain heading in
//! dB from the first to the last (floored at -150 dB) over the length. `REVERSE` (mix) plays
//! the response backwards scaled by the mix. Jittered echoes and the reverb generator use
//! REAPER's own random sequence, and the filter, normalise and channel tool modules are not
//! modelled; states with them are refused, so REAPER's binary runs them when installed.

use std::path::{Path, PathBuf};

use crate::convolve::Convolver;
use crate::fx::{Effect, ProcessContext};
use crate::media::{decode_file, resample};

#[derive(Debug, Clone, PartialEq)]
pub struct ReaVerbSettings {
    pub wet: f32,
    pub dry: f32,
    pub wet_muted: bool,
    pub ir: Option<PathBuf>,
    /// Echo generator and reverse modules, applied after the file in order.
    pub generators: Vec<Generator>,
    /// Modules this reimplementation lacks.
    pub unsupported: Vec<String>,
}

impl ReaVerbSettings {
    /// Header of ten floats (format marker, wet gain, dry gain, wet mute, …) followed by
    /// modules: an 8-byte tag, a u32 size and the module data.
    pub fn parse(chunk: &[u8]) -> Option<Self> {
        let f = |i: usize| chunk.get(i * 4..i * 4 + 4).map(|b| f32::from_le_bytes(b.try_into().unwrap()));
        if f(0)? != -9.0 {
            return None;
        }
        let mut s = Self {
            wet: f(1)?,
            dry: f(2)?,
            wet_muted: f(3)? != 0.0,
            ir: None,
            generators: Vec::new(),
            unsupported: Vec::new(),
        };
        let floats =
            |data: &[u8]| -> Vec<f32> { data.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect() };
        let mut at = 40;
        while at + 12 <= chunk.len() {
            let tag = String::from_utf8_lossy(&chunk[at..at + 8]).trim_end_matches('\0').to_owned();
            let size = u32::from_le_bytes(chunk[at + 8..at + 12].try_into().ok()?) as usize;
            let data = chunk.get(at + 12..at + 12 + size)?;
            match tag.as_str() {
                "FILELDR" if s.ir.is_none() => {
                    let path = data.get(4..)?.split(|&b| b == 0).next()?;
                    s.ir = Some(PathBuf::from(String::from_utf8_lossy(path).into_owned()));
                }
                "ECHOGEN" => match floats(data)[..] {
                    [start, length, spacing, first, last, 0.0, ..] => {
                        s.generators.push(Generator::Echo { start, length, spacing, first, last })
                    }
                    _ => s.unsupported.push("ECHOGEN with jitter".into()),
                },
                "REVERSE" => s.generators.push(Generator::Reverse(floats(data).first().copied().unwrap_or(1.0))),
                _ => s.unsupported.push(tag),
            }
            at += 12 + size;
        }
        Some(s)
    }
}

/// A module that builds or changes the impulse response.
#[derive(Debug, Clone, PartialEq)]
pub enum Generator {
    Echo { start: f32, length: f32, spacing: f32, first: f32, last: f32 },
    Reverse(f32),
}

impl Generator {
    fn apply(&self, ir: Option<[Vec<f32>; 2]>, sample_rate: u32) -> [Vec<f32>; 2] {
        let [mut l, mut r] = ir.unwrap_or_default();
        match *self {
            Generator::Echo { start, length, spacing, first, last } => {
                let ms = |t: f64| (t * sample_rate as f64 / 1000.0) as usize;
                let (start, length, spacing) = (start as f64, length as f64, spacing as f64);
                let end = ms(start) + ms(length);
                if l.len() < end {
                    l.resize(end, 0.0);
                    r.resize(end, 0.0);
                }
                let db = |g: f32| (20.0 * (g as f64).log10()).max(-150.0);
                let (from, to) = (db(first), db(last));
                let mut k = 0.0;
                while spacing > 0.0 && k * spacing < length {
                    let t = k * spacing;
                    let at = ms(start) + ms(t);
                    let gain = 10f64.powf((from + (to - from) * t / length) / 20.0) as f32;
                    if at < l.len() {
                        l[at] += gain;
                        r[at] += gain;
                    }
                    k += 1.0;
                }
            }
            Generator::Reverse(mix) => {
                for ch in [&mut l, &mut r] {
                    ch.reverse();
                    ch.iter_mut().for_each(|s| *s *= mix);
                }
            }
        }
        [l, r]
    }
}

pub struct ReaVerbCompat {
    settings: ReaVerbSettings,
    /// The stored chunk, patched with edited gains on save.
    pub chunk: Vec<u8>,
    left: Option<Convolver>,
    right: Option<Convolver>,
}

impl ReaVerbCompat {
    pub fn new(settings: ReaVerbSettings, sample_rate: u32, project_dir: Option<&Path>) -> Result<Self, String> {
        if !settings.unsupported.is_empty() {
            return Err(format!("ReaVerb modules not supported yet: {}", settings.unsupported.join(", ")));
        }
        let file = match &settings.ir {
            Some(path) => {
                let resolved = match (path.is_absolute(), project_dir) {
                    (false, Some(dir)) => dir.join(path),
                    _ => path.clone(),
                };
                let audio = decode_file(&resolved).map_err(|e| format!("impulse response: {e}"))?;
                let audio = if audio.sample_rate == sample_rate {
                    audio
                } else {
                    resample(&audio, sample_rate).map_err(|e| e.to_string())?
                };
                let l = audio.channels[0].clone();
                let r = audio.channels.get(1).cloned().unwrap_or_else(|| l.clone());
                Some([l, r])
            }
            None => None,
        };
        let ir = settings.generators.iter().fold(file, |ir, g| Some(g.apply(ir, sample_rate)));
        let (left, right) = match ir {
            Some([l, r]) => (Some(Convolver::new(&l)), Some(Convolver::new(&r))),
            None => (None, None),
        };
        Ok(Self { settings, left, right, chunk: Vec::new() })
    }
}

impl Effect for ReaVerbCompat {
    fn params(&mut self) -> Vec<crate::clap_host::ParamInfo> {
        use crate::stock::param;
        vec![param(0, "Wet (dB)", -60.0, 6.0, -12.0, false), param(1, "Dry (dB)", -60.0, 6.0, 0.0, false)]
    }

    fn param(&mut self, id: u32) -> Option<(f64, String)> {
        let g = match id {
            0 => self.settings.wet,
            1 => self.settings.dry,
            _ => return None,
        };
        Some(crate::stock::db_value(g as f64))
    }

    fn set_param(&mut self, id: u32, value: f64) {
        let g = if value <= -59.9 { 0.0 } else { 10f32.powf(value as f32 / 20.0) };
        match id {
            0 => self.settings.wet = g,
            1 => self.settings.dry = g,
            _ => {}
        }
    }

    fn set_normalized(&mut self, index: usize, v: f64) -> bool {
        if index > 1 {
            return false;
        }
        self.set_param(index as u32, crate::normalized::amp2_db(v));
        true
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        let mut c = self.chunk.clone();
        c.get_mut(4..8)?.copy_from_slice(&self.settings.wet.to_le_bytes());
        c.get_mut(8..12)?.copy_from_slice(&self.settings.dry.to_le_bytes());
        Some(c)
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], _ctx: &ProcessContext) {
        let (dry, wet) = (self.settings.dry, if self.settings.wet_muted { 0.0 } else { self.settings.wet });
        for (ch, buf) in [(&mut self.left, left), (&mut self.right, right)] {
            match ch {
                Some(conv) => {
                    for s in buf.iter_mut() {
                        let w = conv.process_sample(*s);
                        *s = *s * dry + w * wet;
                    }
                }
                None => buf.iter_mut().for_each(|s| *s *= dry),
            }
        }
    }

    fn set_named_param(&mut self, name: &str, value: f64) -> bool {
        match name {
            "_Wet" => self.settings.wet = value as f32,
            "_Dry" => self.settings.dry = value as f32,
            _ => return false,
        }
        true
    }

    fn reset(&mut self) {
        for c in [&mut self.left, &mut self.right].into_iter().flatten() {
            c.reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_header_and_file_loader() {
        let mut d: Vec<u8> =
            [-9.0f32, 0.25, 1.0, 0.0, 1.0, 1.0, 0.5, 1.0, 1.0, 2.0].iter().flat_map(|v| v.to_le_bytes()).collect();
        let path = b"/ir/cab.wav\0";
        d.extend(b"FILELDR\0");
        d.extend(((4 + path.len()) as u32).to_le_bytes());
        d.extend(0x3cu32.to_le_bytes());
        d.extend(path);
        let s = ReaVerbSettings::parse(&d).unwrap();
        assert_eq!((s.wet, s.dry, s.wet_muted), (0.25, 1.0, false));
        assert_eq!(s.ir, Some(PathBuf::from("/ir/cab.wav")));
        assert!(s.unsupported.is_empty());
    }

    /// Taps REAPER 7.73 rendered for an echo generator at 48 kHz: 33.3 ms in, every 7.3 ms.
    #[test]
    fn echo_taps_match_reaper() {
        let echo = Generator::Echo { start: 33.3, length: 120.0, spacing: 7.3, first: 0.8, last: 0.0 };
        let [l, r] = echo.apply(None, 48000);
        assert_eq!(l, r);
        let taps: Vec<usize> = (0..l.len()).filter(|&i| l[i] != 0.0).collect();
        assert_eq!(&taps[..4], &[1598, 1948, 2298, 2649]);
        assert_eq!(taps.last(), Some(&7204));
        assert!((l[1948] / 0.8 - 0.354_52).abs() < 1e-4);
        let [rev, _] = Generator::Reverse(0.5).apply(Some([l.clone(), r]), 48000);
        assert_eq!(rev[l.len() - 1 - 1598], 0.4);
    }
}
