//! The audio device's channels as effects see them in the live engine, for hardware inserts
//! (ReaInsert): the input of the current device block, and outputs effects add to. The engine
//! opens a block with `begin`, marks where each rendered segment starts with `segment`, and
//! mixes what the effects sent into the device output with `mix_into`. Offline renders have no
//! hardware, so inserts there play silence, as in REAPER without an audio device.
//!
//! A loopback (outputs fed back to the inputs after a fixed number of frames) stands in for a
//! cable when testing without a device.

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::render::RenderScratch;

const MAX_BLOCK: usize = RenderScratch::MAX_BLOCK;
/// Outputs a loopback feeds back.
const LOOPBACK_CHANNELS: usize = 16;

#[derive(Default)]
struct Block {
    /// Device input channels of the current block (silent without an input).
    input: Vec<Vec<f32>>,
    /// What effects send to each device output in the current block.
    sends: Vec<Vec<f32>>,
    frames: usize,
    /// Counts segments, so an effect can tell a new one from the next slice of the same.
    segment: u64,
    /// Frame of the device block the current segment starts at.
    offset: usize,
    /// Per output channel: frames sent but not yet looped back.
    loopback: Option<(usize, Vec<VecDeque<f32>>)>,
}

#[derive(Default)]
pub struct HardwareIo(Mutex<Block>);

impl HardwareIo {
    /// Feeds every output back to the input of the same number `latency` frames later.
    pub fn set_loopback(&self, latency: Option<usize>) {
        if let Ok(mut b) = self.0.lock() {
            b.loopback = latency.map(|l| (l, vec![std::iter::repeat_n(0.0, l).collect(); LOOPBACK_CHANNELS]));
        }
    }

    /// Starts a device block of `frames` frames with these input channels.
    pub fn begin(&self, frames: usize, input: Option<&[Vec<f32>]>) {
        let Ok(mut b) = self.0.try_lock() else { return };
        let b = &mut *b;
        let frames = frames.min(MAX_BLOCK);
        b.frames = frames;
        let looped = b.loopback.as_ref().map_or(0, |(_, rings)| rings.len());
        let wanted = input.map_or(0, <[_]>::len).max(b.sends.len()).max(looped);
        if b.input.len() < wanted {
            b.input.resize_with(wanted, || vec![0.0; MAX_BLOCK]);
        }
        for (c, buf) in b.input.iter_mut().enumerate() {
            match input.and_then(|i| i.get(c)) {
                Some(src) => buf[..frames].copy_from_slice(&src[..frames]),
                None => buf[..frames].fill(0.0),
            }
        }
        if let Some((_, rings)) = &mut b.loopback {
            for (c, ring) in rings.iter_mut().enumerate() {
                for s in &mut b.input[c][..frames] {
                    *s += ring.pop_front().unwrap_or(0.0);
                }
            }
        }
        for s in &mut b.sends {
            s[..frames].fill(0.0);
        }
    }

    /// The next rendered segment starts at frame `offset` of the device block.
    pub fn segment(&self, offset: usize) {
        if let Ok(mut b) = self.0.try_lock() {
            b.segment += 1;
            b.offset = offset;
        }
    }

    /// Adds what effects sent in this block to the device outputs (`left`, `right`, then
    /// `extra`), and keeps it for the loopback.
    pub fn mix_into(&self, left: &mut [f32], right: &mut [f32], extra: &mut [Vec<f32>]) {
        let Ok(mut b) = self.0.try_lock() else { return };
        let b = &mut *b;
        let n = b.frames;
        for (c, send) in b.sends.iter().enumerate() {
            let out = match c {
                0 => Some(&mut left[..n]),
                1 => Some(&mut right[..n]),
                c => extra.get_mut(c - 2).map(|e| &mut e[..n]),
            };
            if let Some(out) = out {
                out.iter_mut().zip(&send[..n]).for_each(|(o, s)| *o += s);
            }
        }
        if let Some((_, rings)) = &mut b.loopback {
            for (c, ring) in rings.iter_mut().enumerate() {
                match b.sends.get(c) {
                    Some(send) => ring.extend(&send[..n]),
                    None => ring.extend(std::iter::repeat_n(0.0, n)),
                }
            }
        }
    }

    /// Runs `f` on the current segment: the frame of the device block an effect's next frame
    /// is at (`at`, which it keeps between calls), the input channels and the outputs.
    pub(crate) fn with_segment(
        &self,
        seen: &mut u64,
        at: &mut usize,
        outputs: usize,
        f: impl FnOnce(usize, &[Vec<f32>], &mut [Vec<f32>]),
    ) -> bool {
        let Ok(mut b) = self.0.try_lock() else { return false };
        let b = &mut *b;
        if b.sends.len() < outputs {
            b.sends.resize_with(outputs, || vec![0.0; MAX_BLOCK]);
        }
        if *seen != b.segment {
            *seen = b.segment;
            *at = b.offset;
        }
        f(*at, &b.input, &mut b.sends);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_loopback_returns_what_was_sent_later() {
        let hw = HardwareIo::default();
        hw.set_loopback(Some(100));
        let (mut seen, mut at) = (0, 0);
        let mut got = Vec::new();
        for block in 0..4 {
            hw.begin(64, None);
            hw.segment(0);
            hw.with_segment(&mut seen, &mut at, 3, |at, input, sends| {
                assert_eq!(at, 0);
                if block == 0 {
                    sends[2][5] = 1.0;
                }
                got.extend(input.get(2).map_or(vec![0.0; 64], |i| i[..64].to_vec()));
            });
            let (mut l, mut r, mut extra) = (vec![0.0; 64], vec![0.0; 64], vec![vec![0.0; 64]]);
            hw.mix_into(&mut l, &mut r, &mut extra);
            assert_eq!(extra[0][5], if block == 0 { 1.0 } else { 0.0 });
        }
        assert_eq!(got.iter().position(|&v| v == 1.0), Some(105));
    }
}
