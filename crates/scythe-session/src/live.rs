//! Playing live from one project: marker actions coming back from the engine, and the
//! setlists built from the project's songs. Session state, so a headless server runs a
//! setlist as the desktop app does.

use scythe_core::marker_action::{self, Command, TransportAction};
use scythe_core::setlist::{self, PlayerCommand, Setlist, SetlistPlayer, Setlists, Song, SongRef};

use crate::Session;

/// Marker commands kept for the host at most.
const COMMAND_BACKLOG: usize = 64;

#[derive(Debug, Default)]
pub struct LiveState {
    /// Index of the setlist being edited and played.
    pub list: usize,
    pub player: SetlistPlayer,
    /// The setlist started the playback, so the song's stop marker moves it on.
    pub driving: bool,
    /// Marker commands the session has no action for, for the host to run.
    commands: Vec<u32>,
}

impl Session {
    pub fn songs(&self) -> Vec<Song> {
        setlist::songs(&self.project)
    }

    pub fn setlists(&self) -> Setlists {
        Setlists::load(&self.project)
    }

    /// Changes the setlists as one undoable edit.
    pub fn edit_setlists(&mut self, label: &str, f: impl FnOnce(&mut Setlists)) {
        let mut sets = self.setlists();
        let before = sets.clone();
        f(&mut sets);
        if sets != before {
            self.edit(label, |p| sets.store(p));
        }
        let lists = self.setlists().lists;
        self.live.list = self.live.list.min(lists.len().saturating_sub(1));
        if let Some(list) = lists.get(self.live.list) {
            self.live.player.clamp(list);
        }
    }

    /// The setlist being played, if there is one.
    pub fn current_setlist(&self) -> Option<Setlist> {
        self.setlists().lists.into_iter().nth(self.live.list)
    }

    /// Makes setlist `index` the one played, from its first song.
    pub fn choose_setlist(&mut self, index: usize) {
        if index != self.live.list {
            self.live.list = index;
            self.live.player = SetlistPlayer::default();
            self.live.driving = false;
        }
    }

    fn run_player(&mut self, command: Option<PlayerCommand>) {
        match command {
            Some(PlayerCommand::Cue(at)) => {
                self.set_cursor(at);
                self.live.driving = false;
            }
            Some(PlayerCommand::PlayFrom(at)) => {
                if self.is_recording() {
                    return;
                }
                self.project.cursor = at;
                let count_in = self.current_setlist().is_some_and(|l| l.count_in);
                self.start_playback(at, count_in || self.project.metronome.counts_in(false));
                self.live.driving = true;
            }
            None => {}
        }
    }

    /// Plays the setlist's current song from its marker.
    pub fn setlist_play(&mut self) {
        let Some(list) = self.current_setlist() else {
            self.notify("Make a setlist first");
            return;
        };
        let songs = self.songs();
        let command = self.live.player.play(&list, &songs);
        if command.is_none() {
            self.notify("This song's marker is missing from the project");
        }
        self.run_player(command);
    }

    /// Plays entry `entry` of the setlist; false when it has no song in the project.
    pub fn setlist_play_entry(&mut self, entry: usize) -> bool {
        let Some(list) = self.current_setlist() else { return false };
        let songs = self.songs();
        let command = self.live.player.play_entry(entry, &list, &songs);
        let found = command.is_some();
        self.run_player(command);
        found
    }

    /// Selects a setlist entry: cues it when stopped, plays it when playing. False when it has
    /// no song in the project.
    pub fn setlist_select(&mut self, entry: usize) -> bool {
        let Some(list) = self.current_setlist() else { return false };
        let songs = self.songs();
        let command = self.live.player.select(entry, &list, &songs, self.is_playing());
        let found = command.is_some();
        self.run_player(command);
        found
    }

    /// Makes `entry` the one that plays next without touching the transport while playing;
    /// when stopped, moves the edit cursor to it. False when it has no song in the project.
    pub fn setlist_cue(&mut self, entry: usize) -> bool {
        if !self.is_playing() {
            return self.setlist_select(entry);
        }
        let Some(list) = self.current_setlist() else { return false };
        let found = list.resolve(&self.songs()).get(entry).copied().flatten().is_some();
        if found {
            self.live.player.entry = entry;
        }
        found
    }

    pub fn setlist_step(&mut self, forward: bool) {
        let Some(list) = self.current_setlist() else {
            self.notify("Make a setlist first");
            return;
        };
        let songs = self.songs();
        let command = self.live.player.step(forward, &list, &songs, self.is_playing());
        self.run_player(command);
    }

    /// Plays a song from the project's markers, outside any setlist.
    pub fn play_song(&mut self, song: &Song) {
        if self.is_recording() {
            return;
        }
        self.project.cursor = song.start;
        self.start_playback(song.start, self.project.metronome.counts_in(false));
        self.live.driving = false;
    }

    pub fn new_setlist(&mut self, name: Option<&str>, songs: Vec<SongRef>) -> usize {
        let mut index = 0;
        self.edit_setlists("New setlist", |s| {
            let name = s.unique_name(name.filter(|n| !n.trim().is_empty()).unwrap_or("Setlist").trim());
            s.lists.push(Setlist { name, songs, count_in: false });
            index = s.lists.len() - 1;
        });
        self.choose_setlist(index);
        index
    }

    /// Copies setlist `at` next to it; answers the copy's index.
    pub fn duplicate_setlist(&mut self, at: usize, name: Option<&str>) -> Option<usize> {
        let list = self.setlists().lists.get(at)?.clone();
        self.edit_setlists("Duplicate setlist", |s| {
            let name = s.unique_name(name.unwrap_or(&format!("{} copy", list.name)));
            s.lists.insert(at + 1, Setlist { name, ..list });
        });
        self.choose_setlist(at + 1);
        Some(at + 1)
    }

    pub fn rename_setlist(&mut self, at: usize, name: &str) {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return;
        }
        self.edit_setlists("Rename setlist", |s| {
            if at < s.lists.len() && s.lists[at].name != name {
                s.lists[at].name = s.unique_name(&name);
            }
        });
    }

    pub fn delete_setlist(&mut self, at: usize) {
        let current = self.live.list;
        self.edit_setlists("Delete setlist", |s| {
            if at < s.lists.len() {
                s.lists.remove(at);
            }
        });
        if at < current {
            self.live.list = current - 1;
        } else if at == current {
            self.live.player = SetlistPlayer::default();
            self.live.driving = false;
        }
    }

    pub fn add_to_setlist(&mut self, song: &Song) {
        if self.current_setlist().is_none() {
            self.new_setlist(None, Vec::new());
        }
        let at = self.live.list;
        self.edit_setlists("Add song to setlist", |s| s.lists[at].songs.push(SongRef::to(song)));
    }

    /// Whether setlist `at` counts each song in.
    pub fn set_setlist_count_in(&mut self, at: usize, on: bool) {
        self.edit_setlists(if on { "Count in setlist songs" } else { "Play setlist songs without count-in" }, |s| {
            if let Some(list) = s.lists.get_mut(at) {
                list.count_in = on;
            }
        });
    }

    pub fn set_auto_advance(&mut self, on: bool) {
        self.edit_setlists(if on { "Enable auto-advance" } else { "Disable auto-advance" }, |s| s.auto_advance = on);
    }

    /// Adds a setlist from text, matching its songs to the project's by name. Answers its
    /// index and how many songs have no marker in the project.
    pub fn import_setlist_text(&mut self, text: &str) -> (usize, usize) {
        let songs = self.songs();
        let mut list = Setlist::from_text(text);
        list.bind(&songs);
        let missing = list.resolve(&songs).iter().filter(|r| r.is_none()).count();
        let mut index = 0;
        self.edit_setlists("Import setlist", |s| {
            list.name = s.unique_name(&list.name);
            s.lists.push(list);
            index = s.lists.len() - 1;
        });
        self.choose_setlist(index);
        (index, missing)
    }

    /// Marker commands without a session action, by REAPER command id, for a host that maps
    /// them to its own actions.
    pub fn take_marker_commands(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.live.commands)
    }

    /// Runs what the engine reports from crossed `!` markers. Transport commands already took
    /// effect on the audio thread; this follows up as REAPER does (Stop returns to the edit
    /// cursor), runs the commands the session has, and keeps the rest for the host.
    pub(crate) fn handle_marker_actions(&mut self) {
        for action in self.engine.take_marker_actions() {
            match action.applied {
                Some(stop @ (TransportAction::Stop | TransportAction::Pause)) => {
                    if stop == TransportAction::Pause {
                        self.project.cursor = action.position;
                    }
                    if self.is_recording() {
                        self.toggle_record();
                    } else {
                        self.engine.seek(self.project.cursor);
                    }
                    self.song_ended();
                }
                Some(TransportAction::GoToStart) => self.project.cursor = 0.0,
                Some(TransportAction::GoToEnd) => self.project.cursor = setlist::project_end(&self.project),
                Some(TransportAction::GoToLoopStart) => {
                    self.project.cursor = self.project.time_selection.map_or(0.0, |r| r.start);
                }
                Some(TransportAction::ToggleRepeat) => self.toggle_loop(),
                Some(TransportAction::Play) => {}
                None => match action.command {
                    Command::Id(marker_action::RECORD) => self.toggle_record(),
                    Command::Id(METRONOME) => self.toggle_metronome(),
                    Command::Id(UNDO) => self.undo(),
                    Command::Id(REDO) => self.redo(),
                    Command::Id(id) => {
                        if self.live.commands.len() >= COMMAND_BACKLOG {
                            self.live.commands.remove(0);
                        }
                        self.live.commands.push(id);
                    }
                    Command::Named(name) => self.notify(format!("Marker action {name} is not available in Scythe")),
                },
            }
        }
    }

    fn song_ended(&mut self) {
        if !std::mem::take(&mut self.live.driving) {
            return;
        }
        let Some(list) = self.current_setlist() else { return };
        let songs = self.songs();
        let auto = self.setlists().auto_advance;
        let command = self.live.player.song_ended(&list, &songs, auto);
        self.run_player(command);
    }
}

const METRONOME: u32 = 40364;
const UNDO: u32 = 40029;
const REDO: u32 = 40030;
