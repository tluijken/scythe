//! What a media file holds, from its header, without decoding it.

use std::fs::File;
use std::path::Path;

use serde::{Deserialize, Serialize};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileInfo {
    /// The container, from the extension: `wav`, `flac`, `mp3`…
    pub format: String,
    pub sample_rate: u32,
    pub channels: usize,
    /// Sample frames, when the header says.
    pub frames: Option<u64>,
    /// Seconds, when the header says.
    pub duration: Option<f64>,
    /// Whether `frames` is the length REAPER gives an item of the file: true for PCM and FLAC,
    /// false for codecs with encoder delay (MP3, AAC, Vorbis, Opus), which have to be decoded.
    pub exact: bool,
}

/// Lossless formats whose header length is the decoded length.
const EXACT: [&str; 8] = ["wav", "wave", "w64", "aif", "aiff", "aifc", "flac", "caf"];

pub fn probe(path: &Path) -> Result<FileInfo, String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let mut hint = Hint::new();
    hint.with_extension(&ext);
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| format!("{}: not a media file Scythe reads ({e})", path.display()))?;
    let track = format.default_track(TrackType::Audio).ok_or_else(|| format!("{}: no audio in it", path.display()))?;
    let params = track.codec_params.as_ref().and_then(|p| p.audio());
    let sample_rate = params.and_then(|p| p.sample_rate).unwrap_or(0);
    let channels = params.and_then(|p| p.channels.as_ref()).map_or(0, |c| c.count());
    let frames = track.num_frames;
    let duration = frames.filter(|_| sample_rate > 0).map(|f| f as f64 / sample_rate as f64);
    let exact = EXACT.contains(&ext.as_str()) && frames.is_some() && sample_rate > 0;
    Ok(FileInfo { format: ext, sample_rate, channels, frames, duration, exact })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_wav_header() {
        let dir = std::env::temp_dir().join(format!("scythe-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for _ in 0..44_100 * 3 {
            w.write_sample(0i16).unwrap();
        }
        w.finalize().unwrap();
        let info = probe(&path).unwrap();
        assert_eq!((info.sample_rate, info.channels, info.frames), (44_100, 2, Some(66_150)));
        assert!(info.exact);
        assert!((info.duration.unwrap() - 1.5).abs() < 1e-9);
        assert!(probe(&dir.join("missing.wav")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
