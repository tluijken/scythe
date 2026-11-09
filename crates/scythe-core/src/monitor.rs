//! Monitor mixes ("cue buses") for in-ear monitoring. A monitor mix is a plain REAPER track: it
//! receives the sources it mixes (pre-fader post-FX by default), does not feed the master, and
//! plays through its own hardware output. Track extension values mark it and hold what REAPER
//! has no field for (whose mix it is, which sources are the musician's own, the "more me"
//! amount), so a project stays a REAPER project and REAPER plays the mixes the same way.

use crate::model::{Color, FxSlot, HwOut, Project, Send, SendMode, TRACK_PALETTE, Track, TrackId, new_guid};
use crate::rpp::{Chunk, Node};
use crate::units::db_to_gain;

/// Extension key that marks a monitor mix; the value is the musician's name.
pub const EXT_MUSICIAN: &str = "scythe_monitor";
/// GUIDs of the musician's own source tracks, space separated.
pub const EXT_OWN: &str = "scythe_monitor_own";
/// The "more me" amount in dB.
pub const EXT_MORE_ME: &str = "scythe_monitor_more_me";
/// Monitor mix tracks are named after the musician with this prefix.
pub const NAME_PREFIX: &str = "MON ";
/// Range of the "more me" control in dB.
pub const MORE_ME_RANGE: std::ops::RangeInclusive<f64> = -24.0..=24.0;

/// Where a monitor mix plays: device outputs, 0-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorOutput {
    /// Outputs `n` and `n + 1`.
    Stereo(u32),
    /// The mix summed to output `n`.
    Mono(u32),
}

impl MonitorOutput {
    pub fn hw_out(self) -> HwOut {
        match self {
            Self::Stereo(n) => HwOut::stereo(n),
            Self::Mono(n) => HwOut::mono(n),
        }
    }

    /// The output a hardware output plays to, when it is a plain stereo or mono one.
    pub fn from_hw_out(o: &HwOut) -> Option<Self> {
        if o.is_rearoute() || o.src_channels < 0 {
            return None;
        }
        let first = o.first_output() as u32;
        Some(if o.is_mono() { Self::Mono(first) } else { Self::Stereo(first) })
    }

    /// Device outputs used, 0-based.
    pub fn channels(self) -> std::ops::Range<u32> {
        match self {
            Self::Stereo(n) => n..n + 2,
            Self::Mono(n) => n..n + 1,
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::Stereo(n) => format!("Out {}/{}", n + 1, n + 2),
            Self::Mono(n) => format!("Out {} (mono)", n + 1),
        }
    }
}

/// How a cue bus is made, after SWS's "Create cue buss from track selection" settings.
#[derive(Debug, Clone, PartialEq)]
pub struct CueBusOptions {
    pub name: String,
    /// Where each receive taps its source.
    pub mode: SendMode,
    /// Start each receive at its source's fader volume instead of 0 dB.
    pub volumes_from_tracks: bool,
    /// Start each receive at its source's pan instead of centre.
    pub pans_from_tracks: bool,
    pub output: Option<MonitorOutput>,
    /// Whether the bus also feeds the master (off for an in-ear mix).
    pub send_to_master: bool,
    /// A track (from a track template) the bus starts from: its FX, volume, colour and so on.
    pub template: Option<Track>,
    /// Effects for the bus when there is no template, such as a safety limiter.
    pub fx: Vec<FxSlot>,
}

impl Default for CueBusOptions {
    fn default() -> Self {
        Self {
            name: "Cue Buss".to_owned(),
            mode: SendMode::PostFx,
            volumes_from_tracks: false,
            pans_from_tracks: false,
            output: None,
            send_to_master: false,
            template: None,
            fx: Vec::new(),
        }
    }
}

/// A monitor mix as found in the project.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorMix {
    pub track: TrackId,
    pub musician: String,
    /// The musician's own sources, which "more me" raises.
    pub own: Vec<TrackId>,
    pub more_me: f64,
    /// The first hardware output, when it is a plain stereo or mono one.
    pub output: Option<MonitorOutput>,
}

/// One source of a monitor mix: the receive from `track`.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorSource {
    pub track: TrackId,
    pub volume: f64,
    pub pan: f64,
    pub muted: bool,
    pub mode: SendMode,
    pub own: bool,
}

/// Gain the "more me" amount gives the musician's own sources; the others get its inverse, so
/// the mix stays about as loud.
fn more_me_gain(db: f64, own: bool) -> f64 {
    db_to_gain(if own { db / 2.0 } else { -db / 2.0 })
}

impl Project {
    /// Whether `id` is a monitor mix.
    pub fn is_monitor_mix(&self, id: TrackId) -> bool {
        self.track(id).is_some_and(|t| t.ext(EXT_MUSICIAN).is_some())
    }

    /// Every monitor mix, in track order.
    pub fn monitor_mixes(&self) -> Vec<MonitorMix> {
        self.tracks.iter().filter_map(|t| self.monitor_mix(t.id)).collect()
    }

    pub fn monitor_mix(&self, id: TrackId) -> Option<MonitorMix> {
        let t = self.track(id)?;
        let musician = t.ext(EXT_MUSICIAN)?;
        let own = t
            .ext(EXT_OWN)
            .unwrap_or_default()
            .split_whitespace()
            .filter_map(|g| self.tracks.iter().find(|s| s.guid == g).map(|s| s.id))
            .collect();
        Some(MonitorMix {
            track: id,
            musician,
            own,
            more_me: t.ext(EXT_MORE_ME).and_then(|v| v.parse().ok()).unwrap_or(0.0),
            output: t.hw_outs.first().and_then(MonitorOutput::from_hw_out),
        })
    }

    /// Tracks a new monitor mix takes by default: everything except monitor mixes and folder
    /// parents, whose children are already in the mix.
    pub fn monitor_candidates(&self) -> Vec<TrackId> {
        self.tracks.iter().filter(|t| t.ext(EXT_MUSICIAN).is_none() && t.folder_depth <= 0).map(|t| t.id).collect()
    }

    /// The receives of a monitor mix (or any track), in source track order.
    pub fn monitor_sources(&self, mix: TrackId) -> Vec<MonitorSource> {
        let own = self.monitor_mix(mix).map(|m| m.own).unwrap_or_default();
        self.tracks
            .iter()
            .filter_map(|t| {
                let s = t.sends.iter().find(|s| s.dest == mix)?;
                Some(MonitorSource {
                    track: t.id,
                    volume: s.volume,
                    pan: s.pan,
                    muted: s.muted,
                    mode: s.mode,
                    own: own.contains(&t.id),
                })
            })
            .collect()
    }

    /// The lowest device output (0-based) from which `width` outputs are not used by any
    /// hardware output yet; the master's default output counts as its channel count.
    pub fn next_free_output(&self, width: u32) -> u32 {
        let mut used: Vec<std::ops::Range<usize>> =
            self.tracks.iter().flat_map(|t| &t.hw_outs).map(HwOut::outputs).collect();
        if self.master_default_output() {
            used.push(0..self.master.channels.max(2) as usize);
        } else {
            used.extend(self.master.hw_outs.iter().map(HwOut::outputs));
        }
        let free = |n: u32| {
            let want = n as usize..(n + width) as usize;
            used.iter().all(|u| u.end <= want.start || u.start >= want.end)
        };
        let step = width.clamp(1, 2);
        (0..).step_by(step as usize).find(|&n| free(n)).unwrap_or(0)
    }

    /// A bus receiving `sources` the way SWS's "Create cue buss from track selection" makes
    /// one, appended after the last track. Returns the new track.
    pub fn create_cue_bus(&mut self, sources: &[TrackId], opts: &CueBusOptions) -> TrackId {
        let mut bus = match &opts.template {
            Some(t) => Track { id: TrackId::new(), guid: new_guid(), folder_depth: 0, ..t.clone() },
            None => {
                let mut t = Track::new("");
                t.color = Some(TRACK_PALETTE[self.tracks.len() % TRACK_PALETTE.len()]);
                t.fx = opts.fx.clone();
                t
            }
        };
        bus.name = opts.name.clone();
        bus.items.clear();
        bus.sends.clear();
        bus.main_send = opts.send_to_master;
        if let Some(out) = opts.output {
            bus.hw_outs = vec![out.hw_out()];
        }
        let id = bus.id;
        // The new track goes outside any folder the last track closes into.
        if let Some(last) = self.tracks.last_mut()
            && last.folder_depth > 0
        {
            last.folder_depth = 0;
        }
        self.tracks.push(bus);
        for &src in sources {
            if src == id || self.is_monitor_mix(src) {
                continue;
            }
            let Some(t) = self.track(src) else { continue };
            let send = Send {
                volume: if opts.volumes_from_tracks { t.volume } else { 1.0 },
                pan: if opts.pans_from_tracks { t.pan } else { 0.0 },
                mode: opts.mode,
                ..Send::new(id)
            };
            self.track_mut(src).unwrap().sends.push(send);
        }
        id
    }

    /// A monitor mix for `musician` hearing `sources`, of which `own` are the musician's own.
    /// `opts.name` is ignored: the track is named after the musician.
    pub fn create_monitor_mix(
        &mut self,
        musician: &str,
        sources: &[TrackId],
        own: &[TrackId],
        opts: &CueBusOptions,
    ) -> TrackId {
        let opts = CueBusOptions { name: format!("{NAME_PREFIX}{musician}"), ..opts.clone() };
        let id = self.create_cue_bus(sources, &opts);
        let t = self.track_mut(id).unwrap();
        t.set_ext(EXT_MUSICIAN, Some(musician));
        self.write_own(id, own);
        id
    }

    fn write_own(&mut self, mix: TrackId, own: &[TrackId]) {
        let guids: Vec<String> = self.tracks.iter().filter(|t| own.contains(&t.id)).map(|t| t.guid.clone()).collect();
        let value = guids.join(" ");
        if let Some(t) = self.track_mut(mix) {
            t.set_ext(EXT_OWN, (!value.is_empty()).then_some(value.as_str()));
        }
    }

    fn monitor_send_mut(&mut self, mix: TrackId, source: TrackId) -> Option<&mut Send> {
        self.track_mut(source)?.sends.iter_mut().find(|s| s.dest == mix)
    }

    /// Sets the level of one source in a mix (the receive's volume, as a gain).
    pub fn set_monitor_level(&mut self, mix: TrackId, source: TrackId, volume: f64) -> bool {
        self.monitor_send_mut(mix, source).map(|s| s.volume = volume.max(0.0)).is_some()
    }

    pub fn set_monitor_pan(&mut self, mix: TrackId, source: TrackId, pan: f64) -> bool {
        self.monitor_send_mut(mix, source).map(|s| s.pan = pan.clamp(-1.0, 1.0)).is_some()
    }

    pub fn set_monitor_muted(&mut self, mix: TrackId, source: TrackId, muted: bool) -> bool {
        self.monitor_send_mut(mix, source).map(|s| s.muted = muted).is_some()
    }

    /// Adds `source` to a mix at the level "more me" gives it. False when it is already there
    /// or either track is missing.
    pub fn add_monitor_source(&mut self, mix: TrackId, source: TrackId, mode: SendMode) -> bool {
        if source == mix || self.track(mix).is_none() || self.monitor_send_mut(mix, source).is_some() {
            return false;
        }
        let gain = self.monitor_mix(mix).map_or(1.0, |m| more_me_gain(m.more_me, m.own.contains(&source)));
        let Some(t) = self.track_mut(source) else { return false };
        t.sends.push(Send { mode, volume: gain, ..Send::new(mix) });
        true
    }

    pub fn remove_monitor_source(&mut self, mix: TrackId, source: TrackId) -> bool {
        let Some(t) = self.track_mut(source) else { return false };
        let before = t.sends.len();
        t.sends.retain(|s| s.dest != mix);
        t.sends.len() != before
    }

    /// Sets the "more me" amount: the musician's own sources sit `db / 2` above where the
    /// levels put them and the rest `db / 2` below. The receive volumes themselves change, so
    /// REAPER plays the mix the same way.
    pub fn set_more_me(&mut self, mix: TrackId, db: f64) -> bool {
        let Some(m) = self.monitor_mix(mix) else { return false };
        let db = db.clamp(*MORE_ME_RANGE.start(), *MORE_ME_RANGE.end());
        let delta = db - m.more_me;
        for t in &mut self.tracks {
            let own = m.own.contains(&t.id);
            for s in t.sends.iter_mut().filter(|s| s.dest == mix) {
                s.volume *= more_me_gain(delta, own);
            }
        }
        let value = if db == 0.0 { None } else { Some(crate::rpp::fmt_num((db * 100.0).round() / 100.0)) };
        self.track_mut(mix).unwrap().set_ext(EXT_MORE_ME, value.as_deref());
        true
    }

    /// Changes which sources are the musician's own, keeping the "more me" amount.
    pub fn set_monitor_own(&mut self, mix: TrackId, own: &[TrackId]) -> bool {
        let Some(m) = self.monitor_mix(mix) else { return false };
        self.set_more_me(mix, 0.0);
        self.write_own(mix, own);
        self.set_more_me(mix, m.more_me)
    }

    pub fn rename_monitor_mix(&mut self, mix: TrackId, musician: &str) -> bool {
        if !self.is_monitor_mix(mix) {
            return false;
        }
        let t = self.track_mut(mix).unwrap();
        t.name = format!("{NAME_PREFIX}{musician}");
        t.set_ext(EXT_MUSICIAN, Some(musician));
        true
    }

    /// Replaces the track's first hardware output (or removes it with `None`); further outputs
    /// stay.
    pub fn set_monitor_output(&mut self, mix: TrackId, output: Option<MonitorOutput>) -> bool {
        let Some(t) = self.track_mut(mix) else { return false };
        match (output, t.hw_outs.is_empty()) {
            (Some(o), true) => t.hw_outs.push(o.hw_out()),
            (Some(o), false) => {
                let old = &mut t.hw_outs[0];
                let new = o.hw_out();
                old.dst_channels = new.dst_channels;
                old.src_channels = new.src_channels;
            }
            (None, true) => {}
            (None, false) => {
                t.hw_outs.remove(0);
            }
        }
        true
    }

    /// Removes a monitor mix and the receives into it.
    pub fn delete_monitor_mix(&mut self, mix: TrackId) -> bool {
        self.is_monitor_mix(mix) && self.remove_track(mix).is_some()
    }

    /// A copy of a mix for `musician`, as a starting point: the same sources, levels, effects
    /// and own sources, on the next free output pair. Returns the new track.
    pub fn copy_monitor_mix(&mut self, mix: TrackId, musician: &str) -> Option<TrackId> {
        let m = self.monitor_mix(mix)?;
        let mut copy = self.track(mix)?.clone();
        copy.id = TrackId::new();
        copy.guid = new_guid();
        copy.name = format!("{NAME_PREFIX}{musician}");
        copy.folder_depth = 0;
        copy.color = Some(next_color(copy.color));
        copy.set_ext(EXT_MUSICIAN, Some(musician));
        if let Some(out) = m.output {
            let width = out.channels().len() as u32;
            let first = self.next_free_output(width);
            copy.hw_outs[0] = HwOut {
                raw: None,
                trailing: Vec::new(),
                ..match out {
                    MonitorOutput::Stereo(_) => MonitorOutput::Stereo(first),
                    MonitorOutput::Mono(_) => MonitorOutput::Mono(first),
                }
                .hw_out()
            };
        }
        let id = copy.id;
        if let Some(last) = self.tracks.last_mut()
            && last.folder_depth > 0
        {
            last.folder_depth = 0;
        }
        self.tracks.push(copy);
        for t in &mut self.tracks {
            if let Some(s) = t.sends.iter().find(|s| s.dest == mix).cloned() {
                t.sends.push(Send { dest: id, raw: None, ..s });
            }
        }
        Some(id)
    }
}

/// The safety limiter monitor mixes get: REAPER's stock "Master Limiter" JSFX.
pub const LIMITER: &str = "loser/masterLimiter";

/// REAPER's Master Limiter with its threshold and ceiling at -0.1 dB, so it only catches peaks.
pub fn limiter_slot() -> FxSlot {
    let mut js = Chunk::new("JS", &[LIMITER, ""]);
    let mut params = vec!["-0.1", "200", "100", "0", "250", "-0.1", "0"];
    params.resize(64, "-");
    js.line(params);
    FxSlot {
        name: LIMITER.to_owned(),
        enabled: true,
        raw: vec![
            Node::Line("BYPASS 0 0 0".into()),
            Node::Chunk(js),
            Node::Line("FLOATPOS 0 0 0 0".into()),
            Node::Line(format!("FXID {}", new_guid())),
            Node::Line("WAK 0 0".into()),
        ],
    }
}

impl Project {
    /// Whether the mix's safety limiter is on; `None` when it has none.
    pub fn monitor_limiter(&self, mix: TrackId) -> Option<bool> {
        self.track(mix)?.fx.iter().find(|f| f.name == LIMITER).map(|f| f.enabled)
    }

    /// Turns the safety limiter on or off, adding it at the end of the chain when missing.
    pub fn set_monitor_limiter(&mut self, mix: TrackId, on: bool) -> bool {
        let Some(t) = self.track_mut(mix) else { return false };
        match t.fx.iter_mut().find(|f| f.name == LIMITER) {
            Some(f) => f.enabled = on,
            None if on => t.fx.push(limiter_slot()),
            None => {}
        }
        true
    }
}

fn next_color(c: Option<Color>) -> Color {
    let i = c.and_then(|c| TRACK_PALETTE.iter().position(|p| *p == c)).map_or(0, |i| i + 1);
    TRACK_PALETTE[i % TRACK_PALETTE.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpp::{project_from_rpp, project_to_rpp};

    fn band() -> (Project, Vec<TrackId>) {
        let mut p = Project::default();
        let ids = ["Kick", "Snare", "Bass", "Guitar", "Vocal", "Click"].map(|n| p.add_track(n)).to_vec();
        p.track_mut(ids[2]).unwrap().volume = 0.5;
        p.track_mut(ids[2]).unwrap().pan = -0.25;
        (p, ids)
    }

    fn level(p: &Project, mix: TrackId, src: TrackId) -> f64 {
        p.monitor_sources(mix).iter().find(|s| s.track == src).unwrap().volume
    }

    #[test]
    fn cue_bus_like_sws() {
        let (mut p, ids) = band();
        let opts = CueBusOptions {
            volumes_from_tracks: true,
            pans_from_tracks: true,
            output: Some(MonitorOutput::Stereo(2)),
            ..CueBusOptions::default()
        };
        let bus = p.create_cue_bus(&ids[1..3], &opts);
        let t = p.track(bus).unwrap();
        assert_eq!(t.name, "Cue Buss");
        assert!(!t.main_send);
        assert_eq!(t.hw_outs[0].label(), "Out 3/4");
        assert!(!p.is_monitor_mix(bus), "a plain cue bus carries no monitor marks");
        let src = p.monitor_sources(bus);
        assert_eq!(src.iter().map(|s| s.track).collect::<Vec<_>>(), &ids[1..3]);
        assert_eq!((src[1].volume, src[1].pan, src[1].mode), (0.5, -0.25, SendMode::PostFx));
        assert_eq!((src[0].volume, src[0].pan), (1.0, 0.0));
    }

    #[test]
    fn monitor_mix_operations() {
        let (mut p, ids) = band();
        let opts = CueBusOptions { output: Some(MonitorOutput::Stereo(2)), ..CueBusOptions::default() };
        let all = p.monitor_candidates();
        assert_eq!(all, ids);
        let drums = p.create_monitor_mix("Drummer", &all, &ids[..2], &opts);
        let m = p.monitor_mix(drums).unwrap();
        assert_eq!((m.musician.as_str(), m.own.as_slice(), m.more_me), ("Drummer", &ids[..2], 0.0));
        assert_eq!(p.track(drums).unwrap().name, "MON Drummer");
        assert_eq!(m.output, Some(MonitorOutput::Stereo(2)));
        assert_eq!(p.monitor_candidates(), ids, "monitor mixes are not sources");

        assert!(p.set_monitor_level(drums, ids[5], 0.25));
        assert!(p.set_monitor_pan(drums, ids[4], 0.5));
        assert!(p.set_monitor_muted(drums, ids[3], true));
        assert!(p.set_more_me(drums, 6.0));
        let half = db_to_gain(3.0);
        assert!((level(&p, drums, ids[0]) - half).abs() < 1e-12);
        assert!((level(&p, drums, ids[5]) - 0.25 / half).abs() < 1e-12);
        assert_eq!(p.monitor_mix(drums).unwrap().more_me, 6.0);
        // Back to zero restores the levels.
        p.set_more_me(drums, 0.0);
        assert!((level(&p, drums, ids[0]) - 1.0).abs() < 1e-12);
        assert!((level(&p, drums, ids[5]) - 0.25).abs() < 1e-12);
        assert_eq!(p.track(drums).unwrap().ext(EXT_MORE_ME), None);

        p.set_more_me(drums, 4.0);
        p.set_monitor_own(drums, &ids[4..5]);
        assert!((level(&p, drums, ids[0]) - db_to_gain(-2.0)).abs() < 1e-12);
        assert!((level(&p, drums, ids[4]) - db_to_gain(2.0)).abs() < 1e-12);

        // A source added later comes in at the level "more me" gives it.
        p.remove_monitor_source(drums, ids[1]);
        assert!(p.monitor_sources(drums).iter().all(|s| s.track != ids[1]));
        assert!(p.add_monitor_source(drums, ids[1], SendMode::PostFx));
        assert!((level(&p, drums, ids[1]) - db_to_gain(-2.0)).abs() < 1e-12);
        assert!(!p.add_monitor_source(drums, ids[1], SendMode::PostFx));

        let singer = p.copy_monitor_mix(drums, "Singer").unwrap();
        assert_eq!(p.monitor_mix(singer).unwrap().output, Some(MonitorOutput::Stereo(4)));
        assert_eq!(p.monitor_sources(singer).len(), p.monitor_sources(drums).len());
        assert_eq!(p.monitor_mix(singer).unwrap().own, &ids[4..5]);
        p.set_monitor_level(singer, ids[0], 0.1);
        assert!((level(&p, drums, ids[0]) - db_to_gain(-2.0)).abs() < 1e-12, "copies are independent");

        assert!(p.rename_monitor_mix(singer, "Lead singer"));
        assert_eq!(p.track(singer).unwrap().name, "MON Lead singer");
        assert!(p.set_monitor_output(singer, Some(MonitorOutput::Mono(7))));
        assert_eq!(p.track(singer).unwrap().hw_outs[0].label(), "Out 8 (mono)");
        assert_eq!(p.next_free_output(2), 4);

        // Everything survives a save, and REAPER's lines carry it.
        let text = project_to_rpp(&p);
        assert!(text.contains("HWOUT 1031 0 1 0 0 0 0 -1:U -1"), "{text}");
        assert!(text.contains("scythe_monitor \"Lead singer\""), "{text}");
        let back = project_from_rpp(&text).unwrap();
        let mixes = back.monitor_mixes();
        assert_eq!(mixes.len(), 2);
        assert_eq!(mixes[1].musician, "Lead singer");
        assert_eq!(mixes[0].more_me, 4.0);
        assert_eq!(mixes[0].own, [back.tracks[4].id]);
        let src = back.monitor_sources(mixes[0].track);
        assert_eq!(src.len(), 6);
        assert!(src.iter().all(|s| s.mode == SendMode::PostFx));
        assert!(src.iter().any(|s| s.muted));

        assert!(p.delete_monitor_mix(drums));
        assert!(p.tracks.iter().all(|t| t.sends.iter().all(|s| s.dest != drums)));
        assert!(!p.delete_monitor_mix(ids[0]), "only monitor mixes are deleted this way");
    }

    #[test]
    fn templates_and_folders() {
        let (mut p, ids) = band();
        p.tracks[5].folder_depth = 1;
        let mut template = Track::new("tpl");
        template.volume = 0.5;
        template.hw_outs = vec![HwOut::stereo(6)];
        let bus = p.create_cue_bus(&ids, &CueBusOptions { template: Some(template), ..CueBusOptions::default() });
        assert_eq!(p.tracks[5].folder_depth, 0, "the bus does not land inside a folder");
        let t = p.track(bus).unwrap();
        assert_eq!((t.volume, t.hw_outs[0].first_output(), t.name.as_str()), (0.5, 6, "Cue Buss"));
        assert_eq!(p.next_free_output(2), 2);
        assert_eq!(p.next_free_output(1), 2);
    }
}
