//! Setlists: the songs found in the project's markers, the setlists made from them, and
//! playing through one (see `scythe_core::setlist`).

use schemars::JsonSchema;
use scythe_core::setlist::{Setlist, Song, SongEnd, SongRef};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ApiError, json};
use crate::Session;

/// A setlist by its index (from `setlist.list`) or exact name. Left out, the setlist being
/// played.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum SetlistRef {
    Index(usize),
    Name(String),
}

/// A song by its index in `setlist.songs`, or by its start marker's `{GUID}` or name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum SongSel {
    Index(usize),
    Text(String),
}

/// `setlist` names a setlist by its index from `setlist.list` or its exact name; left out, the
/// setlist being played. Songs are an index from `setlist.songs`, or the start marker's
/// `{GUID}` or name. `entry` is a position in the setlist, from 0.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum SetlistCommand {
    /// The songs derived from the markers.
    #[serde(rename = "setlist.songs")]
    Songs,
    /// Every setlist, which one is played, and the options.
    #[serde(rename = "setlist.list")]
    List,
    /// One setlist with its entries and durations.
    #[serde(rename = "setlist.get")]
    Get {
        #[serde(default)]
        setlist: Option<SetlistRef>,
    },
    /// Adds a setlist (with `songs` when given) and plays from it.
    #[serde(rename = "setlist.create")]
    Create {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        songs: Vec<SongSel>,
    },
    /// Renames the setlist (a name already used gets a number).
    #[serde(rename = "setlist.rename")]
    Rename {
        #[serde(default)]
        setlist: Option<SetlistRef>,
        name: String,
    },
    /// Deletes the setlist.
    #[serde(rename = "setlist.delete")]
    Delete {
        #[serde(default)]
        setlist: Option<SetlistRef>,
    },
    /// Copies the setlist, placing the copy after it.
    #[serde(rename = "setlist.duplicate")]
    Duplicate {
        #[serde(default)]
        setlist: Option<SetlistRef>,
        #[serde(default)]
        name: Option<String>,
    },
    /// Replaces the setlist's songs.
    #[serde(rename = "setlist.set")]
    Set {
        #[serde(default)]
        setlist: Option<SetlistRef>,
        songs: Vec<SongSel>,
    },
    /// Moves entry `from` to position `to`.
    #[serde(rename = "setlist.move")]
    Move {
        #[serde(default)]
        setlist: Option<SetlistRef>,
        /// Entry position, from 0.
        from: usize,
        /// Entry position, from 0.
        to: usize,
    },
    /// Adds a setlist from text: a title line, then one song per line (numbering optional).
    #[serde(rename = "setlist.import")]
    Import { text: String },
    /// The setlist as text, in the form `setlist.import` takes.
    #[serde(rename = "setlist.export")]
    Export {
        #[serde(default)]
        setlist: Option<SetlistRef>,
    },
    /// Makes a setlist the one played, from its first song.
    #[serde(rename = "setlist.select")]
    Select { setlist: SetlistRef },
    /// Makes `entry` play next; when stopped the edit cursor moves to it.
    #[serde(rename = "setlist.cue")]
    Cue {
        /// Position in the setlist, from 0.
        entry: usize,
    },
    /// Plays `entry` (the current one when left out) from its marker.
    #[serde(rename = "setlist.play")]
    Play {
        /// Position in the setlist, from 0.
        #[serde(default)]
        entry: Option<usize>,
    },
    /// Stops playback.
    #[serde(rename = "setlist.stop")]
    Stop,
    /// The next song: cued when stopped, played when playing.
    #[serde(rename = "setlist.next")]
    Next,
    /// The previous song: cued when stopped, played when playing.
    #[serde(rename = "setlist.previous")]
    Previous,
    /// Whether the next song is cued after a song stops at its stop marker; toggles when
    /// `enabled` is left out.
    #[serde(rename = "setlist.auto_advance")]
    AutoAdvance {
        #[serde(default)]
        enabled: Option<bool>,
    },
    /// Whether crossing a `!` marker runs its actions (`!1016` stops).
    #[serde(rename = "setlist.marker_actions")]
    MarkerActions {
        #[serde(default)]
        enabled: Option<bool>,
    },
    /// The setlist played, its current and next song, elapsed and remaining time.
    #[serde(rename = "setlist.status")]
    Status,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SongInfo {
    pub index: usize,
    /// The start marker's number.
    pub number: u32,
    pub guid: Option<String>,
    pub name: String,
    pub start: f64,
    pub end: f64,
    pub duration: f64,
    /// `stop_marker`, `next_song` or `project_end`.
    pub ends_at: String,
    /// Markers inside the song (cues), by position.
    pub sections: Vec<SectionInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SectionInfo {
    pub name: String,
    pub position: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetlistInfo {
    pub index: usize,
    pub name: String,
    pub entries: Vec<EntryInfo>,
    /// Total length of the songs found, in seconds.
    pub duration: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntryInfo {
    pub name: String,
    pub guid: Option<String>,
    /// Index in `setlist.songs`, `None` when the project has no such song.
    pub song: Option<usize>,
    pub duration: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetlistsInfo {
    pub setlists: Vec<SetlistInfo>,
    /// The setlist being played.
    pub current: Option<usize>,
    pub auto_advance: bool,
    pub marker_actions: bool,
}

/// Where the setlist is: also sent as the `setlist` event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetlistStatus {
    pub setlist: Option<usize>,
    pub name: Option<String>,
    pub playing: bool,
    /// The entry playing, or played next when stopped.
    pub current: Option<EntryStatus>,
    pub next: Option<EntryStatus>,
    /// Seconds into the current song while it plays.
    pub elapsed: Option<f64>,
    pub remaining: Option<f64>,
    pub auto_advance: bool,
    pub marker_actions: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntryStatus {
    pub entry: usize,
    pub name: String,
    pub start: f64,
    pub duration: f64,
}

pub fn song_info(index: usize, s: &Song) -> SongInfo {
    SongInfo {
        index,
        number: s.number,
        guid: s.guid.clone(),
        name: s.name.clone(),
        start: s.start,
        end: s.end,
        duration: s.duration(),
        ends_at: match s.end_kind {
            SongEnd::StopMarker => "stop_marker",
            SongEnd::NextSong => "next_song",
            SongEnd::ProjectEnd => "project_end",
        }
        .into(),
        sections: s
            .sections
            .iter()
            .map(|(name, position)| SectionInfo { name: name.clone(), position: *position })
            .collect(),
    }
}

fn setlist_info(index: usize, list: &Setlist, songs: &[Song]) -> SetlistInfo {
    let entries = list
        .songs
        .iter()
        .zip(list.resolve(songs))
        .map(|(r, s)| EntryInfo {
            name: r.name.clone(),
            guid: r.guid.clone(),
            song: s,
            duration: s.map(|s| songs[s].duration()),
        })
        .collect();
    SetlistInfo { index, name: list.name.clone(), entries, duration: list.duration(songs) }
}

pub fn status(session: &Session) -> SetlistStatus {
    let playing = session.is_playing();
    let list = session.current_setlist();
    let songs = session.songs();
    let marker_actions = session.engine.marker_actions();
    let auto_advance = session.setlists().auto_advance;
    let Some(list) = list else {
        let none = SetlistStatus {
            setlist: None,
            name: None,
            playing,
            current: None,
            next: None,
            elapsed: None,
            remaining: None,
            auto_advance,
            marker_actions,
        };
        return none;
    };
    let resolved = list.resolve(&songs);
    let st = session.live.player.status(&list, &songs, session.position(), playing);
    let entry = |e: Option<usize>| {
        let e = e?;
        let song = &songs[resolved.get(e).copied().flatten()?];
        Some(EntryStatus { entry: e, name: song.name.clone(), start: song.start, duration: song.duration() })
    };
    let current = entry(st.current);
    let remaining = st.elapsed.zip(current.as_ref()).map(|(e, c)| (c.duration - e).max(0.0));
    SetlistStatus {
        setlist: Some(session.live.list),
        name: Some(list.name),
        playing,
        current,
        next: entry(st.next),
        elapsed: st.elapsed,
        remaining,
        auto_advance,
        marker_actions,
    }
}

fn find_setlist(session: &Session, r: Option<&SetlistRef>) -> Result<usize, ApiError> {
    let lists = session.setlists().lists;
    let found = match r {
        None => (session.live.list < lists.len()).then_some(session.live.list),
        Some(SetlistRef::Index(i)) => (*i < lists.len()).then_some(*i),
        Some(SetlistRef::Name(n)) => lists.iter().position(|l| &l.name == n),
    };
    found.ok_or_else(|| match r {
        None => ApiError::not_found("there is no setlist yet"),
        Some(SetlistRef::Index(i)) => ApiError::not_found(format!("no setlist {i}")),
        Some(SetlistRef::Name(n)) => ApiError::not_found(format!("no setlist named \"{n}\"")),
    })
}

fn song_refs(songs: &[Song], sel: &[SongSel]) -> Result<Vec<SongRef>, ApiError> {
    sel.iter()
        .map(|s| {
            let found = match s {
                SongSel::Index(i) => songs.get(*i),
                SongSel::Text(t) => songs.iter().find(|s| s.guid.as_deref() == Some(t.as_str())).or_else(|| {
                    let t = t.trim().to_lowercase();
                    songs.iter().find(|s| s.name.trim().to_lowercase() == t)
                }),
            };
            found.map(SongRef::to).ok_or_else(|| match s {
                SongSel::Index(i) => ApiError::not_found(format!("no song {i}")),
                SongSel::Text(t) => ApiError::not_found(format!("no song \"{t}\"")),
            })
        })
        .collect()
}

fn get(session: &Session, index: usize) -> Value {
    let lists = session.setlists().lists;
    json(setlist_info(index, &lists[index], &session.songs()))
}

fn entry_of(session: &Session, entry: usize) -> Result<(), ApiError> {
    let list = session.current_setlist().ok_or_else(|| ApiError::not_found("there is no setlist yet"))?;
    match list.resolve(&session.songs()).get(entry) {
        None => Err(ApiError::not_found(format!("the setlist has no entry {entry}"))),
        Some(None) => Err(ApiError::failed(format!("entry {entry} has no marker in the project"))),
        Some(Some(_)) => Ok(()),
    }
}

pub fn execute(session: &mut Session, command: SetlistCommand) -> Result<Value, ApiError> {
    match command {
        SetlistCommand::Songs => {
            Ok(json(session.songs().iter().enumerate().map(|(i, s)| song_info(i, s)).collect::<Vec<_>>()))
        }
        SetlistCommand::List => {
            let sets = session.setlists();
            let songs = session.songs();
            let setlists = sets.lists.iter().enumerate().map(|(i, l)| setlist_info(i, l, &songs)).collect();
            let current = (session.live.list < sets.lists.len()).then_some(session.live.list);
            Ok(json(SetlistsInfo {
                setlists,
                current,
                auto_advance: sets.auto_advance,
                marker_actions: session.engine.marker_actions(),
            }))
        }
        SetlistCommand::Get { setlist } => {
            let at = find_setlist(session, setlist.as_ref())?;
            Ok(get(session, at))
        }
        SetlistCommand::Create { name, songs } => {
            let refs = song_refs(&session.songs(), &songs)?;
            let at = session.new_setlist(name.as_deref(), refs);
            Ok(get(session, at))
        }
        SetlistCommand::Rename { setlist, name } => {
            let at = find_setlist(session, setlist.as_ref())?;
            if name.trim().is_empty() {
                return Err(ApiError::invalid("a setlist needs a name"));
            }
            session.rename_setlist(at, &name);
            Ok(get(session, at))
        }
        SetlistCommand::Delete { setlist } => {
            let at = find_setlist(session, setlist.as_ref())?;
            session.delete_setlist(at);
            Ok(super::done())
        }
        SetlistCommand::Duplicate { setlist, name } => {
            let at = find_setlist(session, setlist.as_ref())?;
            let copy =
                session.duplicate_setlist(at, name.as_deref()).ok_or_else(|| ApiError::not_found("no setlist"))?;
            Ok(get(session, copy))
        }
        SetlistCommand::Set { setlist, songs } => {
            let at = find_setlist(session, setlist.as_ref())?;
            let refs = song_refs(&session.songs(), &songs)?;
            session.edit_setlists("Set setlist songs", |s| s.lists[at].songs = refs);
            Ok(get(session, at))
        }
        SetlistCommand::Move { setlist, from, to } => {
            let at = find_setlist(session, setlist.as_ref())?;
            let len = session.setlists().lists[at].songs.len();
            if from >= len || to >= len {
                return Err(ApiError::invalid(format!("entries run from 0 to {}", len.saturating_sub(1))));
            }
            session.edit_setlists("Reorder setlist", |s| {
                let entries = &mut s.lists[at].songs;
                let moved = entries.remove(from);
                entries.insert(to, moved);
            });
            if at == session.live.list && session.live.player.entry == from {
                session.live.player.entry = to;
            }
            Ok(get(session, at))
        }
        SetlistCommand::Import { text } => {
            if text.trim().is_empty() {
                return Err(ApiError::invalid("the text is empty"));
            }
            let (at, _) = session.import_setlist_text(&text);
            Ok(get(session, at))
        }
        SetlistCommand::Export { setlist } => {
            let at = find_setlist(session, setlist.as_ref())?;
            let text = session.setlists().lists[at].to_text(false);
            Ok(serde_json::json!({ "text": text }))
        }
        SetlistCommand::Select { setlist } => {
            let at = find_setlist(session, Some(&setlist))?;
            session.choose_setlist(at);
            Ok(json(status(session)))
        }
        SetlistCommand::Cue { entry } => {
            entry_of(session, entry)?;
            session.setlist_cue(entry);
            Ok(json(status(session)))
        }
        SetlistCommand::Play { entry } => {
            let entry = entry.unwrap_or(session.live.player.entry);
            entry_of(session, entry)?;
            if session.is_recording() {
                return Err(ApiError::failed("stop recording first"));
            }
            session.setlist_play_entry(entry);
            Ok(json(status(session)))
        }
        SetlistCommand::Stop => {
            session.stop();
            Ok(json(status(session)))
        }
        SetlistCommand::Next | SetlistCommand::Previous => {
            find_setlist(session, None)?;
            session.setlist_step(matches!(command, SetlistCommand::Next));
            Ok(json(status(session)))
        }
        SetlistCommand::AutoAdvance { enabled } => {
            let on = enabled.unwrap_or(!session.setlists().auto_advance);
            session.set_auto_advance(on);
            Ok(json(status(session)))
        }
        SetlistCommand::MarkerActions { enabled } => {
            let on = enabled.unwrap_or(!session.engine.marker_actions());
            session.engine.set_marker_actions(on);
            Ok(json(status(session)))
        }
        SetlistCommand::Status => Ok(json(status(session))),
    }
}
