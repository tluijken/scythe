//! The media explorer: folders and REAPER media databases (`MediaDB/*.ReaperFileList`) with
//! their metadata columns, search, favourites, preview and inserting into the project. Admin
//! only: paths name files on the machine running Scythe.

use std::path::PathBuf;

use schemars::JsonSchema;
use scythe_core::mediadb::{MediaInfo, Shortcut};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json as value};

use super::{ApiError, TrackRef, done, json};
use crate::Session;
use crate::explorer::{Entry, InsertTarget, Location, TempoMatch};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TempoMatchMode {
    Off,
    On,
    /// At half the project tempo.
    Half,
    /// At double the project tempo.
    Double,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum ExplorerCommand {
    /// The shortcut list (folders and databases) and the preview settings.
    #[serde(rename = "media.shortcuts")]
    Shortcuts,
    /// Adds a folder to the shortcut list.
    #[serde(rename = "media.add_shortcut")]
    AddShortcut { folder: PathBuf },
    /// Removes entry `index` (from `media.shortcuts`) from the shortcut list; a database's
    /// file stays.
    #[serde(rename = "media.remove_shortcut")]
    RemoveShortcut { index: usize },
    /// Creates a database scanning `folders` (with their subfolders), adds it to the shortcut
    /// list and scans it.
    #[serde(rename = "media.create_database")]
    CreateDatabase { name: String, folders: Vec<PathBuf> },
    /// Adds a database's new files (REAPER's "Scan database for new files"); `rescan` reads
    /// every file's metadata again; `remove_missing` drops files that are gone.
    #[serde(rename = "media.scan_database")]
    ScanDatabase {
        database: String,
        #[serde(default)]
        rescan: bool,
        #[serde(default)]
        remove_missing: bool,
    },
    /// The subfolders and media files of a folder, or a database's files, with their
    /// metadata (length, rate, channels, bits or bitrate, title, artist, BPM, key …).
    #[serde(rename = "media.browse")]
    Browse {
        /// A folder on the machine running Scythe.
        #[serde(default)]
        folder: Option<PathBuf>,
        /// A media database by file name (`00.ReaperFileList`) or shortcut title.
        #[serde(default)]
        database: Option<String>,
    },
    /// Files whose name or metadata match `query`: every word must appear, `-word` must not,
    /// `"two words"` together. A folder search includes subfolders unless `subfolders` is false.
    #[serde(rename = "media.search")]
    Search {
        /// A folder on the machine running Scythe.
        #[serde(default)]
        folder: Option<PathBuf>,
        /// A media database by file name (`00.ReaperFileList`) or shortcut title.
        #[serde(default)]
        database: Option<String>,
        query: String,
        #[serde(default)]
        subfolders: Option<bool>,
    },
    /// Marks a file as a favourite (or not); in `database` when given.
    #[serde(rename = "media.favorite")]
    Favorite {
        path: PathBuf,
        #[serde(default = "yes")]
        on: bool,
        #[serde(default)]
        database: Option<String>,
    },
    /// Plays a file through the preview player, straight to the device output after the master.
    /// Set the options first with `media.preview_settings`.
    #[serde(rename = "media.preview")]
    Preview {
        path: PathBuf,
        /// Seconds into the file to start at.
        #[serde(default)]
        start: Option<f64>,
    },
    /// Stops the preview.
    #[serde(rename = "media.preview_stop")]
    PreviewStop,
    /// The preview's file, position and length.
    #[serde(rename = "media.preview_state")]
    PreviewState,
    /// Preview and insert options: tempo match, pitch (semitones), volume (dB), repeat,
    /// preserve pitch while tempo matching. Fields left out stay.
    #[serde(rename = "media.preview_settings")]
    PreviewSettings {
        #[serde(default)]
        tempo_match: Option<TempoMatchMode>,
        #[serde(default)]
        pitch: Option<f64>,
        #[serde(default)]
        volume_db: Option<f64>,
        #[serde(default)]
        repeat: Option<bool>,
        #[serde(default)]
        preserve_pitch: Option<bool>,
    },
    /// Inserts a file at the edit cursor: on `track`, on a new track (`new_track`), or on the
    /// selected track (a new one when none is selected). With tempo match on and a tempo in the
    /// file's metadata, the take's rate follows the project tempo.
    /// Runs a media explorer action by its REAPER id (section 32063: 1008 preview play, 1009
    /// stop, 40020 tempo match on, 41000 insert into project, 42084 set as favorite, 42006 scan
    /// database …) on `path` (the selected file) and the folder or database given.
    #[serde(rename = "media.action")]
    Action {
        command: u32,
        #[serde(default)]
        path: Option<PathBuf>,
        #[serde(default)]
        folder: Option<PathBuf>,
        #[serde(default)]
        database: Option<String>,
    },
    #[serde(rename = "media.insert")]
    Insert {
        path: PathBuf,
        #[serde(default)]
        track: Option<TrackRef>,
        #[serde(default)]
        new_track: bool,
    },
}

fn yes() -> bool {
    true
}

fn failed(e: String) -> ApiError {
    ApiError::failed(e)
}

/// A database named by file name or shortcut title.
fn database(session: &Session, name: &str) -> Result<String, ApiError> {
    let list = session.explorer_shortcuts();
    if let Some(s) = list.iter().find(|s| s.is_database() && (s.target == name || s.name() == name)) {
        return Ok(s.target.clone());
    }
    let dir = session.database_dir().map_err(failed)?;
    if dir.join(name).is_file() {
        return Ok(name.to_owned());
    }
    Err(ApiError::not_found(format!("no media database \"{name}\"")))
}

fn location(session: &Session, folder: Option<PathBuf>, db: Option<String>) -> Result<Location, ApiError> {
    match (folder, db) {
        (Some(f), None) => Ok(Location::Folder(f)),
        (None, Some(d)) => Ok(Location::Database(database(session, &d)?)),
        _ => Err(ApiError::invalid("give either \"folder\" or \"database\"")),
    }
}

fn info_json(info: &MediaInfo) -> Value {
    value!({
        "length": info.length, "sample_rate": info.sample_rate, "channels": info.channels,
        "bits": info.bits, "bitrate_kbps": info.bitrate, "title": info.title, "artist": info.artist,
        "album": info.album, "date": info.date, "genre": info.genre, "comment": info.comment,
        "description": info.description, "bpm": info.bpm, "key": info.key,
    })
}

fn entry_json(e: &Entry) -> Value {
    let mut v = value!({
        "path": e.path, "name": e.name(), "folder": e.folder, "size": e.size, "modified": e.modified,
        "favorite": e.favorite,
    });
    if let Some(i) = &e.info {
        v["metadata"] = info_json(i);
    }
    v
}

fn shortcut_json(i: usize, s: &Shortcut) -> Value {
    value!({ "index": i, "name": s.name(), "target": s.target, "database": s.is_database() })
}

fn mode(m: TempoMatch) -> TempoMatchMode {
    match m {
        TempoMatch::Off => TempoMatchMode::Off,
        TempoMatch::On => TempoMatchMode::On,
        TempoMatch::Half => TempoMatchMode::Half,
        TempoMatch::Double => TempoMatchMode::Double,
    }
}

fn settings(session: &Session) -> Value {
    let e = &session.explorer;
    value!({
        "tempo_match": json(mode(e.tempo_match)), "pitch": e.pitch, "volume_db": e.volume_db,
        "repeat": session.engine.preview().repeat(), "preserve_pitch": e.preserve_pitch,
    })
}

pub fn execute(session: &mut Session, command: ExplorerCommand) -> Result<Value, ApiError> {
    match command {
        ExplorerCommand::Shortcuts => {
            let list = session.explorer_shortcuts();
            let shortcuts: Vec<Value> = list.iter().enumerate().map(|(i, s)| shortcut_json(i, s)).collect();
            Ok(value!({ "shortcuts": shortcuts, "preview": settings(session) }))
        }
        ExplorerCommand::AddShortcut { folder } => {
            if !folder.is_dir() {
                return Err(ApiError::not_found(format!("no folder {}", folder.display())));
            }
            let mut list = session.explorer_shortcuts();
            list.push(Shortcut { target: folder.display().to_string(), title: None });
            session.set_explorer_shortcuts(&list).map_err(failed)?;
            Ok(value!({ "index": list.len() - 1 }))
        }
        ExplorerCommand::RemoveShortcut { index } => {
            let mut list = session.explorer_shortcuts();
            if index >= list.len() {
                return Err(ApiError::not_found(format!("no shortcut {index}")));
            }
            list.remove(index);
            session.set_explorer_shortcuts(&list).map_err(failed)?;
            Ok(done())
        }
        ExplorerCommand::CreateDatabase { name, folders } => {
            if let Some(f) = folders.iter().find(|f| !f.is_dir()) {
                return Err(ApiError::not_found(format!("no folder {}", f.display())));
            }
            let file = session.create_database(&name, &folders).map_err(failed)?;
            let files = session.read_database(&file).map_err(failed)?.files.len();
            Ok(value!({ "database": file, "files": files }))
        }
        ExplorerCommand::ScanDatabase { database: name, rescan, remove_missing } => {
            let file = database(session, &name)?;
            let removed =
                if remove_missing { session.remove_missing_from_database(&file).map_err(failed)? } else { 0 };
            let added = session.scan_database(&file, rescan).map_err(failed)?;
            let files = session.read_database(&file).map_err(failed)?.files.len();
            Ok(value!({ "database": file, "added": added, "removed": removed, "files": files }))
        }
        ExplorerCommand::Browse { folder, database: db } => {
            let loc = location(session, folder, db)?;
            let entries = session.browse(&loc).map_err(failed)?;
            Ok(value!({ "entries": entries.iter().map(entry_json).collect::<Vec<_>>() }))
        }
        ExplorerCommand::Search { folder, database: db, query, subfolders } => {
            let loc = location(session, folder, db)?;
            let keep = session.explorer.search_subfolders;
            if let Some(s) = subfolders {
                session.explorer.search_subfolders = s;
            }
            let found = session.search_media(&loc, &query);
            session.explorer.search_subfolders = keep;
            let found = found.map_err(failed)?;
            Ok(value!({ "entries": found.iter().map(entry_json).collect::<Vec<_>>() }))
        }
        ExplorerCommand::Favorite { path, on, database: db } => {
            let db = db.map(|d| database(session, &d)).transpose()?;
            session.set_favorite(&path, on, db.as_deref()).map_err(failed)?;
            Ok(done())
        }
        ExplorerCommand::Preview { path, start } => {
            let start = super::time(start.unwrap_or(0.0), "start")?;
            let rate = session.tempo_match_rate(&path);
            session.preview_media(&path, start).map_err(failed)?;
            Ok(value!({ "path": path, "rate": rate }))
        }
        ExplorerCommand::PreviewStop => {
            session.preview_stop();
            Ok(done())
        }
        ExplorerCommand::PreviewState => {
            let p = session.engine.preview();
            let (position, length) = p.position().unzip();
            Ok(value!({ "path": p.path(), "playing": p.is_playing(), "position": position, "length": length }))
        }
        ExplorerCommand::PreviewSettings { tempo_match, pitch, volume_db, repeat, preserve_pitch } => {
            let e = &mut session.explorer;
            if let Some(m) = tempo_match {
                e.tempo_match = match m {
                    TempoMatchMode::Off => TempoMatch::Off,
                    TempoMatchMode::On => TempoMatch::On,
                    TempoMatchMode::Half => TempoMatch::Half,
                    TempoMatchMode::Double => TempoMatch::Double,
                };
            }
            if let Some(p) = pitch {
                if !(-48.0..=48.0).contains(&p) {
                    return Err(ApiError::invalid("pitch runs from -48 to 48 semitones"));
                }
                e.pitch = p;
            }
            if let Some(v) = volume_db {
                if !(-150.0..=24.0).contains(&v) {
                    return Err(ApiError::invalid("volume_db runs from -150 to 24"));
                }
                e.volume_db = v;
                session.engine.preview().set_gain(scythe_core::units::db_to_gain(v) as f32);
            }
            if let Some(p) = preserve_pitch {
                session.explorer.preserve_pitch = p;
            }
            if let Some(r) = repeat {
                session.engine.preview().set_repeat(r);
            }
            Ok(settings(session))
        }
        ExplorerCommand::Action { command, path, folder, database: db } => {
            if folder.is_some() || db.is_some() {
                session.explorer.location = Some(location(session, folder, db)?);
            }
            if path.is_some() {
                session.explorer.selected = path;
            }
            session.run_explorer_action(command).map_err(failed)?;
            Ok(settings(session))
        }
        ExplorerCommand::Insert { path, track, new_track } => {
            if !path.is_file() {
                return Err(ApiError::not_found(format!("no file {}", path.display())));
            }
            let target = match (track, new_track) {
                (Some(_), true) => return Err(ApiError::invalid("give \"track\" or \"new_track\", not both")),
                (Some(t), false) => InsertTarget::Track(t.track(&session.project)?),
                (None, true) => InsertTarget::NewTrack,
                (None, false) => InsertTarget::Project,
            };
            let (track, item) = session.insert_media(&path, target).map_err(failed)?;
            let it = session.project.track(track).and_then(|t| t.item(item));
            Ok(value!({
                "track": track.0, "item": item.0,
                "position": it.map(|i| i.position), "length": it.map(|i| i.length),
                "playrate": it.and_then(|i| i.takes.first()).map(|t| t.playrate),
            }))
        }
    }
}
