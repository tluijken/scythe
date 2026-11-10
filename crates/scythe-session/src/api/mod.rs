//! The control API: the one way in for remote clients (web UI, MCP, tablets). A [`Command`]
//! is JSON with a `cmd` field naming `domain.action`; [`execute`] runs it on the session
//! through the same undoable edits the UI uses and answers with JSON. Each domain lives in its
//! own module with its own command enum; adding a domain means a module, a [`Command`]
//! variant and a line in each of [`Command`]'s two `match`es.

pub mod agents;
pub mod analysis;
pub mod audio;
pub mod bay;
pub mod customize;
pub mod editing;
pub mod envelope;
pub mod eq;
pub mod events;
pub mod explorer;
pub mod feed;
pub mod fx;
pub mod groups;
pub mod items;
pub mod markers;
pub mod media;
pub mod midi;
pub mod midi_edit;
pub mod mix;
pub mod modulation;
pub mod nam;
pub mod project;
pub mod recording;
pub mod render;
pub mod rig;
pub mod script;
pub mod sends;
pub mod setlist;
pub mod snapshot;
pub mod surface;
pub mod tempo;
pub mod theme;
pub mod timing;
pub mod tracks;
pub mod transport;

use schemars::JsonSchema;
use scythe_core::units::db_to_gain;
use scythe_core::{Project, Track, TrackId};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::Session;

pub use events::Event;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Command {
    Transport(transport::TransportCommand),
    Project(project::ProjectCommand),
    Track(tracks::TrackCommand),
    Send(sends::SendCommand),
    Fx(fx::FxCommand),
    Group(groups::GroupCommand),
    Item(items::ItemCommand),
    Marker(markers::MarkerCommand),
    Mix(mix::MixCommand),
    Setlist(setlist::SetlistCommand),
    Tempo(tempo::TempoCommand),
    Surface(surface::SurfaceCommand),
    Midi(midi::MidiCommand),
    MidiEdit(midi_edit::MidiEditCommand),
    Timing(timing::TimingCommand),
    Modulation(modulation::ModulationCommand),
    Render(render::RenderCommand),
    Recording(recording::RecordingCommand),
    Customize(customize::CustomizeCommand),
    Envelope(envelope::EnvelopeCommand),
    Media(media::MediaCommand),
    Explorer(explorer::ExplorerCommand),
    Bay(bay::BayCommand),
    Theme(theme::ThemeCommand),
    Analysis(analysis::AnalysisCommand),
    Events(feed::EventsCommand),
    Script(script::ScriptCommand),
    Editing(editing::EditingCommand),
    Rig(rig::RigCommand),
    Nam(nam::NamCommand),
    Audio(audio::AudioCommand),
    Agents(agents::AgentsCommand),
    Eq(eq::EqCommand),
}

/// Commands of other domains that `timing` answers, besides `item.stretch_markers*`.
const TIMING: &[&str] = &["item.timebase", "take.channel_mode", "project.timebase", "project.playrate"];

/// Commands of the `midi` domain about devices; `midi_edit` answers the rest.
const MIDI_DEVICES: &[&str] = &["midi.devices", "midi.rescan", "midi.device", "midi.keyboard"];

/// Commands of the `media` domain that `media` answers; the media explorer answers the rest.
const MEDIA_IMPORT: &[&str] = &["media.import", "media.probe"];

/// Commands of the `track` domain that `recording` answers.
const RECORDING: &[&str] = &["track.record_mode", "track.monitor_mode"];

impl<'de> Deserialize<'de> for Command {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let Some(cmd) = value.get("cmd").and_then(Value::as_str) else {
            return Err(D::Error::custom("a command needs a \"cmd\" field naming it, such as \"transport.play\""));
        };
        if cmd.starts_with("item.stretch_markers") || TIMING.contains(&cmd) {
            return serde_json::from_value(value).map(Command::Timing).map_err(D::Error::custom);
        }
        if editing::COMMANDS.contains(&cmd)
            || cmd.starts_with("edit.")
            || (cmd.starts_with("take.") && !TIMING.contains(&cmd))
        {
            return serde_json::from_value(value).map(Command::Editing).map_err(D::Error::custom);
        }
        if RECORDING.contains(&cmd) {
            return serde_json::from_value(value).map(Command::Recording).map_err(D::Error::custom);
        }
        let domain = cmd.split_once('.').map_or(cmd, |(d, _)| d).to_owned();
        let parsed = match domain.as_str() {
            "transport" => serde_json::from_value(value).map(Command::Transport),
            "project" | "snapshot" => serde_json::from_value(value).map(Command::Project),
            "track" => serde_json::from_value(value).map(Command::Track),
            "send" => serde_json::from_value(value).map(Command::Send),
            "fx" if cmd.starts_with("fx.modulation.") || cmd.starts_with("fx.learn.") => {
                serde_json::from_value(value).map(Command::Modulation)
            }
            "fx" => serde_json::from_value(value).map(Command::Fx),
            "action" if cmd.starts_with("action.learn.") => serde_json::from_value(value).map(Command::Modulation),
            "action" | "toolbar" | "mouse" => serde_json::from_value(value).map(Command::Customize),
            "group" => serde_json::from_value(value).map(Command::Group),
            "item" => serde_json::from_value(value).map(Command::Item),
            "marker" => serde_json::from_value(value).map(Command::Marker),
            "mix" => serde_json::from_value(value).map(Command::Mix),
            "setlist" => serde_json::from_value(value).map(Command::Setlist),
            "tempo" => serde_json::from_value(value).map(Command::Tempo),
            "envelope" => serde_json::from_value(value).map(Command::Envelope),
            "surface" => serde_json::from_value(value).map(Command::Surface),
            "midi" if !MIDI_DEVICES.contains(&cmd) => serde_json::from_value(value).map(Command::MidiEdit),
            "midi" | "sync" => serde_json::from_value(value).map(Command::Midi),
            "render" => serde_json::from_value(value).map(Command::Render),
            "record" | "lanes" => serde_json::from_value(value).map(Command::Recording),
            "media" if MEDIA_IMPORT.contains(&cmd) => serde_json::from_value(value).map(Command::Media),
            "media" => serde_json::from_value(value).map(Command::Explorer),
            "bay" => serde_json::from_value(value).map(Command::Bay),
            "theme" => serde_json::from_value(value).map(Command::Theme),
            "analysis" => serde_json::from_value(value).map(Command::Analysis),
            "events" => serde_json::from_value(value).map(Command::Events),
            "script" => serde_json::from_value(value).map(Command::Script),
            "rig" => serde_json::from_value(value).map(Command::Rig),
            "nam" => serde_json::from_value(value).map(Command::Nam),
            "audio" => serde_json::from_value(value).map(Command::Audio),
            "agents" => serde_json::from_value(value).map(Command::Agents),
            "eq" => serde_json::from_value(value).map(Command::Eq),
            _ => return Err(D::Error::custom(format!("unknown command domain \"{domain}\""))),
        };
        parsed.map_err(D::Error::custom)
    }
}

/// Runs `command` on the session and returns its answer.
pub fn execute(session: &mut Session, command: Command) -> Result<Value, ApiError> {
    match command {
        Command::Transport(c) => transport::execute(session, c),
        Command::Project(c) => project::execute(session, c),
        Command::Track(c) => tracks::execute(session, c),
        Command::Send(c) => sends::execute(session, c),
        Command::Fx(c) => fx::execute(session, c),
        Command::Group(c) => groups::execute(session, c),
        Command::Item(c) => items::execute(session, c),
        Command::Marker(c) => markers::execute(session, c),
        Command::Mix(c) => mix::execute(session, c),
        Command::Setlist(c) => setlist::execute(session, c),
        Command::Tempo(c) => tempo::execute(session, c),
        Command::Surface(c) => surface::execute(session, c),
        Command::Midi(c) => midi::execute(session, c),
        Command::MidiEdit(c) => midi_edit::execute(session, c),
        Command::Timing(c) => timing::execute(session, c),
        Command::Modulation(c) => modulation::execute(session, c),
        Command::Render(c) => render::execute(session, c),
        Command::Recording(c) => recording::execute(session, c),
        Command::Customize(c) => customize::execute(session, c),
        Command::Envelope(c) => envelope::execute(session, c),
        Command::Media(c) => media::execute(session, c),
        Command::Explorer(c) => explorer::execute(session, c),
        Command::Bay(c) => bay::execute(session, c),
        Command::Theme(c) => theme::execute(session, c),
        Command::Analysis(c) => analysis::execute(session, c),
        Command::Events(c) => feed::execute(session, c),
        Command::Script(c) => script::execute(session, c),
        Command::Editing(c) => editing::execute(session, c),
        Command::Rig(c) => rig::execute(session, c),
        Command::Nam(c) => nam::execute(session, c),
        Command::Audio(c) => audio::execute(session, c),
        Command::Agents(c) => agents::execute(session, c),
        Command::Eq(c) => eq::execute(session, c),
    }
}

/// Parses and runs one command given as JSON.
pub fn execute_json(session: &mut Session, json: Value) -> Result<Value, ApiError> {
    let command: Command = serde_json::from_value(json).map_err(|e| ApiError::invalid(e.to_string()))?;
    execute(session, command)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// The command is malformed or its values are out of range.
    Invalid,
    /// A track, item, send, effect or marker it names does not exist.
    NotFound,
    /// It was understood but could not be done (a file did not open, nothing to undo).
    Failed,
    /// The token in use may not run this command (a musician's or the stage's link).
    Forbidden,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiError {
    pub kind: ErrorKind,
    pub message: String,
}

impl ApiError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Invalid, message: message.into() }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self { kind: ErrorKind::NotFound, message: message.into() }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Failed, message: message.into() }
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Forbidden, message: message.into() }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

/// A track by id (a number from a snapshot), by exact name, or `"master"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum TrackRef {
    Id(u64),
    Name(String),
}

/// What a [`TrackRef`] names in the current project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Master,
    Track(TrackId),
}

impl TrackRef {
    pub fn resolve(&self, project: &Project) -> Result<Target, ApiError> {
        match self {
            Self::Name(n) if n.eq_ignore_ascii_case("master") => Ok(Target::Master),
            Self::Id(id) => project.track(TrackId(*id)).map(|t| Target::Track(t.id)).ok_or_else(|| missing_track(self)),
            Self::Name(name) => project
                .tracks
                .iter()
                .find(|t| &t.name == name)
                .map(|t| Target::Track(t.id))
                .ok_or_else(|| missing_track(self)),
        }
    }

    /// The track this names, refusing the master.
    pub fn track(&self, project: &Project) -> Result<TrackId, ApiError> {
        match self.resolve(project)? {
            Target::Track(id) => Ok(id),
            Target::Master => Err(ApiError::invalid("this does not apply to the master track")),
        }
    }
}

fn missing_track(r: &TrackRef) -> ApiError {
    match r {
        TrackRef::Id(id) => ApiError::not_found(format!("no track with id {id}")),
        TrackRef::Name(n) => ApiError::not_found(format!("no track named \"{n}\"")),
    }
}

impl Target {
    pub fn get<'a>(&self, project: &'a Project) -> Option<&'a Track> {
        match self {
            Self::Master => Some(&project.master),
            Self::Track(id) => project.track(*id),
        }
    }

    pub fn get_mut<'a>(&self, project: &'a mut Project) -> Option<&'a mut Track> {
        match self {
            Self::Master => Some(&mut project.master),
            Self::Track(id) => project.track_mut(*id),
        }
    }

    /// A short key for folding edits of one control into one undo step.
    fn key(&self) -> String {
        match self {
            Self::Master => "master".into(),
            Self::Track(id) => id.0.to_string(),
        }
    }
}

/// A level given as gain (`volume`, 1.0 = 0 dB) or in dB (`db`).
pub(crate) fn level(volume: Option<f64>, db: Option<f64>) -> Result<f64, ApiError> {
    let gain = match (volume, db) {
        (Some(v), None) => v,
        (None, Some(db)) => db_to_gain(db),
        (None, None) => return Err(ApiError::invalid("give \"volume\" (gain) or \"db\"")),
        (Some(_), Some(_)) => return Err(ApiError::invalid("give either \"volume\" or \"db\", not both")),
    };
    if !gain.is_finite() || gain < 0.0 {
        return Err(ApiError::invalid("volume must be a gain of 0 or more"));
    }
    Ok(gain.min(db_to_gain(24.0)))
}

pub(crate) fn pan(value: f64) -> Result<f64, ApiError> {
    if !(-1.0..=1.0).contains(&value) {
        return Err(ApiError::invalid("pan runs from -1 (left) to 1 (right)"));
    }
    Ok(value)
}

pub(crate) fn time(value: f64, what: &str) -> Result<f64, ApiError> {
    if !value.is_finite() || value < 0.0 {
        return Err(ApiError::invalid(format!("{what} must be a time of 0 seconds or more")));
    }
    Ok(value)
}

/// `{"ok": true}`-style answers for commands with nothing else to say.
pub(crate) fn done() -> Value {
    Value::Object(Default::default())
}

pub(crate) fn json(value: impl Serialize) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}
