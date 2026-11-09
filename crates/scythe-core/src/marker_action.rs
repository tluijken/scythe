//! Marker actions: a marker whose name starts with `!` runs the actions listed after it when
//! the play cursor crosses it, e.g. `!1016` stops playback there and `!1016 40042` stops and
//! returns to the project start. Command ids are REAPER's; a token that is not a number names
//! a custom or extension action (`!_SWS_SAVESEL`).
//!
//! In REAPER this is the SWS extension's "marker actions" feature (on by default, a global
//! option rather than a project setting).

use crate::model::Marker;

pub const PLAY: u32 = 1007;
pub const PAUSE: u32 = 1008;
pub const RECORD: u32 = 1013;
pub const STOP: u32 = 1016;
pub const TOGGLE_REPEAT: u32 = 1068;
pub const GO_TO_START: u32 = 40042;
pub const GO_TO_END: u32 = 40043;
pub const PLAY_STOP: u32 = 40044;
pub const PLAY_PAUSE: u32 = 40073;
pub const PLAY_STOP_MOVE_CURSOR: u32 = 40328;
pub const GO_TO_LOOP_START: u32 = 40632;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Command {
    /// A numeric command id from REAPER's main action section.
    Id(u32),
    /// A named command (`_SWS_…`, `_RS…`): custom actions, scripts and extensions.
    Named(String),
}

impl std::fmt::Display for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Command::Id(id) => write!(f, "{id}"),
            Command::Named(name) => f.write_str(name),
        }
    }
}

/// What a command does to the transport while it plays. Everything else is left to the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportAction {
    /// Already playing: nothing to do.
    Play,
    /// Stop; the play cursor returns to the edit cursor.
    Stop,
    /// Stop and leave the edit cursor where playback stopped.
    Pause,
    GoToStart,
    GoToEnd,
    GoToLoopStart,
    ToggleRepeat,
}

impl TransportAction {
    pub fn of(command: &Command) -> Option<Self> {
        let Command::Id(id) = command else { return None };
        Some(match *id {
            PLAY => Self::Play,
            STOP | PLAY_STOP => Self::Stop,
            PAUSE | PLAY_PAUSE | PLAY_STOP_MOVE_CURSOR => Self::Pause,
            GO_TO_START => Self::GoToStart,
            GO_TO_END => Self::GoToEnd,
            GO_TO_LOOP_START => Self::GoToLoopStart,
            TOGGLE_REPEAT => Self::ToggleRepeat,
            _ => return None,
        })
    }

    pub fn stops(self) -> bool {
        matches!(self, Self::Stop | Self::Pause)
    }
}

/// The commands of a marker action name, or `None` when the name does not start with `!`.
/// Tokens are separated by whitespace and may be quoted; a token that parses as a non-zero
/// number is a command id, anything else a named command.
pub fn commands(name: &str) -> Option<Vec<Command>> {
    let rest = name.strip_prefix('!')?;
    Some(
        crate::rpp::tokenize(rest)
            .into_iter()
            .filter(|t| !t.is_empty())
            .map(|t| match t.parse::<u32>() {
                Ok(id) if id != 0 => Command::Id(id),
                _ => Command::Named(t),
            })
            .collect(),
    )
}

pub fn is_action_marker(name: &str) -> bool {
    name.starts_with('!')
}

/// Whether crossing a marker with this name ends playback.
pub fn stops_playback(name: &str) -> bool {
    commands(name).is_some_and(|c| c.iter().any(|c| TransportAction::of(c).is_some_and(TransportAction::stops)))
}

impl Marker {
    /// The marker's `{GUID}` as REAPER stores it, when the project has one.
    pub fn guid(&self) -> Option<&str> {
        let toks = self.lines.get("MARKER")?;
        toks.iter().skip(6).find(|t| t.starts_with('{') && t.ends_with('}')).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ids_and_named_commands() {
        assert_eq!(commands("Intro"), None);
        assert_eq!(commands("!1016"), Some(vec![Command::Id(STOP)]));
        assert_eq!(commands("!1016 40044"), Some(vec![Command::Id(1016), Command::Id(40044)]));
        assert_eq!(
            commands("! _SWS_SAVESEL  40042"),
            Some(vec![Command::Named("_SWS_SAVESEL".into()), Command::Id(40042)])
        );
        assert_eq!(commands("!"), Some(vec![]));
        assert_eq!(commands("!0"), Some(vec![Command::Named("0".into())]));
    }

    #[test]
    fn stop_markers() {
        assert!(stops_playback("!1016"));
        assert!(stops_playback("!40042 40044"));
        assert!(stops_playback("!1008"));
        assert!(!stops_playback("!40042"));
        assert!(!stops_playback("1016"));
        assert!(!stops_playback("!_SWS_STOP"));
    }
}
