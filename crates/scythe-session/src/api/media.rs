//! Bringing audio and MIDI files into the project from paths on the machine running Scythe, and
//! reading what a file holds before importing it. Admin only: a path can name any file the
//! Scythe process may read.

use schemars::JsonSchema;
use scythe_core::{Item, Take, TrackId};
use scythe_engine::analysis::probe::{self, FileInfo};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json as value};
use std::path::{Path, PathBuf};

use super::{ApiError, TrackRef, json, time};
use crate::Session;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum MediaCommand {
    /// Places audio files (WAV, FLAC, AIFF, MP3, Ogg, Opus…) and MIDI files (.mid, as
    /// in-project MIDI items) as items, as one undo step per file, and answers with the items.
    /// Without `track` each file gets a new track named after it; with `sequence` they go one
    /// after another on one track.
    #[serde(rename = "media.import")]
    Import {
        /// Files on the machine running Scythe.
        paths: Vec<PathBuf>,
        /// The track to place them on (id or name); left out, new tracks.
        #[serde(default)]
        track: Option<TrackRef>,
        /// Project time of the first item, in seconds; 0 when left out.
        #[serde(default)]
        position: Option<f64>,
        /// All files on one track, each after the one before, `gap` seconds apart.
        #[serde(default)]
        sequence: bool,
        /// Seconds between items with `sequence` (default 0).
        #[serde(default)]
        gap: Option<f64>,
        /// The name of the new track, when one track is made for all of them.
        #[serde(default)]
        name: Option<String>,
    },
    /// What a file holds: format, sample rate, channels and length from its header, and its
    /// loudness (integrated LUFS), true peak and loudness range once decoded. Starts a job
    /// like the `analysis.*` commands (see `analysis.job`).
    #[serde(rename = "media.probe")]
    Probe {
        /// A file on the machine running Scythe.
        path: PathBuf,
    },
}

fn is_midi(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("mid") || e.eq_ignore_ascii_case("midi"))
}

fn absolute(path: &Path) -> Result<PathBuf, ApiError> {
    if !path.is_file() {
        return Err(ApiError::not_found(format!("no file {}", path.display())));
    }
    Ok(scythe_core::paths::canonical(path).unwrap_or_else(|_| path.to_owned()))
}

/// The length an item of `path` gets: from the header for PCM and FLAC, else from decoding it
/// into the engine's pool (which it needs to play anyway).
fn audio_length(session: &Session, path: &Path) -> Result<f64, ApiError> {
    let info = probe::probe(path).map_err(ApiError::failed)?;
    if info.exact
        && let Some(d) = info.duration
    {
        return Ok(d);
    }
    let media =
        session.engine.pool().load(path, session.engine.sample_rate()).map_err(|e| ApiError::failed(e.to_string()))?;
    Ok(media.duration())
}

/// A new track named `name`, at the end.
fn new_track(session: &mut Session, name: &str) -> TrackId {
    let mut id = TrackId(0);
    session.edit("Add track", |p| id = p.add_track(name));
    id
}

fn stem(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn import(
    session: &mut Session,
    paths: Vec<PathBuf>,
    track: Option<TrackRef>,
    position: f64,
    sequence: bool,
    gap: f64,
    name: Option<String>,
) -> Result<Value, ApiError> {
    if paths.is_empty() {
        return Err(ApiError::invalid("give at least one file in \"paths\""));
    }
    let files: Vec<PathBuf> = paths.iter().map(|p| absolute(p)).collect::<Result<_, _>>()?;
    let given = track.map(|t| t.track(&session.project)).transpose()?;
    // Work out every item before changing anything, so a file that does not open leaves the
    // project as it was.
    let mut planned: Vec<(PathBuf, Item, bool)> = Vec::new();
    let mut at = position;
    for path in &files {
        let start = if sequence { at } else { position };
        let item = if is_midi(path) {
            session.midi_item(path, start).map_err(|e| ApiError::failed(format!("{}: {e}", path.display())))?
        } else {
            Item::new(start, audio_length(session, path)?, Take::from_file(path.clone()))
        };
        at = start + item.length + gap;
        planned.push((path.clone(), item, is_midi(path)));
    }
    let shared = match (given, sequence) {
        (Some(t), _) => Some(t),
        (None, true) => Some(new_track(session, &name.clone().unwrap_or_else(|| stem(&files[0])))),
        (None, false) => None,
    };
    let mut placed = Vec::new();
    for (i, (path, item, midi)) in planned.into_iter().enumerate() {
        let track = match shared {
            Some(t) if sequence || i == 0 => t,
            _ => {
                let label = if i == 0 { name.clone().unwrap_or_else(|| stem(&path)) } else { stem(&path) };
                new_track(session, &label)
            }
        };
        let (id, start, length) = (item.id, item.position, item.length);
        session.edit(if midi { "Import MIDI" } else { "Import media" }, |p| {
            if let Some(t) = p.track_mut(track) {
                t.items.push(item);
            }
        });
        let track_name = session.project.track(track).map(|t| t.name.clone()).unwrap_or_default();
        placed.push(value!({
            "track": track.0, "track_name": track_name, "item": id.0, "path": path,
            "position": start, "length": length, "kind": if midi { "midi" } else { "audio" },
        }));
    }
    session.request_media();
    let end = placed
        .iter()
        .map(|p| p["position"].as_f64().unwrap_or(0.0) + p["length"].as_f64().unwrap_or(0.0))
        .fold(position, f64::max);
    Ok(value!({ "items": placed, "end": end }))
}

/// The header, and what decoding the file tells about its level.
fn probe_file(path: &Path) -> Result<Value, String> {
    if is_midi(path) {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let (source, _) = scythe_core::smf::read_smf(&bytes).map_err(|e| e.to_string())?;
        return Ok(value!({"kind": "midi", "format": "mid", "length_qn": source.length_qn()}));
    }
    let info: FileInfo = probe::probe(path)?;
    let data = scythe_engine::media::decode_file(path).map_err(|e| e.to_string())?;
    let a = scythe_engine::analysis::analyse(&data.channels, data.sample_rate, None, None);
    let mut answer = json(&info);
    answer["kind"] = "audio".into();
    answer["duration"] = data.duration().into();
    answer["integrated_lufs"] = a.integrated_lufs.into();
    answer["true_peak_dbtp"] = a.true_peak_dbtp.into();
    answer["loudness_range_lu"] = a.loudness_range_lu.into();
    answer["sample_peak_dbfs"] = a.sample_peak_dbfs.into();
    Ok(answer)
}

pub fn execute(session: &mut Session, command: MediaCommand) -> Result<Value, ApiError> {
    match command {
        MediaCommand::Import { paths, track, position, sequence, gap, name } => {
            let position = time(position.unwrap_or(0.0), "position")?;
            let gap = time(gap.unwrap_or(0.0), "gap")?;
            import(session, paths, track, position, sequence, gap, name)
        }
        MediaCommand::Probe { path } => {
            let path = absolute(&path)?;
            Ok(super::analysis::spawn_job("probe", None, Box::new(move || probe_file(&path).map(|v| (v, None)))))
        }
    }
}
