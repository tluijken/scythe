//! The media explorer's preview: one file playing straight to the device after the master
//! (REAPER's explorer previews skip the project's mix), whether or not the project plays.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwapOption;

use crate::media::AudioData;

/// A file loaded for preview, at the engine's rate (already stretched when tempo-matched).
#[derive(Debug)]
pub struct Voice {
    pub path: PathBuf,
    pub audio: Arc<AudioData>,
}

/// Shared between the main thread, which starts and stops previews, and the audio thread,
/// which mixes the playing one into the device output.
#[derive(Debug)]
pub struct Preview {
    voice: ArcSwapOption<Voice>,
    playing: AtomicBool,
    /// Frames into the voice.
    position: AtomicU64,
    /// Linear gain as `f32` bits.
    gain: AtomicU32,
    repeat: AtomicBool,
    /// Voices replaced lately: the audio thread may hold one until its callback ends, so the
    /// last drop happens here rather than there.
    retired: Mutex<Vec<Arc<Voice>>>,
}

impl Default for Preview {
    fn default() -> Self {
        Self {
            voice: ArcSwapOption::empty(),
            playing: AtomicBool::new(false),
            position: AtomicU64::new(0),
            gain: AtomicU32::new(1f32.to_bits()),
            repeat: AtomicBool::new(false),
            retired: Mutex::default(),
        }
    }
}

impl Preview {
    /// Plays `audio` from `start` seconds, replacing what was previewing.
    pub fn play(&self, path: &Path, audio: Arc<AudioData>, start: f64) {
        let frame = (start.max(0.0) * audio.sample_rate as f64) as u64;
        self.playing.store(false, Ordering::Release);
        let old = self.voice.swap(Some(Arc::new(Voice { path: path.to_owned(), audio })));
        self.retire(old);
        self.position.store(frame, Ordering::Relaxed);
        self.playing.store(true, Ordering::Release);
    }

    fn retire(&self, old: Option<Arc<Voice>>) {
        if let (Some(old), Ok(mut list)) = (old, self.retired.lock()) {
            list.push(old);
            let excess = list.len().saturating_sub(4);
            list.drain(..excess);
        }
    }

    /// Stops and rewinds.
    pub fn stop(&self) {
        self.playing.store(false, Ordering::Release);
        self.position.store(0, Ordering::Relaxed);
    }

    pub fn pause(&self) {
        self.playing.store(false, Ordering::Release);
    }

    /// Carries on from where it paused; false when nothing is loaded.
    pub fn resume(&self) -> bool {
        let loaded = self.voice.load().is_some();
        if loaded {
            self.playing.store(true, Ordering::Release);
        }
        loaded
    }

    /// Unloads the preview media.
    pub fn unload(&self) {
        self.stop();
        let old = self.voice.swap(None);
        self.retire(old);
    }

    pub fn seek(&self, seconds: f64) {
        if let Some(v) = self.voice.load().as_ref() {
            let frame = (seconds.max(0.0) * v.audio.sample_rate as f64) as u64;
            self.position.store(frame.min(v.audio.frames() as u64), Ordering::Relaxed);
        }
    }

    pub fn set_gain(&self, gain: f32) {
        self.gain.store(gain.max(0.0).to_bits(), Ordering::Relaxed);
    }

    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Ordering::Relaxed))
    }

    pub fn set_repeat(&self, on: bool) {
        self.repeat.store(on, Ordering::Relaxed);
    }

    pub fn repeat(&self) -> bool {
        self.repeat.load(Ordering::Relaxed)
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Acquire)
    }

    /// The file loaded for preview.
    pub fn path(&self) -> Option<PathBuf> {
        self.voice.load().as_ref().map(|v| v.path.clone())
    }

    /// Seconds into the preview and its length.
    pub fn position(&self) -> Option<(f64, f64)> {
        let v = self.voice.load();
        let v = v.as_ref()?;
        let rate = v.audio.sample_rate.max(1) as f64;
        Some((self.position.load(Ordering::Relaxed) as f64 / rate, v.audio.duration()))
    }

    /// Adds the playing preview to a block of the device output. For the audio thread.
    pub fn mix_into(&self, left: &mut [f32], right: &mut [f32]) {
        if !self.playing.load(Ordering::Acquire) {
            return;
        }
        let guard = self.voice.load();
        let Some(voice) = guard.as_ref() else { return };
        let audio = &voice.audio;
        let total = audio.frames();
        let (l, r) = match audio.channels.as_slice() {
            [] => return,
            [mono] => (mono, mono),
            [l, r, ..] => (l, r),
        };
        let gain = self.gain();
        let mut pos = self.position.load(Ordering::Relaxed) as usize;
        let repeat = self.repeat.load(Ordering::Relaxed);
        for (ol, or) in left.iter_mut().zip(right.iter_mut()) {
            if pos >= total {
                if !repeat || total == 0 {
                    self.playing.store(false, Ordering::Release);
                    pos = 0;
                    break;
                }
                pos = 0;
            }
            *ol += l[pos] * gain;
            *or += r[pos] * gain;
            pos += 1;
        }
        self.position.store(pos as u64, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audio(frames: usize) -> Arc<AudioData> {
        Arc::new(AudioData { sample_rate: 10, channels: vec![(0..frames).map(|i| i as f32).collect()] })
    }

    #[test]
    fn plays_once_then_stops() {
        let p = Preview::default();
        let (mut l, mut r) = (vec![0.0; 4], vec![0.0; 4]);
        p.mix_into(&mut l, &mut r);
        assert_eq!(l, [0.0; 4]);
        p.play(Path::new("/a.wav"), audio(6), 0.0);
        p.mix_into(&mut l, &mut r);
        assert_eq!((l.clone(), r.clone()), (vec![0.0, 1.0, 2.0, 3.0], vec![0.0, 1.0, 2.0, 3.0]));
        assert_eq!(p.position(), Some((0.4, 0.6)));
        let (mut l, mut r) = (vec![0.0; 4], vec![0.0; 4]);
        p.mix_into(&mut l, &mut r);
        assert_eq!(l, [4.0, 5.0, 0.0, 0.0]);
        assert!(!p.is_playing());
        assert_eq!(p.path().as_deref(), Some(Path::new("/a.wav")));
    }

    #[test]
    fn repeats_with_gain() {
        let p = Preview::default();
        p.set_repeat(true);
        p.set_gain(0.5);
        p.play(Path::new("/a.wav"), audio(3), 0.1);
        let (mut l, mut r) = (vec![0.0; 5], vec![0.0; 5]);
        p.mix_into(&mut l, &mut r);
        assert_eq!(l, [0.5, 1.0, 0.0, 0.5, 1.0]);
        assert!(p.is_playing());
        p.pause();
        assert!(!p.is_playing() && p.resume());
        p.unload();
        assert!(!p.resume());
    }
}
