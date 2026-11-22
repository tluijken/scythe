//! Decoded and converted media kept on disk, so a set opens fast the next time: REAPER reads
//! media as it plays and converts its rate on the fly, while Scythe decodes every file and
//! converts it to the engine's rate when the project opens, which for a long live set at another
//! rate than its files takes a minute. An entry is keyed by the file's path, modification time
//! and size, the engine rate and the resample mode, so a file written anew, another device rate
//! or another mode makes a new entry and never reads a stale one. The oldest entries go when the
//! cache grows past its limit.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use scythe_core::render_settings::ResampleMode;

use crate::media::AudioData;

/// Bump when what an entry holds changes (the decoder or a resampler), so old entries are not read.
const FORMAT: u32 = 1;
const MAGIC: &[u8; 8] = b"SCYMEDIA";
/// What the cache may hold before the least recently used entries go.
pub const DEFAULT_LIMIT: u64 = 16 << 30;

#[derive(Debug, Clone)]
pub struct DiskCache {
    dir: PathBuf,
    limit: u64,
}

/// One entry: the file's own sample rate and its audio at the engine's rate.
pub struct Entry {
    pub source_rate: u32,
    pub audio: AudioData,
}

impl DiskCache {
    pub fn new(dir: PathBuf, limit: u64) -> Self {
        Self { dir, limit }
    }

    /// The cache of decoded media under the user's cache directory, unless `SCYTHE_MEDIA_CACHE`
    /// is `0` (or names another directory).
    pub fn for_user() -> Option<Self> {
        let dir = match std::env::var_os("SCYTHE_MEDIA_CACHE") {
            Some(v) if v == "0" => return None,
            Some(v) if !v.is_empty() => PathBuf::from(v),
            _ => scythe_core::paths::cache_home()?.join("scythe").join("media"),
        };
        Some(Self::new(dir, DEFAULT_LIMIT))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The entry file for `path` as it is now, at `rate` with `mode`; `None` when the file is
    /// gone.
    pub fn entry_path(&self, path: &Path, rate: u32, mode: ResampleMode) -> Option<PathBuf> {
        let meta = std::fs::metadata(path).ok()?;
        let modified = meta.modified().ok()?.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_nanos();
        let mut h = Fnv::default();
        h.write(&FORMAT.to_le_bytes());
        h.write(path.to_string_lossy().as_bytes());
        h.write(&modified.to_le_bytes());
        h.write(&meta.len().to_le_bytes());
        h.write(&rate.to_le_bytes());
        h.write(format!("{mode:?}").as_bytes());
        Some(self.dir.join(format!("{:016x}.scymedia", h.0)))
    }

    /// Reads an entry; `None` when there is none or it does not read back whole.
    pub fn read(&self, entry: &Path) -> Option<Entry> {
        let file = File::open(entry).ok()?;
        let len = file.metadata().ok()?.len();
        let mut r = BufReader::with_capacity(1 << 20, file);
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic).ok()?;
        if &magic != MAGIC || read_u32(&mut r)? != FORMAT {
            return None;
        }
        let (source_rate, sample_rate, channels) = (read_u32(&mut r)?, read_u32(&mut r)?, read_u32(&mut r)?);
        let frames = read_u64(&mut r)?;
        // The header: magic, format, source rate, rate, channels (4 bytes each) and frames (8).
        if 32 + channels as u64 * frames * 4 != len {
            return None;
        }
        let mut bytes = vec![0u8; frames as usize * 4];
        let mut out = Vec::with_capacity(channels as usize);
        for _ in 0..channels {
            r.read_exact(&mut bytes).ok()?;
            out.push(bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect());
        }
        // Reading it marks it used, so pruning drops the entries no set has opened for longest.
        let _ = File::options().write(true).open(entry).and_then(|f| f.set_modified(SystemTime::now()));
        Some(Entry { source_rate, audio: AudioData { sample_rate, channels: out } })
    }

    /// Writes an entry (to a temporary name first, so a reader never sees half of one), then
    /// prunes the cache to its limit.
    pub fn write(&self, entry: &Path, source_rate: u32, audio: &AudioData) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let partial = entry.with_extension(format!("part{}", std::process::id()));
        let result = (|| {
            let mut w = BufWriter::with_capacity(1 << 20, File::create(&partial)?);
            w.write_all(MAGIC)?;
            for v in [FORMAT, source_rate, audio.sample_rate, audio.channels.len() as u32] {
                w.write_all(&v.to_le_bytes())?;
            }
            w.write_all(&(audio.frames() as u64).to_le_bytes())?;
            let mut bytes = Vec::with_capacity(1 << 16);
            for ch in &audio.channels {
                for chunk in ch.chunks(1 << 14) {
                    bytes.clear();
                    bytes.extend(chunk.iter().flat_map(|s| s.to_le_bytes()));
                    w.write_all(&bytes)?;
                }
            }
            w.into_inner().map_err(|e| e.into_error())?.sync_all()
        })()
        .and_then(|()| std::fs::rename(&partial, entry));
        if result.is_err() {
            let _ = std::fs::remove_file(&partial);
        }
        result?;
        self.prune();
        Ok(())
    }

    /// Removes the least recently used entries until the cache holds at most 80 % of its limit,
    /// once it holds more than the limit.
    pub fn prune(&self) {
        let Ok(dir) = std::fs::read_dir(&self.dir) else { return };
        let mut entries: Vec<(SystemTime, u64, PathBuf)> = dir
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "scymedia"))
            .filter_map(|e| {
                let meta = e.metadata().ok()?;
                Some((meta.modified().ok()?, meta.len(), e.path()))
            })
            .collect();
        let mut total: u64 = entries.iter().map(|e| e.1).sum();
        if total <= self.limit {
            return;
        }
        entries.sort();
        for (_, len, path) in entries {
            if total <= self.limit / 10 * 8 {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total -= len;
            }
        }
    }
}

/// Whether reading `path` again costs enough to keep it: anything converted to another rate, and
/// compressed formats, which take far longer to decode than plain PCM files take to read.
pub fn worth_keeping(path: &Path, source_rate: u32, rate: u32) -> bool {
    let plain = path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        ["wav", "wave", "aif", "aiff", "aifc", "w64", "caf", "rf64", "bwf"].contains(&e.to_ascii_lowercase().as_str())
    });
    source_rate != rate || !plain
}

fn read_u32(r: &mut impl Read) -> Option<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).ok()?;
    Some(u32::from_le_bytes(b))
}

fn read_u64(r: &mut impl Read) -> Option<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b).ok()?;
    Some(u64::from_le_bytes(b))
}

/// FNV-1a: a hash that stays the same across Rust versions, for names on disk.
struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
        // A separator, so ("ab", "c") and ("a", "bc") differ.
        self.0 = (self.0 ^ 0xff).wrapping_mul(0x0100_0000_01b3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_reads_back_and_changes_with_the_file_rate_and_mode() {
        let dir = std::env::temp_dir().join(format!("scythe-media-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cache = DiskCache::new(dir.join("cache"), DEFAULT_LIMIT);
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("a.mp3");
        std::fs::write(&source, b"one").unwrap();
        let mode = ResampleMode::PLAYBACK;
        let entry = cache.entry_path(&source, 48_000, mode).unwrap();
        assert!(cache.read(&entry).is_none());

        let audio = AudioData { sample_rate: 48_000, channels: vec![vec![0.5, -0.25, 1.0], vec![0.0, 0.125, -1.0]] };
        cache.write(&entry, 44_100, &audio).unwrap();
        let back = cache.read(&entry).unwrap();
        assert_eq!((back.source_rate, back.audio), (44_100, audio));

        assert_ne!(cache.entry_path(&source, 44_100, mode), Some(entry.clone()), "another rate");
        assert_ne!(cache.entry_path(&source, 48_000, ResampleMode::Linear), Some(entry.clone()), "another mode");
        std::fs::write(&source, b"longer").unwrap();
        assert_ne!(cache.entry_path(&source, 48_000, mode), Some(entry.clone()), "the file changed");

        std::fs::write(&entry, b"SCYMEDIA short").unwrap();
        assert!(cache.read(&entry).is_none(), "a damaged entry is not read");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pruning_drops_the_least_recently_used_entries() {
        let dir = std::env::temp_dir().join(format!("scythe-media-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache = DiskCache::new(dir.clone(), 250);
        let now = SystemTime::now();
        for (i, name) in ["old", "mid", "new"].into_iter().enumerate() {
            let path = dir.join(format!("{name}.scymedia"));
            std::fs::write(&path, vec![0u8; 100]).unwrap();
            let age = std::time::Duration::from_secs(100 * (3 - i as u64));
            File::options().write(true).open(&path).unwrap().set_modified(now - age).unwrap();
        }
        cache.prune();
        let left: Vec<bool> = ["old", "mid", "new"].iter().map(|n| dir.join(format!("{n}.scymedia")).exists()).collect();
        assert_eq!(left, [false, true, true], "down to 80 % of the limit, oldest first");
        assert!(worth_keeping(Path::new("x.wav"), 44_100, 48_000));
        assert!(worth_keeping(Path::new("x.mp3"), 48_000, 48_000));
        assert!(!worth_keeping(Path::new("x.WAV"), 48_000, 48_000));
        std::fs::remove_dir_all(&dir).ok();
    }
}
