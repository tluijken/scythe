use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ApiError, json, snapshot};
use crate::Session;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd")]
pub enum ProjectCommand {
    /// The whole project: transport, tracks with their FX, sends and items, markers.
    #[serde(rename = "snapshot", alias = "project.snapshot")]
    Snapshot,
    #[serde(rename = "project.open")]
    Open { path: PathBuf },
    /// Saves to `path`, or to the project's own file.
    #[serde(rename = "project.save")]
    Save {
        #[serde(default)]
        path: Option<PathBuf>,
    },
    /// Starts an empty project; unsaved changes are dropped.
    #[serde(rename = "project.new")]
    New,
    #[serde(rename = "project.undo")]
    Undo,
    #[serde(rename = "project.redo")]
    Redo,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryState {
    pub revision: u64,
    pub dirty: bool,
    /// The step undone or redone.
    pub label: Option<String>,
    pub undo: Option<String>,
    pub redo: Option<String>,
}

fn history(session: &Session, label: Option<String>) -> Value {
    json(HistoryState {
        revision: session.revision(),
        dirty: session.dirty,
        label,
        undo: session.history.undo_label().map(Into::into),
        redo: session.history.redo_label().map(Into::into),
    })
}

pub fn execute(session: &mut Session, command: ProjectCommand) -> Result<Value, ApiError> {
    match command {
        ProjectCommand::Snapshot => Ok(json(snapshot::snapshot(session))),
        ProjectCommand::Open { path } => {
            session.stop();
            session.open_path(&path).map_err(ApiError::failed)?;
            Ok(json(snapshot::snapshot(session)))
        }
        ProjectCommand::Save { path } => {
            let path = path
                .or_else(|| session.project.path.clone())
                .ok_or_else(|| ApiError::invalid("the project has no file yet: give a \"path\""))?;
            session.save_to(&path).map_err(ApiError::failed)?;
            Ok(history(session, None))
        }
        ProjectCommand::New => {
            session.stop();
            session.new_project();
            Ok(json(snapshot::snapshot(session)))
        }
        ProjectCommand::Undo => {
            let label = session.history.undo_label().map(str::to_owned);
            if label.is_none() {
                return Err(ApiError::failed("nothing to undo"));
            }
            session.undo();
            Ok(history(session, label))
        }
        ProjectCommand::Redo => {
            let label = session.history.redo_label().map(str::to_owned);
            if label.is_none() {
                return Err(ApiError::failed("nothing to redo"));
            }
            session.redo();
            Ok(history(session, label))
        }
    }
}
