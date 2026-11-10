//! The performance meter, as REAPER's: how much of the real-time budget the audio takes, the
//! longest block, xruns, and per track the CPU its effects take, the longest block it took
//! and its plugin delay. Sampled about once a second from the engine's running counters.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use scythe_core::TrackId;
use scythe_engine::fx::FxOwner;
use scythe_engine::perf::chain_cpu;
use serde::{Deserialize, Serialize};

use crate::Session;

/// How often the meter takes a new reading.
const WINDOW: Duration = Duration::from_secs(1);

/// The meter's latest reading.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PerfMeter {
    /// Render time over buffer time of the last callback, in percent ("RT CPU").
    pub rt_cpu: f64,
    /// The longest block of the last few seconds, and the time a block may take, in
    /// milliseconds ("RT longest-block").
    pub rt_longest_block_ms: f64,
    pub block_ms: f64,
    /// Device xruns and blocks that took longer than their budget, since the start.
    pub xruns: u64,
    pub overloads: u64,
    /// The tracks' CPU added up, in percent of one core.
    pub tracks_cpu: f64,
    /// Every track in project order, then the master when it has effects.
    pub tracks: Vec<TrackPerf>,
}

/// One row of the meter.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TrackPerf {
    /// The track's number from 1; 0 for the master.
    pub index: usize,
    pub name: String,
    /// Time rendering the track (its items and effects) takes, in percent of one core ("FX CPU").
    pub cpu: f64,
    /// The longest block it took in the last reading, in milliseconds ("FX Longest-Block").
    pub longest_block_ms: f64,
    /// Frames its effects delay it by ("PDC").
    pub pdc: i64,
    /// Its effects with what each takes, in chain order, containers' contents included.
    pub fx: Vec<FxPerf>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FxPerf {
    pub name: String,
    /// Percent of one core.
    pub cpu: f64,
}

/// Running totals from the previous reading, to turn the engine's counters into rates.
#[derive(Debug)]
pub(crate) struct PerfSampler {
    at: Instant,
    tracks: HashMap<TrackId, u64>,
    fx: HashMap<usize, u64>,
    pub(crate) reading: PerfMeter,
}

impl Default for PerfSampler {
    fn default() -> Self {
        Self { at: Instant::now(), tracks: HashMap::new(), fx: HashMap::new(), reading: PerfMeter::default() }
    }
}

impl Session {
    /// Takes a new reading once a window has passed.
    pub(crate) fn sample_performance(&mut self) {
        let elapsed = self.perf.at.elapsed();
        if elapsed < WINDOW {
            return;
        }
        let ns = elapsed.as_nanos() as f64;
        let health = self.health();
        let percent = |delta: u64| (delta as f64 / ns * 1000.0).round() / 10.0;
        let fx_registry = self.engine.fx();
        let sampler = &mut self.perf;
        let mut fx_totals = HashMap::new();
        let mut chain = |owner: FxOwner, slots: &[scythe_core::FxSlot]| -> Vec<FxPerf> {
            chain_cpu(fx_registry, &owner, slots)
                .into_iter()
                .map(|f| {
                    let key = std::sync::Arc::as_ptr(&f.instance) as usize;
                    let total = f.instance.cpu.total();
                    fx_totals.insert(key, total);
                    let before = sampler.fx.get(&key).copied().unwrap_or(total);
                    FxPerf { name: f.name, cpu: percent(total.saturating_sub(before)) }
                })
                .collect()
        };
        let timing: HashMap<TrackId, _> =
            self.engine.track_timing().into_iter().map(|(id, meter, pdc)| (id, (meter, pdc))).collect();
        let mut track_totals = HashMap::new();
        let mut tracks: Vec<TrackPerf> = Vec::with_capacity(self.project.tracks.len() + 1);
        for (i, track) in self.project.tracks.iter().enumerate() {
            let (cpu, longest, pdc) = match timing.get(&track.id) {
                Some((meter, pdc)) => {
                    let total = meter.cpu.total();
                    track_totals.insert(track.id, total);
                    let before = sampler.tracks.get(&track.id).copied().unwrap_or(total);
                    (percent(total.saturating_sub(before)), meter.cpu.take_peak() as f64 / 1e6, *pdc)
                }
                None => (0.0, 0.0, 0),
            };
            let fx = chain(FxOwner::Track(track.id), &track.fx);
            tracks.push(TrackPerf {
                index: i + 1,
                name: track.name.clone(),
                cpu,
                longest_block_ms: (longest * 100.0).round() / 100.0,
                pdc,
                fx,
            });
        }
        let master = chain(FxOwner::Master, &self.project.master.fx);
        if !master.is_empty() {
            let cpu = master.iter().map(|f| f.cpu).sum::<f64>();
            tracks.push(TrackPerf { index: 0, name: "MASTER".into(), cpu, fx: master, ..Default::default() });
        }
        let block_ms = health.block_size.map_or(0.0, |b| b as f64 * 1000.0 / health.sample_rate as f64);
        let round = |v: f64| (v * 100.0).round() / 100.0;
        sampler.reading = PerfMeter {
            rt_cpu: (health.load as f64 * 1000.0).round() / 10.0,
            rt_longest_block_ms: round(health.load_peak as f64 * block_ms),
            block_ms: round(block_ms),
            xruns: health.xruns,
            overloads: health.overloads,
            tracks_cpu: (tracks.iter().map(|t| t.cpu).sum::<f64>() * 10.0).round() / 10.0,
            tracks,
        };
        sampler.tracks = track_totals;
        sampler.fx = fx_totals;
        sampler.at = Instant::now();
    }

    /// The performance meter's latest reading.
    pub fn performance(&self) -> &PerfMeter {
        &self.perf.reading
    }
}
