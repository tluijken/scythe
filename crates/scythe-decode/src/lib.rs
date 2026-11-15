//! Audio files decoded with Symphonia the way REAPER reads them: what Scythe's media items and
//! ScyEQ-Pro's EQ Match references (in Scythe and in the plugin) are read with.

use std::fmt;
use std::fs::File;
use std::path::Path;

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::codecs::audio::well_known::{CODEC_ID_MP1, CODEC_ID_MP2, CODEC_ID_MP3};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;

/// The file types offered when picking a reference file for EQ Match.
pub const REFERENCE_EXTENSIONS: &[&str] = &["wav", "flac", "mp3", "ogg", "aif", "aiff"];

/// Decoded audio, planar, one `Vec` per channel.
#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    pub sample_rate: u32,
    pub channels: Vec<Vec<f32>>,
}

#[derive(Debug)]
pub enum Error {
    Open(std::io::Error),
    Decode(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Open(e) => write!(f, "cannot open: {e}"),
            Error::Decode(message) => write!(f, "unsupported or corrupt media: {message}"),
        }
    }
}

impl std::error::Error for Error {}

/// Decodes `path` as a REAPER media item reads it, or with `gapless` as ReaSamplOmatic5000
/// loads a sample: MP3 encoder delay and padding (from the LAME header) trimmed.
pub fn decode(path: &Path, gapless: bool) -> Result<Audio, Error> {
    let decode_err = Error::Decode;
    let open_err = Error::Open;
    let file = File::open(path).map_err(open_err)?;
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probe = |source: Box<dyn MediaSource>| {
        let mss = MediaSourceStream::new(source, Default::default());
        symphonia::default::get_probe()
            .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
            .map_err(|e| decode_err(e.to_string()))
    };

    let mut format = probe(Box::new(file))?;
    let is_mp3 = format
        .default_track(TrackType::Audio)
        .and_then(|t| t.codec_params.as_ref())
        .and_then(|p| p.audio())
        .is_some_and(|p| p.codec == CODEC_ID_MP3);
    if is_mp3 {
        let mut bytes = std::fs::read(path).map_err(open_err)?;
        if lower_lsf_mid_side_gain(&mut bytes) {
            format = probe(Box::new(std::io::Cursor::new(bytes)))?;
        }
    }
    let track = format.default_track(TrackType::Audio).ok_or_else(|| decode_err("no audio track".into()))?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| decode_err("no audio codec parameters".into()))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| decode_err(e.to_string()))?;
    // REAPER's MPEG decoder scales its output by 32767/32768. Media items keep the encoder delay
    // and padding and play a Xing/Info/VBRI header frame as one frame of silence.
    let mpeg = [CODEC_ID_MP1, CODEC_ID_MP2, CODEC_ID_MP3].contains(&params.codec);
    let untrimmed = mpeg && !gapless;
    let mut header_frame = untrimmed && starts_with_info_frame(path);

    let mut sample_rate = params.sample_rate.unwrap_or(0);
    let mut channels: Vec<Vec<f32>> = Vec::new();
    let mut planes: Vec<Vec<f32>> = Vec::new();
    loop {
        let mut packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(decode_err(e.to_string())),
        };
        if packet.track_id != track_id {
            continue;
        }
        if untrimmed {
            packet.trim_start = Default::default();
            packet.trim_end = Default::default();
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                sample_rate = buf.spec().rate();
                buf.copy_to_vecs_planar::<f32>(&mut planes);
                let frames = buf.frames();
                if channels.len() < planes.len() {
                    let have = channels.first().map_or(0, Vec::len);
                    channels.resize_with(planes.len(), || vec![0.0; have]);
                }
                if std::mem::take(&mut header_frame) {
                    for dst in &mut channels {
                        dst.resize(dst.len() + frames, 0.0);
                    }
                }
                for (dst, src) in channels.iter_mut().zip(&planes) {
                    dst.extend_from_slice(&src[..frames.min(src.len())]);
                }
            }
            // A corrupt packet is skipped; the rest of the stream is usually fine.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(decode_err(e.to_string())),
        }
    }
    if channels.is_empty() || sample_rate == 0 {
        return Err(decode_err("stream contains no audio".into()));
    }
    if mpeg {
        // The sampler keeps the decoder's 16-bit output; items get it unrounded and unclipped.
        for s in channels.iter_mut().flatten() {
            *s = if gapless {
                (*s * 32767.0).round().clamp(-32768.0, 32767.0) / 32768.0
            } else {
                *s * (32767.0 / 32768.0)
            };
        }
    }
    Ok(Audio { sample_rate, channels })
}

/// REAPER's MP3 decoder plays mid/side granules of MPEG-2 and 2.5 (LSF) Layer III streams
/// 3 dB low: their 1/sqrt(2) mid/side factor is applied twice. MPEG-1 is decoded correctly.
/// Lowering the global gain of such granules by 2 steps (2^(-2/4)) makes a correct decoder
/// produce the same samples. Returns whether any frame changed.
fn lower_lsf_mid_side_gain(bytes: &mut [u8]) -> bool {
    const LSF_BITRATES: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
    const LSF_RATES: [[u32; 3]; 2] = [[11025, 12000, 8000], [22050, 24000, 16000]];
    // (version, frame length) of a Layer III header at `at`, when it is one.
    let header = |bytes: &[u8], at: usize| -> Option<(u32, usize)> {
        let h = u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?);
        let (version, layer, bitrate, rate) = ((h >> 19) & 3, (h >> 17) & 3, (h >> 12) & 15, (h >> 10) & 3);
        if h >> 21 != 0x7ff || version == 1 || layer != 1 || bitrate == 0 || bitrate == 15 || rate == 3 {
            return None;
        }
        let padding = ((h >> 9) & 1) as usize;
        let len = if version == 3 {
            const BITRATES: [u32; 15] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
            const RATES: [u32; 3] = [44100, 48000, 32000];
            144_000 * BITRATES[bitrate as usize] / RATES[rate as usize]
        } else {
            72_000 * LSF_BITRATES[bitrate as usize] / LSF_RATES[(version / 2) as usize][rate as usize]
        };
        Some((version, len as usize + padding))
    };
    let mut at = 0;
    if bytes.starts_with(b"ID3") && bytes.len() >= 10 {
        let size = bytes[6..10].iter().fold(0usize, |n, &b| (n << 7) | (b & 0x7f) as usize);
        at = 10 + size + if bytes[5] & 0x10 != 0 { 10 } else { 0 };
    }
    let mut changed = false;
    let mut synced = false;
    while at + 4 <= bytes.len() {
        let Some((version, len)) = header(bytes, at) else {
            synced = false;
            at += 1;
            continue;
        };
        // A sync word found by scanning counts only when the next frame follows it.
        if !synced && at + len + 4 <= bytes.len() && header(bytes, at + len).is_none() {
            at += 1;
            continue;
        }
        synced = true;
        let (mode, mode_ext) = ((bytes[at + 3] >> 6) & 3, (bytes[at + 3] >> 4) & 3);
        let side_info = at + 4 + if bytes[at + 1] & 1 == 0 { 2 } else { 0 };
        if version != 3 && mode == 1 && mode_ext & 2 != 0 && side_info + 13 <= bytes.len() {
            // LSF side info: main_data_begin (8), private bits (2), then per channel 63 bits
            // with the 8-bit global gain at bit 21.
            for bit in [side_info * 8 + 31, side_info * 8 + 94] {
                let (byte, shift) = (bit / 8, 8 - bit % 8);
                let word = u16::from_be_bytes([bytes[byte], bytes[byte + 1]]);
                let gain = (word >> shift) & 0xff;
                let lowered = gain.saturating_sub(2);
                if lowered != gain {
                    let word = (word & !(0xff << shift)) | (lowered << shift);
                    bytes[byte..byte + 2].copy_from_slice(&word.to_be_bytes());
                    changed = true;
                }
            }
        }
        at += len.max(1);
    }
    changed
}

/// Whether the first MPEG frame (after any ID3v2 tag) carries a Xing, Info or VBRI header.
fn starts_with_info_frame(path: &Path) -> bool {
    use std::io::Read;
    let mut head = Vec::new();
    if File::open(path).and_then(|f| f.take(1 << 16).read_to_end(&mut head)).is_err() {
        return false;
    }
    let mut at = 0;
    if head.starts_with(b"ID3") && head.len() >= 10 {
        let size = head[6..10].iter().fold(0usize, |n, &b| (n << 7) | (b & 0x7f) as usize);
        at = 10 + size + if head[5] & 0x10 != 0 { 10 } else { 0 };
    }
    let Some(sync) = head.get(at..).and_then(|h| h.windows(2).position(|w| w[0] == 0xff && w[1] & 0xe0 == 0xe0)) else {
        return false;
    };
    let frame = &head[at + sync..];
    let tag_at = |offset: usize, tag: &[u8]| frame.get(offset..offset + 4) == Some(tag);
    (4..=40).any(|o| tag_at(o, b"Xing") || tag_at(o, b"Info")) || tag_at(36, b"VBRI")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowers_only_lsf_mid_side_granules() {
        let gain = |bytes: &[u8], bit: usize| {
            (u16::from_be_bytes([bytes[bit / 8], bytes[bit / 8 + 1]]) >> (8 - bit % 8)) & 0xff
        };
        let set = |bytes: &mut [u8], bit: usize, v: u16| {
            let word = u16::from_be_bytes([bytes[bit / 8], bytes[bit / 8 + 1]]) & !(0xff << (8 - bit % 8));
            bytes[bit / 8..bit / 8 + 2].copy_from_slice(&(word | (v << (8 - bit % 8))).to_be_bytes());
        };
        // MPEG-2 Layer III, 64 kbps, 22.05 kHz, joint stereo with mid/side: 208-byte frames.
        let frame = |mode_byte: u8| {
            let mut f = vec![0u8; 208];
            f[..4].copy_from_slice(&[0xff, 0xf3, 0x80, mode_byte]);
            set(&mut f, 32 + 31, 150);
            set(&mut f, 32 + 94, 1);
            f
        };
        let mut bytes = [frame(0x60), frame(0x40), frame(0x60)].concat();
        assert!(lower_lsf_mid_side_gain(&mut bytes));
        assert_eq!((gain(&bytes, 32 + 31), gain(&bytes, 32 + 94)), (148, 0));
        assert_eq!(gain(&bytes, (208 + 4) * 8 + 31), 150, "left/right frame");
        assert_eq!(gain(&bytes, (416 + 4) * 8 + 31), 148);

        let mut mpeg1 = frame(0x60);
        mpeg1[1] = 0xfb;
        let mut mpeg1 = [mpeg1.clone(), mpeg1].concat();
        assert!(!lower_lsf_mid_side_gain(&mut mpeg1));
    }

    #[test]
    fn finds_the_mp3_info_frame() {
        let dir = std::env::temp_dir().join(format!("scythe-info-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.mp3");
        let mut bytes = b"ID3\x04\x00\x00\x00\x00\x00\x02\x00\x00".to_vec();
        bytes.extend([0xff, 0xfb, 0x90, 0x64]);
        bytes.extend([0; 32]);
        bytes.extend(b"Info");
        std::fs::write(&path, &bytes).unwrap();
        assert!(starts_with_info_frame(&path));
        bytes[48..52].copy_from_slice(b"Junk");
        std::fs::write(&path, &bytes).unwrap();
        assert!(!starts_with_info_frame(&path));
        std::fs::remove_dir_all(&dir).ok();
    }
}
