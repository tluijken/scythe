use serde::{Deserialize, Serialize};

use super::setlist::SetlistStatus;
use super::snapshot::TransportState;

/// What the session tells every client without being asked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// About 20 times a second while playing or recording, and whenever the state changes.
    Transport(TransportState),
    /// Peak levels (gain, decaying like the UI's meters), about 20 times a second.
    Meters(Meters),
    /// The project changed; `revision` counts changes, so a client whose snapshot has an older
    /// revision fetches a new one.
    ProjectChanged { revision: u64 },
    /// A message the UI would show as a toast (saved, could not open, recording started…).
    Notice { message: String },
    /// The setlist played, its current or next song, playing or stopped, or its options changed.
    Setlist(SetlistStatus),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Meters {
    /// Left and right peak of the master.
    pub master: (f32, f32),
    pub tracks: Vec<TrackMeter>,
    /// The monitor mixes' own levels (their tracks' meters), for a page showing one mix.
    #[serde(default)]
    pub mixes: Vec<TrackMeter>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackMeter {
    pub id: u64,
    pub peak: (f32, f32),
}
