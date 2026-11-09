//! Who may do what over the control API. The admin token may do everything; a musician's link
//! only reads and changes that musician's own monitor mix; the stage link plays the setlist
//! without editing it. Every command from a client passes [`run`] on the session's thread, so
//! the rules hold for the web UI, scripts and anything else alike.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Session;
use crate::api::events::Meters;
use crate::api::mix::MixCommand;
use crate::api::project::ProjectCommand;
use crate::api::setlist::SetlistCommand;
use crate::api::transport::TransportCommand;
use crate::api::{self, ApiError, Command, Event, TrackRef, snapshot};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Scope {
    /// Everything, including opening and saving files.
    Admin,
    /// One musician's monitor mix ("MON Drummer" for `Drummer`), found by name so the link
    /// keeps working after the project is opened again.
    Musician { musician: String },
    /// The stage tablet: the setlist and play/stop, without editing.
    Stage,
}

impl Scope {
    pub fn is_admin(&self) -> bool {
        matches!(self, Self::Admin)
    }

    /// What a client with this scope hears of `event`: `None` when nothing, the event itself
    /// or a narrowed copy otherwise.
    pub fn event(&self, event: &Event) -> Option<Event> {
        match (self, event) {
            (Self::Admin, e) => Some(e.clone()),
            (Self::Musician { musician }, Event::Meters(m)) => Some(Event::Meters(Meters {
                master: (0.0, 0.0),
                tracks: Vec::new(),
                mixes: m
                    .mixes
                    .iter()
                    .filter(|t| t.musician.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(musician)))
                    .cloned()
                    .collect(),
            })),
            (Self::Musician { .. }, Event::ProjectChanged { .. }) => Some(event.clone()),
            (
                Self::Stage,
                Event::Transport(_) | Event::Setlist(_) | Event::Health(_) | Event::ProjectChanged { .. },
            ) => Some(event.clone()),
            _ => None,
        }
    }
}

fn denied(scope: &Scope) -> ApiError {
    match scope {
        Scope::Admin => ApiError::forbidden("not allowed"),
        Scope::Musician { musician } => {
            ApiError::forbidden(format!("this link only reaches the monitor mix of {musician}"))
        }
        Scope::Stage => ApiError::forbidden("the stage link plays the setlist; editing needs the admin token"),
    }
}

/// The track of `musician`'s mix.
fn own_mix(session: &Session, musician: &str) -> Result<scythe_core::TrackId, ApiError> {
    session
        .project
        .monitor_mixes()
        .into_iter()
        .find(|m| m.musician.eq_ignore_ascii_case(musician))
        .map(|m| m.track)
        .ok_or_else(|| ApiError::not_found(format!("this project has no monitor mix for {musician}")))
}

fn check_mix(session: &Session, scope: &Scope, musician: &str, mix: &TrackRef) -> Result<(), ApiError> {
    let own = own_mix(session, musician)?;
    match mix.track(&session.project) {
        Ok(id) if id == own => Ok(()),
        _ => Err(denied(scope)),
    }
}

/// The little a restricted client needs on connecting, in place of the snapshot.
fn lite_snapshot(session: &Session, scope: &Scope) -> Value {
    json!({
        "revision": session.revision(),
        "name": session.project.name(),
        "transport": snapshot::transport(session),
        "access": scope,
    })
}

/// Runs `command` for a client with `scope`: refused when the scope does not allow it, and
/// with answers narrowed to what the scope may see.
pub fn run(session: &mut Session, scope: &Scope, command: Command) -> Result<Value, ApiError> {
    let musician = match scope {
        Scope::Admin => return api::execute(session, command),
        Scope::Musician { musician } => Some(musician.as_str()),
        Scope::Stage => None,
    };
    if matches!(command, Command::Project(ProjectCommand::Snapshot)) {
        return Ok(lite_snapshot(session, scope));
    }
    if let Some(musician) = musician {
        let allowed = match &command {
            Command::Mix(MixCommand::List) => {
                let own = own_mix(session, musician).ok();
                let mixes = api::mix::list(&session.project).into_iter().filter(|m| Some(m.id) == own.map(|t| t.0));
                return Ok(api::json(mixes.collect::<Vec<_>>()));
            }
            Command::Mix(
                MixCommand::Get { mix }
                | MixCommand::Level { mix, .. }
                | MixCommand::Pan { mix, .. }
                | MixCommand::Mute { mix, .. }
                | MixCommand::MoreMe { mix, .. }
                | MixCommand::Volume { mix, .. }
                | MixCommand::Click { mix, .. },
            ) => {
                check_mix(session, scope, musician, mix)?;
                true
            }
            Command::Transport(TransportCommand::State) | Command::Setlist(SetlistCommand::Status) => true,
            _ => false,
        };
        if !allowed {
            return Err(denied(scope));
        }
        return api::execute(session, command);
    }
    let allowed = matches!(
        &command,
        Command::Transport(
            TransportCommand::Play | TransportCommand::Stop | TransportCommand::State | TransportCommand::Health
        ) | Command::Setlist(
            SetlistCommand::Songs
                | SetlistCommand::List
                | SetlistCommand::Get { .. }
                | SetlistCommand::Export { .. }
                | SetlistCommand::Select { .. }
                | SetlistCommand::Cue { .. }
                | SetlistCommand::Play { .. }
                | SetlistCommand::Stop
                | SetlistCommand::Next
                | SetlistCommand::Previous
                | SetlistCommand::AutoAdvance { .. }
                | SetlistCommand::CountIn { .. }
                | SetlistCommand::Status
        )
    );
    if !allowed {
        return Err(denied(scope));
    }
    api::execute(session, command)
}
