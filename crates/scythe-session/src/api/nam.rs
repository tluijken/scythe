//! Trying Neural Amp Modeler models before keeping them: `nam.preview` plays a model in a NAM
//! slot (or with `rig.preview` in a rig's amp or cab), every further try folding into the same
//! undo step; `nam.keep` keeps what plays and `nam.revert` puts back what played before.

use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{ApiError, TrackRef};
use crate::Session;
use crate::rig::{Preview, PreviewBefore, PreviewSlot};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum NamCommand {
    /// Plays the model `model` (a `.nam` file) in the Neural Amp Modeler slot at `index`
    /// without making it final. The first try remembers what the slot played; later ones, in
    /// this slot, fold into the same undo step.
    #[serde(rename = "nam.preview")]
    Preview {
        track: TrackRef,
        /// Position of the NAM slot in the track's chain, from 0.
        index: usize,
        /// Path of the `.nam` file.
        model: String,
    },
    /// Ends the running preview (of a NAM slot or a rig) keeping what it plays.
    #[serde(rename = "nam.keep")]
    Keep,
    /// Ends the running preview (of a NAM slot or a rig), putting back what played before it.
    #[serde(rename = "nam.revert")]
    Revert,
    /// The running preview: where it plays, what, and what it would revert to; null when none.
    #[serde(rename = "nam.previewing")]
    Previewing,
}

/// A preview as the API shows it.
pub fn describe(p: &Preview) -> Value {
    let (kind, track, index) = match p.slot {
        PreviewSlot::Nam { track, index } => ("nam", track, index),
        PreviewSlot::Amp(at) => ("rig_amp", at.track, at.index),
        PreviewSlot::Cab(at) => ("rig_cab", at.track, at.index),
    };
    let before = match &p.before {
        PreviewBefore::Nam(model) => json!({ "model": model }),
        PreviewBefore::Rig(s) => json!({ "rig": s }),
    };
    json!({ "slot": kind, "track": track.0, "index": index, "path": p.path, "before": before })
}

pub fn execute(session: &mut Session, command: NamCommand) -> Result<Value, ApiError> {
    match command {
        NamCommand::Preview { track, index, model } => {
            let track = track.track(&session.project)?;
            session.preview_nam(track, index, Path::new(&model)).map(describe)
        }
        NamCommand::Keep => session.end_preview(true).map(|p| describe(&p)),
        NamCommand::Revert => session.end_preview(false).map(|p| describe(&p)),
        NamCommand::Previewing => Ok(session.preview.as_ref().map_or(Value::Null, describe)),
    }
}
