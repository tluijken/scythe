//! REAPER's open-source pitch shifters, run from the system libraries: SoundTouch (LGPL-2.1,
//! `libSoundTouchDll`, loaded at run time) and, with the `rubberband` cargo feature, Rubber Band
//! (GPL-2.0-or-later, `librubberband`). Neither is linked at build time; without the library
//! (or the feature) the caller falls back to Scythe's phase vocoder.
//!
//! REAPER 7.73 ships SoundTouch 1.9.0 and Rubber Band 1.8 and drives them from its take reader:
//! it asks for 512 frames at a time, takes what the shifter has ready, and feeds it 512 more
//! source frames whenever that falls short, without trimming the shifter's latency. The
//! settings per sub-mode are the ones its plug-ins pass.

use std::sync::OnceLock;

use libloading::Library;

use crate::media::AudioData;

/// REAPER's pitch shifters by their index in `Take::pitch_mode`'s high 16 bits.
pub const SOUNDTOUCH: i32 = 0;
pub const RUBBER_BAND: i32 = 13;

const BLOCK: usize = 512;

/// A pitch shifter fed and drained in interleaved frames.
trait Shifter {
    fn put(&mut self, frames: &[f32]);
    fn available(&self) -> usize;
    fn take(&mut self, out: &mut [f32]) -> usize;
}

/// `data` stretched by `time` and shifted by `pitch` with REAPER's shifter `mode` (shifter in
/// the high 16 bits, sub-mode in the low ones), or `None` when that shifter's library is not
/// there.
pub fn process(data: &AudioData, time: f64, pitch: f64, mode: i32) -> Option<AudioData> {
    let (shifter, sub) = (mode >> 16, mode & 0xffff);
    let channels = data.channels.len();
    let frames = (data.frames() as f64 * time).round() as usize;
    let rate = data.sample_rate;
    let groups: Vec<Vec<usize>> = match shifter {
        // Multi-mono runs one instance per channel; multi-stereo is the default for stereo.
        SOUNDTOUCH if sub / 3 == 2 => (0..channels).map(|c| vec![c]).collect(),
        SOUNDTOUCH => vec![(0..channels).collect()],
        #[cfg(feature = "rubberband")]
        RUBBER_BAND => vec![(0..channels).collect()],
        _ => return None,
    };
    let mut out = vec![Vec::new(); channels];
    for group in groups {
        let n = group.len();
        let mut shifter: Box<dyn Shifter> = match shifter {
            SOUNDTOUCH => Box::new(SoundTouch::new(rate, n, time, pitch, sub)?),
            #[cfg(feature = "rubberband")]
            RUBBER_BAND => Box::new(rubberband::RubberBand::new(rate, n, time, pitch, sub)?),
            _ => return None,
        };
        let input: Vec<f32> =
            (0..data.frames()).flat_map(|i| group.iter().map(move |&c| data.channels[c][i])).collect();
        let result = drive(shifter.as_mut(), &input, n, frames);
        for (k, &c) in group.iter().enumerate() {
            out[c] = result.iter().skip(k).step_by(n).copied().collect();
        }
    }
    Some(AudioData { sample_rate: rate, channels: out })
}

/// Runs `shifter` the way REAPER's take reader does, for `frames` output frames.
fn drive(shifter: &mut dyn Shifter, input: &[f32], channels: usize, frames: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; frames * channels];
    let mut feed = vec![0.0f32; BLOCK * channels];
    let (mut done, mut read) = (0, 0);
    while done < frames {
        let end = (done + BLOCK).min(frames);
        while done < end {
            let got = shifter.available().min(end - done);
            if got > 0 {
                done += shifter.take(&mut out[done * channels..(done + got) * channels]);
            }
            if done < end {
                for (i, v) in feed.iter_mut().enumerate() {
                    *v = input.get(read * channels + i).copied().unwrap_or(0.0);
                }
                shifter.put(&feed);
                read += BLOCK;
            }
        }
    }
    out
}

/// Opens the first of `names` that loads.
fn open(names: &[&str]) -> Option<Library> {
    names.iter().find_map(|n| unsafe { Library::new(n) }.ok())
}

type Handle = *mut std::ffi::c_void;

struct SoundTouchLib {
    create: unsafe extern "C" fn() -> Handle,
    destroy: unsafe extern "C" fn(Handle),
    set_tempo: unsafe extern "C" fn(Handle, f32),
    set_pitch: unsafe extern "C" fn(Handle, f32),
    set_channels: unsafe extern "C" fn(Handle, u32),
    set_sample_rate: unsafe extern "C" fn(Handle, u32),
    set_setting: unsafe extern "C" fn(Handle, i32, i32) -> i32,
    put: unsafe extern "C" fn(Handle, *const f32, u32),
    receive: unsafe extern "C" fn(Handle, *mut f32, u32) -> u32,
    num_samples: unsafe extern "C" fn(Handle) -> u32,
    _lib: Library,
}

fn soundtouch_lib() -> Option<&'static SoundTouchLib> {
    static LIB: OnceLock<Option<SoundTouchLib>> = OnceLock::new();
    LIB.get_or_init(|| {
        let lib = open(&["libSoundTouchDll.so.1", "libSoundTouchDll.so", "libSoundTouchDll.dylib"])?;
        unsafe {
            Some(SoundTouchLib {
                create: *lib.get(b"soundtouch_createInstance\0").ok()?,
                destroy: *lib.get(b"soundtouch_destroyInstance\0").ok()?,
                set_tempo: *lib.get(b"soundtouch_setTempo\0").ok()?,
                set_pitch: *lib.get(b"soundtouch_setPitch\0").ok()?,
                set_channels: *lib.get(b"soundtouch_setChannels\0").ok()?,
                set_sample_rate: *lib.get(b"soundtouch_setSampleRate\0").ok()?,
                set_setting: *lib.get(b"soundtouch_setSetting\0").ok()?,
                put: *lib.get(b"soundtouch_putSamples\0").ok()?,
                receive: *lib.get(b"soundtouch_receiveSamples\0").ok()?,
                num_samples: *lib.get(b"soundtouch_numSamples\0").ok()?,
                _lib: lib,
            })
        }
    })
    .as_ref()
}

/// Whether SoundTouch's library could be loaded.
pub fn soundtouch_available() -> bool {
    soundtouch_lib().is_some()
}

struct SoundTouch {
    lib: &'static SoundTouchLib,
    handle: Handle,
    channels: usize,
}

impl SoundTouch {
    /// `sub` is REAPER's sub-mode: its remainder by 3 picks default, high-quality or fast
    /// settings.
    fn new(rate: u32, channels: usize, time: f64, pitch: f64, sub: i32) -> Option<Self> {
        let lib = soundtouch_lib()?;
        let handle = unsafe { (lib.create)() };
        if handle.is_null() {
            return None;
        }
        let quality = sub % 3;
        // (setting id, default, high quality, fast): anti-alias filter on, its length, quick
        // seek, sequence, seek window and overlap in ms.
        let settings: [(i32, [i32; 3]); 6] = [
            (0, [1, 1, 0]),
            (1, [32, 128, 32]),
            (2, [0, 0, 1]),
            (3, [82, 41, 164]),
            (4, [14, 28, 7]),
            (5, [12, 12, 6]),
        ];
        unsafe {
            (lib.set_sample_rate)(handle, rate);
            (lib.set_tempo)(handle, (1.0 / time) as f32);
            (lib.set_pitch)(handle, pitch as f32);
            (lib.set_channels)(handle, channels as u32);
            for (id, values) in settings {
                (lib.set_setting)(handle, id, values[quality as usize]);
            }
        }
        Some(Self { lib, handle, channels })
    }
}

impl Shifter for SoundTouch {
    fn put(&mut self, frames: &[f32]) {
        unsafe { (self.lib.put)(self.handle, frames.as_ptr(), (frames.len() / self.channels) as u32) }
    }

    fn available(&self) -> usize {
        unsafe { (self.lib.num_samples)(self.handle) as usize }
    }

    fn take(&mut self, out: &mut [f32]) -> usize {
        unsafe { (self.lib.receive)(self.handle, out.as_mut_ptr(), (out.len() / self.channels) as u32) as usize }
    }
}

impl Drop for SoundTouch {
    fn drop(&mut self) {
        unsafe { (self.lib.destroy)(self.handle) }
    }
}

#[cfg(feature = "rubberband")]
mod rubberband {
    use std::sync::OnceLock;

    use libloading::Library;

    use super::{Handle, Shifter, open};

    struct Lib {
        new: unsafe extern "C" fn(u32, u32, i32, f64, f64) -> Handle,
        delete: unsafe extern "C" fn(Handle),
        process: unsafe extern "C" fn(Handle, *const *const f32, u32, i32),
        available: unsafe extern "C" fn(Handle) -> i32,
        retrieve: unsafe extern "C" fn(Handle, *const *mut f32, u32) -> u32,
        _lib: Library,
    }

    fn lib() -> Option<&'static Lib> {
        static LIB: OnceLock<Option<Lib>> = OnceLock::new();
        LIB.get_or_init(|| {
            let lib = open(&["librubberband.so.3", "librubberband.so.2", "librubberband.so", "librubberband.dylib"])?;
            unsafe {
                Some(Lib {
                    new: *lib.get(b"rubberband_new\0").ok()?,
                    delete: *lib.get(b"rubberband_delete\0").ok()?,
                    process: *lib.get(b"rubberband_process\0").ok()?,
                    available: *lib.get(b"rubberband_available\0").ok()?,
                    retrieve: *lib.get(b"rubberband_retrieve\0").ok()?,
                    _lib: lib,
                })
            }
        })
        .as_ref()
    }

    /// Whether Rubber Band's library could be loaded.
    pub fn available() -> bool {
        lib().is_some()
    }

    /// REAPER's options for sub-mode `sub`: real-time processing, then four flags and base-3
    /// digits for transients, detector, pitch mode and window.
    pub(super) fn options(sub: i32) -> i32 {
        let mut options = 0x1 | 0x10;
        for (bit, flag) in [(1, 0x0100_0000), (2, 0x1000_0000), (4, 0x2000), (8, 0x80_0000)] {
            if sub & bit != 0 {
                options |= flag;
            }
        }
        let mut rest = sub / 16;
        for choices in [[0x100, 0x200], [0x400, 0x800], [0x0200_0000, 0x0400_0000], [0x10_0000, 0x20_0000]] {
            match rest % 3 {
                1 => options |= choices[0],
                2 => options |= choices[1],
                _ => {}
            }
            rest /= 3;
        }
        options
    }

    pub(super) struct RubberBand {
        lib: &'static Lib,
        handle: Handle,
        channels: usize,
        planar: Vec<Vec<f32>>,
    }

    impl RubberBand {
        pub(super) fn new(rate: u32, channels: usize, time: f64, pitch: f64, sub: i32) -> Option<Self> {
            let lib = lib()?;
            let handle = unsafe { (lib.new)(rate, channels as u32, options(sub), time, pitch) };
            (!handle.is_null()).then(|| Self { lib, handle, channels, planar: vec![Vec::new(); channels] })
        }
    }

    impl Shifter for RubberBand {
        fn put(&mut self, frames: &[f32]) {
            let n = frames.len() / self.channels;
            for (c, ch) in self.planar.iter_mut().enumerate() {
                ch.clear();
                ch.extend(frames.iter().skip(c).step_by(self.channels));
            }
            let ptrs: Vec<*const f32> = self.planar.iter().map(|c| c.as_ptr()).collect();
            unsafe { (self.lib.process)(self.handle, ptrs.as_ptr(), n as u32, 0) }
        }

        fn available(&self) -> usize {
            unsafe { (self.lib.available)(self.handle).max(0) as usize }
        }

        fn take(&mut self, out: &mut [f32]) -> usize {
            let n = out.len() / self.channels;
            let mut planar = vec![vec![0.0f32; n]; self.channels];
            let ptrs: Vec<*mut f32> = planar.iter_mut().map(|c| c.as_mut_ptr()).collect();
            let got = unsafe { (self.lib.retrieve)(self.handle, ptrs.as_ptr(), n as u32) } as usize;
            for (c, ch) in planar.iter().enumerate() {
                for (i, v) in ch[..got].iter().enumerate() {
                    out[i * self.channels + c] = *v;
                }
            }
            got
        }
    }

    impl Drop for RubberBand {
        fn drop(&mut self) {
            unsafe { (self.lib.delete)(self.handle) }
        }
    }
}

/// Whether Rubber Band can run (the `rubberband` feature and its library).
pub fn rubberband_available() -> bool {
    #[cfg(feature = "rubberband")]
    return rubberband::available();
    #[cfg(not(feature = "rubberband"))]
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames: usize) -> AudioData {
        let ch: Vec<f32> =
            (0..frames).map(|i| 0.5 * (i as f32 * 440.0 * std::f32::consts::TAU / 48000.0).sin()).collect();
        AudioData { sample_rate: 48000, channels: vec![ch.clone(), ch] }
    }

    #[test]
    fn soundtouch_stretches_when_installed() {
        if !soundtouch_available() {
            return;
        }
        for sub in [0, 1, 2, 6] {
            let out = process(&tone(48000), 1.5, 1.0, sub).unwrap();
            assert_eq!(out.frames(), 72000);
            let mid = &out.channels[0][20000..50000];
            let rms = (mid.iter().map(|v| v * v).sum::<f32>() / mid.len() as f32).sqrt();
            assert!((rms - 0.3536).abs() < 0.03, "sub {sub}: rms {rms}");
        }
    }

    #[test]
    fn other_shifters_are_left_to_the_caller() {
        assert!(process(&tone(1000), 1.5, 1.0, 9 << 16).is_none());
    }

    #[cfg(feature = "rubberband")]
    #[test]
    fn rubberband_options_follow_reapers_sub_modes() {
        assert_eq!(rubberband::options(0), 0x11);
        assert_eq!(rubberband::options(1), 0x11 | 0x0100_0000);
        assert_eq!(rubberband::options(16), 0x11 | 0x100);
        assert_eq!(rubberband::options(48), 0x11 | 0x400);
        assert_eq!(rubberband::options(144), 0x11 | 0x0200_0000);
    }
}
