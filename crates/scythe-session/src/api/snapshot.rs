//! The project as clients see it: plain JSON-friendly copies of what the model holds.

use scythe_core::units::gain_to_db;
use scythe_core::{Color, Item, Marker, Track};
use scythe_engine::fx::FxStatus;
use serde::{Deserialize, Serialize};

use super::Target;
use crate::Session;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectSnapshot {
    pub revision: u64,
    pub name: String,
    pub path: Option<String>,
    pub dirty: bool,
    pub sample_rate: u32,
    /// End of the last item, in seconds.
    pub length: f64,
    pub bpm: f64,
    pub time_signature: (u32, u32),
    pub undo: Option<String>,
    pub redo: Option<String>,
    pub transport: TransportState,
    pub master: TrackInfo,
    pub tracks: Vec<TrackInfo>,
    pub markers: Vec<MarkerInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransportState {
    pub playing: bool,
    pub recording: bool,
    /// The play position while playing, else the edit cursor, in seconds.
    pub position: f64,
    pub cursor: f64,
    pub loop_enabled: bool,
    /// The time selection the loop repeats, `[start, end]` in seconds.
    pub loop_range: Option<(f64, f64)>,
    pub metronome: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackInfo {
    /// Stable for the session; `0` for the master.
    pub id: u64,
    /// Position from the top, `None` for the master.
    pub index: Option<usize>,
    pub guid: String,
    pub name: String,
    /// `#rrggbb`.
    pub color: Option<String>,
    /// Gain, 1.0 = 0 dB.
    pub volume: f64,
    pub volume_db: f64,
    /// -1 (left) to 1 (right).
    pub pan: f64,
    pub muted: bool,
    pub solo: bool,
    pub armed: bool,
    pub monitor: bool,
    /// REAPER's record input (`I_RECINPUT`), inputs from 0: mono input n is n, stereo pair from n is
    /// 1024 + n, MIDI is 4096 + device * 32 + channel; -1 is none.
    pub input: i64,
    /// REAPER's folder depth change (`I_FOLDERDEPTH`): 1 opens a folder, -n closes n.
    pub folder_depth: i32,
    /// How deep in folders the track sits, 0 at the top.
    pub level: usize,
    pub channels: u32,
    pub fx: Vec<FxInfo>,
    pub sends: Vec<SendInfo>,
    pub items: Vec<ItemInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FxInfo {
    pub index: usize,
    pub name: String,
    pub enabled: bool,
    /// Whether it processes audio (built in or hosted) rather than passing it through.
    pub running: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendInfo {
    pub index: usize,
    /// The receiving track's id.
    pub dest: u64,
    pub volume: f64,
    pub volume_db: f64,
    pub pan: f64,
    pub muted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemInfo {
    pub id: u64,
    pub track: u64,
    pub name: String,
    pub position: f64,
    pub length: f64,
    pub volume: f64,
    pub muted: bool,
    pub takes: usize,
    pub active_take: usize,
    /// The active take's media file, when it plays one.
    pub file: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarkerInfo {
    /// REAPER's marker or region number.
    pub number: u32,
    pub region: bool,
    pub name: String,
    pub position: f64,
    /// Where a region ends.
    pub end: Option<f64>,
    pub color: Option<String>,
}

pub fn color_hex(c: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

pub fn parse_color(text: &str) -> Option<Color> {
    let hex = text.strip_prefix('#').unwrap_or(text);
    (hex.len() == 6).then(|| u32::from_str_radix(hex, 16).ok().map(Color::from_hex)).flatten()
}

pub fn snapshot(session: &Session) -> ProjectSnapshot {
    let p = &session.project;
    let levels = p.folder_levels();
    ProjectSnapshot {
        revision: session.revision(),
        name: p.name(),
        path: p.path.as_ref().map(|p| p.display().to_string()),
        dirty: session.dirty,
        sample_rate: session.engine.sample_rate(),
        length: p.length(),
        bpm: p.tempo.bpm_at(0.0),
        time_signature: p.tempo.time_sig_at(0.0),
        undo: session.history.undo_label().map(Into::into),
        redo: session.history.redo_label().map(Into::into),
        transport: transport(session),
        master: track(session, &p.master, None, 0),
        tracks: p.tracks.iter().enumerate().map(|(i, t)| track(session, t, Some(i), levels[i])).collect(),
        markers: p.markers.iter().map(marker).collect(),
    }
}

pub fn transport(session: &Session) -> TransportState {
    let p = &session.project;
    TransportState {
        playing: session.is_playing(),
        recording: session.is_recording(),
        position: session.position(),
        cursor: p.cursor,
        loop_enabled: p.loop_enabled,
        loop_range: p.time_selection.filter(|r| !r.is_empty()).map(|r| (r.start, r.end)),
        metronome: session.metronome,
    }
}

pub fn track(session: &Session, t: &Track, index: Option<usize>, level: usize) -> TrackInfo {
    TrackInfo {
        id: if index.is_some() { t.id.0 } else { 0 },
        index,
        guid: t.guid.clone(),
        name: t.name.clone(),
        color: t.color.map(color_hex),
        volume: t.volume,
        volume_db: gain_to_db(t.volume),
        pan: t.pan,
        muted: t.muted,
        solo: t.solo,
        armed: t.armed,
        monitor: t.monitor,
        input: t.rec_input,
        folder_depth: t.folder_depth,
        level,
        channels: t.channels,
        fx: fx_list(session, if index.is_some() { Target::Track(t.id) } else { Target::Master }, t),
        sends: send_list(t),
        items: t.items.iter().map(|it| item(t, it)).collect(),
    }
}

pub fn fx_list(session: &Session, target: Target, t: &Track) -> Vec<FxInfo> {
    let track = match target {
        Target::Master => None,
        Target::Track(id) => Some(id),
    };
    t.fx.iter()
        .enumerate()
        .map(|(i, slot)| FxInfo {
            index: i,
            name: slot.name.clone(),
            enabled: slot.enabled,
            running: !matches!(session.engine.fx().status(track, slot, i), None | Some(FxStatus::Unavailable(_))),
        })
        .collect()
}

pub fn send_list(t: &Track) -> Vec<SendInfo> {
    t.sends
        .iter()
        .enumerate()
        .map(|(i, s)| SendInfo {
            index: i,
            dest: s.dest.0,
            volume: s.volume,
            volume_db: gain_to_db(s.volume),
            pan: s.pan,
            muted: s.muted,
        })
        .collect()
}

pub fn item(t: &Track, it: &Item) -> ItemInfo {
    ItemInfo {
        id: it.id.0,
        track: t.id.0,
        name: it.name().to_owned(),
        position: it.position,
        length: it.length,
        volume: it.volume,
        muted: it.muted,
        takes: it.takes.len(),
        active_take: it.active_take,
        file: it.take().and_then(|tk| tk.source.file()).map(|f| f.display().to_string()),
    }
}

pub fn marker(m: &Marker) -> MarkerInfo {
    MarkerInfo {
        number: m.index,
        region: m.region_end.is_some(),
        name: m.name.clone(),
        position: m.position,
        end: m.region_end,
        color: m.color.map(color_hex),
    }
}
