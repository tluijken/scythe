//! Measuring mixes and files to match a mix to a reference: loudness, peaks, spectrum, width
//! (see `scythe_engine::analysis`). Measuring takes seconds, so each command starts a job on a
//! thread of its own and answers at once; `analysis.job` reports it and its result. Mixes are
//! rendered offline through the render path, never played. Reference files are measured on
//! this machine only, and their measurements kept (`analysis-cache` in Scythe's data folder),
//! so measuring the same file again is instant.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, UNIX_EPOCH};

use schemars::JsonSchema;
use scythe_core::model::SoloMode;
use scythe_core::render_settings::{RenderSettings, RenderSource, ResampleMode};
use scythe_core::{Project, TimeRange};
use scythe_engine::MediaPool;
use scythe_engine::analysis::eq::{self, EqBand, EqKind, MatchOptions};
use scythe_engine::analysis::segments::{self, SegmentOptions};
use scythe_engine::analysis::{self, Analysis, Analyzer};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json as value};

use super::{ApiError, Target, TrackRef, json};
use crate::Session;

/// What to measure: an audio file, the project's mix, or a measurement already made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Subject {
    /// A finished `analysis.file` or `analysis.mix` job, by its number.
    Job { job: u64 },
    /// An audio file on the machine running Scythe, such as a reference track.
    File {
        file: PathBuf,
        /// Seconds into the file to start at; the start when left out.
        #[serde(default)]
        start: Option<f64>,
        /// Seconds into the file to end at; the end when left out.
        #[serde(default)]
        end: Option<f64>,
    },
    /// The project rendered offline: `"master"`, a track (a folder track renders as its bus),
    /// or `{"group": n}` for the tracks of a track group.
    Mix {
        mix: MixTarget,
        /// Project time to start at, in seconds; 0 when left out.
        #[serde(default)]
        start: Option<f64>,
        /// Project time to end at; the project's end when left out.
        #[serde(default)]
        end: Option<f64>,
    },
}

/// What part of the mix to render.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum MixTarget {
    /// The tracks of track group `group` (1-64), soloed through the master.
    Group { group: usize },
    /// `"master"`, or a track by id or name (its own output, a folder with its children).
    Track(TrackRef),
}

/// An EQ band as `analysis.match_eq` proposes it and `analysis.apply_eq` takes it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Band {
    /// `low_shelf`, `high_shelf` or `peak`.
    pub kind: BandKind,
    /// Hz.
    pub freq: f64,
    /// dB, at most 12 either way.
    pub gain_db: f64,
    /// Octaves (for shelves, the slope: 1 is a gentle one).
    #[serde(default = "one")]
    pub bandwidth: f64,
}

fn one() -> f64 {
    1.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BandKind {
    LowShelf,
    HighShelf,
    Peak,
}

impl From<Band> for EqBand {
    fn from(b: Band) -> Self {
        let kind = match b.kind {
            BandKind::LowShelf => EqKind::LowShelf,
            BandKind::HighShelf => EqKind::HighShelf,
            BandKind::Peak => EqKind::Peak,
        };
        EqBand { kind, freq: b.freq, gain_db: b.gain_db, bandwidth: b.bandwidth }
    }
}

/// Measuring commands start a job and answer `{"job", "status": "running"}` (or `"done"` with
/// the `result` when it was measured before); `analysis.job` gives the result once done. MCP
/// tools wait for the job themselves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum AnalysisCommand {
    /// Measures an audio file: integrated, short-term and momentary loudness (EBU R128),
    /// loudness range, true peak, crest factor, a third-octave long-term spectrum with a
    /// tonal balance curve normalised to the loudness, stereo width and correlation per band,
    /// a low-end mono check, spectral centroid, brightness and transients per second.
    #[serde(rename = "analysis.file")]
    File {
        /// An audio file on the machine running Scythe.
        path: PathBuf,
        /// Seconds into the file to start at.
        #[serde(default)]
        start: Option<f64>,
        /// Seconds into the file to end at.
        #[serde(default)]
        end: Option<f64>,
    },
    /// Renders the mix offline (nothing is heard) through Scythe's render path and measures it
    /// as `analysis.file` does: the master, one track's output, or a track group.
    #[serde(rename = "analysis.mix")]
    Mix {
        /// `"master"` (default), a track id or name, or `{"group": n}`.
        #[serde(default)]
        target: Option<MixTarget>,
        /// Project time to start at, in seconds; 0 when left out.
        #[serde(default)]
        start: Option<f64>,
        /// Project time to end at; the project's end when left out.
        #[serde(default)]
        end: Option<f64>,
    },
    /// A job's state: `running`, `done` with its `result`, or `failed` with its `error`.
    #[serde(rename = "analysis.job")]
    Job { job: u64 },
    /// Measures both and reports how `mix` differs from `reference`: loudness, peaks,
    /// dynamics, width, the tonal balance per third octave and per region, and a summary in
    /// plain words.
    #[serde(rename = "analysis.compare")]
    Compare { reference: Subject, mix: Subject },
    /// Proposes a few EQ bands (smoothed, limited gain) that bring `mix`'s tonal balance
    /// towards `reference`'s, with advice on compression and limiting for its loudness and
    /// peaks. With `apply`, the bands go onto that track (or the master) as a new ReaEQ in one
    /// undo step once the job is done.
    #[serde(rename = "analysis.match_eq")]
    MatchEq {
        reference: Subject,
        mix: Subject,
        /// A track id or name, or "master", to put the EQ on.
        #[serde(default)]
        apply: Option<TrackRef>,
        /// At most this many bands (default 6).
        #[serde(default)]
        max_bands: Option<usize>,
        /// No band beyond this many dB either way (default 6, at most 12).
        #[serde(default)]
        max_gain_db: Option<f64>,
        /// The range to match, in Hz (default 30 to 16000).
        #[serde(default)]
        min_hz: Option<f64>,
        #[serde(default)]
        max_hz: Option<f64>,
    },
    /// Adds a ReaEQ with these bands to a track's (or the master's) effects, as one undo step.
    #[serde(rename = "analysis.apply_eq")]
    ApplyEq { track: TrackRef, bands: Vec<Band> },
    /// Finds the songs in a backing-track or click file (or an item's file): sound between
    /// silences, with start and end, loudness and, for a click, the tempo. Proposes a song
    /// marker at each start and a `!1016` stop marker after each end; with `markers` it adds
    /// them (in project time, for an item) as one undo step.
    #[serde(rename = "analysis.segments")]
    Segments {
        /// An audio file; or give `item`.
        #[serde(default)]
        path: Option<PathBuf>,
        /// An item's id: its active take's file, placed where the item is.
        #[serde(default)]
        item: Option<u64>,
        /// Below this peak level counts as silence (default -50 dBFS).
        #[serde(default)]
        threshold_db: Option<f64>,
        /// Silence of at least this many seconds separates songs (default 2).
        #[serde(default)]
        min_silence: Option<f64>,
        /// Shorter sounds are not songs (default 5 seconds).
        #[serde(default)]
        min_length: Option<f64>,
        /// Add the proposed markers to the project.
        #[serde(default)]
        markers: bool,
        /// Song marker names: this followed by a number (default "Song").
        #[serde(default)]
        name: Option<String>,
    },
}

// ---- jobs ------------------------------------------------------------------------------------

enum State {
    Running,
    Done(Value),
    Failed(String),
}

struct Job {
    kind: &'static str,
    state: State,
    started: Instant,
    took: Option<f64>,
    /// The measurement, for jobs that measure one, so later commands can name the job.
    analysis: Option<Arc<Analysis>>,
    /// Where to put the proposed EQ when done.
    apply: Option<TrackRef>,
    applied: Option<Value>,
}

#[derive(Default)]
struct Jobs {
    next: u64,
    jobs: HashMap<u64, Job>,
    order: VecDeque<u64>,
}

/// Jobs kept for reading back; older ones are forgotten.
const KEPT: usize = 200;

fn jobs() -> &'static Mutex<Jobs> {
    static JOBS: OnceLock<Mutex<Jobs>> = OnceLock::new();
    JOBS.get_or_init(Default::default)
}

/// The outcome of a job's work: its answer, and the measurement when it made one.
type Work = Box<dyn FnOnce() -> Result<(Value, Option<Arc<Analysis>>), String> + Send>;

pub(crate) fn spawn_job(kind: &'static str, apply: Option<TrackRef>, work: Work) -> Value {
    let id = {
        let mut j = jobs().lock().unwrap();
        j.next += 1;
        let id = j.next;
        j.jobs.insert(
            id,
            Job {
                kind,
                state: State::Running,
                started: Instant::now(),
                took: None,
                analysis: None,
                apply,
                applied: None,
            },
        );
        j.order.push_back(id);
        while j.order.len() > KEPT {
            if let Some(old) = j.order.pop_front() {
                j.jobs.remove(&old);
            }
        }
        id
    };
    std::thread::Builder::new()
        .name(format!("scythe-{kind}"))
        .spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                .unwrap_or_else(|_| Err("the measurement failed unexpectedly".into()));
            let mut j = jobs().lock().unwrap();
            if let Some(job) = j.jobs.get_mut(&id) {
                job.took = Some(job.started.elapsed().as_secs_f64());
                match outcome {
                    Ok((value, analysis)) => {
                        job.state = State::Done(value);
                        job.analysis = analysis;
                    }
                    Err(e) => job.state = State::Failed(e),
                }
            }
        })
        .map(|_| ())
        .unwrap_or_else(|e| {
            if let Some(job) = jobs().lock().unwrap().jobs.get_mut(&id) {
                job.state = State::Failed(format!("cannot start a thread: {e}"));
            }
        });
    // Cached measurements finish at once; answer with them rather than make the client ask.
    std::thread::yield_now();
    report(id).unwrap_or_else(|_| value!({"job": id, "status": "running"}))
}

fn report(id: u64) -> Result<Value, ApiError> {
    let j = jobs().lock().unwrap();
    let job = j.jobs.get(&id).ok_or_else(|| ApiError::not_found(format!("no analysis job {id}")))?;
    let mut answer = value!({"job": id, "kind": job.kind});
    match &job.state {
        State::Running => {
            answer["status"] = "running".into();
            answer["elapsed"] = ((job.started.elapsed().as_secs_f64() * 10.0).round() / 10.0).into();
        }
        State::Done(result) => {
            answer["status"] = "done".into();
            answer["result"] = result.clone();
            if let Some(t) = job.took {
                answer["seconds"] = ((t * 100.0).round() / 100.0).into();
            }
            if let Some(applied) = &job.applied {
                answer["applied"] = applied.clone();
            }
        }
        State::Failed(e) => {
            answer["status"] = "failed".into();
            answer["error"] = e.clone().into();
        }
    }
    Ok(answer)
}

fn finished_analysis(id: u64) -> Result<Arc<Analysis>, ApiError> {
    let j = jobs().lock().unwrap();
    let job = j.jobs.get(&id).ok_or_else(|| ApiError::not_found(format!("no analysis job {id}")))?;
    match (&job.state, &job.analysis) {
        (State::Done(_), Some(a)) => Ok(a.clone()),
        (State::Done(_), None) => Err(ApiError::invalid(format!("job {id} measured nothing to compare"))),
        (State::Running, _) => {
            Err(ApiError::failed(format!("job {id} is still running; wait for it with analysis.job")))
        }
        (State::Failed(e), _) => Err(ApiError::failed(format!("job {id} failed: {e}"))),
    }
}

// ---- measuring -------------------------------------------------------------------------------

/// What a job measures, gathered on the session's thread.
enum Source {
    Ready(Arc<Analysis>),
    File { path: PathBuf, start: Option<f64>, end: Option<f64> },
    Render(Box<Render>),
}

struct Render {
    project: Project,
    pool: Arc<MediaPool>,
    rate: u32,
    range: TimeRange,
    files: Vec<PathBuf>,
}

impl Source {
    fn measure(self) -> Result<Arc<Analysis>, String> {
        match self {
            Self::Ready(a) => Ok(a),
            Self::File { path, start, end } => analyse_file(&path, start, end),
            Self::Render(r) => r.measure().map(Arc::new),
        }
    }
}

impl Render {
    fn measure(self) -> Result<Analysis, String> {
        for file in &self.files {
            self.pool.load(file, self.rate).map_err(|e| e.to_string())?;
        }
        self.pool.prepare_project(&self.project);
        let mut analyzer = Analyzer::new(self.rate, 2);
        scythe_engine::bounce::render_blocks(
            &self.project,
            &self.pool,
            self.range,
            self.rate,
            |l, r| analyzer.push(l, r),
            |_| {},
        )
        .map_err(|e| e.to_string())?;
        Ok(analyzer.finish())
    }
}

fn mix_source(
    session: &mut Session,
    target: &MixTarget,
    start: Option<f64>,
    end: Option<f64>,
) -> Result<Source, ApiError> {
    session.store_plugin_states();
    let p = &session.project;
    let project = match target {
        MixTarget::Track(t) => match t.resolve(p)? {
            Target::Master => p.clone(),
            Target::Track(id) => scythe_core::render_source::project_for(p, RenderSource::Stem(id)),
        },
        MixTarget::Group { group } => {
            if !(1..=scythe_core::groups::GROUP_COUNT).contains(group) {
                return Err(ApiError::invalid("groups are numbered 1 to 64"));
            }
            let bit = 1u64 << (group - 1);
            if !p.tracks.iter().any(|t| t.groups.groups() & bit != 0) {
                return Err(ApiError::not_found(format!("group {group} has no tracks")));
            }
            let mut copy = p.clone();
            for t in &mut copy.tracks {
                (t.solo, t.solo_defeat) = (t.groups.groups() & bit != 0, false);
                if t.solo {
                    t.solo_mode = SoloMode::InPlace;
                }
            }
            copy
        }
    };
    let start = start.unwrap_or(0.0).max(0.0);
    let end = end.unwrap_or_else(|| p.length());
    if end - start < 0.1 {
        return Err(ApiError::invalid("nothing to render: give a range of at least 0.1 s, or add items first"));
    }
    let mut files: Vec<PathBuf> = project
        .tracks
        .iter()
        .flat_map(|t| &t.items)
        .flat_map(|i| &i.takes)
        .filter_map(|t| t.source.file().map(|f| scythe_core::rpp::resolve_media_path(&project, f)))
        .collect();
    files.sort();
    files.dedup();
    let settings = RenderSettings::from_project(p);
    let rate = settings.render_rate(p);
    let pool = if rate == session.engine.sample_rate() {
        session.engine.pool().clone()
    } else {
        render_pool(rate, settings.resample)
    };
    Ok(Source::Render(Box::new(Render { project, pool, rate, range: TimeRange::new(start, end), files })))
}

/// Media decoded at `rate` for measurements that do not run at the engine's rate, whose pool
/// holds it at the device's; kept for the next measurement at the same rate.
fn render_pool(rate: u32, resample: ResampleMode) -> Arc<MediaPool> {
    static POOL: Mutex<Option<(u32, ResampleMode, Arc<MediaPool>)>> = Mutex::new(None);
    let mut kept = POOL.lock().unwrap();
    match &*kept {
        Some((r, m, pool)) if (*r, *m) == (rate, resample) => pool.clone(),
        _ => {
            let pool = Arc::new(MediaPool::with_resample(resample));
            *kept = Some((rate, resample, pool.clone()));
            pool
        }
    }
}

fn subject_source(session: &mut Session, subject: &Subject) -> Result<Source, ApiError> {
    match subject {
        Subject::Job { job } => finished_analysis(*job).map(Source::Ready),
        Subject::File { file, start, end } => {
            check_file(file)?;
            Ok(Source::File { path: file.clone(), start: *start, end: *end })
        }
        Subject::Mix { mix, start, end } => mix_source(session, mix, *start, *end),
    }
}

fn check_file(path: &Path) -> Result<(), ApiError> {
    if !path.is_file() {
        return Err(ApiError::not_found(format!("no file {}", path.display())));
    }
    Ok(())
}

/// Where measurements of files are kept between runs.
fn cache_dir() -> PathBuf {
    crate::data_dir().join("analysis-cache")
}

/// Bumped when the measurements change, so older cached ones are not used.
const CACHE_VERSION: u32 = 1;

fn cache_key(path: &Path, start: Option<f64>, end: Option<f64>) -> Option<String> {
    let path = path.canonicalize().ok()?;
    let meta = std::fs::metadata(&path).ok()?;
    let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos();
    let text = format!("{CACHE_VERSION}|{}|{}|{modified}|{start:?}|{end:?}", path.display(), meta.len());
    let mut h = std::hash::DefaultHasher::new();
    std::hash::Hash::hash(&text, &mut h);
    Some(format!("{:016x}", std::hash::Hasher::finish(&h)))
}

fn memory_cache() -> &'static Mutex<HashMap<String, Arc<Analysis>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<Analysis>>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn analyse_file(path: &Path, start: Option<f64>, end: Option<f64>) -> Result<Arc<Analysis>, String> {
    let key = cache_key(path, start, end);
    if let Some(k) = &key {
        if let Some(a) = memory_cache().lock().unwrap().get(k) {
            return Ok(a.clone());
        }
        let stored = std::fs::read_to_string(cache_dir().join(format!("{k}.json"))).ok();
        if let Some(a) = stored.and_then(|t| serde_json::from_str::<Analysis>(&t).ok()) {
            let a = Arc::new(a);
            memory_cache().lock().unwrap().insert(k.clone(), a.clone());
            return Ok(a);
        }
    }
    let data = scythe_engine::media::decode_file(path).map_err(|e| e.to_string())?;
    let a = Arc::new(analysis::analyse(&data.channels, data.sample_rate, start, end));
    if let Some(k) = key {
        let dir = cache_dir();
        if std::fs::create_dir_all(&dir).is_ok() {
            let _ = std::fs::write(dir.join(format!("{k}.json")), serde_json::to_string(&*a).unwrap_or_default());
        }
        memory_cache().lock().unwrap().insert(k, a.clone());
    }
    Ok(a)
}

/// The headline numbers of a measurement, without the bands.
fn overview(a: &Analysis) -> Value {
    let mut v = json(a);
    if let Some(o) = v.as_object_mut() {
        o.remove("bands");
    }
    v
}

fn apply_bands(session: &mut Session, track: &TrackRef, bands: &[EqBand]) -> Result<Value, ApiError> {
    if bands.is_empty() {
        return Err(ApiError::invalid("no bands to apply"));
    }
    if bands.iter().any(|b| !(b.gain_db.is_finite() && b.gain_db.abs() <= eq::SAFE_GAIN_DB)) {
        return Err(ApiError::invalid(format!("band gains are limited to {} dB either way", eq::SAFE_GAIN_DB)));
    }
    if bands.iter().any(|b| !(10.0..=24_000.0).contains(&b.freq) || !(0.05..=4.0).contains(&b.bandwidth)) {
        return Err(ApiError::invalid("band frequencies run from 10 Hz to 24 kHz, bandwidths from 0.05 to 4 octaves"));
    }
    let target = track.resolve(&session.project)?;
    let state: Vec<(u32, f64, f64, f64)> =
        bands.iter().map(|b| (b.kind.code(), b.freq, 10f64.powf(b.gain_db / 20.0), b.bandwidth)).collect();
    let slot = scythe_engine::builtin::Builtin::Eq.slot_with(&scythe_engine::builtin::eq_state(&state));
    let mut index = 0;
    session.edit("Match EQ", |p| {
        if let Some(t) = target.get_mut(p) {
            t.fx.push(slot);
            index = t.fx.len() - 1;
        }
    });
    let name = target.get(&session.project).map(|t| t.name.clone()).unwrap_or_default();
    Ok(
        value!({"track": if target == Target::Master { "master".to_owned() } else { name }, "fx_index": index, "bands": bands}),
    )
}

fn segments_job(session: &Session, path: Option<PathBuf>, item: Option<u64>) -> Result<(PathBuf, f64, f64), ApiError> {
    match (path, item) {
        (Some(path), None) => {
            check_file(&path)?;
            Ok((path, 0.0, 0.0))
        }
        (None, Some(item)) => {
            let (_, it) = session
                .project
                .find_item(scythe_core::ItemId(item))
                .ok_or_else(|| ApiError::not_found(format!("no item with id {item}")))?;
            let take = it.takes.get(it.active_take).ok_or_else(|| ApiError::invalid("the item has no take"))?;
            let file = take.source.file().ok_or_else(|| ApiError::invalid("the item does not play a file"))?;
            Ok((scythe_core::rpp::resolve_media_path(&session.project, file), it.position, take.source_offset))
        }
        _ => Err(ApiError::invalid("give either \"path\" or \"item\"")),
    }
}

/// Song and stop markers for found segments, in project time.
fn proposed_markers(found: &segments::Segments, shift: f64, name: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for (i, s) in found.segments.iter().enumerate() {
        let next = found.segments.get(i + 1).map_or(found.duration, |n| n.start);
        let tail = ((next - s.end) / 2.0).clamp(0.0, 0.5);
        let at = |t: f64| ((t + shift).max(0.0) * 1000.0).round() / 1000.0;
        out.push(value!({"position": at(s.start), "name": format!("{name} {}", i + 1)}));
        out.push(value!({"position": at(s.end + tail), "name": "!1016"}));
    }
    out
}

pub fn execute(session: &mut Session, command: AnalysisCommand) -> Result<Value, ApiError> {
    match command {
        AnalysisCommand::File { path, start, end } => {
            check_file(&path)?;
            Ok(spawn_job(
                "file",
                None,
                Box::new(move || {
                    let a = analyse_file(&path, start, end)?;
                    Ok((json(&*a), Some(a)))
                }),
            ))
        }
        AnalysisCommand::Mix { target, start, end } => {
            let target = target.unwrap_or(MixTarget::Track(TrackRef::Name("master".into())));
            let source = mix_source(session, &target, start, end)?;
            Ok(spawn_job(
                "mix",
                None,
                Box::new(move || {
                    let a = source.measure()?;
                    Ok((json(&*a), Some(a)))
                }),
            ))
        }
        AnalysisCommand::Job { job } => {
            let pending = {
                let mut j = jobs().lock().unwrap();
                let job = j.jobs.get_mut(&job).ok_or_else(|| ApiError::not_found(format!("no analysis job {job}")))?;
                match (&job.state, job.applied.is_none()) {
                    (State::Done(result), true) => job.apply.take().map(|t| (t, result["bands"].clone())),
                    _ => None,
                }
            };
            if let Some((track, bands)) = pending {
                let bands: Vec<EqBand> = serde_json::from_value(bands).unwrap_or_default();
                let applied = if bands.is_empty() {
                    value!({"note": "nothing to apply: the tonal balance already matches"})
                } else {
                    apply_bands(session, &track, &bands).unwrap_or_else(|e| value!({"error": e.message}))
                };
                if let Some(j) = jobs().lock().unwrap().jobs.get_mut(&job) {
                    j.applied = Some(applied);
                }
            }
            report(job)
        }
        AnalysisCommand::Compare { reference, mix } => {
            let (a, b) = (subject_source(session, &reference)?, subject_source(session, &mix)?);
            Ok(spawn_job(
                "compare",
                None,
                Box::new(move || {
                    let (a, b) = (a.measure()?, b.measure()?);
                    let c = analysis::compare(&a, &b);
                    Ok((value!({"reference": overview(&a), "mix": overview(&b), "comparison": c}), None))
                }),
            ))
        }
        AnalysisCommand::MatchEq { reference, mix, apply, max_bands, max_gain_db, min_hz, max_hz } => {
            if let Some(t) = &apply {
                t.resolve(&session.project)?;
            }
            let defaults = MatchOptions::default();
            let options = MatchOptions {
                max_bands: max_bands.unwrap_or(defaults.max_bands).clamp(1, 12),
                max_gain_db: max_gain_db.unwrap_or(defaults.max_gain_db).clamp(0.5, eq::SAFE_GAIN_DB),
                min_hz: min_hz.unwrap_or(defaults.min_hz).max(20.0),
                max_hz: max_hz.unwrap_or(defaults.max_hz).min(20_000.0),
            };
            let (a, b) = (subject_source(session, &reference)?, subject_source(session, &mix)?);
            Ok(spawn_job(
                "match_eq",
                apply,
                Box::new(move || {
                    let (a, b) = (a.measure()?, b.measure()?);
                    let proposal = eq::match_eq(&a, &b, options);
                    let dynamics = eq::dynamics_advice(&a, &b);
                    let mut answer = json(&proposal);
                    answer["dynamics"] = json(&dynamics);
                    answer["loudness_lufs"] = value!({"reference": a.integrated_lufs, "mix": b.integrated_lufs});
                    answer["true_peak_dbtp"] = value!({"reference": a.true_peak_dbtp, "mix": b.true_peak_dbtp});
                    Ok((answer, None))
                }),
            ))
        }
        AnalysisCommand::ApplyEq { track, bands } => {
            let bands: Vec<EqBand> = bands.into_iter().map(EqBand::from).collect();
            apply_bands(session, &track, &bands)
        }
        AnalysisCommand::Segments { path, item, threshold_db, min_silence, min_length, markers, name } => {
            let defaults = SegmentOptions::default();
            let options = SegmentOptions {
                threshold_db: threshold_db.unwrap_or(defaults.threshold_db).clamp(-120.0, 0.0),
                min_silence: min_silence.unwrap_or(defaults.min_silence).max(0.05),
                min_length: min_length.unwrap_or(defaults.min_length).max(0.0),
            };
            let (file, position, offset) = segments_job(session, path, item)?;
            let name = name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "Song".into());
            let shift = position - offset;
            if markers {
                // Markers need the session; segments of a file are quick enough to find here.
                let data = scythe_engine::media::decode_file(&file).map_err(|e| ApiError::failed(e.to_string()))?;
                let found = segments::find(&data.channels, data.sample_rate, options);
                let proposed = proposed_markers(&found, shift, &name);
                session.edit("Song markers", |p| {
                    for m in &proposed {
                        p.add_marker(m["position"].as_f64().unwrap_or(0.0), m["name"].as_str().unwrap_or_default());
                    }
                });
                let mut answer = json(&found);
                answer["markers"] = proposed.into();
                answer["added"] = true.into();
                return Ok(value!({"job": null, "status": "done", "result": answer}));
            }
            Ok(spawn_job(
                "segments",
                None,
                Box::new(move || {
                    let data = scythe_engine::media::decode_file(&file).map_err(|e| e.to_string())?;
                    let found = segments::find(&data.channels, data.sample_rate, options);
                    let mut answer = json(&found);
                    answer["markers"] = proposed_markers(&found, shift, &name).into();
                    answer["added"] = false.into();
                    Ok((answer, None))
                }),
            ))
        }
    }
}
