//! The media explorer (REAPER's View: Media explorer): browsing folders and REAPER's media
//! databases, searching them, favourites, previewing a file through the preview player
//! (tempo-matched to the project when asked) and inserting it into the project.
//!
//! The shortcut list lives in the `[reaper_sexplorer]` section of the preferences file and the
//! databases in `MediaDB/` beside it, as REAPER keeps them in its resource folder, so REAPER's
//! own can be copied over.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use scythe_core::ini::Ini;
use scythe_core::mediadb::{self, DbFile, MediaDb, MediaInfo, Query, Shortcut};
use scythe_core::{Processing, TrackId};

use crate::Session;

/// Where the explorer looks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    Folder(PathBuf),
    /// A database by its file name in `MediaDB` (`00.ReaperFileList`).
    Database(String),
}

/// REAPER's tempo match: off, on, or on at half or double the tempo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TempoMatch {
    #[default]
    Off,
    On,
    Half,
    Double,
}

impl TempoMatch {
    fn factor(self) -> Option<f64> {
        match self {
            Self::Off => None,
            Self::On => Some(1.0),
            Self::Half => Some(0.5),
            Self::Double => Some(2.0),
        }
    }
}

/// A file or folder as the explorer lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub path: PathBuf,
    pub folder: bool,
    pub size: u64,
    /// Unix seconds.
    pub modified: u64,
    pub info: Option<MediaInfo>,
    pub favorite: bool,
}

impl Entry {
    pub fn name(&self) -> String {
        self.path.file_name().map_or_else(|| self.path.display().to_string(), |n| n.to_string_lossy().into_owned())
    }

    fn texts(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self.path.file_name().and_then(|n| n.to_str()).into_iter().collect();
        if let Some(i) = &self.info {
            out.extend(i.search_fields());
        }
        out
    }
}

/// Where inserted media goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertTarget {
    /// At the edit cursor on the selected track, or a new one when none is selected
    /// (Media: Insert into project).
    Project,
    /// At the edit cursor on a new track (Media: Insert on a new track).
    NewTrack,
    /// At the edit cursor on this track.
    Track(TrackId),
}

/// The explorer's settings and what it has read of files so far.
#[derive(Debug, Clone)]
pub struct Explorer {
    pub location: Option<Location>,
    pub tempo_match: TempoMatch,
    /// Keep the pitch when tempo matching (REAPER's "Preserve pitch when tempo-matching").
    pub preserve_pitch: bool,
    /// Preview pitch shift in semitones.
    pub pitch: f64,
    pub volume_db: f64,
    /// Search subfolders in folder searches.
    pub search_subfolders: bool,
    /// The file selected in the list, which the explorer's actions work on.
    pub selected: Option<PathBuf>,
    /// Metadata by file, with the size and time it was read at.
    cache: HashMap<PathBuf, (u64, u64, Option<MediaInfo>)>,
}

impl Default for Explorer {
    fn default() -> Self {
        Self {
            location: None,
            tempo_match: TempoMatch::Off,
            preserve_pitch: true,
            pitch: 0.0,
            volume_db: 0.0,
            search_subfolders: true,
            selected: None,
            cache: HashMap::new(),
        }
    }
}

/// The media explorer section's actions (keymap section 32063) Scythe runs, with REAPER 7.73's
/// ids and names.
pub const ACTIONS: &[(u32, &str)] = &[
    (1004, "Browser: Go to parent folder"),
    (1008, "Preview: Play"),
    (1009, "Preview: Stop"),
    (1010, "Preview: Pause"),
    (1068, "Preview: Toggle repeat on/off"),
    (40018, "Browser: Refresh"),
    (40019, "Tempo match: Off"),
    (40020, "Tempo match: On"),
    (40021, "Tempo match: /2"),
    (40022, "Tempo match: x2"),
    (40023, "Tempo match: Toggle on/off"),
    (40024, "Preview: Play/stop"),
    (40068, "Options: Preserve pitch when tempo-matching or changing play rate"),
    (41000, "Media: Insert into project"),
    (41001, "Media: Insert on a new track"),
    (42006, "Scan database for new files"),
    (42009, "Remove missing files from database"),
    (42084, "Set as favorite"),
];

const FAVORITES_FILE: &str = "explorer-favorites.txt";
/// Most files a folder search looks through, so a search of a huge tree stays bounded.
const SEARCH_LIMIT: usize = 20_000;

fn stat(path: &Path) -> Option<(u64, u64, bool)> {
    let m = std::fs::metadata(path).ok()?;
    let modified = m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
    Some((m.len(), modified, m.is_dir()))
}

/// The media files under `dir`, depth first, sorted by name, at most `limit`.
fn walk(dir: &Path, limit: usize, out: &mut Vec<PathBuf>) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    let mut paths: Vec<PathBuf> = read.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.retain(|p| !hidden(p));
    paths.sort();
    for p in paths {
        if out.len() >= limit {
            return;
        }
        if p.is_dir() {
            walk(&p, limit, out);
        } else if mediadb::is_media(&p) {
            out.push(p);
        }
    }
}

impl Session {
    fn explorer_dir(&self) -> Result<PathBuf, String> {
        self.prefs_file
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_owned)
            .ok_or_else(|| "the explorer keeps its files beside the preferences; none are loaded".to_owned())
    }

    /// `MediaDB/`, where the databases are.
    pub fn database_dir(&self) -> Result<PathBuf, String> {
        Ok(self.explorer_dir()?.join(mediadb::DB_DIR))
    }

    fn read_prefs_ini(&self) -> Ini {
        let text = self.prefs_file.as_deref().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
        Ini::parse(&text)
    }

    /// The shortcut list: folders and databases.
    pub fn explorer_shortcuts(&self) -> Vec<Shortcut> {
        mediadb::shortcuts(&self.read_prefs_ini())
    }

    pub fn set_explorer_shortcuts(&mut self, list: &[Shortcut]) -> Result<(), String> {
        let path = self.prefs_file.clone().ok_or("no preferences file is loaded")?;
        let mut ini = self.read_prefs_ini();
        mediadb::set_shortcuts(&mut ini, list);
        std::fs::write(&path, ini.write()).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn read_database(&self, name: &str) -> Result<MediaDb, String> {
        let path = self.database_dir()?.join(name);
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(MediaDb::parse(&text))
    }

    fn write_database(&self, name: &str, db: &MediaDb) -> Result<(), String> {
        let dir = self.database_dir()?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join(name), db.write()).map_err(|e| e.to_string())
    }

    /// Makes a database scanning `paths`, adds it to the shortcut list and scans it. Answers
    /// its file name.
    pub fn create_database(&mut self, title: &str, paths: &[PathBuf]) -> Result<String, String> {
        let dir = self.database_dir()?;
        let existing: Vec<String> = std::fs::read_dir(&dir)
            .map(|r| r.filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned())).collect())
            .unwrap_or_default();
        let name = mediadb::new_database_name(&existing);
        let db = MediaDb { paths: paths.iter().map(|p| p.display().to_string()).collect(), ..Default::default() };
        self.write_database(&name, &db)?;
        let mut list = self.explorer_shortcuts();
        list.push(Shortcut { target: name.clone(), title: Some(title.to_owned()) });
        self.set_explorer_shortcuts(&list)?;
        self.scan_database(&name, false)?;
        Ok(name)
    }

    /// Adds the files of the database's folders it does not have yet (REAPER's "Scan database
    /// for new files"); with `rescan` reads every file's metadata again. Answers how many files
    /// were added.
    pub fn scan_database(&mut self, name: &str, rescan: bool) -> Result<usize, String> {
        let mut db = self.read_database(name)?;
        let mut found = Vec::new();
        for p in &db.paths {
            walk(Path::new(p), usize::MAX, &mut found);
        }
        let mut added = 0;
        for path in found {
            let key = path.display().to_string();
            let Some((size, modified, _)) = stat(&path) else { continue };
            let known = db.file(&key).is_some_and(|f| f.size == size && f.modified == modified);
            if known && !rescan {
                continue;
            }
            let info = self.media_info(&path).unwrap_or_default();
            match db.file_mut(&key) {
                Some(f) => {
                    (f.size, f.modified) = (size, modified);
                    f.set_info(&info);
                }
                None => {
                    let mut f = DbFile { path: key, size, modified, flags: 0, tags: Vec::new() };
                    f.set_info(&info);
                    db.files.push(f);
                    added += 1;
                }
            }
        }
        db.files.sort_by(|a, b| a.path.cmp(&b.path));
        self.write_database(name, &db)?;
        Ok(added)
    }

    /// Takes the files that no longer exist out of a database; answers how many.
    pub fn remove_missing_from_database(&mut self, name: &str) -> Result<usize, String> {
        let mut db = self.read_database(name)?;
        let before = db.files.len();
        db.files.retain(|f| Path::new(&f.path).exists());
        self.write_database(name, &db)?;
        Ok(before - db.files.len())
    }

    /// A file's metadata, read once per version of the file.
    pub fn media_info(&mut self, path: &Path) -> Option<MediaInfo> {
        let (size, modified, _) = stat(path)?;
        if let Some((s, m, info)) = self.explorer.cache.get(path)
            && (*s, *m) == (size, modified)
        {
            return info.clone();
        }
        let info = scythe_engine::media_info::read(path);
        self.explorer.cache.insert(path.to_owned(), (size, modified, info.clone()));
        info
    }

    fn folder_favorites(&self) -> Vec<PathBuf> {
        let Ok(dir) = self.explorer_dir() else { return Vec::new() };
        std::fs::read_to_string(dir.join(FAVORITES_FILE))
            .map(|t| t.lines().filter(|l| !l.trim().is_empty()).map(PathBuf::from).collect())
            .unwrap_or_default()
    }

    /// Marks a file as a favourite or not: in the database when it is one of its files, else
    /// in Scythe's list of favourites for folders.
    pub fn set_favorite(&mut self, path: &Path, on: bool, database: Option<&str>) -> Result<(), String> {
        if let Some(name) = database {
            let mut db = self.read_database(name)?;
            let f = db.file_mut(&path.display().to_string()).ok_or("the file is not in that database")?;
            f.flags = if on { f.flags | mediadb::FAVORITE } else { f.flags & !mediadb::FAVORITE };
            return self.write_database(name, &db);
        }
        let mut list = self.folder_favorites();
        list.retain(|p| p != path);
        if on {
            list.push(path.to_owned());
        }
        let text: String = list.iter().map(|p| format!("{}\n", p.display())).collect();
        let file = self.explorer_dir()?.join(FAVORITES_FILE);
        std::fs::write(&file, text).map_err(|e| format!("{}: {e}", file.display()))
    }

    /// What a folder or database holds: subfolders first, then media files, by name.
    pub fn browse(&mut self, location: &Location) -> Result<Vec<Entry>, String> {
        match location {
            Location::Database(name) => Ok(self.database_entries(name)?),
            Location::Folder(dir) => {
                let read = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                let mut paths: Vec<PathBuf> = read.filter_map(|e| e.ok().map(|e| e.path())).collect();
                paths.retain(|p| !hidden(p) && (p.is_dir() || mediadb::is_media(p)));
                paths.sort_by_key(|p| (!p.is_dir(), p.file_name().map(|n| n.to_string_lossy().to_lowercase())));
                Ok(self.entries(paths))
            }
        }
    }

    fn entries(&mut self, paths: Vec<PathBuf>) -> Vec<Entry> {
        let favorites = self.folder_favorites();
        paths
            .into_iter()
            .filter_map(|path| {
                let (size, modified, folder) = stat(&path)?;
                let info = if folder { None } else { self.media_info(&path) };
                let favorite = favorites.contains(&path);
                Some(Entry { path, folder, size, modified, info, favorite })
            })
            .collect()
    }

    fn database_entries(&self, name: &str) -> Result<Vec<Entry>, String> {
        let db = self.read_database(name)?;
        Ok(db
            .files
            .iter()
            .map(|f| Entry {
                path: PathBuf::from(&f.path),
                folder: false,
                size: f.size,
                modified: f.modified,
                info: Some(f.info()),
                favorite: f.favorite(),
            })
            .collect())
    }

    /// The files of a folder (and its subfolders when `search_subfolders` is on) or a
    /// database whose name or metadata match `query` (see [`Query`]).
    pub fn search_media(&mut self, location: &Location, query: &str) -> Result<Vec<Entry>, String> {
        let q = Query::parse(query);
        let entries = match location {
            Location::Database(name) => self.database_entries(name)?,
            Location::Folder(dir) if self.explorer.search_subfolders => {
                let mut found = Vec::new();
                walk(dir, SEARCH_LIMIT, &mut found);
                // Matching names need no metadata; read it only for the rest.
                let quick: Vec<PathBuf> =
                    found.iter().filter(|p| !q.include.is_empty() && name_matches(&q, p)).cloned().collect();
                let rest: Vec<PathBuf> = found.into_iter().filter(|p| !quick.contains(p)).collect();
                let mut all = self.entries(quick);
                all.extend(self.entries(rest));
                all.sort_by(|a, b| a.path.cmp(&b.path));
                all
            }
            Location::Folder(_) => self.browse(location)?.into_iter().filter(|e| !e.folder).collect(),
        };
        Ok(entries.into_iter().filter(|e| !e.folder && q.matches(e.texts())).collect())
    }

    /// The rate a file previews and inserts at when tempo matching: the project tempo at the
    /// edit cursor over the file's tempo.
    pub fn tempo_match_rate(&mut self, path: &Path) -> Option<f64> {
        let factor = self.explorer.tempo_match.factor()?;
        let bpm = self.media_info(path)?.bpm?;
        let project = self.project.tempo.bpm_at(self.project.cursor);
        Some((project * factor / bpm).clamp(0.1, 10.0))
    }

    /// Starts previewing `path` from `start` seconds. Loading (and stretching, when tempo
    /// matching) happens on a worker thread; the preview starts when it is ready.
    pub fn preview_media(&mut self, path: &Path, start: f64) -> Result<(), String> {
        if !path.is_file() {
            return Err(format!("no file {}", path.display()));
        }
        if mediadb::is_midi(path) {
            return Err("MIDI files do not preview yet".into());
        }
        let rate = self.tempo_match_rate(path);
        let pitch = self.explorer.pitch;
        let processing = (rate.is_some() || pitch != 0.0).then(|| {
            let r = rate.unwrap_or(1.0);
            let shift = 2f64.powf(pitch / 12.0);
            if self.explorer.preserve_pitch {
                Processing { time: 1.0 / r, pitch: shift, rate: 1.0, mode: Processing::DEFAULT_MODE }
            } else {
                Processing { time: 1.0, pitch: shift, rate: r, mode: Processing::DEFAULT_MODE }
            }
        });
        let preview = self.engine.preview().clone();
        preview.set_gain(scythe_core::units::db_to_gain(self.explorer.volume_db) as f32);
        let pool = self.engine.pool().clone();
        let engine_rate = self.engine.sample_rate();
        let path = path.to_owned();
        let wake = self.waker.clone();
        std::thread::spawn(move || {
            let Ok(media) = pool.load(&path, engine_rate) else { return };
            let audio = match processing {
                Some(p) => match pool.prepare_processed(&path, &p) {
                    Some(a) => a,
                    None => return,
                },
                None => media.audio.clone(),
            };
            let ratio = processing.map_or(1.0, |p| p.length_ratio());
            preview.play(&path, audio, start * ratio);
            wake();
        });
        Ok(())
    }

    pub fn preview_stop(&mut self) {
        self.engine.preview().stop();
    }

    /// Inserts `path` at the edit cursor as REAPER's explorer does, tempo-matched (the take's
    /// rate set, pitch kept) when tempo match is on and the file has a tempo. Answers the new
    /// item's track and id.
    pub fn insert_media(
        &mut self,
        path: &Path,
        target: InsertTarget,
    ) -> Result<(TrackId, scythe_core::ItemId), String> {
        use crate::api::media::MediaCommand;
        use crate::api::{Command, TrackRef, execute};
        let track = match target {
            InsertTarget::Project => self.selected_track,
            InsertTarget::NewTrack => None,
            InsertTarget::Track(t) => Some(t),
        };
        let rate = self.tempo_match_rate(path);
        let answer = execute(
            self,
            Command::Media(MediaCommand::Import {
                paths: vec![path.to_owned()],
                track: track.map(|t| TrackRef::Id(t.0)),
                position: Some(self.project.cursor),
                sequence: false,
                gap: None,
                name: None,
            }),
        )
        .map_err(|e| e.message)?;
        let placed = &answer["items"][0];
        let track = TrackId(placed["track"].as_u64().ok_or("nothing was inserted")?);
        let item = scythe_core::ItemId(placed["item"].as_u64().ok_or("nothing was inserted")?);
        if let Some(r) = rate.filter(|r| (r - 1.0).abs() > 1e-9 && !mediadb::is_midi(path)) {
            let preserve = self.explorer.preserve_pitch;
            self.edit("Tempo-match inserted media", |p| {
                if let Some(it) = p.tracks.iter_mut().find_map(|t| t.item_mut(item)) {
                    it.length /= r;
                    for take in &mut it.takes {
                        take.playrate = r;
                        take.preserve_pitch = preserve;
                    }
                }
            });
            self.request_media();
        }
        Ok((track, item))
    }
    /// Runs a media explorer action (see [`ACTIONS`]) on the explorer's location and
    /// selected file.
    pub fn run_explorer_action(&mut self, id: u32) -> Result<(), String> {
        let file = self.explorer.selected.clone();
        let selected = || file.clone().ok_or_else(|| "select a file first".to_owned());
        let database = match &self.explorer.location {
            Some(Location::Database(d)) => Some(d.clone()),
            _ => None,
        };
        let preview = self.engine.preview().clone();
        match id {
            1004 => {
                if let Some(Location::Folder(dir)) = &self.explorer.location
                    && let Some(parent) = dir.parent()
                {
                    self.explorer.location = Some(Location::Folder(parent.to_owned()));
                }
            }
            1008 => self.preview_media(&selected()?, 0.0)?,
            1009 => preview.stop(),
            1010 => preview.pause(),
            1068 => preview.set_repeat(!preview.repeat()),
            40018 => self.explorer.cache.clear(),
            40019 => self.explorer.tempo_match = TempoMatch::Off,
            40020 => self.explorer.tempo_match = TempoMatch::On,
            40021 => self.explorer.tempo_match = TempoMatch::Half,
            40022 => self.explorer.tempo_match = TempoMatch::Double,
            40023 => {
                self.explorer.tempo_match =
                    if self.explorer.tempo_match == TempoMatch::Off { TempoMatch::On } else { TempoMatch::Off }
            }
            40024 if preview.is_playing() => preview.stop(),
            40024 => self.preview_media(&selected()?, 0.0)?,
            40068 => self.explorer.preserve_pitch = !self.explorer.preserve_pitch,
            41000 | 41001 => {
                let target = if id == 41000 { InsertTarget::Project } else { InsertTarget::NewTrack };
                self.insert_media(&selected()?, target)?;
            }
            42006 | 42009 => {
                let db = database.ok_or("open a database first")?;
                if id == 42006 {
                    self.scan_database(&db, false)?;
                } else {
                    self.remove_missing_from_database(&db)?;
                }
            }
            42084 => {
                let path = selected()?;
                let on = match &database {
                    Some(d) => !self.read_database(d)?.file(&path.display().to_string()).is_some_and(DbFile::favorite),
                    None => !self.folder_favorites().contains(&path),
                };
                self.set_favorite(&path, on, database.as_deref())?;
            }
            _ => return Err(format!("media explorer action {id} is not available in Scythe")),
        }
        Ok(())
    }
}

fn hidden(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'))
}

fn name_matches(q: &Query, path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(|n| q.matches([n]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(dir: &Path) -> Session {
        let pool = std::sync::Arc::new(scythe_engine::MediaPool::default());
        let mut s = Session::new(scythe_engine::Engine::start_offline(pool, 48_000));
        std::fs::create_dir_all(dir).unwrap();
        s.load_preferences(dir.join("preferences.ini"));
        s
    }

    fn wav(path: &Path, seconds: f64) {
        let rate = 48_000u32;
        let frames = (seconds * rate as f64) as u32;
        let mut f = b"RIFF".to_vec();
        f.extend((36 + frames * 2).to_le_bytes());
        f.extend(b"WAVEfmt ");
        f.extend(16u32.to_le_bytes());
        f.extend(1u16.to_le_bytes());
        f.extend(1u16.to_le_bytes());
        f.extend(rate.to_le_bytes());
        f.extend((rate * 2).to_le_bytes());
        f.extend(2u16.to_le_bytes());
        f.extend(16u16.to_le_bytes());
        f.extend(b"data");
        f.extend((frames * 2).to_le_bytes());
        f.extend(vec![0u8; frames as usize * 2]);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, f).unwrap();
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("scythe-explorer-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn databases_index_search_and_favorites() {
        let dir = scratch("db");
        let lib = dir.join("lib");
        wav(&lib.join("kick dry.wav"), 0.5);
        wav(&lib.join("sub/snare.wav"), 1.0);
        std::fs::write(lib.join("notes.txt"), "x").unwrap();
        let mut s = session(&dir.join("cfg"));
        let name = s.create_database("Drums", std::slice::from_ref(&lib)).unwrap();
        assert_eq!(name, "00.ReaperFileList");
        let shortcuts = s.explorer_shortcuts();
        assert_eq!(shortcuts.len(), 1);
        assert_eq!(shortcuts[0].name(), "Drums");
        let all = s.browse(&Location::Database(name.clone())).unwrap();
        assert_eq!(all.len(), 2);
        let info = all[0].info.as_ref().unwrap();
        assert_eq!(
            (info.length, info.sample_rate, info.channels, info.bits),
            (Some(0.5), Some(48000), Some(1), Some(16))
        );
        let hits = s.search_media(&Location::Database(name.clone()), "snare").unwrap();
        assert_eq!(hits.len(), 1);
        s.set_favorite(&hits[0].path, true, Some(&name)).unwrap();
        assert!(s.browse(&Location::Database(name.clone())).unwrap()[1].favorite);
        wav(&lib.join("hat.wav"), 0.25);
        assert_eq!(s.scan_database(&name, false).unwrap(), 1);
        std::fs::remove_file(lib.join("kick dry.wav")).unwrap();
        assert_eq!(s.remove_missing_from_database(&name).unwrap(), 1);
        let text = std::fs::read_to_string(dir.join("cfg/MediaDB/00.ReaperFileList")).unwrap();
        assert!(text.starts_with(&format!("PATH \"{}\"\n", lib.display())), "{text}");
        assert!(text.contains("DATA s:48000 n:1 l:0:00.250 i:16\n"), "{text}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Scans the folder of a database REAPER 7.73 wrote (`SCYTHE_REAPER_MEDIADB`) and compares
    /// every file's size, time and metadata tokens with REAPER's.
    #[test]
    fn scans_like_reaper() {
        let Ok(file) = std::env::var("SCYTHE_REAPER_MEDIADB") else { return };
        let reaper = MediaDb::parse(&std::fs::read_to_string(&file).unwrap());
        let dir = scratch("reaper");
        let mut s = session(&dir);
        let paths: Vec<PathBuf> = reaper.paths.iter().map(PathBuf::from).collect();
        let name = s.create_database("Ours", &paths).unwrap();
        let ours = s.read_database(&name).unwrap();
        assert_eq!(ours.files.len(), reaper.files.len());
        for theirs in &reaper.files {
            let mine = ours.file(&theirs.path).unwrap_or_else(|| panic!("{} not scanned", theirs.path));
            assert_eq!((mine.size, mine.modified), (theirs.size, theirs.modified), "{}", theirs.path);
            assert_eq!(mine.tags, theirs.tags, "{}", theirs.path);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn folders_list_media_and_search_subfolders() {
        let dir = scratch("folder");
        let lib = dir.join("lib");
        wav(&lib.join("b.wav"), 0.1);
        wav(&lib.join("deep/a loop.wav"), 0.1);
        std::fs::write(lib.join("x.txt"), "x").unwrap();
        let mut s = session(&dir.join("cfg"));
        let list = s.browse(&Location::Folder(lib.clone())).unwrap();
        let names: Vec<String> = list.iter().map(Entry::name).collect();
        assert_eq!(names, ["deep", "b.wav"]);
        assert!(list[0].folder);
        let found = s.search_media(&Location::Folder(lib.clone()), "loop").unwrap();
        assert_eq!(found.len(), 1);
        s.explorer.search_subfolders = false;
        assert!(s.search_media(&Location::Folder(lib.clone()), "loop").unwrap().is_empty());
        s.set_favorite(&lib.join("b.wav"), true, None).unwrap();
        assert!(s.browse(&Location::Folder(lib.clone())).unwrap()[1].favorite);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn inserts_tempo_matched_and_previews() {
        let dir = scratch("insert");
        let file = dir.join("loop.wav");
        wav(&file, 2.0);
        let mut s = session(&dir.join("cfg"));
        s.explorer.tempo_match = TempoMatch::On;
        s.explorer.cache.insert(
            file.clone(),
            (
                stat(&file).unwrap().0,
                stat(&file).unwrap().1,
                Some(MediaInfo { bpm: Some(60.0), length: Some(2.0), ..Default::default() }),
            ),
        );
        let bpm = s.project.tempo.bpm_at(0.0);
        let (track, item) = s.insert_media(&file, InsertTarget::NewTrack).unwrap();
        let it = s.project.track(track).unwrap().item(item).unwrap();
        assert!((it.takes[0].playrate - bpm / 60.0).abs() < 1e-9);
        assert!((it.length - 2.0 * 60.0 / bpm).abs() < 1e-9);
        s.explorer.tempo_match = TempoMatch::Off;
        s.preview_media(&file, 0.0).unwrap();
        let start = std::time::Instant::now();
        while s.engine.preview().path().is_none() && start.elapsed().as_secs() < 10 {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(s.engine.preview().path().as_deref(), Some(file.as_path()));
        s.preview_stop();
        assert!(!s.engine.preview().is_playing());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
