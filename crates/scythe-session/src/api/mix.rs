//! Monitor mixes (in-ear "cue buses"), one per musician: the `mix.*` commands a phone or
//! tablet page uses to build its own mix. The mixes are plain REAPER tracks; see
//! `scythe_core::monitor`.

use schemars::JsonSchema;
use scythe_core::monitor::{CueBusOptions, MonitorMix, MonitorOutput};
use scythe_core::units::gain_to_db;
use scythe_core::{Project, SendMode, TrackId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::snapshot::Tap;
use super::{ApiError, TrackRef, json, level, pan};
use crate::Session;

/// `mix` names a monitor mix by its track's id or name ("MON Drummer"); `source` is a track it
/// receives. `mode` is where sources are tapped: `post_fx` (pre-fader, the default for mixes),
/// `pre_fx` or `post_fader`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum MixCommand {
    /// Every monitor mix, with its sources.
    #[serde(rename = "mix.list")]
    List,
    /// One monitor mix with its sources.
    #[serde(rename = "mix.get")]
    Get { mix: TrackRef },
    /// A mix for `musician` from `sources` (every track that is not a monitor mix or folder
    /// parent when left out), `own` of them the musician's own. Plays pre-fader post-FX
    /// (`mode`) to device output `output` (from 0; the next free pair or channel when left
    /// out), with a safety limiter unless `limiter` is false. Answers with the new mix.
    #[serde(rename = "mix.create")]
    Create {
        musician: String,
        /// The tracks the mix receives.
        #[serde(default)]
        sources: Option<Vec<TrackRef>>,
        /// Sources that are the musician's own, which "more me" raises.
        #[serde(default)]
        own: Vec<TrackRef>,
        /// The first device output, from 0.
        #[serde(default)]
        output: Option<usize>,
        #[serde(default)]
        mono: bool,
        #[serde(default)]
        mode: Option<Tap>,
        /// Start each source at its track's fader level instead of 0 dB.
        #[serde(default)]
        levels_from_tracks: bool,
        /// Start each source at its track's pan instead of centre.
        #[serde(default)]
        pans_from_tracks: bool,
        #[serde(default)]
        limiter: Option<bool>,
    },
    /// A copy of `mix` for another musician, on the next free output.
    #[serde(rename = "mix.copy")]
    Copy { mix: TrackRef, musician: String },
    /// Renames the mix for another musician.
    #[serde(rename = "mix.rename")]
    Rename { mix: TrackRef, musician: String },
    /// Deletes the mix (its track and the sends feeding it).
    #[serde(rename = "mix.delete")]
    Delete { mix: TrackRef },
    /// The level of one source in the mix.
    #[serde(rename = "mix.level")]
    Level {
        mix: TrackRef,
        /// The source track.
        source: TrackRef,
        /// Gain: 1.0 is 0 dB.
        #[serde(default)]
        volume: Option<f64>,
        /// Level in dB, instead of `volume`.
        #[serde(default)]
        db: Option<f64>,
    },
    /// The pan of one source in the mix.
    #[serde(rename = "mix.pan")]
    Pan {
        mix: TrackRef,
        /// The source track.
        source: TrackRef,
        /// From -1 (left) to 1 (right).
        pan: f64,
    },
    /// Mutes a source in the mix, or toggles it when `muted` is left out.
    #[serde(rename = "mix.mute")]
    Mute {
        mix: TrackRef,
        source: TrackRef,
        #[serde(default)]
        muted: Option<bool>,
    },
    /// Marks a source as the musician's own (or not; toggles when `own` is left out).
    #[serde(rename = "mix.own")]
    Own {
        mix: TrackRef,
        source: TrackRef,
        #[serde(default)]
        own: Option<bool>,
    },
    /// Raises the musician's own sources `db` / 2 above the levels and the rest `db` / 2 below
    /// (-24 to 24).
    #[serde(rename = "mix.more_me")]
    MoreMe {
        mix: TrackRef,
        /// -24 to 24; 0 is the levels as set.
        db: f64,
    },
    /// The whole mix's level (its track fader).
    #[serde(rename = "mix.volume")]
    Volume {
        mix: TrackRef,
        /// Gain: 1.0 is 0 dB.
        #[serde(default)]
        volume: Option<f64>,
        /// Level in dB, instead of `volume`.
        #[serde(default)]
        db: Option<f64>,
    },
    /// Where the mix plays: device output `output` (from 0), or nowhere with `null`.
    #[serde(rename = "mix.output")]
    Output {
        mix: TrackRef,
        output: Option<usize>,
        #[serde(default)]
        mono: bool,
    },
    /// Turns the mix's safety limiter on or off; toggles when `on` is left out.
    #[serde(rename = "mix.limiter")]
    Limiter {
        mix: TrackRef,
        #[serde(default)]
        on: Option<bool>,
    },
    /// Adds a track to the mix.
    #[serde(rename = "mix.add_source")]
    AddSource {
        mix: TrackRef,
        source: TrackRef,
        #[serde(default)]
        mode: Option<Tap>,
    },
    /// Takes a track out of the mix.
    #[serde(rename = "mix.remove_source")]
    RemoveSource { mix: TrackRef, source: TrackRef },
    /// SWS's "Create cue buss from track selection": a bus named `name` receiving `sources`,
    /// with SWS's options. It feeds the master only when it has no `output`.
    #[serde(rename = "mix.cue_bus")]
    CueBus {
        sources: Vec<TrackRef>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        mode: Option<Tap>,
        #[serde(default)]
        volumes_from_tracks: bool,
        #[serde(default)]
        pans_from_tracks: bool,
        #[serde(default)]
        output: Option<usize>,
        #[serde(default)]
        mono: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MixInfo {
    /// The mix's track id.
    pub id: u64,
    pub musician: String,
    pub name: String,
    pub color: Option<String>,
    /// The mix's own level (track fader).
    pub volume: f64,
    pub volume_db: f64,
    pub more_me: f64,
    pub output: Option<OutputInfo>,
    /// `None` when the mix has no safety limiter.
    pub limiter: Option<bool>,
    pub sources: Vec<MixSource>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputInfo {
    /// First device output, from 0.
    pub output: u32,
    pub mono: bool,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MixSource {
    /// The source track's id.
    pub track: u64,
    pub name: String,
    pub color: Option<String>,
    pub volume: f64,
    pub volume_db: f64,
    pub pan: f64,
    pub muted: bool,
    pub mode: Tap,
    pub own: bool,
}

fn color(c: Option<scythe_core::Color>) -> Option<String> {
    c.map(|c| format!("#{:06x}", c.to_hex()))
}

pub fn info(p: &Project, m: &MonitorMix) -> MixInfo {
    let t = p.track(m.track);
    MixInfo {
        id: m.track.0,
        musician: m.musician.clone(),
        name: t.map(|t| t.name.clone()).unwrap_or_default(),
        color: color(t.and_then(|t| t.color)),
        volume: t.map_or(1.0, |t| t.volume),
        volume_db: gain_to_db(t.map_or(1.0, |t| t.volume)),
        more_me: m.more_me,
        output: m.output.map(|o| match o {
            MonitorOutput::Stereo(n) => OutputInfo { output: n, mono: false, label: o.label() },
            MonitorOutput::Mono(n) => OutputInfo { output: n, mono: true, label: o.label() },
        }),
        limiter: p.monitor_limiter(m.track),
        sources: p
            .monitor_sources(m.track)
            .into_iter()
            .map(|s| {
                let src = p.track(s.track);
                MixSource {
                    track: s.track.0,
                    name: src.map(|t| t.name.clone()).unwrap_or_default(),
                    color: color(src.and_then(|t| t.color)),
                    volume: s.volume,
                    volume_db: gain_to_db(s.volume),
                    pan: s.pan,
                    muted: s.muted,
                    mode: s.mode.into(),
                    own: s.own,
                }
            })
            .collect(),
    }
}

pub fn list(p: &Project) -> Vec<MixInfo> {
    p.monitor_mixes().iter().map(|m| info(p, m)).collect()
}

fn mix_info(session: &Session, id: TrackId) -> Value {
    session.project.monitor_mix(id).map_or(Value::Null, |m| json(info(&session.project, &m)))
}

/// The monitor mix `r` names.
fn mix(session: &Session, r: &TrackRef) -> Result<TrackId, ApiError> {
    let id = r.track(&session.project)?;
    if !session.project.is_monitor_mix(id) {
        return Err(ApiError::invalid("that track is not a monitor mix"));
    }
    Ok(id)
}

/// The mix and one of its sources.
fn source(session: &Session, m: &TrackRef, s: &TrackRef) -> Result<(TrackId, TrackId), ApiError> {
    let id = mix(session, m)?;
    let src = s.track(&session.project)?;
    if !session.project.monitor_sources(id).iter().any(|x| x.track == src) {
        return Err(ApiError::not_found("that track is not a source of this mix"));
    }
    Ok((id, src))
}

fn tracks(session: &Session, refs: &[TrackRef]) -> Result<Vec<TrackId>, ApiError> {
    refs.iter().map(|r| r.track(&session.project)).collect()
}

fn output(p: &Project, output: Option<usize>, mono: bool) -> Result<MonitorOutput, ApiError> {
    let n = match output {
        Some(n) => {
            u32::try_from(n).ok().filter(|n| *n < 512).ok_or(ApiError::invalid("device outputs run from 0 to 511"))?
        }
        None => p.next_free_output(if mono { 1 } else { 2 }),
    };
    Ok(if mono { MonitorOutput::Mono(n) } else { MonitorOutput::Stereo(n) })
}

fn name(n: &str) -> Result<String, ApiError> {
    let n = n.trim();
    if n.is_empty() {
        return Err(ApiError::invalid("give the musician a name"));
    }
    Ok(n.to_owned())
}

pub fn execute(session: &mut Session, command: MixCommand) -> Result<Value, ApiError> {
    match command {
        MixCommand::List => Ok(json(list(&session.project))),
        MixCommand::Get { mix: m } => {
            let id = mix(session, &m)?;
            Ok(mix_info(session, id))
        }
        MixCommand::Create {
            musician,
            sources,
            own,
            output: out,
            mono,
            mode,
            levels_from_tracks,
            pans_from_tracks,
            limiter,
        } => {
            let musician = name(&musician)?;
            let sources = match sources {
                Some(s) => tracks(session, &s)?,
                None => session.project.monitor_candidates(),
            };
            let own = tracks(session, &own)?;
            let opts = CueBusOptions {
                mode: mode.map_or(SendMode::PostFx, SendMode::from),
                volumes_from_tracks: levels_from_tracks,
                pans_from_tracks,
                output: Some(output(&session.project, out, mono)?),
                fx: if limiter.unwrap_or(true) { vec![scythe_core::monitor::limiter_slot()] } else { Vec::new() },
                ..CueBusOptions::default()
            };
            let mut id = TrackId(0);
            session.edit("Create monitor mix", |p| id = p.create_monitor_mix(&musician, &sources, &own, &opts));
            Ok(mix_info(session, id))
        }
        MixCommand::Copy { mix: m, musician } => {
            let id = mix(session, &m)?;
            let musician = name(&musician)?;
            let mut new = None;
            session.edit("Copy monitor mix", |p| new = p.copy_monitor_mix(id, &musician));
            let new = new.ok_or(ApiError::failed("the mix could not be copied"))?;
            Ok(mix_info(session, new))
        }
        MixCommand::Rename { mix: m, musician } => {
            let id = mix(session, &m)?;
            let musician = name(&musician)?;
            session.edit("Rename monitor mix", |p| {
                p.rename_monitor_mix(id, &musician);
            });
            Ok(mix_info(session, id))
        }
        MixCommand::Delete { mix: m } => {
            let id = mix(session, &m)?;
            session.edit("Delete monitor mix", |p| {
                p.delete_monitor_mix(id);
            });
            Ok(json(list(&session.project)))
        }
        MixCommand::Level { mix: m, source: s, volume, db } => {
            let gain = level(volume, db)?;
            let (id, src) = source(session, &m, &s)?;
            session.edit_merging("Monitor source level", &format!("mix:{}:{}", id.0, src.0), |p| {
                p.set_monitor_level(id, src, gain);
            });
            Ok(mix_info(session, id))
        }
        MixCommand::Pan { mix: m, source: s, pan: value } => {
            let value = pan(value)?;
            let (id, src) = source(session, &m, &s)?;
            session.edit_merging("Monitor source pan", &format!("mix-pan:{}:{}", id.0, src.0), |p| {
                p.set_monitor_pan(id, src, value);
            });
            Ok(mix_info(session, id))
        }
        MixCommand::Mute { mix: m, source: s, muted } => {
            let (id, src) = source(session, &m, &s)?;
            let now = session.project.monitor_sources(id).iter().any(|x| x.track == src && x.muted);
            session.edit("Toggle monitor source mute", |p| {
                p.set_monitor_muted(id, src, muted.unwrap_or(!now));
            });
            Ok(mix_info(session, id))
        }
        MixCommand::Own { mix: m, source: s, own } => {
            let (id, src) = source(session, &m, &s)?;
            let mut list = session.project.monitor_mix(id).map(|m| m.own).unwrap_or_default();
            let is_own = list.contains(&src);
            if own.unwrap_or(!is_own) {
                if !is_own {
                    list.push(src);
                }
            } else {
                list.retain(|t| *t != src);
            }
            session.edit("Set own monitor sources", |p| {
                p.set_monitor_own(id, &list);
            });
            Ok(mix_info(session, id))
        }
        MixCommand::MoreMe { mix: m, db } => {
            let id = mix(session, &m)?;
            let range = scythe_core::monitor::MORE_ME_RANGE;
            if !range.contains(&db) {
                return Err(ApiError::invalid("more me runs from -24 to 24 dB"));
            }
            session.edit_merging("More me", &format!("more-me:{}", id.0), |p| {
                p.set_more_me(id, db);
            });
            Ok(mix_info(session, id))
        }
        MixCommand::Volume { mix: m, volume, db } => {
            let gain = level(volume, db)?;
            let id = mix(session, &m)?;
            session.edit_merging("Monitor mix level", &format!("mix-level:{}", id.0), |p| {
                if let Some(t) = p.track_mut(id) {
                    t.volume = gain;
                }
            });
            Ok(mix_info(session, id))
        }
        MixCommand::Output { mix: m, output: out, mono } => {
            let id = mix(session, &m)?;
            let o = match out {
                Some(n) => Some(output(&session.project, Some(n), mono)?),
                None => None,
            };
            session.edit("Set monitor output", |p| {
                p.set_monitor_output(id, o);
            });
            Ok(mix_info(session, id))
        }
        MixCommand::Limiter { mix: m, on } => {
            let id = mix(session, &m)?;
            let on = on.unwrap_or(session.project.monitor_limiter(id) != Some(true));
            session.edit("Toggle monitor limiter", |p| {
                p.set_monitor_limiter(id, on);
            });
            Ok(mix_info(session, id))
        }
        MixCommand::AddSource { mix: m, source: s, mode } => {
            let id = mix(session, &m)?;
            let src = s.track(&session.project)?;
            if session.project.is_monitor_mix(src) {
                return Err(ApiError::invalid("a monitor mix cannot be a source of another"));
            }
            let mode = mode.map_or(SendMode::PostFx, SendMode::from);
            let mut added = false;
            session.edit("Add monitor source", |p| added = p.add_monitor_source(id, src, mode));
            if !added {
                return Err(ApiError::invalid("that track is already in the mix"));
            }
            Ok(mix_info(session, id))
        }
        MixCommand::RemoveSource { mix: m, source: s } => {
            let (id, src) = source(session, &m, &s)?;
            session.edit("Remove monitor source", |p| {
                p.remove_monitor_source(id, src);
            });
            Ok(mix_info(session, id))
        }
        MixCommand::CueBus { sources, name: bus, mode, volumes_from_tracks, pans_from_tracks, output: out, mono } => {
            let sources = tracks(session, &sources)?;
            let o = match out {
                Some(n) => Some(output(&session.project, Some(n), mono)?),
                None => None,
            };
            let opts = CueBusOptions {
                name: bus.unwrap_or_else(|| "Cue Buss".to_owned()),
                mode: mode.map_or(SendMode::PostFx, SendMode::from),
                volumes_from_tracks,
                pans_from_tracks,
                output: o,
                send_to_master: o.is_none(),
                ..CueBusOptions::default()
            };
            let mut id = TrackId(0);
            session.edit("Create cue buss", |p| id = p.create_cue_bus(&sources, &opts));
            let t = session.project.track(id).unwrap();
            Ok(json(super::snapshot::track(session, t, session.project.track_index(id), 0)))
        }
    }
}
