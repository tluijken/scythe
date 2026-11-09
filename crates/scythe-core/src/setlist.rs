//! Songs derived from a live set's markers, setlists of them stored in the project, and the
//! UI-free logic for playing through a setlist.
//!
//! A live project holds every song on one timeline: each song starts at a marker and runs to
//! the next stop marker (`!1016`, see [`crate::marker_action`]). With stop markers present, the
//! first plain marker after a stop (or the project start) begins a song and plain markers
//! inside it are its sections (solo cues and the like). Without any stop marker, every plain
//! marker begins a song that runs to the next one.

use crate::marker_action::{is_action_marker, stops_playback};
use crate::model::Project;

/// Project ext-state section holding the setlists.
pub const SECTION: &str = "SCYTHE_SETLISTS";
const LIST_KEY: &str = "SETLIST";
const AUTO_ADVANCE_KEY: &str = "AUTOADVANCE";
/// Per setlist, numbered like its `SETLIST` key: "1" when each song plays after a count-in.
const COUNT_IN_KEY: &str = "COUNTIN";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SongEnd {
    StopMarker,
    NextSong,
    ProjectEnd,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Song {
    /// Index into `Project::markers` of the start marker.
    pub marker: usize,
    pub guid: Option<String>,
    /// The marker's displayed number.
    pub number: u32,
    pub name: String,
    pub start: f64,
    pub end: f64,
    pub end_kind: SongEnd,
    /// Plain markers inside the song: name and position.
    pub sections: Vec<(String, f64)>,
}

impl Song {
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }

    pub fn contains(&self, t: f64) -> bool {
        t >= self.start - 1e-9 && t < self.end
    }
}

/// Where a live project ends: the last item or marker.
pub fn project_end(project: &Project) -> f64 {
    project.markers.iter().map(|m| m.region_end.unwrap_or(m.position)).fold(project.length(), f64::max)
}

pub fn songs(project: &Project) -> Vec<Song> {
    let mut order: Vec<usize> =
        (0..project.markers.len()).filter(|&i| project.markers[i].region_end.is_none()).collect();
    // A stop marker on the same spot as the next song's start ends the song before it.
    order.sort_by(|&a, &b| {
        let (ma, mb) = (&project.markers[a], &project.markers[b]);
        ma.position.total_cmp(&mb.position).then(stops_playback(&mb.name).cmp(&stops_playback(&ma.name)))
    });
    let has_stops = order.iter().any(|&i| stops_playback(&project.markers[i].name));
    let end = project_end(project);
    let mut out: Vec<Song> = Vec::new();
    let mut open: Option<Song> = None;
    for i in order {
        let m = &project.markers[i];
        if stops_playback(&m.name) {
            if let Some(mut song) = open.take() {
                song.end = m.position;
                song.end_kind = SongEnd::StopMarker;
                out.push(song);
            }
            continue;
        }
        if is_action_marker(&m.name) {
            continue;
        }
        if has_stops && let Some(song) = &mut open {
            song.sections.push((m.name.clone(), m.position));
            continue;
        }
        if let Some(mut song) = open.take() {
            song.end = m.position;
            song.end_kind = SongEnd::NextSong;
            out.push(song);
        }
        let name = if m.name.trim().is_empty() { format!("Marker {}", m.index) } else { m.name.clone() };
        open = Some(Song {
            marker: i,
            guid: m.guid().map(str::to_owned),
            number: m.index,
            name,
            start: m.position,
            end,
            end_kind: SongEnd::ProjectEnd,
            sections: Vec::new(),
        });
    }
    if let Some(mut song) = open {
        song.end = song.end.max(song.start);
        out.push(song);
    }
    out
}

/// A setlist entry: the start marker's GUID when it has one, and its name, so the entry
/// survives renumbering and moved markers and still resolves in projects without GUIDs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SongRef {
    pub guid: Option<String>,
    pub name: String,
}

impl SongRef {
    pub fn to(song: &Song) -> Self {
        Self { guid: song.guid.clone(), name: song.name.clone() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Setlist {
    pub name: String,
    pub songs: Vec<SongRef>,
    /// Playing a song counts in first (the project's metronome count-in length), so the band
    /// starts together without the drummer counting.
    pub count_in: bool,
}

impl Setlist {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), ..Self::default() }
    }

    /// The song each entry plays: by GUID, else by name (case-insensitive).
    pub fn resolve(&self, songs: &[Song]) -> Vec<Option<usize>> {
        self.songs
            .iter()
            .map(|r| {
                let by_guid = r.guid.as_ref().and_then(|g| songs.iter().position(|s| s.guid.as_ref() == Some(g)));
                by_guid.or_else(|| {
                    let name = r.name.trim().to_lowercase();
                    songs.iter().position(|s| s.name.trim().to_lowercase() == name)
                })
            })
            .collect()
    }

    /// Refreshes names and fills in GUIDs from the songs the entries resolve to.
    pub fn bind(&mut self, songs: &[Song]) {
        let resolved = self.resolve(songs);
        for (r, i) in self.songs.iter_mut().zip(resolved) {
            if let Some(i) = i {
                *r = SongRef::to(&songs[i]);
            }
        }
    }

    pub fn duration(&self, songs: &[Song]) -> f64 {
        self.resolve(songs).into_iter().flatten().map(|i| songs[i].duration()).sum()
    }

    /// The setlist as text: its name, then one numbered song per line. With `ids`, each line
    /// ends with the start marker's GUID so the list survives renamed markers.
    pub fn to_text(&self, ids: bool) -> String {
        let name = if self.name.trim().is_empty() { "Setlist" } else { self.name.trim() };
        let mut out = format!("{name}\n");
        for (i, s) in self.songs.iter().enumerate() {
            out.push_str(&format!("{}. {}", i + 1, s.name));
            if ids && let Some(g) = &s.guid {
                out.push_str(&format!("  {g}"));
            }
            out.push('\n');
        }
        out
    }

    /// Reads [`Self::to_text`]'s format. Numbering (`3.`, `3)`), bullets and blank lines are
    /// optional, so a list typed by hand or pasted from a chat works too.
    pub fn from_text(text: &str) -> Self {
        let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
        let name = lines.next().unwrap_or_default().to_owned();
        let songs = lines
            .filter_map(|l| {
                let l = l.trim_start_matches(['-', '*', '•']).trim_start();
                let digits = l.len() - l.trim_start_matches(|c: char| c.is_ascii_digit()).len();
                let l = match l[digits..].strip_prefix(['.', ')']) {
                    Some(rest) if digits > 0 => rest.trim_start(),
                    _ => l,
                };
                let (name, guid) = match l.rsplit_once(char::is_whitespace) {
                    Some((name, g)) if is_guid(g) => (name.trim_end(), Some(g.to_owned())),
                    _ if is_guid(l) => ("", Some(l.to_owned())),
                    _ => (l, None),
                };
                (!name.is_empty() || guid.is_some()).then(|| SongRef { guid, name: name.to_owned() })
            })
            .collect();
        Self { name, songs, count_in: false }
    }
}

fn is_guid(s: &str) -> bool {
    s.len() == 38 && s.starts_with('{') && s.ends_with('}')
}

/// The project's setlists and playback options.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Setlists {
    pub lists: Vec<Setlist>,
    /// After a song ends at its stop marker, cue the next one (without playing it).
    pub auto_advance: bool,
}

impl Setlists {
    pub fn load(project: &Project) -> Self {
        let lists = (1..)
            .map_while(|n| {
                let text = project.ext_state(SECTION, &format!("{LIST_KEY}{n}"))?;
                let count_in =
                    project.ext_state(SECTION, &format!("{COUNT_IN_KEY}{n}")).is_some_and(|v| v.trim() == "1");
                Some(Setlist { count_in, ..Setlist::from_text(&text) })
            })
            .collect();
        let auto_advance = project.ext_state(SECTION, AUTO_ADVANCE_KEY).is_some_and(|v| v.trim() == "1");
        Self { lists, auto_advance }
    }

    pub fn store(&self, project: &mut Project) {
        for (n, list) in self.lists.iter().enumerate() {
            project.set_ext_state(SECTION, &format!("{LIST_KEY}{}", n + 1), Some(&list.to_text(true)));
            project.set_ext_state(SECTION, &format!("{COUNT_IN_KEY}{}", n + 1), list.count_in.then_some("1"));
        }
        let stale = project
            .ext_state_section(SECTION)
            .into_iter()
            .filter_map(|(k, _)| {
                let n = k.strip_prefix(LIST_KEY).or_else(|| k.strip_prefix(COUNT_IN_KEY))?;
                Some((k.clone(), n.parse::<usize>().ok()?))
            })
            .filter(|&(_, n)| n > self.lists.len())
            .collect::<Vec<_>>();
        for (key, _) in stale {
            project.set_ext_state(SECTION, &key, None);
        }
        project.set_ext_state(SECTION, AUTO_ADVANCE_KEY, self.auto_advance.then_some("1"));
    }

    /// A name not used by another setlist, from `base`.
    pub fn unique_name(&self, base: &str) -> String {
        let taken = |n: &str| self.lists.iter().any(|l| l.name == n);
        if !taken(base) {
            return base.to_owned();
        }
        (2..).map(|i| format!("{base} {i}")).find(|n| !taken(n)).unwrap()
    }
}

/// What the host should do with the transport.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlayerCommand {
    /// Move the edit cursor here, leaving playback alone (stopped).
    Cue(f64),
    /// Seek here and play.
    PlayFrom(f64),
}

/// Where a setlist is: the entry that plays (or plays next when stopped).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SetlistPlayer {
    pub entry: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlayerStatus {
    /// The entry playing, or about to play when stopped.
    pub current: Option<usize>,
    pub next: Option<usize>,
    /// Seconds into the current song while it plays.
    pub elapsed: Option<f64>,
}

impl SetlistPlayer {
    /// Plays entry `entry` from its start marker.
    pub fn play_entry(&mut self, entry: usize, setlist: &Setlist, songs: &[Song]) -> Option<PlayerCommand> {
        let song = setlist.resolve(songs).get(entry).copied().flatten()?;
        self.entry = entry;
        Some(PlayerCommand::PlayFrom(songs[song].start))
    }

    pub fn play(&mut self, setlist: &Setlist, songs: &[Song]) -> Option<PlayerCommand> {
        self.play_entry(self.entry, setlist, songs)
    }

    /// Selects entry `entry`: plays it when the transport plays, else cues it.
    pub fn select(&mut self, entry: usize, setlist: &Setlist, songs: &[Song], playing: bool) -> Option<PlayerCommand> {
        let song = setlist.resolve(songs).get(entry).copied().flatten()?;
        self.entry = entry;
        let start = songs[song].start;
        Some(if playing { PlayerCommand::PlayFrom(start) } else { PlayerCommand::Cue(start) })
    }

    /// Moves to the next (`forward`) or previous playable entry; see [`Self::select`].
    pub fn step(&mut self, forward: bool, setlist: &Setlist, songs: &[Song], playing: bool) -> Option<PlayerCommand> {
        let target = neighbour(&setlist.resolve(songs), self.entry, forward)?;
        self.select(target, setlist, songs, playing)
    }

    /// The song stopped at its stop marker: cue the next entry with auto-advance, else return
    /// to the start of the one that played.
    pub fn song_ended(&mut self, setlist: &Setlist, songs: &[Song], auto_advance: bool) -> Option<PlayerCommand> {
        let resolved = setlist.resolve(songs);
        if auto_advance && let Some(next) = neighbour(&resolved, self.entry, true) {
            return self.select(next, setlist, songs, false);
        }
        resolved.get(self.entry).copied().flatten().map(|s| PlayerCommand::Cue(songs[s].start))
    }

    /// Keeps `entry` on the list after entries were removed.
    pub fn clamp(&mut self, setlist: &Setlist) {
        self.entry = self.entry.min(setlist.songs.len().saturating_sub(1));
    }

    pub fn status(&self, setlist: &Setlist, songs: &[Song], position: f64, playing: bool) -> PlayerStatus {
        let resolved = setlist.resolve(songs);
        let current = resolved.get(self.entry).copied().flatten().map(|_| self.entry);
        let elapsed = current.and_then(|e| {
            let song = &songs[resolved[e]?];
            (playing && song.contains(position)).then_some(position - song.start)
        });
        PlayerStatus { current, next: neighbour(&resolved, self.entry, true), elapsed }
    }
}

fn neighbour(resolved: &[Option<usize>], from: usize, forward: bool) -> Option<usize> {
    if forward {
        (from + 1..resolved.len()).find(|&i| resolved[i].is_some())
    } else {
        (0..from.min(resolved.len())).rev().find(|&i| resolved[i].is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Item, Take};
    use crate::rpp::{project_from_rpp, project_to_rpp};

    /// The shape of a band's live set: one item per song with its click and backing, solo
    /// cue markers inside the songs, and a stop marker after each song. An unnamed marker
    /// sits inside a song, and one stop marker shares its spot with the next song's start.
    fn live_set() -> Project {
        let mut p = Project::default();
        let t = p.add_track("Backing");
        let songs = [
            ("01 - Monumental Misery", 0.0, 336.07),
            ("02 - Slaves of existence", 384.0, 332.9),
            ("03 - Chronicles of the fall", 736.0, 205.95),
            ("04 - Blackened Sun", 992.0, 297.49),
            ("05 - Catch Kill Release", 1312.0, 309.23),
        ];
        for (name, start, len) in songs {
            let item = Item::new(start, len, Take::from_file(format!("Media/{name}.mp3").into()));
            p.track_mut(t).unwrap().items.push(item);
            p.add_marker(start, name);
            if !name.starts_with("03") {
                p.add_marker(start + len, "!1016");
            }
        }
        p.add_marker(992.0, "!1016");
        p.add_marker(203.0, "start solo mm");
        p.add_marker(241.0, "einde solo mm");
        p.add_marker(1232.76, "");
        p.add_marker(1400.0, "!40042");
        p
    }

    #[test]
    fn songs_run_from_their_marker_to_the_stop_marker() {
        let p = live_set();
        let s = songs(&p);
        let names: Vec<&str> = s.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "01 - Monumental Misery",
                "02 - Slaves of existence",
                "03 - Chronicles of the fall",
                "04 - Blackened Sun",
                "05 - Catch Kill Release"
            ]
        );
        assert_eq!((s[0].start, s[0].end, s[0].end_kind), (0.0, 336.07, SongEnd::StopMarker));
        assert_eq!(s[0].sections, [("start solo mm".to_owned(), 203.0), ("einde solo mm".to_owned(), 241.0)]);
        assert_eq!((s[2].start, s[2].end), (736.0, 992.0));
        assert_eq!((s[3].start, s[3].end), (992.0, 992.0 + 297.49));
        assert_eq!(s[3].sections, [(String::new(), 1232.76)]);
        assert!((s[4].duration() - 309.23).abs() < 1e-9);
    }

    #[test]
    fn without_stop_markers_every_marker_is_a_song() {
        let mut p = Project::default();
        let t = p.add_track("t");
        p.track_mut(t).unwrap().items.push(Item::new(0.0, 100.0, Take::from_file("a.wav".into())));
        p.add_marker(0.0, "A");
        p.add_marker(30.0, "");
        p.add_marker(60.0, "!40042");
        p.add_marker(70.0, "C");
        let s = songs(&p);
        assert_eq!(s.len(), 3);
        assert_eq!((s[0].end, s[0].end_kind), (30.0, SongEnd::NextSong));
        assert_eq!(s[1].name, "Marker 2");
        assert_eq!((s[2].end, s[2].end_kind), (100.0, SongEnd::ProjectEnd));
    }

    #[test]
    fn guids_come_from_reaper_markers() {
        let text = "<REAPER_PROJECT 0.1 \"7.59\" 0\n  MARKER 11 203 \"Song\" 0 0 1 B {24777E83-6425-D14D-BD7C-36DCA6A188CE} 0\n  MARKER 12 241 !1016 0 0 1 B {8DC91102-3546-EE42-55BA-BE005CAD48E6} 0\n>\n";
        let p = project_from_rpp(text).unwrap();
        let s = songs(&p);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].guid.as_deref(), Some("{24777E83-6425-D14D-BD7C-36DCA6A188CE}"));
        assert_eq!((s[0].number, s[0].start, s[0].end), (11, 203.0, 241.0));
    }

    #[test]
    fn entries_resolve_by_guid_then_name() {
        let mut s = songs(&live_set());
        s[1].guid = Some("{11111111-1111-1111-1111-111111111111}".into());
        let list = Setlist {
            name: "Show".into(),
            songs: vec![
                SongRef { guid: Some("{11111111-1111-1111-1111-111111111111}".into()), name: "renamed".into() },
                SongRef { guid: None, name: "04 - blackened sun ".into() },
                SongRef { guid: None, name: "Gone".into() },
            ],
            count_in: false,
        };
        assert_eq!(list.resolve(&s), [Some(1), Some(3), None]);
        let mut bound = list.clone();
        bound.bind(&s);
        assert_eq!(bound.songs[0].name, "02 - Slaves of existence");
        assert_eq!(bound.songs[2].name, "Gone");
        assert!((list.duration(&s) - (332.9 + 297.49)).abs() < 1e-9);
    }

    #[test]
    fn text_round_trip_and_loose_import() {
        let list = Setlist {
            name: "Saturday".into(),
            songs: vec![
                SongRef { guid: Some("{24777E83-6425-D14D-BD7C-36DCA6A188CE}".into()), name: "Intro  jam".into() },
                SongRef { guid: None, name: "2. Not a number".into() },
            ],
            count_in: false,
        };
        let text = list.to_text(true);
        assert_eq!(text, "Saturday\n1. Intro  jam  {24777E83-6425-D14D-BD7C-36DCA6A188CE}\n2. 2. Not a number\n");
        assert_eq!(Setlist::from_text(&text), list);
        assert_eq!(list.to_text(false), "Saturday\n1. Intro  jam\n2. 2. Not a number\n");
        let typed = Setlist::from_text("\n Festival \n\n- Song A\n3) Song B\n• Song C\n10 Years\n");
        let names: Vec<&str> = typed.songs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(typed.name, "Festival");
        assert_eq!(names, ["Song A", "Song B", "Song C", "10 Years"]);
    }

    #[test]
    fn setlists_round_trip_through_the_project() {
        let mut p = live_set();
        let s = songs(&p);
        let mut sets = Setlists {
            lists: vec![
                Setlist {
                    name: "Long \"set\" 'A' `x`".into(),
                    songs: s.iter().map(SongRef::to).collect(),
                    count_in: false,
                },
                Setlist { name: "Short".into(), songs: vec![SongRef::to(&s[4]), SongRef::to(&s[0])], count_in: true },
            ],
            auto_advance: true,
        };
        sets.store(&mut p);
        let q = project_from_rpp(&project_to_rpp(&p)).unwrap();
        assert_eq!(Setlists::load(&q), sets);
        sets.lists.pop();
        sets.auto_advance = false;
        sets.store(&mut p);
        assert_eq!(Setlists::load(&p), sets);
        assert_eq!(p.ext_state(SECTION, "SETLIST2"), None);
        assert_eq!(p.ext_state(SECTION, "COUNTIN2"), None);
        assert_eq!(sets.unique_name("Short"), "Short");
        assert_eq!(sets.unique_name("Long \"set\" 'A' `x`"), "Long \"set\" 'A' `x` 2");
    }

    #[test]
    fn player_walks_the_setlist() {
        let s = songs(&live_set());
        let list = Setlist {
            name: "Show".into(),
            songs: vec![SongRef::to(&s[2]), SongRef { guid: None, name: "missing".into() }, SongRef::to(&s[0])],
            count_in: false,
        };
        let mut player = SetlistPlayer::default();
        assert_eq!(player.play(&list, &s), Some(PlayerCommand::PlayFrom(736.0)));
        let st = player.status(&list, &s, 740.0, true);
        assert_eq!((st.current, st.next, st.elapsed), (Some(0), Some(2), Some(4.0)));
        assert_eq!(player.step(true, &list, &s, false), Some(PlayerCommand::Cue(0.0)));
        assert_eq!(player.entry, 2);
        assert_eq!(player.step(true, &list, &s, false), None);
        assert_eq!(player.step(false, &list, &s, true), Some(PlayerCommand::PlayFrom(736.0)));
        assert_eq!(player.entry, 0);
        assert_eq!(player.song_ended(&list, &s, false), Some(PlayerCommand::Cue(736.0)));
        assert_eq!(player.entry, 0);
        assert_eq!(player.song_ended(&list, &s, true), Some(PlayerCommand::Cue(0.0)));
        assert_eq!(player.entry, 2);
        assert_eq!(player.song_ended(&list, &s, true), Some(PlayerCommand::Cue(0.0)));
        assert_eq!(player.play_entry(1, &list, &s), None);
        assert_eq!(player.status(&list, &s, 5.0, false).elapsed, None);
    }
}
