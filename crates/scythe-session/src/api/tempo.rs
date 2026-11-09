//! The tempo map: tempo and time signature markers (REAPER's `TEMPOENVEX` points), numbered
//! from 0 in time order.

use schemars::JsonSchema;
use scythe_core::Project;
use scythe_core::tempo::{TempoMap, TempoPoint};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::{ApiError, json, time};
use crate::Session;

const MIN_BPM: f64 = 20.0;
const MAX_BPM: f64 = 400.0;

/// Markers are numbered from 0 in time order, as `tempo.list` gives them. Setting a time
/// signature moves the marker to the next bar line, as in REAPER.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum TempoCommand {
    /// Every tempo and time signature marker, with the tempo and time signature at the start.
    #[serde(rename = "tempo.list")]
    List,
    /// Adds a marker at `time` and answers with the tempo map.
    #[serde(rename = "tempo.add")]
    Add {
        /// Seconds.
        time: f64,
        /// Beats per minute, 20 to 400; the tempo in effect there when left out.
        #[serde(default)]
        bpm: Option<f64>,
        /// `[beats per bar, beat unit]`, such as `[7, 8]`; no change of time signature when
        /// left out.
        #[serde(default)]
        time_signature: Option<(u32, u32)>,
        /// Ramp linearly to the next marker's tempo instead of jumping.
        #[serde(default)]
        linear: bool,
    },
    /// Changes what is given and answers with the tempo map. Repeated changes of one marker
    /// within a second are one undo step, so a dragged marker is undone in one go.
    #[serde(rename = "tempo.edit")]
    Edit {
        /// The marker's number from `tempo.list`.
        index: usize,
        /// Seconds.
        #[serde(default)]
        time: Option<f64>,
        /// Beats per minute, 20 to 400.
        #[serde(default)]
        bpm: Option<f64>,
        /// `[beats per bar, beat unit]`, or `null` to keep the time signature that runs on.
        #[serde(default, deserialize_with = "present")]
        #[schemars(with = "Option<(u32, u32)>")]
        time_signature: Option<Option<(u32, u32)>>,
        /// Ramp linearly to the next marker's tempo instead of jumping.
        #[serde(default)]
        linear: Option<bool>,
    },
    /// Deletes the marker and answers with the tempo map.
    #[serde(rename = "tempo.delete")]
    Delete {
        /// The marker's number from `tempo.list`.
        index: usize,
    },
}

/// A field given as `null` is `Some(None)`, told apart from one left out (`None`).
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TempoMarker {
    pub index: usize,
    pub time: f64,
    pub bpm: f64,
    pub linear: bool,
    /// Set where the marker starts a new time signature.
    pub time_signature: Option<(u32, u32)>,
    /// REAPER's `bar.beat.hundredths`.
    pub bar_beat: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TempoInfo {
    /// The tempo and time signature at the start.
    pub bpm: f64,
    pub time_signature: (u32, u32),
    pub markers: Vec<TempoMarker>,
}

pub fn markers(p: &Project) -> Vec<TempoMarker> {
    p.tempo
        .points
        .iter()
        .enumerate()
        .map(|(index, pt)| TempoMarker {
            index,
            time: pt.time,
            bpm: pt.bpm,
            linear: pt.linear,
            time_signature: pt.time_sig,
            bar_beat: p.tempo.format_bar_beat(pt.time),
        })
        .collect()
}

fn info(p: &Project) -> Value {
    json(TempoInfo { bpm: p.tempo.bpm_at(0.0), time_signature: p.tempo.time_sig_at(0.0), markers: markers(p) })
}

fn bpm(value: f64) -> Result<f64, ApiError> {
    if !(MIN_BPM..=MAX_BPM).contains(&value) {
        return Err(ApiError::invalid(format!("tempo runs from {MIN_BPM} to {MAX_BPM} bpm")));
    }
    Ok(value)
}

fn signature(sig: (u32, u32)) -> Result<(u32, u32), ApiError> {
    let (num, denom) = sig;
    if !(1..=64).contains(&num) || ![1, 2, 4, 8, 16, 32, 64].contains(&denom) {
        return Err(ApiError::invalid("a time signature is 1 to 64 beats of a 1, 2, 4, 8, 16, 32 or 64 note"));
    }
    Ok(sig)
}

/// The first bar line at or after `t`, where a time signature may start.
fn bar_line(tempo: &TempoMap, t: f64) -> f64 {
    let timeline = tempo.timeline();
    let bb = timeline.bar_beat(t);
    if bb.beat == 1 && bb.fraction < 1e-6 { t } else { timeline.bar_start(bb.bar + 1) }
}

/// Keeps the points in time order and the base tempo and signature (the RPP `TEMPO` line) in
/// step with a point at the start.
fn settle(p: &mut Project) {
    let tempo = &mut p.tempo;
    tempo.points.sort_by(|a, b| a.time.total_cmp(&b.time));
    if let Some(first) = tempo.points.first().filter(|pt| pt.time <= 0.0) {
        tempo.bpm = first.bpm;
        if let Some((num, denom)) = first.time_sig {
            tempo.beats_per_bar = num;
            tempo.beat_unit = denom;
        }
    }
}

fn point(session: &Session, index: usize) -> Result<&TempoPoint, ApiError> {
    session.project.tempo.points.get(index).ok_or_else(|| ApiError::not_found(format!("no tempo marker {index}")))
}

pub fn execute(session: &mut Session, command: TempoCommand) -> Result<Value, ApiError> {
    match command {
        TempoCommand::List => Ok(info(&session.project)),
        TempoCommand::Add { time: at, bpm: tempo, time_signature, linear } => {
            let mut at = time(at, "time")?;
            let tempo = tempo.map(bpm).transpose()?.unwrap_or_else(|| session.project.tempo.bpm_at(at));
            let sig = time_signature.map(signature).transpose()?;
            if sig.is_some() {
                at = bar_line(&session.project.tempo, at);
            }
            if session.project.tempo.points.iter().any(|p| (p.time - at).abs() < 1e-6) {
                return Err(ApiError::invalid("there is a tempo marker there already"));
            }
            session.edit("Insert tempo marker", |p| {
                let tempo_map = &mut p.tempo;
                if tempo_map.points.is_empty() && at > 0.0 {
                    tempo_map.points.push(TempoPoint {
                        time_sig: Some((tempo_map.beats_per_bar, tempo_map.beat_unit)),
                        ..TempoPoint::new(0.0, tempo_map.bpm)
                    });
                }
                tempo_map.points.push(TempoPoint { linear, time_sig: sig, ..TempoPoint::new(at, tempo) });
                settle(p);
            });
            Ok(info(&session.project))
        }
        TempoCommand::Edit { index, time: at, bpm: tempo, time_signature, linear } => {
            let current = point(session, index)?.clone();
            let tempo = tempo.map(bpm).transpose()?;
            let sig = match time_signature {
                Some(Some(s)) => Some(Some(signature(s)?)),
                other => other,
            };
            let mut at = at.map(|t| time(t, "time")).transpose()?.unwrap_or(current.time);
            if sig.unwrap_or(current.time_sig).is_some() {
                let mut others = session.project.tempo.clone();
                others.points.remove(index);
                at = bar_line(&others, at);
            }
            let taken =
                session.project.tempo.points.iter().enumerate().any(|(i, p)| i != index && (p.time - at).abs() < 1e-6);
            if taken {
                return Err(ApiError::invalid("there is a tempo marker there already"));
            }
            session.edit_merging("Edit tempo marker", &format!("tempo:{index}"), |p| {
                let pt = &mut p.tempo.points[index];
                pt.time = at;
                if let Some(b) = tempo {
                    pt.bpm = b;
                }
                if let Some(s) = sig {
                    pt.time_sig = s;
                }
                if let Some(l) = linear {
                    pt.linear = l;
                }
                settle(p);
            });
            Ok(info(&session.project))
        }
        TempoCommand::Delete { index } => {
            point(session, index)?;
            session.edit("Remove tempo marker", |p| {
                p.tempo.points.remove(index);
                settle(p);
            });
            Ok(info(&session.project))
        }
    }
}
