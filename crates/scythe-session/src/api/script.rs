//! Running ReaScript text (Lua or EEL) in the session. The interpreters live in the desktop
//! app, which hands the session a [`ScriptRunner`]; a session without one (the headless
//! server) refuses. A script's changes to the project are one undo step.

use schemars::JsonSchema;
use scythe_core::Project;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json as value};

use super::ApiError;
use crate::Session;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    Lua,
    Eel,
}

/// What a script did.
#[derive(Debug, Default)]
pub struct ScriptRun {
    /// The project as the script left it, when it changed it.
    pub project: Option<Project>,
    /// What it printed (`ShowConsoleMsg`).
    pub console: String,
    /// The undo step's name it gave (`Undo_EndBlock`).
    pub undo: Option<String>,
    /// Where it left the edit cursor, when it moved it.
    pub cursor: Option<f64>,
}

/// Runs `code` in `language` over the session's project; `Err` carries the interpreter's
/// message. A script that keeps running (`defer`) is stopped after its first pass.
pub type ScriptRunner = Box<dyn Fn(&Session, Language, &str) -> Result<ScriptRun, String> + Send>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum ScriptCommand {
    /// Runs ReaScript code (REAPER's `reaper.*` API, the subset Scythe implements) and answers
    /// with what it printed. Its changes to the project are one undo step. It can change
    /// anything in the project, so it needs the admin token; it runs in the desktop app only.
    #[serde(rename = "script.run")]
    Run {
        /// The script's text.
        code: String,
        /// `lua` (default) or `eel`.
        #[serde(default)]
        language: Language,
    },
}

pub fn execute(session: &mut Session, command: ScriptCommand) -> Result<Value, ApiError> {
    let ScriptCommand::Run { code, language } = command;
    let Some(runner) = session.script_runner.take() else {
        return Err(ApiError::failed(
            "this Scythe runs no scripts: ReaScript runs in the desktop app (scythe --control), not the headless server",
        ));
    };
    let outcome = runner(session, language, &code);
    session.script_runner = Some(runner);
    let run = outcome.map_err(ApiError::failed)?;
    let changed = run.project.is_some();
    if let Some(project) = run.project {
        let label = run.undo.clone().unwrap_or_else(|| "Run script".into());
        session.edit(&label, |p| *p = project);
    }
    if let Some(t) = run.cursor {
        session.project.cursor = t.max(0.0);
        if !session.is_playing() {
            session.engine.seek(session.project.cursor);
        }
    }
    Ok(value!({"console": run.console, "changed": changed, "undo": run.undo}))
}
