//! What surfaces show, read from the session once per update: the tracks with their
//! controls and meters, the transport and the markers. Surfaces compare it with what they
//! last sent and send the difference.

use scythe_core::fader::{db_to_slider, slider_to_db};
use scythe_core::units::{db_to_gain, gain_to_db};
use scythe_core::{AutoMode, Project, Track, TrackId};
use scythe_session::api::setlist::{self, SetlistStatus};
use scythe_session::{AutoTarget, Session};

/// The level REAPER reports for silence and treats as the bottom of the fader.
pub const FLOOR_DB: f64 = -150.0;

#[derive(Debug, Clone, PartialEq)]
pub struct SendView {
    pub name: String,
    pub volume: f64,
    pub pan: f64,
    pub muted: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FxView {
    pub name: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrackView {
    /// `None` for the master.
    pub id: Option<TrackId>,
    /// 0 for the master, else the track's position from 1.
    pub number: usize,
    pub name: String,
    pub volume: f64,
    pub pan: f64,
    pub muted: bool,
    pub solo: bool,
    pub armed: bool,
    pub monitor: bool,
    pub auto_mode: AutoMode,
    /// Peak levels (gain) with the meters' decay.
    pub peak: (f32, f32),
    pub sends: Vec<SendView>,
    pub fx: Vec<FxView>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MarkerView {
    pub number: u32,
    pub name: String,
    pub position: f64,
    /// For regions.
    pub end: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub master: TrackView,
    pub tracks: Vec<TrackView>,
    pub playing: bool,
    pub paused: bool,
    pub recording: bool,
    pub position: f64,
    pub bar_beat: String,
    /// Bar, beat and ticks (0..999) of the position, for time code displays.
    pub bar_beat_ticks: (i64, u32, u32),
    pub loop_enabled: bool,
    pub loop_range: Option<(f64, f64)>,
    pub metronome: bool,
    pub tempo: f64,
    pub markers: Vec<MarkerView>,
    pub regions: Vec<MarkerView>,
    pub setlist: SetlistStatus,
    /// The track selected in Scythe.
    pub selected: Option<TrackId>,
}

impl View {
    pub fn read(session: &Session) -> Self {
        let p = &session.project;
        let peak = |id: TrackId| session.levels.get(&id).copied().unwrap_or_default();
        let master = track_view(p, &p.master, 0, session.master_level);
        let tracks = p
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let mut v = track_view(p, t, i + 1, peak(t.id));
                v.volume = session.automation_value(t.id, AutoTarget::Volume).unwrap_or(v.volume);
                v.pan = session.automation_value(t.id, AutoTarget::Pan).unwrap_or(v.pan);
                v
            })
            .collect();
        let position = session.position();
        let bb = p.tempo.bar_beat(position);
        let (markers, regions) = p
            .markers
            .iter()
            .map(|m| MarkerView { number: m.index, name: m.name.clone(), position: m.position, end: m.region_end })
            .partition(|m| m.end.is_none());
        let playing = session.is_playing();
        Self {
            master,
            tracks,
            playing,
            paused: false,
            recording: session.is_recording(),
            position,
            bar_beat: p.tempo.format_bar_beat(position),
            bar_beat_ticks: (bb.bar, bb.beat, (bb.fraction * 1000.0).floor().clamp(0.0, 999.0) as u32),
            loop_enabled: p.loop_enabled,
            loop_range: p.time_selection.map(|r| (r.start, r.end)),
            metronome: session.metronome(),
            tempo: p.tempo.bpm_at(position),
            markers,
            regions,
            setlist: setlist::status(session),
            selected: session.selected_track,
        }
    }

    /// Track `number` (0 is the master), if there is one.
    pub fn track(&self, number: usize) -> Option<&TrackView> {
        if number == 0 { Some(&self.master) } else { self.tracks.get(number - 1) }
    }

    pub fn any_solo(&self) -> bool {
        self.tracks.iter().any(|t| t.solo)
    }

    /// The last marker at or before the position.
    pub fn last_marker(&self) -> Option<&MarkerView> {
        self.markers
            .iter()
            .filter(|m| m.position <= self.position + 1e-9)
            .max_by(|a, b| a.position.total_cmp(&b.position))
    }

    /// The region the position is in.
    pub fn current_region(&self) -> Option<&MarkerView> {
        self.regions.iter().rfind(|r| r.position <= self.position + 1e-9 && r.end.is_some_and(|e| self.position < e))
    }
}

fn track_view(p: &Project, t: &Track, number: usize, peak: (f32, f32)) -> TrackView {
    let name = if number == 0 {
        "MASTER".to_owned()
    } else if t.name.is_empty() {
        format!("Track {number}")
    } else {
        t.name.clone()
    };
    let sends = t
        .sends
        .iter()
        .map(|s| SendView {
            name: p
                .track_index(s.dest)
                .and_then(|i| {
                    p.tracks.get(i).map(|d| if d.name.is_empty() { format!("Track {}", i + 1) } else { d.name.clone() })
                })
                .unwrap_or_default(),
            volume: s.volume,
            pan: s.pan,
            muted: s.muted,
        })
        .collect();
    TrackView {
        id: (number > 0).then_some(t.id),
        number,
        name,
        volume: t.volume,
        pan: t.pan,
        muted: t.muted,
        solo: t.solo,
        armed: t.armed,
        monitor: t.monitor,
        auto_mode: t.auto_mode,
        peak,
        sends,
        fx: t.fx.iter().map(|f| FxView { name: f.name.clone(), enabled: f.enabled }).collect(),
    }
}

/// A gain as REAPER's fader position, 0..1 (0 dB at 0.716, +12 dB at the top).
pub fn gain_to_fader(gain: f64) -> f64 {
    db_to_slider(gain_to_db(gain).max(FLOOR_DB)) / 1000.0
}

/// A fader position 0..1 as a gain; the bottom is silence.
pub fn fader_to_gain(position: f64) -> f64 {
    if position <= 0.0 { 0.0 } else { db_to_gain(slider_to_db(position.min(1.0) * 1000.0)) }
}

/// A level as REAPER writes it on surfaces: `0.00dB`, `-6.02dB`, `+6.02dB`, `-11.0dB` (cut,
/// not rounded), `-inf dB`.
pub fn db_text(gain: f64) -> String {
    let db = gain_to_db(gain);
    if db <= FLOOR_DB || !db.is_finite() {
        "-inf dB".to_owned()
    } else if db.abs() >= 10.0 {
        let db = (db * 10.0).trunc() / 10.0;
        format!("{}{db:.1}dB", if db > 0.0 { "+" } else { "" })
    } else {
        let db = (db * 100.0).trunc() / 100.0;
        format!("{}{:.2}dB", if db > 0.0 { "+" } else { "" }, if db == 0.0 { 0.0 } else { db })
    }
}

/// A gain in dB, bottoming out at REAPER's floor.
pub fn db_value(gain: f64) -> f64 {
    gain_to_db(gain).clamp(FLOOR_DB, 24.0)
}

/// A track pan as REAPER writes it on surfaces: `C`, `25%L`, `50%R`.
pub fn pan_text(pan: f64) -> String {
    let pct = (pan.abs() * 100.0).round();
    if pct == 0.0 { "C".to_owned() } else { format!("{pct:.0}%{}", if pan < 0.0 { 'L' } else { 'R' }) }
}

/// A send's pan as REAPER writes it: `center`, `25%L`, `50%R`.
pub fn send_pan_text(pan: f64) -> String {
    if (pan.abs() * 100.0).round() == 0.0 { "center".to_owned() } else { pan_text(pan) }
}

/// A peak level as a meter position 0..1, linear in dB from the floor of `range_db` below 0 dB
/// to 0 dB.
pub fn meter_position(peak: f32, range_db: f64) -> f64 {
    if peak <= 0.0 {
        return 0.0;
    }
    ((gain_to_db(peak as f64) + range_db) / range_db).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts_and_positions_as_reaper_sends_them() {
        // Values REAPER 7.73 sent an OSC device for tracks at 1.0, 0.5 and 2.0 gain and for an
        // empty bank slot.
        assert!((gain_to_fader(1.0) - 0.716).abs() < 1e-9);
        assert!((gain_to_fader(0.5) - 0.592449).abs() < 5e-5);
        assert!((gain_to_fader(2.0) - 0.852774).abs() < 5e-5);
        assert!((gain_to_fader(0.0) - 0.002514).abs() < 1e-6);
        assert_eq!(db_text(1.0), "0.00dB");
        assert_eq!(db_text(0.5), "-6.02dB");
        assert_eq!(db_text(2.0), "+6.02dB");
        assert_eq!(db_text(0.0), "-inf dB");
        assert_eq!(db_text(db_to_gain(-11.059349)), "-11.0dB");
        assert_eq!(db_text(db_to_gain(-12.0)), "-12.0dB");
        assert!((db_value(0.5) + 6.0206).abs() < 1e-4);
        assert_eq!(db_value(0.0), -150.0);
        assert_eq!(pan_text(0.0), "C");
        assert_eq!(pan_text(-0.25), "25%L");
        assert_eq!(pan_text(0.5), "50%R");
        assert_eq!(send_pan_text(0.0), "center");
        assert!((fader_to_gain(gain_to_fader(0.5)) - 0.5).abs() < 1e-9);
        assert_eq!(fader_to_gain(0.0), 0.0);
    }
}
