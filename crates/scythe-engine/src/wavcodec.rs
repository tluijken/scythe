//! WAV encodings hound does not write: 64-bit float and REAPER's ADPCM modes (IMA ADPCM and
//! its 2-bit cADPCM), converted from a rendered 32-bit float WAV.

use std::io::{self, Write};
use std::path::Path;

/// IMA ADPCM step sizes.
const STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107,
    118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963,
    1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894,
    6484, 7132, 7845, 8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794,
    32767,
];
const INDEX_MOVES: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

/// REAPER's ADPCM modes, both IMA-style with 1024-byte blocks per channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adpcm {
    /// 4-bit IMA ADPCM (format tag 0x11).
    Ima,
    /// REAPER's 2-bit cADPCM (format tag 0xac0c): the IMA coder with only the sign and the
    /// top magnitude bit kept.
    Cockos,
}

impl Adpcm {
    fn bits(self) -> usize {
        match self {
            Self::Ima => 4,
            Self::Cockos => 2,
        }
    }

    fn tag(self) -> u16 {
        match self {
            Self::Ima => 0x11,
            Self::Cockos => 0xac0c,
        }
    }
}

/// Reads a float WAV, averaging its two channels when `mono`.
fn read_float_wav(path: &Path, mono: bool) -> io::Result<(u16, u32, Vec<f64>)> {
    let mut reader = hound::WavReader::open(path).map_err(io::Error::other)?;
    let spec = reader.spec();
    let samples: Vec<f64> =
        reader.samples::<f32>().map(|s| s.map(f64::from)).collect::<Result<_, _>>().map_err(io::Error::other)?;
    if mono && spec.channels == 2 {
        return Ok((1, spec.sample_rate, samples.as_chunks::<2>().0.iter().map(|[l, r]| 0.5 * (l + r)).collect()));
    }
    Ok((spec.channels, spec.sample_rate, samples))
}

/// Writes a RIFF WAVE file from its `fmt ` body, optional `fact` frame count and data.
fn write_riff(out: &Path, fmt: &[u8], fact: Option<u32>, data: &[u8]) -> io::Result<()> {
    let mut body = Vec::with_capacity(data.len() + 64);
    body.extend_from_slice(b"WAVEfmt ");
    body.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
    body.extend_from_slice(fmt);
    if let Some(frames) = fact {
        body.extend_from_slice(b"fact");
        body.extend_from_slice(&4u32.to_le_bytes());
        body.extend_from_slice(&frames.to_le_bytes());
    }
    body.extend_from_slice(b"data");
    body.extend_from_slice(&(data.len() as u32).to_le_bytes());
    body.extend_from_slice(data);
    if data.len() % 2 == 1 {
        body.push(0);
    }
    let mut file = io::BufWriter::new(std::fs::File::create(out)?);
    file.write_all(b"RIFF")?;
    file.write_all(&(body.len() as u32).to_le_bytes())?;
    file.write_all(&body)?;
    file.flush()
}

fn fmt_chunk(tag: u16, channels: u16, rate: u32, byte_rate: u32, align: u16, bits: u16, extra: &[u8]) -> Vec<u8> {
    let mut fmt = Vec::with_capacity(20);
    fmt.extend_from_slice(&tag.to_le_bytes());
    fmt.extend_from_slice(&channels.to_le_bytes());
    fmt.extend_from_slice(&rate.to_le_bytes());
    fmt.extend_from_slice(&byte_rate.to_le_bytes());
    fmt.extend_from_slice(&align.to_le_bytes());
    fmt.extend_from_slice(&bits.to_le_bytes());
    fmt.extend_from_slice(&(extra.len() as u16).to_le_bytes());
    fmt.extend_from_slice(extra);
    fmt
}

/// Rewrites a float WAV as 64-bit float samples, mixed down to mono when asked.
pub fn write_f64(src: &Path, mono: bool, out: &Path) -> io::Result<()> {
    let (channels, rate, samples) = read_float_wav(src, mono)?;
    let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let align = channels * 8;
    let fmt = fmt_chunk(3, channels, rate, rate * align as u32, align, 64, &[]);
    write_riff(out, &fmt, None, &data)
}

/// Rewrites a float WAV in one of REAPER's ADPCM modes, mixed down to mono when asked.
pub fn write_adpcm(src: &Path, mono: bool, out: &Path, mode: Adpcm) -> io::Result<()> {
    let (channels, rate, samples) = read_float_wav(src, mono)?;
    let pcm: Vec<i16> = samples.iter().map(|&s| crate::bounce::quantize(s, 16) as i16).collect();
    let (align, frames_per_block, data) = encode_adpcm(&pcm, channels as usize, mode);
    let byte_rate = rate * mode.bits() as u32 * channels as u32 / 8;
    let fmt = fmt_chunk(
        mode.tag(),
        channels,
        rate,
        byte_rate,
        align as u16,
        mode.bits() as u16,
        &(frames_per_block as u16).to_le_bytes(),
    );
    write_riff(out, &fmt, Some((pcm.len() / channels.max(1) as usize) as u32), &data)
}

/// Encodes interleaved 16-bit samples as REAPER does: blocks of 1024 bytes per channel, each
/// starting with the channel's sample and step index, then groups of 4 bytes per channel. A
/// sample's code is the step-relative difference rounded down after taking half a step off,
/// the prediction wraps at 16 bits, and the last block is filled up by coding silence.
/// Returns the block size, frames per block and the data.
fn encode_adpcm(pcm: &[i16], channels: usize, mode: Adpcm) -> (usize, usize, Vec<u8>) {
    let channels = channels.max(1);
    let bits = mode.bits();
    let per_byte = 8 / bits;
    let align = 1024 * channels;
    let frames_per_block = (align - 4 * channels) * per_byte / channels + 1;
    let frames = pcm.len() / channels;
    let blocks = frames.div_ceil(frames_per_block);
    let sample = |frame: usize, c: usize| if frame < frames { pcm[frame * channels + c] as i32 } else { 0 };
    let mut index = vec![0i32; channels];
    let mut data = Vec::with_capacity(blocks * align);
    let mut codes: Vec<Vec<u8>> = vec![Vec::with_capacity(frames_per_block); channels];
    for block in 0..blocks {
        let start = block * frames_per_block;
        for (c, codes) in codes.iter_mut().enumerate() {
            let mut predicted = sample(start, c);
            data.extend_from_slice(&(predicted as i16).to_le_bytes());
            data.extend_from_slice(&[index[c] as u8, 0]);
            codes.clear();
            for frame in start + 1..start + frames_per_block {
                let step = STEPS[index[c] as usize];
                let diff = sample(frame, c) - predicted;
                let negative = diff < 0;
                // Integer division truncates towards zero, so a negative quotient becomes 0.
                let mut magnitude = ((4 * diff.abs() - step / 2) / step).clamp(0, 7);
                if mode == Adpcm::Cockos {
                    magnitude &= 4;
                }
                let delta = step * magnitude / 4 + step / 8;
                predicted = (if negative { predicted - delta } else { predicted + delta }) as i16 as i32;
                index[c] = (index[c] + INDEX_MOVES[magnitude as usize]).clamp(0, 88);
                let code = magnitude as u8 | if negative { 8 } else { 0 };
                codes.push(if mode == Adpcm::Cockos { code >> 2 } else { code });
            }
        }
        for group in 0..(frames_per_block - 1) / (4 * per_byte) {
            for codes in &codes {
                for byte in 0..4 {
                    let first = group * 4 * per_byte + byte * per_byte;
                    let packed =
                        codes[first..first + per_byte].iter().enumerate().fold(0u8, |b, (j, &v)| b | v << (bits * j));
                    data.push(packed);
                }
            }
        }
    }
    (align, frames_per_block, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adpcm_blocks() {
        // The start of REAPER's IMA ADPCM block for this ramp: -32640, index 0, then the codes
        // 0, 7, 7, 7, 7, 7, 2, 3.
        let mut pcm = Vec::new();
        for i in 0..20 {
            pcm.push((-32640 + 256 * (i / 2)) as i16);
        }
        let (align, frames, data) = encode_adpcm(&pcm, 1, Adpcm::Ima);
        assert_eq!((align, frames, data.len()), (1024, 2041, 1024));
        assert_eq!(&data[..8], &[0x80, 0x80, 0, 0, 0x70, 0x77, 0x77, 0x32]);
        let (_, frames, data) = encode_adpcm(&pcm, 2, Adpcm::Cockos);
        assert_eq!((frames, data.len()), (4081, 2048));
    }
}
