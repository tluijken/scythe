use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::envelope::{Envelope, EnvelopeKind};
use crate::fade::Fade;
use crate::rpp::{Chunk, Node};
use crate::tempo::TempoMap;
use crate::units::PanLaw;

/// Original tokens of each modelled RPP line, keyed by its keyword. Writing starts from these
/// and overwrites only the modelled fields, so values the model does not cover survive a save.
pub type LineMemo = BTreeMap<String, Vec<String>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TrackId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemId(pub u64);

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

impl TrackId {
    pub fn new() -> Self {
        Self(next_id())
    }
}

impl Default for TrackId {
    fn default() -> Self {
        Self::new()
    }
}

impl ItemId {
    pub fn new() -> Self {
        Self(next_id())
    }
}

impl Default for ItemId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub fn from_hex(hex: u32) -> Self {
        Self::rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
    }

    pub fn to_hex(self) -> u32 {
        (self.r as u32) << 16 | (self.g as u32) << 8 | self.b as u32
    }
}

/// Track colors cycled through for new tracks, taken from the redesign palette.
pub const TRACK_PALETTE: [Color; 6] = [
    Color::rgb(0x66, 0xd9, 0xef),
    Color::rgb(0xa6, 0xe2, 0x2e),
    Color::rgb(0xe6, 0xdb, 0x74),
    Color::rgb(0xfd, 0x97, 0x1f),
    Color::rgb(0xf9, 0x26, 0x72),
    Color::rgb(0xae, 0x81, 0xff),
];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeRange {
    pub start: f64,
    pub end: f64,
}

impl TimeRange {
    pub fn new(a: f64, b: f64) -> Self {
        Self { start: a.min(b), end: a.max(b) }
    }

    pub fn length(&self) -> f64 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.length() <= 0.0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    File(PathBuf),
    Empty,
    /// A sub-range of another source, optionally played backwards (RPP `<SOURCE SECTION`).
    Section(Section),
    /// A source type that is not decoded yet (MIDI, video…), kept verbatim.
    Raw(Chunk),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub inner: Box<Source>,
    /// Start within the inner source, in seconds.
    pub start: f64,
    /// Length in seconds; zero or less means up to the end of the inner source.
    pub length: f64,
    pub reverse: bool,
    /// The original chunk, so lines the model does not cover survive a save.
    pub raw: Chunk,
}

impl Source {
    /// The media file this source ultimately reads from.
    pub fn file(&self) -> Option<&PathBuf> {
        match self {
            Self::File(p) => Some(p),
            Self::Section(s) => s.inner.file(),
            Self::Empty | Self::Raw(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Take {
    pub name: String,
    pub source: Source,
    /// Offset into the source in seconds (RPP `SOFFS`).
    pub source_offset: f64,
    pub playrate: f64,
    pub volume: f64,
    pub pan: f64,
    pub lines: LineMemo,
    pub extra: Vec<Node>,
}

impl Take {
    /// Maps a time relative to the item start onto the underlying media file, following the
    /// section, reverse and loop settings. `None` where the item plays silence.
    pub fn source_time(&self, rel: f64, looped: bool, media_len: f64) -> Option<f64> {
        let (start, len, reverse) = match &self.source {
            Source::Section(s) => {
                let start = s.start.clamp(0.0, media_len);
                let avail = media_len - start;
                (start, if s.length > 0.0 { s.length.min(avail) } else { avail }, s.reverse)
            }
            _ => (0.0, media_len, false),
        };
        if len <= 0.0 {
            return None;
        }
        let mut pos = self.source_offset + rel * self.playrate;
        if looped {
            pos = pos.rem_euclid(len);
        }
        if !(0.0..len).contains(&pos) {
            return None;
        }
        Some(if reverse { start + len - pos } else { start + pos })
    }

    pub fn from_file(path: PathBuf) -> Self {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Self {
            name,
            source: Source::File(path),
            source_offset: 0.0,
            playrate: 1.0,
            volume: 1.0,
            pan: 0.0,
            lines: LineMemo::new(),
            extra: Vec::new(),
        }
    }
}

/// Shortest item an edit can leave behind, in seconds.
pub const MIN_ITEM_LENGTH: f64 = 0.001;

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: ItemId,
    pub guid: String,
    pub position: f64,
    pub length: f64,
    pub fade_in: Fade,
    pub fade_out: Fade,
    pub volume: f64,
    pub muted: bool,
    pub looped: bool,
    /// Vertical placement within the track as (top, height) fractions (RPP `YPOS`), used by
    /// fixed lanes and free item positioning.
    pub ypos: Option<(f64, f64)>,
    pub takes: Vec<Take>,
    pub active_take: usize,
    pub lines: LineMemo,
    pub extra: Vec<Node>,
}

impl Item {
    pub fn new(position: f64, length: f64, take: Take) -> Self {
        Self {
            id: ItemId::new(),
            guid: new_guid(),
            position,
            length,
            fade_in: Fade::new(0.01),
            fade_out: Fade::new(0.01),
            volume: 1.0,
            muted: false,
            looped: false,
            ypos: None,
            takes: vec![take],
            active_take: 0,
            lines: LineMemo::new(),
            extra: Vec::new(),
        }
    }

    pub fn end(&self) -> f64 {
        self.position + self.length
    }

    /// Moves the left edge to `start`, keeping the audio where it is on the timeline.
    pub fn trim_start(&mut self, start: f64) {
        let start = start.clamp(0.0, self.end() - MIN_ITEM_LENGTH);
        let delta = start - self.position;
        for take in &mut self.takes {
            take.source_offset += delta * take.playrate;
        }
        self.position = start;
        self.length -= delta;
        self.clamp_fades();
    }

    /// Moves the right edge to `end`.
    pub fn trim_end(&mut self, end: f64) {
        self.length = (end - self.position).max(MIN_ITEM_LENGTH);
        self.clamp_fades();
    }

    fn clamp_fades(&mut self) {
        self.fade_in.length = self.fade_in.length.min(self.length);
        self.fade_out.length = self.fade_out.length.min(self.length);
    }

    /// Splits at `at`, keeping the left part and returning the right one (with a fresh
    /// identity). `None` when `at` is not strictly inside the item.
    pub fn split_at(&mut self, at: f64) -> Option<Item> {
        if at <= self.position + MIN_ITEM_LENGTH || at >= self.end() - MIN_ITEM_LENGTH {
            return None;
        }
        let cut = at - self.position;
        let mut right = self.duplicate();
        for take in &mut right.takes {
            take.source_offset += cut * take.playrate;
        }
        right.position = at;
        right.length = self.length - cut;
        right.fade_in = Fade::new(0.01);
        self.length = cut;
        self.fade_out = Fade::new(0.01);
        Some(right)
    }

    /// A copy with a fresh identity, for paste and duplicate.
    pub fn duplicate(&self) -> Self {
        Self { id: ItemId::new(), guid: new_guid(), ..self.clone() }
    }

    pub fn take(&self) -> Option<&Take> {
        self.takes.get(self.active_take)
    }

    pub fn name(&self) -> &str {
        self.take().map(|t| t.name.as_str()).unwrap_or("")
    }
}

/// A plugin slot. Only identity and bypass state are modelled; the opaque plugin chunk is kept
/// verbatim so projects round-trip until plugin hosting exists.
#[derive(Debug, Clone, PartialEq)]
pub struct FxSlot {
    pub name: String,
    pub enabled: bool,
    pub raw: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Send {
    pub dest: TrackId,
    pub volume: f64,
    pub pan: f64,
    pub muted: bool,
    /// Tokens of the original `AUXRECV` line, so channel mapping, MIDI flags and the rest
    /// survive a save when only volume, pan or mute changed.
    pub raw: Option<Vec<String>>,
}

impl Send {
    pub fn new(dest: TrackId) -> Self {
        Self { dest, volume: 1.0, pan: 0.0, muted: false, raw: None }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub id: TrackId,
    pub guid: String,
    pub name: String,
    pub color: Option<Color>,
    pub volume: f64,
    pub pan: f64,
    pub muted: bool,
    pub solo: bool,
    pub armed: bool,
    /// Record input in REAPER's `I_RECINPUT` encoding (`REC` field 2): hardware channel in the
    /// low bits, +1024 for a stereo pair, negative for none.
    pub rec_input: i64,
    /// Hear the record input through the track while armed (`REC` field 3).
    pub monitor: bool,
    /// Folder structure delta after this track: +1 opens a folder (this track is its parent),
    /// -n closes n levels (this track is the last child).
    pub folder_depth: i32,
    /// Whether the track feeds its parent folder, or the master when it has none.
    pub main_send: bool,
    /// Track pan law (`VOLPAN` field 3); negative means the project default.
    pub pan_law: f64,
    pub items: Vec<Item>,
    pub fx: Vec<FxSlot>,
    /// FX-chain level lines (window state and such) that precede the first plugin.
    pub fx_chain_prelude: Vec<Node>,
    pub sends: Vec<Send>,
    pub envelopes: Vec<Envelope>,
    pub lines: LineMemo,
    pub extra: Vec<Node>,
}

impl Track {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: TrackId::new(),
            guid: new_guid(),
            name: name.into(),
            color: None,
            volume: 1.0,
            pan: 0.0,
            muted: false,
            solo: false,
            armed: false,
            rec_input: 0,
            monitor: true,
            folder_depth: 0,
            main_send: true,
            pan_law: -1.0,
            items: Vec::new(),
            fx: Vec::new(),
            fx_chain_prelude: Vec::new(),
            sends: Vec::new(),
            envelopes: Vec::new(),
            lines: LineMemo::new(),
            extra: Vec::new(),
        }
    }

    pub fn item(&self, id: ItemId) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    pub fn item_mut(&mut self, id: ItemId) -> Option<&mut Item> {
        self.items.iter_mut().find(|i| i.id == id)
    }

    pub fn envelope(&self, kind: EnvelopeKind) -> Option<&Envelope> {
        self.envelopes.iter().find(|e| e.kind == kind)
    }

    pub fn envelope_mut(&mut self, kind: EnvelopeKind) -> &mut Envelope {
        match self.envelopes.iter().position(|e| e.kind == kind) {
            Some(i) => &mut self.envelopes[i],
            None => {
                self.envelopes.push(Envelope::new(kind));
                self.envelopes.last_mut().unwrap()
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    pub index: u32,
    pub position: f64,
    pub name: String,
    pub color: Option<Color>,
    /// Some(end) for regions.
    pub region_end: Option<f64>,
    pub lines: LineMemo,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    pub path: Option<PathBuf>,
    pub tempo: TempoMap,
    pub sample_rate: Option<u32>,
    /// Default pan law for tracks that do not set their own (`PANLAW`, `PANLAWFLAGS`).
    pub pan_law: PanLaw,
    pub master: Track,
    pub tracks: Vec<Track>,
    pub markers: Vec<Marker>,
    pub time_selection: Option<TimeRange>,
    pub loop_enabled: bool,
    pub cursor: f64,
    pub lines: LineMemo,
    pub extra: Vec<Node>,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            path: None,
            tempo: TempoMap::default(),
            sample_rate: None,
            pan_law: PanLaw::default(),
            master: Track::new("Master"),
            tracks: Vec::new(),
            markers: Vec::new(),
            time_selection: None,
            loop_enabled: false,
            cursor: 0.0,
            lines: LineMemo::new(),
            extra: Vec::new(),
        }
    }
}

impl Project {
    pub fn name(&self) -> String {
        self.path
            .as_deref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".to_owned())
    }

    /// The pan law that applies to `track`.
    pub fn pan_law_for(&self, track: &Track) -> PanLaw {
        if track.pan_law > 0.0 { PanLaw { law: track.pan_law, ..self.pan_law } } else { self.pan_law }
    }

    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    pub fn track_index(&self, id: TrackId) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    pub fn add_track(&mut self, name: impl Into<String>) -> TrackId {
        let mut track = Track::new(name);
        track.color = Some(TRACK_PALETTE[self.tracks.len() % TRACK_PALETTE.len()]);
        let id = track.id;
        self.tracks.push(track);
        id
    }

    pub fn remove_track(&mut self, id: TrackId) -> Option<Track> {
        let idx = self.track_index(id)?;
        for t in &mut self.tracks {
            t.sends.retain(|s| s.dest != id);
        }
        Some(self.tracks.remove(idx))
    }

    /// Parent folder of every track, derived from the folder depth deltas.
    pub fn folder_parents(&self) -> Vec<Option<usize>> {
        let mut stack: Vec<usize> = Vec::new();
        let mut parents = Vec::with_capacity(self.tracks.len());
        for (i, t) in self.tracks.iter().enumerate() {
            parents.push(stack.last().copied());
            if t.folder_depth > 0 {
                stack.push(i);
            } else {
                for _ in 0..(-t.folder_depth) {
                    stack.pop();
                }
            }
        }
        parents
    }

    /// Nesting level of every track (0 = top level).
    pub fn folder_levels(&self) -> Vec<usize> {
        let parents = self.folder_parents();
        let mut levels = vec![0usize; parents.len()];
        for i in 0..parents.len() {
            levels[i] = parents[i].map_or(0, |p| levels[p] + 1);
        }
        levels
    }

    /// Rewrites the folder depth deltas from per-track nesting levels, clamping each level to at
    /// most one deeper than the track above.
    pub fn set_folder_levels(&mut self, levels: &[usize]) {
        let mut fixed = Vec::with_capacity(levels.len());
        for (i, &l) in levels.iter().enumerate() {
            let max = if i == 0 { 0 } else { fixed[i - 1] + 1 };
            fixed.push(l.min(max));
        }
        for i in 0..self.tracks.len() {
            let next = fixed.get(i + 1).copied().unwrap_or(0) as i32;
            self.tracks[i].folder_depth = next - fixed[i] as i32;
        }
    }

    /// Moves a track one place up or down; it takes the nesting level of the place it lands on.
    pub fn move_track(&mut self, id: TrackId, up: bool) -> bool {
        let Some(i) = self.track_index(id) else { return false };
        let j = if up { i.checked_sub(1) } else { (i + 1 < self.tracks.len()).then_some(i + 1) };
        let Some(j) = j else { return false };
        let levels = self.folder_levels();
        self.tracks.swap(i, j);
        self.set_folder_levels(&levels);
        true
    }

    /// Moves a track one folder level deeper (into the folder above) or shallower.
    pub fn indent_track(&mut self, id: TrackId, deeper: bool) -> bool {
        let Some(i) = self.track_index(id) else { return false };
        let mut levels = self.folder_levels();
        let old = levels[i];
        levels[i] = if deeper { old + 1 } else { old.saturating_sub(1) };
        self.set_folder_levels(&levels);
        self.folder_levels()[i] != old
    }

    pub fn any_solo(&self) -> bool {
        self.tracks.iter().any(|t| t.solo)
    }

    /// Whether a track is heard, taking mute and exclusive solo into account. Soloing a folder
    /// solos its children, and soloing a child keeps its parent folders audible.
    pub fn is_audible(&self, track: &Track) -> bool {
        let parents = self.folder_parents();
        let Some(idx) = self.track_index(track.id) else { return !track.muted };
        let ancestors = || std::iter::successors(parents[idx], |&p| parents[p]);
        if track.muted || ancestors().any(|p| self.tracks[p].muted) {
            return false;
        }
        if !self.any_solo() || track.solo || ancestors().any(|p| self.tracks[p].solo) {
            return true;
        }
        self.tracks
            .iter()
            .enumerate()
            .any(|(i, t)| t.solo && std::iter::successors(parents[i], |&p| parents[p]).any(|p| p == idx))
    }

    /// End of the last item, i.e. the natural project length.
    pub fn length(&self) -> f64 {
        self.tracks.iter().flat_map(|t| &t.items).map(Item::end).fold(0.0, f64::max)
    }

    pub fn find_item(&self, id: ItemId) -> Option<(TrackId, &Item)> {
        self.tracks.iter().find_map(|t| t.item(id).map(|i| (t.id, i)))
    }

    /// Sets the automatic fade lengths on a track from item overlaps: overlapping neighbours
    /// crossfade over the overlap, other items lose their automatic fades.
    pub fn auto_crossfade(&mut self, track: TrackId) {
        let Some(t) = self.track_mut(track) else { return };
        let mut order: Vec<usize> = (0..t.items.len()).collect();
        order.sort_by(|&a, &b| t.items[a].position.total_cmp(&t.items[b].position));
        for it in &mut t.items {
            it.fade_in.auto_length = 0.0;
            it.fade_out.auto_length = 0.0;
        }
        for w in order.windows(2) {
            let (a, b) = (w[0], w[1]);
            let overlap = t.items[a].end().min(t.items[b].end()) - t.items[b].position;
            if overlap > 0.0 && t.items[a].ypos == t.items[b].ypos {
                t.items[a].fade_out.auto_length = overlap;
                t.items[b].fade_in.auto_length = overlap;
            }
        }
    }

    /// Splits every item on the given tracks (all tracks when `None`) that crosses `at`.
    pub fn split_items_at(&mut self, at: f64, tracks: Option<&[TrackId]>) {
        for t in self.tracks.iter_mut().filter(|t| tracks.is_none_or(|ids| ids.contains(&t.id))) {
            let mut added = Vec::new();
            for it in &mut t.items {
                added.extend(it.split_at(at));
            }
            t.items.extend(added);
        }
    }

    /// Removes everything inside `range` on the given tracks (all when `None`). With `ripple`,
    /// later items on those tracks move left to close the gap; when every track ripples, so do
    /// markers and envelope points.
    pub fn remove_range(&mut self, range: TimeRange, tracks: Option<&[TrackId]>, ripple: bool) {
        if range.is_empty() {
            return;
        }
        self.split_items_at(range.start, tracks);
        self.split_items_at(range.end, tracks);
        let len = range.length();
        for t in self.tracks.iter_mut().filter(|t| tracks.is_none_or(|ids| ids.contains(&t.id))) {
            t.items.retain(|it| it.end() <= range.start + 1e-9 || it.position >= range.end - 1e-9);
            if ripple {
                for it in t.items.iter_mut().filter(|it| it.position >= range.end - 1e-9) {
                    it.position -= len;
                }
            }
        }
        if ripple && tracks.is_none() {
            self.markers.retain(|m| m.position < range.start || m.position >= range.end);
            self.shift_after(range.end, -len);
        }
    }

    /// Opens a gap of `range` at `range.start` on every track: items there are split and
    /// everything later, markers and envelope points included, moves right.
    pub fn insert_space(&mut self, range: TimeRange) {
        if range.is_empty() {
            return;
        }
        self.split_items_at(range.start, None);
        for it in self.tracks.iter_mut().flat_map(|t| t.items.iter_mut()).filter(|it| it.position >= range.start - 1e-9)
        {
            it.position += range.length();
        }
        self.shift_after(range.start, range.length());
    }

    /// Moves markers, regions and envelope points at or after `from` by `delta`.
    fn shift_after(&mut self, from: f64, delta: f64) {
        for m in self.markers.iter_mut().filter(|m| m.position >= from - 1e-9) {
            m.position += delta;
            if let Some(end) = m.region_end.as_mut() {
                *end += delta;
            }
        }
        for env in self.tracks.iter_mut().flat_map(|t| t.envelopes.iter_mut()) {
            for p in env.points.iter_mut().filter(|p| p.time >= from - 1e-9) {
                p.time = (p.time + delta).max(0.0);
            }
        }
    }

    /// Moves an item to another track, keeping its id.
    pub fn move_item_to_track(&mut self, item: ItemId, dest: TrackId) -> bool {
        let Some(src_idx) = self.tracks.iter().position(|t| t.item(item).is_some()) else {
            return false;
        };
        let Some(dest_idx) = self.track_index(dest) else {
            return false;
        };
        if src_idx == dest_idx {
            return true;
        }
        let pos = self.tracks[src_idx].items.iter().position(|i| i.id == item).unwrap();
        let it = self.tracks[src_idx].items.remove(pos);
        self.tracks[dest_idx].items.push(it);
        true
    }

    pub fn add_marker(&mut self, position: f64, name: impl Into<String>) {
        let index = self.markers.iter().map(|m| m.index).max().unwrap_or(0) + 1;
        self.markers.push(Marker {
            index,
            position,
            name: name.into(),
            color: None,
            region_end: None,
            lines: LineMemo::new(),
        });
        self.markers.sort_by(|a, b| a.position.total_cmp(&b.position));
    }
}

/// A REAPER-style `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}` GUID. Uniqueness, not
/// unpredictability, is what matters here, so std's randomly keyed hasher is enough.
pub fn new_guid() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    let mut words = [0u64; 2];
    for (i, w) in words.iter_mut().enumerate() {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(next_id());
        h.write_usize(i);
        *w = h.finish();
    }
    let b: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
    let hex = |r: std::ops::Range<usize>| b[r].iter().map(|x| format!("{x:02X}")).collect::<String>();
    format!("{{{}-{}-{}-{}-{}}}", hex(0..4), hex(4..6), hex(6..8), hex(8..10), hex(10..16))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solo_is_exclusive() {
        let mut p = Project::default();
        let a = p.add_track("a");
        let b = p.add_track("b");
        assert!(p.is_audible(p.track(a).unwrap()));
        p.track_mut(b).unwrap().solo = true;
        assert!(!p.is_audible(p.track(a).unwrap()));
        assert!(p.is_audible(p.track(b).unwrap()));
        p.track_mut(b).unwrap().muted = true;
        assert!(!p.is_audible(p.track(b).unwrap()));
    }

    #[test]
    fn removing_track_drops_sends_to_it() {
        let mut p = Project::default();
        let a = p.add_track("a");
        let bus = p.add_track("bus");
        p.track_mut(a).unwrap().sends.push(Send::new(bus));
        p.remove_track(bus);
        assert!(p.track(a).unwrap().sends.is_empty());
    }

    #[test]
    fn move_item_between_tracks() {
        let mut p = Project::default();
        let a = p.add_track("a");
        let b = p.add_track("b");
        let item = Item::new(1.0, 2.0, Take::from_file("x.wav".into()));
        let id = item.id;
        p.track_mut(a).unwrap().items.push(item);
        assert!(p.move_item_to_track(id, b));
        assert_eq!(p.find_item(id).unwrap().0, b);
        assert_eq!(p.length(), 3.0);
    }

    #[test]
    fn folders_and_solo() {
        let mut p = Project::default();
        let bus = p.add_track("bus");
        let a = p.add_track("a");
        let b = p.add_track("b");
        let other = p.add_track("other");
        p.tracks[0].folder_depth = 1;
        p.tracks[2].folder_depth = -1;
        assert_eq!(p.folder_parents(), [None, Some(0), Some(0), None]);
        assert_eq!(p.folder_levels(), [0, 1, 1, 0]);

        p.track_mut(bus).unwrap().solo = true;
        assert!(p.is_audible(p.track(a).unwrap()));
        assert!(!p.is_audible(p.track(other).unwrap()));

        p.track_mut(bus).unwrap().solo = false;
        p.track_mut(b).unwrap().solo = true;
        assert!(p.is_audible(p.track(bus).unwrap()));
        assert!(!p.is_audible(p.track(a).unwrap()));

        p.track_mut(b).unwrap().solo = false;
        p.track_mut(bus).unwrap().muted = true;
        assert!(!p.is_audible(p.track(a).unwrap()));
    }

    #[test]
    fn trimming_keeps_audio_in_place() {
        let mut it = Item::new(10.0, 4.0, Take::from_file("x.wav".into()));
        it.takes[0].playrate = 2.0;
        it.trim_start(11.0);
        assert_eq!((it.position, it.length, it.takes[0].source_offset), (11.0, 3.0, 2.0));
        it.trim_start(9.0);
        assert_eq!((it.position, it.length, it.takes[0].source_offset), (9.0, 5.0, -2.0));
        it.trim_end(10.0);
        assert_eq!(it.length, 1.0);
        it.trim_end(0.0);
        assert_eq!(it.length, MIN_ITEM_LENGTH);
        let d = it.duplicate();
        assert!(d.id != it.id && d.guid != it.guid && d.position == it.position);
    }

    #[test]
    fn folder_editing() {
        let mut p = Project::default();
        let a = p.add_track("a");
        let b = p.add_track("b");
        let c = p.add_track("c");
        assert!(p.indent_track(b, true));
        assert_eq!(p.folder_parents(), [None, Some(0), None]);
        assert!(p.indent_track(c, true));
        assert_eq!(p.folder_parents(), [None, Some(0), Some(0)]);
        assert!(!p.indent_track(a, true), "the first track cannot be indented");
        assert!(p.move_track(c, true));
        assert_eq!(p.tracks.iter().map(|t| t.id).collect::<Vec<_>>(), [a, c, b]);
        assert_eq!(p.folder_parents(), [None, Some(0), Some(0)]);
        assert!(p.indent_track(b, false));
        assert_eq!(p.folder_parents(), [None, Some(0), None]);
        assert_eq!(p.tracks.iter().map(|t| t.folder_depth).sum::<i32>(), 0);
    }

    #[test]
    fn overlapping_items_crossfade() {
        let mut p = Project::default();
        let t = p.add_track("t");
        let tr = p.track_mut(t).unwrap();
        tr.items.push(Item::new(0.0, 2.0, Take::from_file("a.wav".into())));
        tr.items.push(Item::new(1.5, 2.0, Take::from_file("b.wav".into())));
        tr.items.push(Item::new(5.0, 1.0, Take::from_file("c.wav".into())));
        p.auto_crossfade(t);
        let items = &p.track(t).unwrap().items;
        assert_eq!((items[0].fade_out.auto_length, items[1].fade_in.auto_length), (0.5, 0.5));
        assert_eq!((items[1].fade_out.auto_length, items[2].fade_in.auto_length), (0.0, 0.0));
    }

    #[test]
    fn time_selection_editing() {
        let mut p = Project::default();
        let t = p.add_track("t");
        p.track_mut(t).unwrap().items.push(Item::new(0.0, 10.0, Take::from_file("a.wav".into())));
        p.add_marker(8.0, "late");
        p.remove_range(TimeRange::new(2.0, 5.0), None, true);
        let mut items: Vec<(f64, f64, f64)> =
            p.tracks[0].items.iter().map(|i| (i.position, i.length, i.takes[0].source_offset)).collect();
        items.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert_eq!(items, [(0.0, 2.0, 0.0), (2.0, 5.0, 5.0)]);
        assert_eq!(p.markers[0].position, 5.0);
        p.insert_space(TimeRange::new(1.0, 2.0));
        let mut starts: Vec<f64> = p.tracks[0].items.iter().map(|i| i.position).collect();
        starts.sort_by(f64::total_cmp);
        assert_eq!(starts, [0.0, 2.0, 3.0]);
        assert_eq!(p.markers[0].position, 6.0);
    }

    #[test]
    fn guid_shape() {
        let g = new_guid();
        assert_eq!(g.len(), 38);
        assert_ne!(g, new_guid());
    }
}
