use schemars::JsonSchema;
use scythe_engine::scythe_eq::filters::{SLOPES, Shape};
use scythe_engine::scythe_eq::matching::{self, MatchOptions};
use scythe_engine::scythe_eq::params::{self as p, BANDS, GLOBAL, band, global};
use scythe_engine::scythe_eq::{self, BandSettings, ScytheEq};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::fx::{ChainAt, edit_stock, instance, locate};
use super::{ApiError, TrackRef};
use crate::Session;

/// Scythe EQ, Scythe's own dynamic equalizer (24 bands, dynamic and spectral bands, mid/side
/// per band, zero latency, natural or linear phase). Every value can also be read and set as
/// a plain parameter with `fx.params` / `fx.param`; these commands address bands by number and
/// shape by name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum EqCommand {
    /// Every Scythe EQ in the project: track, chain position, name, phase mode and the bands
    /// in use.
    #[serde(rename = "eq.instances")]
    Instances,
    /// One Scythe EQ's global settings and its bands in use, with each dynamic band's current
    /// gain change.
    #[serde(rename = "eq.get")]
    Get {
        track: TrackRef,
        /// Position in the chain, from 0.
        index: usize,
        #[serde(flatten)]
        at: ChainAt,
    },
    /// Adds a band (when `band` is left out: the first free one) or changes one; fields left
    /// out stay as they are. `remove` takes the band out. One undo step.
    #[serde(rename = "eq.band")]
    Band {
        track: TrackRef,
        /// Position in the chain, from 0.
        index: usize,
        /// The band, 0-23; left out to add a band.
        #[serde(default)]
        band: Option<usize>,
        #[serde(default)]
        remove: bool,
        #[serde(default)]
        shape: Option<EqShape>,
        /// Frequency in Hz, 10-30000.
        #[serde(default)]
        freq: Option<f64>,
        /// Gain in dB, -30 to 30 (bells, shelves and tilts).
        #[serde(default)]
        gain_db: Option<f64>,
        /// Q, 0.025-40.
        #[serde(default)]
        q: Option<f64>,
        /// Slope in dB per octave for cuts, shelves and band pass: 6, 12, 18, 24, 30, 36, 48,
        /// 72 or 96, or 0 for a brickwall cut.
        #[serde(default)]
        slope: Option<u32>,
        #[serde(default)]
        stereo: Option<EqStereo>,
        /// Whether the band plays (false bypasses it).
        #[serde(default)]
        enabled: Option<bool>,
        #[serde(default)]
        dynamic: Option<EqDynamic>,
        /// How far a dynamic band moves from its gain, in dB (negative cuts when loud).
        #[serde(default)]
        range_db: Option<f64>,
        /// The detector level, in dB, above which a dynamic band moves.
        #[serde(default)]
        threshold_db: Option<f64>,
        /// The threshold follows the band's own average level.
        #[serde(default)]
        auto_threshold: Option<bool>,
        #[serde(default)]
        attack_ms: Option<f64>,
        #[serde(default)]
        release_ms: Option<f64>,
        /// The detector listens to the track's channels 3/4 instead of the band's own input.
        #[serde(default)]
        external_sidechain: Option<bool>,
        #[serde(flatten)]
        at: ChainAt,
    },
    /// Changes global settings; fields left out stay as they are. One undo step.
    #[serde(rename = "eq.set")]
    Set {
        track: TrackRef,
        /// Position in the chain, from 0.
        index: usize,
        /// Output gain in dB, -36 to 36.
        #[serde(default)]
        output_db: Option<f64>,
        /// Keeps the loudness the same as the curve changes.
        #[serde(default)]
        auto_gain: Option<bool>,
        /// Scales every band's gain and range, in percent from -100 to 200.
        #[serde(default)]
        gain_scale: Option<f64>,
        /// Output balance, -100 (left) to 100 (right).
        #[serde(default)]
        pan: Option<f64>,
        /// Stereo width in percent, 0-200.
        #[serde(default)]
        width: Option<f64>,
        #[serde(default)]
        phase_mode: Option<EqPhase>,
        /// Linear phase resolution, 0 (low, least latency) to 4 (maximum).
        #[serde(default)]
        resolution: Option<u32>,
        #[serde(default)]
        character: Option<EqCharacter>,
        /// Bells narrow as their gain grows.
        #[serde(default)]
        gain_q: Option<bool>,
        #[serde(flatten)]
        at: ChainAt,
    },
    /// What this Scythe EQ puts out next to what a track, the master or another instance
    /// plays, on third-octave bands, with the ranges where they collide (both loud and within
    /// 6 dB). It reads the last third of a second of audio, so ask while the part plays; the
    /// first call after a pause starts listening and may answer `live: false`.
    #[serde(rename = "eq.compare")]
    Compare {
        track: TrackRef,
        /// Position in the chain, from 0.
        index: usize,
        /// What to compare with.
        with: EqSource,
        #[serde(flatten)]
        at: ChainAt,
    },
    /// EQ Match: bells and shelves that move this Scythe EQ's input towards the spectrum of
    /// a track, the master, another instance or an audio file. Live sources are read over the
    /// last third of a second, so ask while the part plays. With `apply` the bands go into
    /// free band slots as one undo step.
    #[serde(rename = "eq.match")]
    Match {
        track: TrackRef,
        /// Position in the chain, from 0.
        index: usize,
        /// What to match to.
        reference: EqSource,
        /// Percent of the difference to correct, 0-100 (100 when left out).
        #[serde(default)]
        amount: Option<f64>,
        /// Smoothing in octaves, 0.05-2 (a third when left out).
        #[serde(default)]
        smoothing: Option<f64>,
        /// Most bands to use, 1-16 (8 when left out).
        #[serde(default)]
        bands: Option<usize>,
        /// Adds the bands to the EQ; otherwise they are only proposed.
        #[serde(default)]
        apply: bool,
        #[serde(flatten)]
        at: ChainAt,
    },
}

/// A signal Scythe EQ compares or matches with: give `track` alone, `track` and `index`, or
/// `file`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EqSource {
    /// A track or `"master"`: its audio after its effects, before its fader.
    #[serde(default)]
    pub track: Option<TrackRef>,
    /// With `track`: the Scythe EQ at this position of the track's chain instead, its output.
    #[serde(default)]
    pub index: Option<usize>,
    /// An audio file: its long-term spectrum.
    #[serde(default)]
    pub file: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EqShape {
    Bell,
    LowShelf,
    HighShelf,
    LowCut,
    HighCut,
    Notch,
    BandPass,
    TiltShelf,
    FlatTilt,
    AllPass,
}

impl EqShape {
    fn shape(self) -> Shape {
        Shape::ALL[self as usize]
    }

    fn of(shape: Shape) -> Self {
        [
            Self::Bell,
            Self::LowShelf,
            Self::HighShelf,
            Self::LowCut,
            Self::HighCut,
            Self::Notch,
            Self::BandPass,
            Self::TiltShelf,
            Self::FlatTilt,
            Self::AllPass,
        ][shape.index()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EqStereo {
    Stereo,
    Left,
    Right,
    Mid,
    Side,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EqDynamic {
    Off,
    Dynamic,
    Spectral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EqPhase {
    ZeroLatency,
    Natural,
    Linear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EqCharacter {
    Clean,
    Subtle,
    Warm,
}

const STEREO: [&str; 5] = ["stereo", "left", "right", "mid", "side"];
const DYNAMIC: [&str; 3] = ["off", "dynamic", "spectral"];
const PHASE: [&str; 3] = ["zero_latency", "natural", "linear"];
const CHARACTER: [&str; 3] = ["clean", "subtle", "warm"];

fn describe_band(values: &[f64], b: usize, dynamic_db: Option<f32>) -> Value {
    let s = BandSettings::read(values, b);
    let v = |o: u32| values[p::band_param(b, o) as usize];
    let slope = SLOPES[(v(band::SLOPE) as usize).min(SLOPES.len() - 1)];
    json!({
        "band": b,
        "shape": EqShape::of(s.design.shape),
        "freq": v(band::FREQ),
        "gain_db": v(band::GAIN),
        "q": v(band::Q),
        "slope": slope,
        "stereo": STEREO[s.placement as usize],
        "enabled": s.enabled,
        "dynamic": DYNAMIC[v(band::DYNAMIC) as usize],
        "range_db": v(band::RANGE),
        "threshold_db": v(band::THRESHOLD),
        "auto_threshold": s.auto_threshold,
        "attack_ms": s.attack,
        "release_ms": s.release,
        "external_sidechain": s.external,
        "dynamic_db": dynamic_db,
    })
}

/// A Scythe EQ's settings as `eq.get` answers them.
pub fn describe(values: &[f64], meters: Option<&scythe_eq::Meters>, latency: Option<usize>) -> Value {
    let g = |o: u32| values[(GLOBAL + o) as usize];
    let bands: Vec<Value> = (0..BANDS)
        .filter(|b| values[p::band_param(*b, band::USED) as usize] >= 0.5)
        .map(|b| describe_band(values, b, meters.map(|m| m.dynamic(b))))
        .collect();
    json!({
        "output_db": g(global::OUTPUT),
        "auto_gain": g(global::AUTO_GAIN) >= 0.5,
        "gain_scale": g(global::GAIN_SCALE),
        "pan": g(global::PAN),
        "width": g(global::WIDTH),
        "phase_mode": PHASE[g(global::PHASE) as usize],
        "resolution": g(global::RESOLUTION) as u32,
        "character": CHARACTER[g(global::CHARACTER) as usize],
        "gain_q": g(global::GAIN_Q) >= 0.5,
        "latency": latency,
        "bands": bands,
    })
}

fn flag(v: bool) -> f64 {
    if v { 1.0 } else { 0.0 }
}

fn slope_index(db: u32) -> Result<f64, ApiError> {
    SLOPES
        .iter()
        .position(|s| *s == db)
        .map(|i| i as f64)
        .ok_or_else(|| ApiError::invalid("slopes are 6, 12, 18, 24, 30, 36, 48, 72 or 96 dB/oct, or 0 for a brickwall"))
}

fn running(
    session: &Session,
    track: &TrackRef,
    index: usize,
    at: &ChainAt,
) -> Result<std::sync::Arc<scythe_engine::fx::FxInstance>, ApiError> {
    let loc = locate(session, track, at)?;
    let inst = instance(session, loc, at, index)?;
    let is_eq = inst.with(|e| e.as_any_mut().is_some_and(|a| a.is::<ScytheEq>())).unwrap_or(false);
    if !is_eq {
        return Err(ApiError::invalid("the effect is not a Scythe EQ"));
    }
    Ok(inst)
}

/// Every Scythe EQ on the tracks and the master: `(track, index)` with `None` for the master.
pub fn instances(session: &Session) -> Vec<(Option<scythe_core::TrackId>, usize, String)> {
    let mut out = Vec::new();
    for (i, slot) in session.project.master.fx.iter().enumerate() {
        if scythe_eq::is_slot(slot) {
            out.push((None, i, slot.name.clone()));
        }
    }
    for t in &session.project.tracks {
        for (i, slot) in t.fx.iter().enumerate() {
            if scythe_eq::is_slot(slot) {
                out.push((Some(t.id), i, slot.name.clone()));
            }
        }
    }
    out
}

/// Points the match is fitted on.
const MATCH_POINTS: usize = 240;
/// Blocks a live source keeps tapping after a read (a few seconds).
const LEASE: u32 = 400;

/// The newest audio of one side of a probe, `None` until it has played.
fn live_audio(probe: &scythe_engine::probe::FxProbe, side: scythe_engine::probe::Side) -> Option<(Vec<f32>, f64)> {
    probe.lease_audio(LEASE);
    let mut buf = vec![0.0f32; scythe_engine::probe::RING];
    let head = probe.latest(side, &mut buf);
    (head >= buf.len() && buf.iter().any(|s| *s != 0.0)).then(|| (buf, probe.sample_rate().max(1) as f64))
}

/// A source's levels at `freqs` over `octaves` wide bands; `Ok(None)` while a live source
/// has not played yet.
fn source_levels(
    session: &Session,
    source: &EqSource,
    freqs: &[f64],
    octaves: f64,
) -> Result<Option<Vec<f64>>, ApiError> {
    use scythe_engine::probe::Side;
    if let Some(file) = &source.file {
        let audio = scythe_engine::media::decode_file(std::path::Path::new(file))
            .map_err(|e| ApiError::invalid(format!("{file}: {e}")))?;
        let a = scythe_engine::analysis::analyse(&audio.channels, audio.sample_rate, None, None);
        let hz: Vec<f64> = a.bands.iter().map(|b| b.hz.ln()).collect();
        let db: Vec<f64> = a.bands.iter().map(|b| b.level_db).collect();
        return Ok(Some(
            freqs
                .iter()
                .map(|f| {
                    let x = f.ln();
                    match hz.iter().position(|h| *h >= x) {
                        Some(0) => db[0],
                        Some(k) => db[k - 1] + (x - hz[k - 1]) / (hz[k] - hz[k - 1]) * (db[k] - db[k - 1]),
                        None => *db.last().unwrap_or(&-150.0),
                    }
                })
                .collect(),
        ));
    }
    let track = source.track.as_ref().ok_or_else(|| ApiError::invalid("give `track` (with `index`) or `file`"))?;
    let audio = match source.index {
        Some(index) => live_audio(&running(session, track, index, &ChainAt::default())?.probe, Side::Post),
        None => {
            let meters = session.engine.meters();
            let meter = match track.resolve(&session.project)? {
                super::Target::Master => Some(&meters.master),
                super::Target::Track(id) => meters.get(id),
            };
            let meter = meter.ok_or_else(|| ApiError::failed("the track does not play"))?;
            live_audio(&meter.probe, Side::Post)
        }
    };
    Ok(audio.map(|(buf, sr)| matching::levels(&buf, sr, freqs, octaves)))
}

fn round(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn compare(
    session: &Session,
    track: &TrackRef,
    index: usize,
    at: &ChainAt,
    with: &EqSource,
) -> Result<Value, ApiError> {
    let inst = running(session, track, index, at)?;
    let thirds: Vec<f64> = (-17..=13).map(|k| 1000.0 * 2f64.powf(k as f64 / 3.0)).collect();
    let fine = matching::log_grid(MATCH_POINTS);
    let ours = live_audio(&inst.probe, scythe_engine::probe::Side::Post);
    let (Some((ours, sr)), Some(theirs), Some(theirs_fine)) =
        (ours, source_levels(session, with, &thirds, 1.0 / 3.0)?, source_levels(session, with, &fine, 1.0 / 6.0)?)
    else {
        return Ok(json!({ "live": false }));
    };
    let ours_thirds = matching::levels(&ours, sr, &thirds, 1.0 / 3.0);
    let ours_fine: Vec<f32> = matching::levels(&ours, sr, &fine, 1.0 / 6.0).iter().map(|v| *v as f32).collect();
    let theirs_fine: Vec<f32> = theirs_fine.iter().map(|v| *v as f32).collect();
    let top = ours_fine.iter().chain(&theirs_fine).copied().fold(-150.0f32, f32::max);
    let bands: Vec<Value> = thirds
        .iter()
        .zip(ours_thirds.iter().zip(&theirs))
        .map(|(hz, (o, t))| json!({ "hz": round(*hz), "ours_db": round(*o), "theirs_db": round(*t) }))
        .collect();
    let collisions: Vec<Value> = matching::collisions(&ours_fine, &theirs_fine, top - 50.0)
        .into_iter()
        .map(|(lo, hi, s)| json!({ "from_hz": round(fine[lo]), "to_hz": round(fine[hi]), "strength": round(s as f64) }))
        .collect();
    Ok(json!({ "live": true, "bands": bands, "collisions": collisions }))
}

fn match_to(
    session: &mut Session,
    track: &TrackRef,
    index: usize,
    at: &ChainAt,
    reference: &EqSource,
    options: MatchOptions,
    apply: bool,
) -> Result<Value, ApiError> {
    let inst = running(session, track, index, at)?;
    let freqs = matching::log_grid(MATCH_POINTS);
    let current = live_audio(&inst.probe, scythe_engine::probe::Side::Pre)
        .map(|(buf, sr)| matching::levels(&buf, sr, &freqs, 1.0 / 6.0));
    let (Some(current), Some(target)) = (current, source_levels(session, reference, &freqs, 1.0 / 6.0)?) else {
        return Ok(json!({ "live": false }));
    };
    // Levels too low to measure would only add noise to the difference.
    let usable: Vec<bool> = current.iter().zip(&target).map(|(c, r)| *c > -110.0 && *r > -110.0).collect();
    let pick = |v: &[f64]| v.iter().zip(&usable).map(|(x, u)| if *u { *x } else { 0.0 }).collect::<Vec<f64>>();
    let curve = matching::target(&freqs, &pick(&current), &pick(&target), &options);
    let proposal = matching::fit(&freqs, &curve, &options);
    let bands: Vec<Value> = proposal
        .iter()
        .map(|d| {
            json!({
                "shape": EqShape::of(d.shape),
                "freq": round(d.freq),
                "gain_db": round(d.gain_db),
                "q": (d.q * 100.0).round() / 100.0,
            })
        })
        .collect();
    let applied = if apply && !proposal.is_empty() {
        edit_stock(session, &inst, "EQ match", "Scythe EQ", |eq: &mut ScytheEq| {
            Ok(matching::apply_bands(eq, &proposal))
        })?
    } else {
        0
    };
    Ok(json!({ "live": true, "bands": bands, "applied": applied }))
}

pub fn execute(session: &mut Session, command: EqCommand) -> Result<Value, ApiError> {
    match command {
        EqCommand::Instances => {
            session.engine.store_plugin_states(&mut session.project);
            let list: Vec<Value> = instances(session)
                .into_iter()
                .map(|(track, index, name)| {
                    let t = track.and_then(|t| session.project.track(t));
                    let slot = match t {
                        Some(t) => t.fx.get(index),
                        None => session.project.master.fx.get(index),
                    };
                    let values = slot.and_then(scythe_eq::slot_values).unwrap_or_else(p::defaults);
                    let used = (0..BANDS).filter(|b| values[p::band_param(*b, band::USED) as usize] >= 0.5).count();
                    json!({
                        "track": track.map(|t| t.0),
                        "track_name": t.map_or("MASTER".to_owned(), |t| t.name.clone()),
                        "index": index,
                        "name": name,
                        "phase_mode": PHASE[values[(GLOBAL + global::PHASE) as usize] as usize],
                        "bands": used,
                    })
                })
                .collect();
            Ok(json!({ "instances": list }))
        }
        EqCommand::Get { track, index, at } => {
            let inst = running(session, &track, index, &at)?;
            inst.with(|e| {
                let latency = e.latency();
                let eq = e.as_any_mut().and_then(|a| a.downcast_mut::<ScytheEq>())?;
                Some(describe(eq.values(), Some(&eq.meters()), Some(latency)))
            })
            .flatten()
            .ok_or_else(|| ApiError::failed("the effect is not running"))
        }
        EqCommand::Band {
            track,
            index,
            band: which,
            remove,
            shape,
            freq,
            gain_db,
            q,
            slope,
            stereo,
            enabled,
            dynamic,
            range_db,
            threshold_db,
            auto_threshold,
            attack_ms,
            release_ms,
            external_sidechain,
            at,
        } => {
            let inst = running(session, &track, index, &at)?;
            let slope = slope.map(slope_index).transpose()?;
            if which.is_some_and(|b| b >= BANDS) {
                return Err(ApiError::invalid(format!("bands are 0-{}", BANDS - 1)));
            }
            let label = if remove {
                "Remove EQ band"
            } else if which.is_none() {
                "Add EQ band"
            } else {
                "Change EQ band"
            };
            edit_stock(session, &inst, label, "Scythe EQ", |eq: &mut ScytheEq| {
                let b = match which {
                    Some(b) => b,
                    None => (0..BANDS)
                        .find(|b| eq.value(p::band_param(*b, band::USED)) < 0.5)
                        .ok_or_else(|| ApiError::failed("all 24 bands are in use"))?,
                };
                let id = |o: u32| p::band_param(b, o);
                if remove {
                    eq.set(id(band::USED), 0.0);
                    return Ok(json!({ "band": b, "removed": true }));
                }
                if which.is_none() {
                    for o in 0..band::COUNT {
                        eq.set(id(o), p::default(id(o)));
                    }
                    eq.set(id(band::USED), 1.0);
                }
                let changes = [
                    (band::SHAPE, shape.map(|s| s.shape().index() as f64)),
                    (band::FREQ, freq),
                    (band::GAIN, gain_db),
                    (band::Q, q),
                    (band::SLOPE, slope),
                    (band::STEREO, stereo.map(|s| s as usize as f64)),
                    (band::ENABLED, enabled.map(flag)),
                    (band::DYNAMIC, dynamic.map(|d| d as usize as f64)),
                    (band::RANGE, range_db),
                    (band::THRESHOLD, threshold_db),
                    (band::AUTO_THRESHOLD, auto_threshold.map(flag)),
                    (band::ATTACK, attack_ms),
                    (band::RELEASE, release_ms),
                    (band::SIDECHAIN, external_sidechain.map(flag)),
                ];
                for (o, v) in changes {
                    if let Some(v) = v {
                        eq.set(id(o), v);
                    }
                }
                Ok(describe_band(eq.values(), b, None))
            })
        }
        EqCommand::Set {
            track,
            index,
            output_db,
            auto_gain,
            gain_scale,
            pan,
            width,
            phase_mode,
            resolution,
            character,
            gain_q,
            at,
        } => {
            let inst = running(session, &track, index, &at)?;
            let out = edit_stock(session, &inst, "Change Scythe EQ", "Scythe EQ", |eq: &mut ScytheEq| {
                let changes = [
                    (global::OUTPUT, output_db),
                    (global::AUTO_GAIN, auto_gain.map(flag)),
                    (global::GAIN_SCALE, gain_scale),
                    (global::PAN, pan),
                    (global::WIDTH, width),
                    (global::PHASE, phase_mode.map(|m| m as usize as f64)),
                    (global::RESOLUTION, resolution.map(f64::from)),
                    (global::CHARACTER, character.map(|c| c as usize as f64)),
                    (global::GAIN_Q, gain_q.map(flag)),
                ];
                for (o, v) in changes {
                    if let Some(v) = v {
                        eq.set(GLOBAL + o, v);
                    }
                }
                Ok(describe(eq.values(), None, None))
            })?;
            Ok(out)
        }
        EqCommand::Compare { track, index, with, at } => compare(session, &track, index, &at, &with),
        EqCommand::Match { track, index, reference, amount, smoothing, bands, apply, at } => {
            let options = MatchOptions {
                amount: amount.unwrap_or(100.0).clamp(0.0, 100.0) / 100.0,
                smoothing: smoothing.unwrap_or(1.0 / 3.0).clamp(0.05, 2.0),
                max_bands: bands.unwrap_or(8).clamp(1, 16),
                ..Default::default()
            };
            match_to(session, &track, index, &at, &reference, options, apply)
        }
    }
}
