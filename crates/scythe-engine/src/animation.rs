//! REAPER's GIF and LCF video sinks: animated GIF (giflib's LZW, a frame written only when the
//! picture changes) and LICEcap's LCF (groups of 20 frames in 128x16 blocks of BGR565 pixels,
//! each block stored once per run of equal frames, deflated at level 9).

use std::io::{self, Read, Write};
use std::path::Path;

use scythe_core::render_settings::{Animation, Container, Video, VideoCodec};

use crate::video::VideoClip;

/// Frames of the render as RGBA: black where no video item plays.
struct Frames {
    child: Option<std::process::Child>,
    /// The composited video the child decodes.
    temp: Option<std::path::PathBuf>,
    frame: Vec<u8>,
    left: usize,
}

impl Frames {
    /// Video items are composited by the video sink into a lossless file, then decoded.
    fn new(a: &Animation, clips: &[VideoClip], duration: f64, out: &Path) -> Result<Self, String> {
        let count = ((duration * a.fps) - 1e-9).ceil().max(1.0) as usize;
        let child = if clips.is_empty() {
            None
        } else {
            let lossless = out.with_extension("scythe-frames.mkv");
            let video = Video {
                container: Container::Matroska,
                video: Some(VideoCodec::Ffv1),
                audio: None,
                video_kbps: 0,
                audio_kbps: 0,
                width: a.width,
                height: a.height,
                fps: a.fps,
                keep_aspect: true,
                quality: 100,
                video_options: String::new(),
                audio_options: String::new(),
            };
            crate::video::write_video(&video, clips, duration, None, &lossless)?;
            let child = std::process::Command::new("ffmpeg")
                .args(["-v", "error", "-nostdin", "-i"])
                .arg(&lossless)
                .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|e| {
                    let _ = std::fs::remove_file(&lossless);
                    format!("video renders need `ffmpeg` on PATH: {e}")
                })?;
            Some((child, lossless))
        };
        let (child, temp) = child.unzip();
        Ok(Self { child, temp, frame: vec![0; a.width as usize * a.height as usize * 4], left: count })
    }

    /// The next frame; ffmpeg's last one repeats when it ends early.
    fn next(&mut self) -> Option<&[u8]> {
        if self.left == 0 {
            return None;
        }
        self.left -= 1;
        if let Some(out) = self.child.as_mut().and_then(|c| c.stdout.as_mut()) {
            let mut fresh = vec![0; self.frame.len()];
            if out.read_exact(&mut fresh).is_ok() {
                self.frame = fresh;
            }
        }
        Some(&self.frame)
    }
}

impl Drop for Frames {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        if let Some(temp) = self.temp.take() {
            let _ = std::fs::remove_file(temp);
        }
    }
}

/// Writes the render's video as GIF or LCF.
pub fn write(a: &Animation, lcf: bool, clips: &[VideoClip], duration: f64, out: &Path) -> Result<(), String> {
    let frames = Frames::new(a, clips, duration, out)?;
    let file = std::fs::File::create(out).map_err(|e| e.to_string())?;
    let mut file = io::BufWriter::new(file);
    let result = if lcf { write_lcf(a, frames, &mut file) } else { write_gif(a, frames, &mut file) };
    result.and_then(|()| file.flush()).map_err(|e| e.to_string())
}

fn write_gif(a: &Animation, mut frames: Frames, out: &mut impl Write) -> io::Result<()> {
    let (w, h) = (a.width as usize, a.height as usize);
    out.write_all(b"GIF89a")?;
    out.write_all(&(w as u16).to_le_bytes())?;
    out.write_all(&(h as u16).to_le_bytes())?;
    out.write_all(&[0x77, 0, 0])?;
    out.write_all(b"\x21\xff\x0bNETSCAPE2.0\x03\x01\x00\x00\x00")?;
    // Frame times add up in milliseconds, as REAPER counts them (60 frames at 30 fps make
    // 1999.99 ms, a 199 cs delay).
    let step = 1000.0 / a.fps;
    // The frame waiting for its delay: its pixels, its rectangle and the time it shows.
    let mut shown: Option<(Vec<u8>, Rect, f64)> = None;
    while let Some(frame) = frames.next() {
        match &mut shown {
            Some((pixels, _, time)) if pixels.as_slice() == frame => *time += step,
            _ => {
                let rect = match &shown {
                    Some((prev, ..)) => changed_rect(prev, frame, w, h),
                    None => (0, 0, w, h),
                };
                if let Some((pixels, rect, time)) = shown.take() {
                    gif_frame(a, out, &pixels, w, rect, time)?;
                }
                shown = Some((frame.to_vec(), rect, step));
            }
        }
    }
    if let Some((pixels, rect, time)) = shown {
        gif_frame(a, out, &pixels, w, rect, time)?;
    }
    out.write_all(b"\x3b")
}

/// x, y, width and height.
type Rect = (usize, usize, usize, usize);

/// The smallest rectangle holding every pixel that differs.
fn changed_rect(a: &[u8], b: &[u8], w: usize, h: usize) -> Rect {
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            if a[i..i + 3] != b[i..i + 3] {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
        }
    }
    if x1 == 0 { (0, 0, 1, 1) } else { (x0, y0, x1 - x0, y1 - y0) }
}

fn gif_frame(
    a: &Animation,
    out: &mut impl Write,
    frame: &[u8],
    w: usize,
    (x, y, rw, rh): Rect,
    time: f64,
) -> io::Result<()> {
    let mut colors: Vec<[u8; 3]> = Vec::new();
    let mut index = Vec::with_capacity(rw * rh);
    for row in y..y + rh {
        for col in x..x + rw {
            let i = (row * w + col) * 4;
            let c = [frame[i], frame[i + 1], frame[i + 2]];
            let k = match colors.iter().position(|p| *p == c) {
                Some(k) => k,
                None if colors.len() < 256 => {
                    colors.push(c);
                    colors.len() - 1
                }
                None => nearest(&colors, c),
            };
            index.push(k as u8);
        }
    }
    let bits = (1..=8).find(|b| colors.len() <= 1 << b).unwrap_or(8);
    let delay = (time / 10.0).floor().min(65535.0) as u16;
    let (packed, transparent) = if a.transparent { (1, 1) } else { (0, 0) };
    out.write_all(&[0x21, 0xf9, 4, packed])?;
    out.write_all(&delay.to_le_bytes())?;
    out.write_all(&[transparent, 0, 0x2c])?;
    for v in [x, y, rw, rh] {
        out.write_all(&(v as u16).to_le_bytes())?;
    }
    out.write_all(&[0x80 | (bits as u8 - 1)])?;
    let mut table = vec![0u8; 3 << bits];
    for (k, c) in colors.iter().enumerate() {
        table[k * 3..k * 3 + 3].copy_from_slice(c);
    }
    out.write_all(&table)?;
    let min = bits.max(2) as u8;
    out.write_all(&[min])?;
    for block in lzw(min, &index).chunks(255) {
        out.write_all(&[block.len() as u8])?;
        out.write_all(block)?;
    }
    out.write_all(&[0])
}

fn nearest(colors: &[[u8; 3]], c: [u8; 3]) -> usize {
    let d = |p: &[u8; 3]| (0..3).map(|i| (p[i] as i32 - c[i] as i32).pow(2)).sum::<i32>();
    (0..colors.len()).min_by_key(|&k| d(&colors[k])).unwrap_or(0)
}

/// giflib's LZW coder: the code size grows after a code is written once the next free code
/// no longer fits, and the table restarts with a clear code when it is full.
fn lzw(min: u8, pixels: &[u8]) -> Vec<u8> {
    let clear = 1u32 << min;
    let eof = clear + 1;
    let mut out = Vec::new();
    let (mut acc, mut nbits) = (0u64, 0u32);
    let mut running = eof + 1;
    let mut bits = min as u32 + 1;
    let mut max1 = 1u32 << bits;
    let mut table = std::collections::HashMap::<u32, u32>::new();
    let mut emit = |code: u32, running: u32, bits: &mut u32, max1: &mut u32, out: &mut Vec<u8>| {
        acc |= (code as u64) << nbits;
        nbits += *bits;
        while nbits >= 8 {
            out.push(acc as u8);
            acc >>= 8;
            nbits -= 8;
        }
        if running >= *max1 && code <= 4095 {
            *bits += 1;
            *max1 = 1 << *bits;
        }
    };
    emit(clear, running, &mut bits, &mut max1, &mut out);
    let Some((&first, rest)) = pixels.split_first() else {
        emit(eof, running, &mut bits, &mut max1, &mut out);
        return out;
    };
    let mut current = first as u32;
    for &p in rest {
        let key = current << 8 | p as u32;
        if let Some(&code) = table.get(&key) {
            current = code;
            continue;
        }
        emit(current, running, &mut bits, &mut max1, &mut out);
        current = p as u32;
        if running >= 4095 {
            emit(clear, running, &mut bits, &mut max1, &mut out);
            running = eof + 1;
            bits = min as u32 + 1;
            max1 = 1 << bits;
            table.clear();
        } else {
            table.insert(key, running);
            running += 1;
        }
    }
    emit(current, running, &mut bits, &mut max1, &mut out);
    emit(eof, running, &mut bits, &mut max1, &mut out);
    if nbits > 0 {
        out.push(acc as u8);
    }
    out
}

const LCF_SLICE: usize = 20;
const LCF_BLOCK: (usize, usize) = (128, 16);

fn write_lcf(a: &Animation, mut frames: Frames, out: &mut impl Write) -> io::Result<()> {
    let (w, h) = (a.width as usize, a.height as usize);
    let ms = |i: usize| (i as f64 * 1000.0 / a.fps).round() as u32;
    let mut index = 0;
    let mut slice: Vec<Vec<u16>> = Vec::with_capacity(LCF_SLICE);
    loop {
        let frame = frames.next();
        if let Some(f) = frame {
            slice.push(
                f.as_chunks::<4>()
                    .0
                    .iter()
                    .map(|p| (p[2] as u16 >> 3) << 11 | (p[1] as u16 >> 2) << 5 | p[0] as u16 >> 3)
                    .collect(),
            );
        }
        if slice.len() == LCF_SLICE || (frame.is_none() && !slice.is_empty()) {
            let raw = lcf_blocks(&slice, w, h);
            let packed = deflate_sync(&raw);
            for v in [0x11ce_b001u32, 16, w as u32, h as u32, LCF_BLOCK.0 as u32, LCF_BLOCK.1 as u32] {
                out.write_all(&v.to_le_bytes())?;
            }
            for v in [slice.len() as u32, packed.len() as u32, raw.len() as u32] {
                out.write_all(&v.to_le_bytes())?;
            }
            for i in index..index + slice.len() {
                out.write_all(&(ms(i + 1) - ms(i)).to_le_bytes())?;
            }
            out.write_all(&packed)?;
            index += slice.len();
            slice.clear();
        }
        if frame.is_none() {
            return Ok(());
        }
    }
}

/// Each block in turn: its pixels in the first frame, then for each run of frames the count of
/// following frames that leave it unchanged, and the pixels of the frame that changes it.
fn lcf_blocks(frames: &[Vec<u16>], w: usize, h: usize) -> Vec<u8> {
    let mut raw = Vec::new();
    for by in (0..h).step_by(LCF_BLOCK.1) {
        for bx in (0..w).step_by(LCF_BLOCK.0) {
            let (bw, bh) = (LCF_BLOCK.0.min(w - bx), LCF_BLOCK.1.min(h - by));
            let block = |f: &[u16]| -> Vec<u16> {
                (by..by + bh).flat_map(|y| f[y * w + bx..y * w + bx + bw].iter().copied()).collect()
            };
            let mut i = 0;
            while i < frames.len() {
                let pixels = block(&frames[i]);
                raw.extend(pixels.iter().flat_map(|p| p.to_le_bytes()));
                let mut same = 0;
                while i + 1 + same < frames.len() && block(&frames[i + 1 + same]) == pixels {
                    same += 1;
                }
                raw.push(same as u8);
                i += 1 + same;
            }
        }
    }
    raw
}

/// zlib at level 9, ended with a sync flush rather than a stream end, as LICEcap writes it.
fn deflate_sync(raw: &[u8]) -> Vec<u8> {
    let mut c = flate2::Compress::new(flate2::Compression::new(9), true);
    let mut out = Vec::with_capacity(raw.len() / 4 + 64);
    let mut input = raw;
    loop {
        out.reserve(64 * 1024);
        let before = c.total_in();
        c.compress_vec(input, &mut out, flate2::FlushCompress::Sync).expect("deflate");
        input = &input[(c.total_in() - before) as usize..];
        if input.is_empty() && out.len() < out.capacity() {
            return out;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lzw_codes_like_giflib() {
        // The first bytes of REAPER's black 640x360 GIF frame.
        let data = lzw(2, &vec![0u8; 640 * 360]);
        assert_eq!(&data[..8], &[0x84, 0x8f, 0xa9, 0xcb, 0xed, 0x0f, 0xa3, 0x9c]);
        assert_eq!(data.len(), 728);
    }
}
