//! The project bay: the media files, items and effects the project uses, retargeting a file,
//! and cleaning the project's recording folder of files it does not use (REAPER's File: Clean
//! current project directory).

use std::path::PathBuf;

use schemars::JsonSchema;
use scythe_core::bay;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json as value};

use super::ApiError;
use crate::Session;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum BayCommand {
    /// The media files the project uses: as the project names them, where they are, whether
    /// they exist, and the takes, items and tracks using each.
    #[serde(rename = "bay.media")]
    Media,
    /// Every item: track, position, length, active take and its file.
    #[serde(rename = "bay.items")]
    Items,
    /// The effects the project uses, with how many instances and on which tracks.
    #[serde(rename = "bay.fx")]
    Fx,
    /// Points every take playing `from` at the file `to` (replace or relink media), as one
    /// undo step.
    #[serde(rename = "bay.retarget")]
    Retarget { from: PathBuf, to: PathBuf },
    /// The media and peaks files in the project's recording folder (not its subfolders) the
    /// project does not use, as REAPER's "Clean current project directory" lists them.
    #[serde(rename = "bay.unused")]
    Unused,
    /// Removes unused files (all of `bay.unused` when `files` is left out) to the trash, or
    /// deletes them for good with `trash: false`.
    #[serde(rename = "bay.clean")]
    Clean {
        #[serde(default)]
        files: Option<Vec<PathBuf>>,
        #[serde(default = "yes")]
        trash: bool,
    },
}

fn yes() -> bool {
    true
}

fn track_name(session: &Session, id: scythe_core::TrackId) -> String {
    session.project.track(id).map(|t| t.name.clone()).unwrap_or_default()
}

pub fn execute(session: &mut Session, command: BayCommand) -> Result<Value, ApiError> {
    match command {
        BayCommand::Media => {
            let list: Vec<Value> = bay::sources(&session.project)
                .into_iter()
                .map(|s| {
                    value!({
                        "file": s.file, "path": s.path, "exists": s.exists, "takes": s.takes,
                        "items": s.items.iter().map(|i| i.0).collect::<Vec<_>>(),
                        "tracks": s.tracks.iter().map(|t| value!({"id": t.0, "name": track_name(session, *t)})).collect::<Vec<_>>(),
                    })
                })
                .collect();
            Ok(value!({ "media": list }))
        }
        BayCommand::Items => {
            let mut items = Vec::new();
            for t in &session.project.tracks {
                for it in &t.items {
                    let take = it.takes.get(it.active_take);
                    items.push(value!({
                        "item": it.id.0, "track": t.id.0, "track_name": t.name, "position": it.position,
                        "length": it.length, "take": take.map(|t| t.name.clone()),
                        "file": take.and_then(bay::take_file),
                    }));
                }
            }
            Ok(value!({ "items": items }))
        }
        BayCommand::Fx => {
            let fx: Vec<Value> = bay::effects(&session.project)
                .into_iter()
                .map(|f| value!({ "name": f.name, "instances": f.instances, "tracks": f.tracks }))
                .collect();
            Ok(value!({ "fx": fx }))
        }
        BayCommand::Retarget { from, to } => {
            let takes = session.retarget_media(&from, &to).map_err(ApiError::failed)?;
            Ok(value!({ "takes": takes }))
        }
        BayCommand::Unused => {
            let u = session.unused_project_files().map_err(ApiError::failed)?;
            Ok(value!({ "folder": u.dir, "total": u.total, "unused": u.files }))
        }
        BayCommand::Clean { files, trash } => {
            let files = match files {
                Some(f) => f,
                None => session.unused_project_files().map_err(ApiError::failed)?.files,
            };
            let removed = session.clean_project_files(&files, trash).map_err(ApiError::failed)?;
            Ok(value!({ "removed": removed, "trash": trash }))
        }
    }
}
