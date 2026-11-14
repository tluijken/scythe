use schemars::JsonSchema;
use scythe_core::fx_chain::Parallel;
use scythe_core::{FxSlot, ItemId, TrackId};
use scythe_engine::builtin::Builtin;
use scythe_engine::fx::FxOwner;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ApiError, Target, TrackRef, json, snapshot};
use crate::Session;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum FxCommand {
    /// The effects on `track` (`"master"` for the master).
    #[serde(rename = "fx.list")]
    List {
        track: TrackRef,
        #[serde(flatten)]
        at: ChainAt,
    },
    /// Effects that `fx.add` can add: the built-in ones and the installed CLAP, LV2 and VST 3
    /// plugins. Answers at once with what is known: plugins are scanned in the background
    /// (the first call starts it; earlier runs' results come from the cache), and while that
    /// runs `scanning` is true with `progress`, so ask again until it is false. `failed` lists
    /// plugin files that hung, crashed or could not be read while scanning.
    #[serde(rename = "fx.catalog")]
    Catalog {
        /// Only entries whose name, id or format contain this, ignoring case.
        #[serde(default)]
        query: Option<String>,
        /// Scans the plugin folders again, checking every file anew (also ones that failed
        /// before) instead of trusting the cache.
        #[serde(default)]
        rescan: bool,
    },
    /// Adds the effect `plugin` names (a catalog `id` or exact `name`) at `index`, at the end
    /// of the chain when left out.
    #[serde(rename = "fx.add")]
    Add {
        track: TrackRef,
        /// A catalog id such as `builtin:compressor`, `container` for an empty FX container,
        /// or an exact name.
        plugin: String,
        /// Position in the chain, from 0.
        #[serde(default)]
        index: Option<usize>,
        #[serde(flatten)]
        at: ChainAt,
    },
    /// Removes the effect at `index` in the chain.
    #[serde(rename = "fx.remove")]
    Remove {
        track: TrackRef,
        /// Position in the chain, from 0.
        index: usize,
        #[serde(flatten)]
        at: ChainAt,
    },
    /// Bypasses the effect, or lets it process again; toggles when `bypassed` is left out.
    #[serde(rename = "fx.bypass")]
    Bypass {
        track: TrackRef,
        /// Position in the chain, from 0.
        index: usize,
        #[serde(default)]
        bypassed: Option<bool>,
        #[serde(flatten)]
        at: ChainAt,
    },
    /// The parameters of a running effect with their values.
    #[serde(rename = "fx.params")]
    Params {
        track: TrackRef,
        /// Position in the chain, from 0.
        index: usize,
        #[serde(flatten)]
        at: ChainAt,
    },
    /// Sets parameter `param` (an id from `fx.params`) to `value`, within its range, as one
    /// undo step.
    #[serde(rename = "fx.param")]
    SetParam {
        track: TrackRef,
        /// Position in the chain, from 0.
        index: usize,
        /// A parameter id from `fx.params`.
        param: u32,
        /// Clamped to the parameter's range.
        value: f64,
        #[serde(flatten)]
        at: ChainAt,
    },
    /// What a running effect measures: its input and output peaks, and for compressors, gates,
    /// limiters and multiband compressors the gain reduction, detector level and gate state.
    /// Measuring starts with the first call and runs for about two seconds after each one, so
    /// a first call can read nothing.
    #[serde(rename = "fx.meters")]
    Meters {
        track: TrackRef,
        /// Position in the chain, from 0.
        index: usize,
        #[serde(flatten)]
        at: ChainAt,
    },
    /// Adds, removes or retypes a band of a ScyEQ (the built-in equalizer).
    #[serde(rename = "fx.eq_band")]
    EqBand {
        track: TrackRef,
        /// Position in the chain, from 0.
        index: usize,
        action: ListAction,
        /// The band to remove or change, from 0.
        #[serde(default)]
        band: Option<usize>,
        /// The band's type, for `add` (a band when left out) and `set`.
        #[serde(default)]
        kind: Option<EqKind>,
        /// Frequency in Hz of a band added (1000 when left out).
        #[serde(default)]
        freq: Option<f64>,
        /// Gain in dB of a band added (0 when left out).
        #[serde(default)]
        gain_db: Option<f64>,
        /// Width in octaves of a band added (1 when left out).
        #[serde(default)]
        width: Option<f64>,
        #[serde(flatten)]
        at: ChainAt,
    },
    /// Adds, removes or changes a tap of a ScyDelay (the built-in delay).
    #[serde(rename = "fx.delay_tap")]
    DelayTap {
        track: TrackRef,
        /// Position in the chain, from 0.
        index: usize,
        action: ListAction,
        /// The tap to remove or change, from 0.
        #[serde(default)]
        tap: Option<usize>,
        /// Delay in milliseconds, added to `musical`.
        #[serde(default)]
        time_ms: Option<f64>,
        /// Delay in eighth notes, added to `time_ms`.
        #[serde(default)]
        musical: Option<f64>,
        /// Feedback in dB (-60 and below for none).
        #[serde(default)]
        feedback_db: Option<f64>,
        /// The tap's level in dB.
        #[serde(default)]
        volume_db: Option<f64>,
        /// -1 (left) to 1 (right).
        #[serde(default)]
        pan: Option<f64>,
        /// Low-pass cutoff in Hz (20000 for off).
        #[serde(default)]
        lowpass: Option<f64>,
        /// High-pass cutoff in Hz (0 for off).
        #[serde(default)]
        highpass: Option<f64>,
        #[serde(default)]
        enabled: Option<bool>,
        #[serde(flatten)]
        at: ChainAt,
    },
    /// Replaces the curve of a ScyFir (the built-in FFT EQ) in its EQ mode: points of
    /// frequency in Hz and gain in dB.
    #[serde(rename = "fx.fir_points")]
    FirPoints {
        track: TrackRef,
        /// Position in the chain, from 0.
        index: usize,
        /// `[hz, db]` pairs.
        points: Vec<[f32; 2]>,
        #[serde(flatten)]
        at: ChainAt,
    },
    /// Sets the effect's wet/dry mix and delta solo (playing only the difference it makes).
    #[serde(rename = "fx.wet")]
    Wet {
        track: TrackRef,
        /// Position in the chain, from 0.
        index: usize,
        /// Wet share, 0 (dry) to 1 (wet).
        #[serde(default)]
        wet: Option<f64>,
        #[serde(default)]
        delta: Option<bool>,
        #[serde(flatten)]
        at: ChainAt,
    },
    /// Runs the effect in parallel with the one before it (on the same input, the outputs
    /// summed), or in series again.
    #[serde(rename = "fx.parallel")]
    Parallel {
        track: TrackRef,
        /// Position in the chain, from 0; the first effect cannot be parallel.
        index: usize,
        mode: ParallelMode,
        #[serde(flatten)]
        at: ChainAt,
    },
    /// Takes the effect offline (unloaded, passing audio through) or back online; toggles when
    /// `offline` is left out.
    #[serde(rename = "fx.offline")]
    Offline {
        track: TrackRef,
        /// Position in the chain, from 0.
        index: usize,
        #[serde(default)]
        offline: Option<bool>,
        #[serde(flatten)]
        at: ChainAt,
    },
    /// Moves the effect at `index` into the container at `container` (at `position` there,
    /// at the end when left out), both in the chain `at` names.
    #[serde(rename = "fx.container.move_in")]
    ContainerMoveIn {
        track: TrackRef,
        /// The effect to move.
        index: usize,
        /// The container to move it into.
        container: usize,
        #[serde(default)]
        position: Option<usize>,
        #[serde(flatten)]
        at: ChainAt,
    },
    /// Moves effect `child` out of the container at `index`, to just after the container.
    #[serde(rename = "fx.container.move_out")]
    ContainerMoveOut {
        track: TrackRef,
        /// The container.
        index: usize,
        /// Position of the effect inside it.
        child: usize,
        #[serde(flatten)]
        at: ChainAt,
    },
    /// Sets a container's channel count and how many input and output pins connect it to the
    /// chain around it.
    #[serde(rename = "fx.container.config")]
    ContainerConfig {
        track: TrackRef,
        /// Position of the container in the chain, from 0.
        index: usize,
        /// The container's own channels, 2-64.
        #[serde(default)]
        channels: Option<u32>,
        #[serde(default)]
        inputs: Option<u32>,
        #[serde(default)]
        outputs: Option<u32>,
        #[serde(flatten)]
        at: ChainAt,
    },
    /// Routes a container's pins: per pin, the outer channels (from 1) it takes its input
    /// from, or feeds its output to.
    #[serde(rename = "fx.container.pins")]
    ContainerPins {
        track: TrackRef,
        /// Position of the container in the chain, from 0.
        index: usize,
        /// Output pins rather than input pins.
        #[serde(default)]
        output: bool,
        /// One list of channels per pin, from pin 1.
        pins: Vec<Vec<u32>>,
        #[serde(flatten)]
        at: ChainAt,
    },
    /// Exposes parameter `param` of the container's effect `child` as a parameter of the
    /// container, so envelopes and controls on the container drive it.
    #[serde(rename = "fx.container.map")]
    ContainerMap {
        track: TrackRef,
        /// Position of the container in the chain, from 0.
        index: usize,
        /// Position of the effect inside the container.
        child: usize,
        /// The parameter's position in the effect's parameter list.
        param: usize,
        #[serde(flatten)]
        at: ChainAt,
    },
    /// Sets how many channels a take's effects run on (2-64); `null` follows the source.
    #[serde(rename = "fx.take_channels")]
    TakeFxChannels {
        /// Item id.
        item: u64,
        /// Take index, the active take when left out.
        #[serde(default)]
        take: Option<usize>,
        #[serde(default)]
        channels: Option<u32>,
    },
}

/// Which chain of a track an `fx.*` command works on, and how deep into containers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ChainAt {
    /// `track` (the default), `input` (the record input chain, on the live input the track monitors),
    /// `take` (with `item` and `take`), or `monitor` (the monitoring chain after the master,
    /// heard on the device only and never rendered; `track` is then ignored).
    #[serde(default)]
    pub chain: Option<ChainKind>,
    /// For the `take` chain: the item id.
    #[serde(default)]
    pub item: Option<u64>,
    /// For the `take` chain: the take index, the active take when left out.
    #[serde(default)]
    pub take: Option<usize>,
    /// The container the effect sits in, as positions from the top of the chain (each one
    /// inside the container before it); left out for the chain itself.
    #[serde(default)]
    pub inside: Vec<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChainKind {
    Track,
    Input,
    Take,
    Monitor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ParallelMode {
    Serial,
    Parallel,
    /// In parallel, its MIDI output merged with the previous effect's.
    MergeMidi,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// What `fx.add` takes: `builtin:<name>`, `clap:<id>`, `lv2:<uri>` or `vst3:<name>`.
    pub id: String,
    pub name: String,
    pub format: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamValue {
    pub id: u32,
    pub name: String,
    pub value: f64,
    /// The value as the plugin shows it.
    pub text: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
}

fn catalog(session: &Session) -> Vec<(CatalogEntry, FxSlot)> {
    catalog_of(&session.engine.fx().catalog().snapshot())
}

fn catalog_of(installed: &scythe_engine::plugin_scan::Snapshot) -> Vec<(CatalogEntry, FxSlot)> {
    let mut out: Vec<(CatalogEntry, FxSlot)> = Builtin::ALL
        .iter()
        .map(|b| {
            let entry = CatalogEntry {
                id: format!("builtin:{}", b.label().to_lowercase().replace(' ', "-")),
                name: b.label().into(),
                format: "built in".into(),
            };
            (entry, b.slot())
        })
        .collect();
    out.extend(installed.clap.iter().map(|d| {
        let entry = CatalogEntry { id: format!("clap:{}", d.id), name: d.name.clone(), format: "CLAP".into() };
        (entry, scythe_engine::fx::clap_slot(d))
    }));
    out.extend(installed.lv2.iter().map(|i| {
        let entry = CatalogEntry { id: format!("lv2:{}", i.uri), name: i.name.clone(), format: "LV2".into() };
        (entry, scythe_engine::fx::lv2_slot(i))
    }));
    out.extend(installed.vst3.iter().map(|i| {
        let entry = CatalogEntry { id: format!("vst3:{}", i.name), name: i.name.clone(), format: "VST3".into() };
        (entry, scythe_engine::fx::vst3_slot(i))
    }));
    out
}

/// Built-in effects first: they never depend on the plugin scan.
fn find_plugin(session: &Session, wanted: &str) -> Option<FxSlot> {
    let builtin = Builtin::ALL.iter().find(|b| {
        let id = format!("builtin:{}", b.label().to_lowercase().replace(' ', "-"));
        id == wanted
            || b.label().eq_ignore_ascii_case(wanted)
            || (matches!(b, Builtin::ScytheEq) && wanted == "builtin:scythe-eq")
    });
    if let Some(b) = builtin {
        return Some(b.slot());
    }
    let all = catalog(session);
    let found = all.iter().find(|(e, _)| e.id == wanted).or_else(|| all.iter().find(|(e, _)| e.name == wanted));
    found.map(|(_, slot)| slot.clone())
}

/// Where a chain lives.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Loc {
    Track(Target),
    Input(TrackId),
    Take(TrackId, ItemId, usize),
    Monitor,
}

impl Loc {
    pub(crate) fn owner(self) -> FxOwner {
        match self {
            Self::Track(Target::Master) => FxOwner::Master,
            Self::Track(Target::Track(id)) => FxOwner::Track(id),
            Self::Input(id) => FxOwner::Input(id),
            Self::Take(_, item, take) => FxOwner::Take(item, take),
            Self::Monitor => FxOwner::Monitor,
        }
    }
}

pub(crate) fn locate(session: &Session, track: &TrackRef, at: &ChainAt) -> Result<Loc, ApiError> {
    match at.chain.unwrap_or(ChainKind::Track) {
        ChainKind::Monitor => Ok(Loc::Monitor),
        ChainKind::Track => Ok(Loc::Track(track.resolve(&session.project)?)),
        ChainKind::Input => Ok(Loc::Input(track.track(&session.project)?)),
        ChainKind::Take => {
            let item = at.item.ok_or_else(|| ApiError::invalid("the take chain needs `item`"))?;
            take_loc(session, item, at.take)
        }
    }
}

fn take_loc(session: &Session, item: u64, take: Option<usize>) -> Result<Loc, ApiError> {
    for t in &session.project.tracks {
        if let Some(it) = t.item(ItemId(item)) {
            let k = take.unwrap_or(it.active_take);
            if k >= it.takes.len() {
                return Err(ApiError::not_found(format!("the item has {} take(s)", it.takes.len())));
            }
            return Ok(Loc::Take(t.id, it.id, k));
        }
    }
    Err(ApiError::not_found(format!("no item with id {item}")))
}

pub(crate) fn chain(session: &Session, loc: Loc) -> Result<Vec<FxSlot>, ApiError> {
    let gone = || ApiError::not_found("the track is gone");
    Ok(match loc {
        Loc::Track(target) => target.get(&session.project).ok_or_else(gone)?.fx.clone(),
        Loc::Input(id) => session.project.track(id).ok_or_else(gone)?.input_fx(),
        Loc::Take(t, item, k) => {
            let take = session.project.track(t).and_then(|t| t.item(item)).and_then(|i| i.takes.get(k));
            take.ok_or_else(|| ApiError::not_found("the take is gone"))?.fx()
        }
        Loc::Monitor => session.monitor_fx(),
    })
}

/// Changes a chain as one undo step (the monitoring chain is saved with the preferences
/// instead).
fn edit_chain(
    session: &mut Session,
    loc: Loc,
    label: &str,
    f: impl FnOnce(&mut Vec<FxSlot>) -> Result<(), ApiError>,
) -> Result<(), ApiError> {
    let mut slots = chain(session, loc)?;
    f(&mut slots)?;
    match loc {
        Loc::Monitor => session.set_monitor_fx(slots),
        Loc::Track(target) => session.edit(label, |p| {
            if let Some(t) = target.get_mut(p) {
                t.fx = slots;
            }
        }),
        Loc::Input(id) => session.edit(label, |p| {
            if let Some(t) = p.track_mut(id) {
                t.set_input_fx(&slots);
            }
        }),
        Loc::Take(t, item, k) => session.edit(label, |p| {
            if let Some(take) = p.track_mut(t).and_then(|t| t.item_mut(item)).and_then(|i| i.takes.get_mut(k)) {
                take.set_fx(&slots);
            }
        }),
    }
    Ok(())
}

/// The list of effects inside the container at `inside` (the chain itself when empty).
fn list_at<'a>(
    slots: &'a mut Vec<FxSlot>,
    inside: &[usize],
    f: &mut dyn FnMut(&mut Vec<FxSlot>) -> Result<(), ApiError>,
) -> Result<(), ApiError> {
    let Some((&first, rest)) = inside.split_first() else { return f(slots) };
    let count = slots.len();
    let slot = slots
        .get_mut(first)
        .ok_or_else(|| ApiError::not_found(format!("the chain has {count} effect(s); there is no effect {first}")))?;
    if !slot.is_container() {
        return Err(ApiError::invalid(format!("effect {first} ({}) is not a container", slot.name)));
    }
    let mut children = slot.container_children();
    list_at(&mut children, rest, f)?;
    slot.set_container_children(&children);
    Ok(())
}

/// Runs `f` on effect `index` of the list `inside` names.
fn slot_at(
    slots: &mut Vec<FxSlot>,
    inside: &[usize],
    index: usize,
    f: impl FnOnce(&mut FxSlot) -> Result<(), ApiError>,
) -> Result<(), ApiError> {
    let mut f = Some(f);
    list_at(slots, inside, &mut |list| {
        let count = list.len();
        let slot = list.get_mut(index).ok_or_else(|| {
            ApiError::not_found(format!("there are {count} effect(s) here; there is no effect {index}"))
        })?;
        (f.take().expect("called once"))(slot)
    })
}

fn get_slot(session: &Session, loc: Loc, at: &ChainAt, index: usize) -> Result<FxSlot, ApiError> {
    let mut slots = chain(session, loc)?;
    let mut found = None;
    slot_at(&mut slots, &at.inside, index, |s| {
        found = Some(s.clone());
        Ok(())
    })?;
    found.ok_or_else(|| ApiError::not_found("no such effect"))
}

fn list(session: &Session, loc: Loc) -> Value {
    match chain(session, loc) {
        Ok(slots) => json(snapshot::chain_info(session, &loc.owner(), &slots)),
        Err(_) => Value::Null,
    }
}

pub(crate) fn instance(
    session: &Session,
    loc: Loc,
    at: &ChainAt,
    index: usize,
) -> Result<std::sync::Arc<scythe_engine::fx::FxInstance>, ApiError> {
    let slot = get_slot(session, loc, at, index)?;
    let slots = chain(session, loc)?;
    let path: Vec<usize> = at.inside.iter().copied().chain([index]).collect();
    session
        .engine
        .fx()
        .instance_at(&loc.owner(), &slots, &path)
        .ok_or_else(|| ApiError::failed(format!("{} is not running", slot.name)))
}

fn params(session: &Session, loc: Loc, at: &ChainAt, index: usize) -> Result<Vec<ParamValue>, ApiError> {
    let inst = instance(session, loc, at, index)?;
    let values = inst
        .with(|e| {
            e.params()
                .into_iter()
                .map(|p| {
                    let (value, text) = e.param(p.id).unwrap_or((p.default, String::new()));
                    ParamValue { id: p.id, name: p.name, value, text, min: p.min, max: p.max, default: p.default }
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(values)
}

/// What a list command does with its entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ListAction {
    Add,
    Remove,
    Set,
}

/// A ScyEQ band type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EqKind {
    LowShelf,
    HighShelf,
    Band,
    Notch,
    LowPass,
    HighPass,
    BandPass,
    AllPass,
}

impl EqKind {
    fn kind(self) -> scythe_engine::reaeq::BandKind {
        use scythe_engine::reaeq::BandKind as K;
        match self {
            Self::LowShelf => K::LowShelf,
            Self::HighShelf => K::HighShelf,
            Self::Band => K::Peak,
            Self::Notch => K::Notch,
            Self::LowPass => K::LowPass,
            Self::HighPass => K::HighPass,
            Self::BandPass => K::BandPass,
            Self::AllPass => K::AllPass,
        }
    }
}

/// Runs `f` on the effect as the stock type `T`, as one undo step.
pub(crate) fn edit_stock<T: 'static, R>(
    session: &mut Session,
    inst: &scythe_engine::fx::FxInstance,
    label: &str,
    what: &str,
    f: impl FnOnce(&mut T) -> Result<R, ApiError>,
) -> Result<R, ApiError> {
    session
        .fx_edit(label, inst, |e| e.as_any_mut().and_then(|a| a.downcast_mut::<T>()).map(f))
        .flatten()
        .ok_or_else(|| ApiError::invalid(format!("the effect is not a {what}")))?
}

fn meters(inst: &scythe_engine::fx::FxInstance) -> Value {
    use scythe_engine::probe;
    let p = &inst.probe;
    p.lease(200);
    let db = |v: f32| if v > 1e-8 { 20.0 * (v as f64).log10() } else { -150.0 };
    let bands: Vec<f64> = (0..probe::BANDS).map(|k| p.take(probe::BAND_REDUCTION + k) as f64).collect();
    let used = bands.iter().rposition(|v| *v > 0.0).map_or(0, |k| k + 1);
    serde_json::json!({
        "input_db": db(p.take(probe::INPUT)),
        "output_db": db(p.take(probe::OUTPUT)),
        "reduction_db": p.take(probe::REDUCTION),
        "detector_db": db(p.take(probe::DETECTOR)),
        "gate_open": p.take(probe::GATE),
        "band_reduction_db": &bands[..used],
    })
}

pub fn execute(session: &mut Session, command: FxCommand) -> Result<Value, ApiError> {
    match command {
        FxCommand::List { track, at } => {
            let loc = locate(session, &track, &at)?;
            Ok(list(session, loc))
        }
        FxCommand::Catalog { query, rescan } => {
            let installed = session.engine.fx().catalog();
            if rescan {
                installed.rescan(true);
            }
            let installed = installed.snapshot();
            let query = query.map(|q| q.to_lowercase());
            let matches = |e: &CatalogEntry| {
                query
                    .as_deref()
                    .is_none_or(|q| [&e.name, &e.id, &e.format].iter().any(|field| field.to_lowercase().contains(q)))
            };
            let plugins: Vec<CatalogEntry> =
                catalog_of(&installed).into_iter().map(|(e, _)| e).filter(matches).collect();
            Ok(serde_json::json!({
                "plugins": plugins,
                "scanning": installed.scanning,
                "progress": installed.progress,
                "failed": *installed.failures,
            }))
        }
        FxCommand::Add { track, plugin, index, at } => {
            let loc = locate(session, &track, &at)?;
            let slot = if plugin.eq_ignore_ascii_case("container") {
                FxSlot::container()
            } else {
                find_plugin(session, &plugin).ok_or_else(|| {
                    let installed = session.engine.fx().catalog().snapshot();
                    if installed.scanning {
                        let p = &installed.progress;
                        ApiError::not_found(format!(
                            "no effect \"{plugin}\" yet: plugins are still being scanned ({} of {}); ask fx.catalog again shortly",
                            p.done, p.total
                        ))
                    } else {
                        ApiError::not_found(format!("no effect \"{plugin}\"; see fx.catalog"))
                    }
                })?
            };
            let mut slot = Some(slot);
            edit_chain(session, loc, "Add plugin", |slots| {
                list_at(slots, &at.inside, &mut |list| {
                    let count = list.len();
                    if index.is_some_and(|i| i > count) {
                        return Err(ApiError::invalid(format!("index runs from 0 to {count}")));
                    }
                    list.insert(index.unwrap_or(count), slot.take().expect("inserted once"));
                    Ok(())
                })
            })?;
            Ok(list(session, loc))
        }
        FxCommand::Remove { track, index, at } => {
            let loc = locate(session, &track, &at)?;
            edit_chain(session, loc, "Remove plugin", |slots| {
                list_at(slots, &at.inside, &mut |list| {
                    if index >= list.len() {
                        return Err(ApiError::not_found(format!("there are {} effect(s) here", list.len())));
                    }
                    list.remove(index);
                    Ok(())
                })
            })?;
            Ok(list(session, loc))
        }
        FxCommand::Bypass { track, index, bypassed, at } => {
            let loc = locate(session, &track, &at)?;
            edit_chain(session, loc, "Toggle FX bypass", |slots| {
                slot_at(slots, &at.inside, index, |s| {
                    s.enabled = !bypassed.unwrap_or(s.enabled);
                    Ok(())
                })
            })?;
            Ok(list(session, loc))
        }
        FxCommand::Offline { track, index, offline, at } => {
            let loc = locate(session, &track, &at)?;
            edit_chain(session, loc, "Toggle FX offline", |slots| {
                slot_at(slots, &at.inside, index, |s| {
                    let off = offline.unwrap_or(!s.offline());
                    s.set_offline(off);
                    Ok(())
                })
            })?;
            Ok(list(session, loc))
        }
        FxCommand::Wet { track, index, wet, delta, at } => {
            let loc = locate(session, &track, &at)?;
            if wet.is_some_and(|w| !(0.0..=1.0).contains(&w)) {
                return Err(ApiError::invalid("wet runs from 0 to 1"));
            }
            edit_chain(session, loc, "FX wet/dry", |slots| {
                slot_at(slots, &at.inside, index, |s| {
                    let (w, d) = s.wet();
                    s.set_wet(wet.unwrap_or(w), delta.unwrap_or(d));
                    Ok(())
                })
            })?;
            Ok(list(session, loc))
        }
        FxCommand::Parallel { track, index, mode, at } => {
            let loc = locate(session, &track, &at)?;
            let mode = match mode {
                ParallelMode::Serial => Parallel::Serial,
                ParallelMode::Parallel => Parallel::Audio,
                ParallelMode::MergeMidi => Parallel::MergeMidi,
            };
            if index == 0 && mode != Parallel::Serial {
                return Err(ApiError::invalid("the first effect has nothing to run in parallel with"));
            }
            edit_chain(session, loc, "FX parallel", |slots| {
                slot_at(slots, &at.inside, index, |s| {
                    s.set_parallel(mode);
                    Ok(())
                })
            })?;
            Ok(list(session, loc))
        }
        FxCommand::ContainerMoveIn { track, index, container, position, at } => {
            let loc = locate(session, &track, &at)?;
            if index == container {
                return Err(ApiError::invalid("an effect cannot go into itself"));
            }
            edit_chain(session, loc, "Move FX into container", |slots| {
                list_at(slots, &at.inside, &mut |list| {
                    let n = list.len();
                    if index >= n || container >= n {
                        return Err(ApiError::not_found(format!("there are {n} effect(s) here")));
                    }
                    if !list[container].is_container() {
                        return Err(ApiError::invalid(format!("effect {container} is not a container")));
                    }
                    let fx = list.remove(index);
                    let c = if index < container { container - 1 } else { container };
                    let mut children = list[c].container_children();
                    let at = position.unwrap_or(children.len()).min(children.len());
                    children.insert(at, fx);
                    list[c].set_container_children(&children);
                    Ok(())
                })
            })?;
            Ok(list(session, loc))
        }
        FxCommand::ContainerMoveOut { track, index, child, at } => {
            let loc = locate(session, &track, &at)?;
            edit_chain(session, loc, "Move FX out of container", |slots| {
                list_at(slots, &at.inside, &mut |list| {
                    let c = list
                        .get_mut(index)
                        .filter(|s| s.is_container())
                        .ok_or_else(|| ApiError::invalid(format!("effect {index} is not a container")))?;
                    let mut children = c.container_children();
                    if child >= children.len() {
                        return Err(ApiError::not_found(format!("the container has {} effect(s)", children.len())));
                    }
                    let fx = children.remove(child);
                    c.set_container_children(&children);
                    list.insert(index + 1, fx);
                    Ok(())
                })
            })?;
            Ok(list(session, loc))
        }
        FxCommand::ContainerConfig { track, index, channels, inputs, outputs, at } => {
            let loc = locate(session, &track, &at)?;
            edit_chain(session, loc, "Container channels", |slots| {
                slot_at(slots, &at.inside, index, |s| {
                    let mut cfg = s.container_config().ok_or_else(|| ApiError::invalid("not a container"))?;
                    cfg.channels = channels.unwrap_or(cfg.channels).clamp(2, 64);
                    cfg.inputs = inputs.unwrap_or(cfg.inputs).clamp(0, cfg.channels);
                    cfg.outputs = outputs.unwrap_or(cfg.outputs).clamp(0, cfg.channels);
                    s.set_container_config(cfg);
                    Ok(())
                })
            })?;
            Ok(list(session, loc))
        }
        FxCommand::ContainerPins { track, index, output, pins, at } => {
            let loc = locate(session, &track, &at)?;
            if pins.iter().flatten().any(|&c| c == 0 || c > 64) {
                return Err(ApiError::invalid("channels run from 1 to 64"));
            }
            let masks: Vec<u64> = pins.iter().map(|p| p.iter().fold(0u64, |m, &c| m | 1 << (c - 1))).collect();
            edit_chain(session, loc, "Container pins", |slots| {
                slot_at(slots, &at.inside, index, |s| {
                    if !s.is_container() {
                        return Err(ApiError::invalid("not a container"));
                    }
                    s.set_listed_pins(output, &masks);
                    Ok(())
                })
            })?;
            Ok(list(session, loc))
        }
        FxCommand::ContainerMap { track, index, child, param, at } => {
            let loc = locate(session, &track, &at)?;
            let mut id = None;
            edit_chain(session, loc, "Map container parameter", |slots| {
                slot_at(slots, &at.inside, index, |s| {
                    if child >= s.container_children().len() {
                        return Err(ApiError::not_found("no such effect in the container"));
                    }
                    id = s.add_container_param(child, param);
                    Ok(())
                })
            })?;
            Ok(serde_json::json!({ "id": id }))
        }
        FxCommand::TakeFxChannels { item, take, channels } => {
            let Loc::Take(t, item, k) = take_loc(session, item, take)? else { unreachable!() };
            if channels.is_some_and(|c| !(2..=64).contains(&c)) {
                return Err(ApiError::invalid("take FX run on 2 to 64 channels"));
            }
            session.edit("Take FX channels", |p| {
                if let Some(take) = p.track_mut(t).and_then(|t| t.item_mut(item)).and_then(|i| i.takes.get_mut(k)) {
                    take.set_fx_channels(channels);
                }
            });
            Ok(serde_json::json!({ "channels": channels }))
        }
        FxCommand::Meters { track, index, at } => {
            let loc = locate(session, &track, &at)?;
            Ok(meters(&*instance(session, loc, &at, index)?))
        }
        FxCommand::EqBand { track, index, action, band, kind, freq, gain_db, width, at } => {
            use scythe_engine::reaeq::{Band, ReaEqCompat};
            let loc = locate(session, &track, &at)?;
            let inst = instance(session, loc, &at, index)?;
            let label = match action {
                ListAction::Add => "Add EQ band",
                ListAction::Remove => "Remove EQ band",
                ListAction::Set => "Change EQ band type",
            };
            let bands = edit_stock(session, &inst, label, "ScyEQ", |eq: &mut ReaEqCompat| {
                let existing = |b: Option<usize>, n: usize| {
                    b.filter(|b| *b < n).ok_or_else(|| ApiError::not_found(format!("the EQ has {n} band(s)")))
                };
                match action {
                    ListAction::Add => {
                        eq.add_band(Band {
                            kind: kind.map_or(scythe_engine::reaeq::BandKind::Peak, EqKind::kind),
                            enabled: true,
                            freq: freq.unwrap_or(1000.0).clamp(10.0, 24000.0),
                            gain: 10f64.powf(gain_db.unwrap_or(0.0).clamp(-60.0, 36.0) / 20.0),
                            bandwidth: width.unwrap_or(1.0).clamp(0.01, 8.0),
                        });
                    }
                    ListAction::Remove => eq.remove_band(existing(band, eq.bands.len())?),
                    ListAction::Set => {
                        let kind = kind.ok_or_else(|| ApiError::invalid("`set` needs `kind`"))?;
                        eq.set_kind(existing(band, eq.bands.len())?, kind.kind());
                    }
                }
                Ok(eq.bands.len())
            })?;
            Ok(serde_json::json!({ "bands": bands }))
        }
        FxCommand::DelayTap {
            track,
            index,
            action,
            tap,
            time_ms,
            musical,
            feedback_db,
            volume_db,
            pan,
            lowpass,
            highpass,
            enabled,
            at,
        } => {
            use scythe_engine::delay::ReaDelayCompat;
            let loc = locate(session, &track, &at)?;
            let inst = instance(session, loc, &at, index)?;
            let label = match action {
                ListAction::Add => "Add delay tap",
                ListAction::Remove => "Remove delay tap",
                ListAction::Set => "Change delay tap",
            };
            let gain = |db: f64| if db <= -59.9 { 0.0 } else { 10f64.powf(db.min(12.0) / 20.0) };
            let taps = edit_stock(session, &inst, label, "ScyDelay", |d: &mut ReaDelayCompat| {
                let taps = d.taps();
                let at = match action {
                    ListAction::Add => None,
                    _ => Some(
                        tap.filter(|t| *t < taps.len())
                            .ok_or_else(|| ApiError::not_found(format!("the delay has {} tap(s)", taps.len())))?,
                    ),
                };
                if action == ListAction::Remove {
                    d.remove_tap(at.unwrap_or_default());
                    return Ok(d.taps().len());
                }
                let mut t = at.map_or_else(|| *taps.last().unwrap(), |k| taps[k]);
                t.time_ms = time_ms.map_or(t.time_ms, |v| v.clamp(0.0, 10_000.0));
                t.musical = musical.map_or(t.musical, |v| v.clamp(0.0, 256.0));
                t.feedback = feedback_db.map_or(t.feedback, |v| gain(v).min(1.0));
                t.volume = volume_db.map_or(t.volume, gain);
                t.pan = pan.map_or(t.pan, |v| v.clamp(-1.0, 1.0));
                t.lowpass = lowpass.map_or(t.lowpass, |v| v.clamp(0.0, 20_000.0));
                t.hipass = highpass.map_or(t.hipass, |v| v.clamp(0.0, 20_000.0));
                t.enabled = enabled.unwrap_or(t.enabled);
                match at {
                    Some(k) => d.set_tap(k, t),
                    None => {
                        d.add_tap(t);
                    }
                }
                Ok(d.taps().len())
            })?;
            Ok(serde_json::json!({ "taps": taps }))
        }
        FxCommand::FirPoints { track, index, points, at } => {
            use scythe_engine::stock::reafir::Fir;
            let loc = locate(session, &track, &at)?;
            let inst = instance(session, loc, &at, index)?;
            if points.iter().flatten().any(|v| !v.is_finite()) || points.len() > 256 {
                return Err(ApiError::invalid("up to 256 points of finite numbers"));
            }
            let points: Vec<(f32, f32)> =
                points.iter().map(|[hz, db]| (hz.clamp(1.0, 24000.0), db.clamp(-90.0, 24.0))).collect();
            let n = edit_stock(session, &inst, "Draw FFT EQ curve", "ScyFir", |f: &mut Fir| {
                f.set_points(points);
                Ok(f.points().len())
            })?;
            Ok(serde_json::json!({ "points": n }))
        }
        FxCommand::Params { track, index, at } => {
            let loc = locate(session, &track, &at)?;
            Ok(json(params(session, loc, &at, index)?))
        }
        FxCommand::SetParam { track, index, param, value, at } => {
            let loc = locate(session, &track, &at)?;
            let inst = instance(session, loc, &at, index)?;
            let info = inst.with(|e| e.params().into_iter().find(|p| p.id == param)).flatten();
            let Some(info) = info else { return Err(ApiError::not_found(format!("no parameter {param}"))) };
            if !value.is_finite() {
                return Err(ApiError::invalid("the value must be a number"));
            }
            let label = format!("Adjust {}", info.name);
            session.fx_edit(&label, &inst, |e| e.set_param(param, value.clamp(info.min, info.max)));
            let values = params(session, loc, &at, index)?;
            Ok(values.into_iter().find(|p| p.id == param).map_or(Value::Null, json))
        }
    }
}

/// The effect and parameter an `fx.param` command (as JSON) names, as people say them:
/// ("ScyComp", "Threshold").
pub fn param_names(session: &Session, command: &Value) -> Option<(String, String)> {
    let Ok(FxCommand::SetParam { track, index, param, at, .. }) = serde_json::from_value(command.clone()) else {
        return None;
    };
    let loc = locate(session, &track, &at).ok()?;
    let effect = crate::agents::plugin_name(&get_slot(session, loc, &at, index).ok()?.name);
    let inst = instance(session, loc, &at, index).ok()?;
    let name = inst.with(|e| e.params().into_iter().find(|p| p.id == param)).flatten()?.name;
    Some((effect, name))
}
