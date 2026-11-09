//! Reads what the media explorer shows of a file without decoding it: length, rate, channels,
//! bit depth or bitrate, and the tags (title, artist, BPM, key …) of its container.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use scythe_core::mediadb::MediaInfo;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, StandardTag, Tag};

/// The metadata of `path`; `None` when it cannot be read as audio.
pub fn read(path: &Path) -> Option<MediaInfo> {
    if scythe_core::mediadb::is_midi(path) {
        return Some(MediaInfo::default());
    }
    let size = std::fs::metadata(path).ok()?.len();
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mss = MediaSourceStream::new(Box::new(File::open(path).ok()?), Default::default());
    let mut format =
        symphonia::default::get_probe().probe(&hint, mss, FormatOptions::default(), MetadataOptions::default()).ok()?;
    let mut info = MediaInfo::default();
    let track = format.default_track(TrackType::Audio)?;
    let params = track.codec_params.as_ref().and_then(|p| p.audio());
    let rate = params.and_then(|p| p.sample_rate);
    info.sample_rate = rate;
    info.channels = params.and_then(|p| p.channels.as_ref()).map(|c| c.count() as u32);
    let frames = track.num_frames;
    let bits = params.and_then(|p| p.bits_per_sample);
    info.length = match (frames, rate) {
        (Some(n), Some(r)) if r > 0 => Some(n as f64 / r as f64),
        _ => crate::media::decode_file(path).ok().map(|a| a.duration()),
    };
    match bits {
        Some(b) => info.bits = Some(b),
        // REAPER shows the average over the whole file, rounded down.
        None => info.bitrate = info.length.filter(|l| *l > 0.0).map(|l| (size as f64 * 8.0 / l / 1000.0) as u32),
    }
    if let Some(rev) = format.metadata().skip_to_latest() {
        for tag in rev.media.tags.iter().chain(rev.per_track.iter().flat_map(|t| &t.metadata.tags)) {
            apply(&mut info, tag);
        }
    }
    if is_riff(path) {
        riff_extras(path, &mut info);
    }
    Some(info)
}

fn apply(info: &mut MediaInfo, tag: &Tag) {
    let set = |field: &mut Option<String>, v: &str| {
        if field.is_none() && !v.trim().is_empty() {
            *field = Some(v.trim().to_owned());
        }
    };
    match &tag.std {
        Some(StandardTag::TrackTitle(v)) => set(&mut info.title, v),
        Some(StandardTag::Artist(v)) => set(&mut info.artist, v),
        Some(StandardTag::Album(v)) => set(&mut info.album, v),
        Some(StandardTag::RecordingDate(v) | StandardTag::ReleaseDate(v)) => set(&mut info.date, v),
        Some(StandardTag::RecordingYear(y) | StandardTag::ReleaseYear(y)) => set(&mut info.date, &y.to_string()),
        Some(StandardTag::Genre(v)) => set(&mut info.genre, v),
        Some(StandardTag::Comment(v)) => set(&mut info.comment, v),
        Some(StandardTag::Description(v)) => set(&mut info.description, v),
        Some(StandardTag::InitialKey(v)) => set(&mut info.key, v),
        _ => {}
    }
    let key = tag.raw.key.to_ascii_uppercase();
    let value = tag.raw.value.to_string();
    match key.as_str() {
        // The raw value keeps a fractional tempo that the standard tag rounds away.
        "BPM" | "TBPM" | "TEMPO" if info.bpm.is_none() => {
            info.bpm = value.trim().replace(',', ".").parse::<f64>().ok().filter(|b| *b > 0.0)
        }
        "KEY" | "TKEY" | "INITIALKEY" => set(&mut info.key, &value),
        _ => {}
    }
}

fn is_riff(path: &Path) -> bool {
    path.extension().is_some_and(|e| ["wav", "wave", "bwf"].iter().any(|w| e.eq_ignore_ascii_case(w)))
}

/// The BWF description (`bext`) and the ACID tempo (`acid`) of a WAV file.
fn riff_extras(path: &Path, info: &mut MediaInfo) {
    let Ok(mut f) = File::open(path) else { return };
    let mut head = [0u8; 12];
    if f.read_exact(&mut head).is_err() || &head[..4] != b"RIFF" || &head[8..] != b"WAVE" {
        return;
    }
    let mut chunk = [0u8; 8];
    while f.read_exact(&mut chunk).is_ok() {
        let len = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as u64;
        let next = f.stream_position().unwrap_or(0) + len + (len & 1);
        match &chunk[..4] {
            b"bext" if info.description.is_none() => {
                let mut desc = [0u8; 256];
                if f.read_exact(&mut desc).is_ok() {
                    let end = desc.iter().position(|&b| b == 0).unwrap_or(desc.len());
                    let text = String::from_utf8_lossy(&desc[..end]).trim().to_owned();
                    if !text.is_empty() {
                        info.description = Some(text);
                    }
                }
            }
            b"LIST" if len >= 4 => {
                let mut body = vec![0u8; len.min(1 << 20) as usize];
                if f.read_exact(&mut body).is_ok() && body.starts_with(b"INFO") {
                    list_info(&body[4..], info);
                }
            }
            b"acid" if info.bpm.is_none() => {
                let mut acid = [0u8; 24];
                if f.read_exact(&mut acid).is_ok() {
                    let tempo = f32::from_le_bytes([acid[20], acid[21], acid[22], acid[23]]) as f64;
                    if tempo > 0.0 && tempo.is_finite() {
                        info.bpm = Some((tempo * 1000.0).round() / 1000.0);
                    }
                }
            }
            _ => {}
        }
        if f.seek(SeekFrom::Start(next)).is_err() {
            break;
        }
    }
}

/// The text tags of a `LIST` `INFO` chunk.
fn list_info(mut body: &[u8], info: &mut MediaInfo) {
    while body.len() >= 8 {
        let len = u32::from_le_bytes([body[4], body[5], body[6], body[7]]) as usize;
        let Some(data) = body.get(8..8 + len) else { break };
        let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
        let text = String::from_utf8_lossy(&data[..end]).trim().to_owned();
        let field = match &body[..4] {
            b"INAM" => Some(&mut info.title),
            b"IART" => Some(&mut info.artist),
            b"IPRD" => Some(&mut info.album),
            b"ICRD" => Some(&mut info.date),
            b"IGNR" => Some(&mut info.genre),
            b"ICMT" => Some(&mut info.comment),
            _ => None,
        };
        if let Some(field) = field
            && field.is_none()
            && !text.is_empty()
        {
            *field = Some(text);
        }
        body = body.get(8 + len + (len & 1)..).unwrap_or(&[]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav_with(extra: &[(&[u8; 4], Vec<u8>)]) -> std::path::PathBuf {
        let rate = 48_000u32;
        let frames = 4_800u32;
        let mut fmt = Vec::new();
        fmt.extend(1u16.to_le_bytes());
        fmt.extend(2u16.to_le_bytes());
        fmt.extend(rate.to_le_bytes());
        fmt.extend((rate * 4).to_le_bytes());
        fmt.extend(4u16.to_le_bytes());
        fmt.extend(16u16.to_le_bytes());
        let mut chunks: Vec<(&[u8; 4], Vec<u8>)> = vec![(b"fmt ", fmt)];
        chunks.extend(extra.iter().cloned());
        chunks.push((b"data", vec![0; frames as usize * 4]));
        let mut body = b"WAVE".to_vec();
        for (id, data) in chunks {
            body.extend(*id);
            body.extend((data.len() as u32).to_le_bytes());
            body.extend(&data);
        }
        let mut file = b"RIFF".to_vec();
        file.extend((body.len() as u32).to_le_bytes());
        file.extend(body);
        let path = std::env::temp_dir().join(format!("scythe-info-{}-{}.wav", std::process::id(), extra.len()));
        std::fs::write(&path, file).unwrap();
        path
    }

    #[test]
    fn reads_length_format_and_riff_tags() {
        let mut bext = b"Kick, dry".to_vec();
        bext.resize(602, 0);
        let mut acid = vec![0u8; 24];
        acid[20..].copy_from_slice(&122.5f32.to_le_bytes());
        let path = wav_with(&[(b"bext", bext), (b"acid", acid)]);
        let info = read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(info.length, Some(0.1));
        assert_eq!((info.sample_rate, info.channels, info.bits, info.bitrate), (Some(48000), Some(2), Some(16), None));
        assert_eq!(info.description.as_deref(), Some("Kick, dry"));
        assert_eq!(info.bpm, Some(122.5));
    }

    #[test]
    fn not_audio() {
        let path = std::env::temp_dir().join(format!("scythe-info-{}.txt", std::process::id()));
        std::fs::write(&path, "hello").unwrap();
        assert!(read(&path).is_none());
        std::fs::remove_file(&path).unwrap();
    }
}
