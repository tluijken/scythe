//! Where the audio threads spend their time, for a performance meter like REAPER's: each
//! track and effect adds up the time it takes to render, and a reader turns two readings into
//! a share of real time.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Time spent rendering, added up since it was made.
#[derive(Debug, Default)]
pub struct CpuTime {
    ns: AtomicU64,
    last: AtomicU64,
    peak: AtomicU64,
}

impl CpuTime {
    /// Adds the time since `started`.
    pub fn add(&self, started: Instant) {
        let ns = started.elapsed().as_nanos() as u64;
        self.ns.fetch_add(ns, Ordering::Relaxed);
        self.last.store(ns, Ordering::Relaxed);
        self.peak.fetch_max(ns, Ordering::Relaxed);
    }

    /// Nanoseconds the last run took.
    pub fn last(&self) -> u64 {
        self.last.load(Ordering::Relaxed)
    }

    /// The longest run since the last call, in nanoseconds.
    pub fn take_peak(&self) -> u64 {
        self.peak.swap(0, Ordering::Relaxed)
    }

    /// Nanoseconds spent so far.
    pub fn total(&self) -> u64 {
        self.ns.load(Ordering::Relaxed)
    }
}

/// One effect of a chain and the time it has taken: its slot path (an index, then indices
/// into containers), its name and its counters.
pub struct FxCpu {
    pub path: Vec<usize>,
    pub name: String,
    pub instance: std::sync::Arc<crate::fx::FxInstance>,
}

/// The effects of `owner`'s chain `slots` that have an instance, containers' contents included.
pub fn chain_cpu(fx: &crate::fx::FxRegistry, owner: &crate::fx::FxOwner, slots: &[scythe_core::FxSlot]) -> Vec<FxCpu> {
    fn walk(
        fx: &crate::fx::FxRegistry,
        owner: &crate::fx::FxOwner,
        root: &[scythe_core::FxSlot],
        slots: &[scythe_core::FxSlot],
        path: &mut Vec<usize>,
        out: &mut Vec<FxCpu>,
    ) {
        for (i, slot) in slots.iter().enumerate() {
            path.push(i);
            if slot.is_container() {
                walk(fx, owner, root, &slot.container_children(), path, out);
            } else if let Some(instance) = fx.instance_at(owner, root, path) {
                out.push(FxCpu { path: path.clone(), name: slot.name.clone(), instance });
            }
            path.pop();
        }
    }
    let mut out = Vec::new();
    walk(fx, owner, slots, slots, &mut Vec::new(), &mut out);
    out
}

/// How long blocks take to render, as a histogram the audio thread fills without locking, for
/// percentiles of the block time rather than only its peak.
#[derive(Debug)]
pub struct BlockTimes {
    buckets: Box<[AtomicU64]>,
    max_ns: AtomicU64,
}

/// What [`BlockTimes`] holds: blocks counted and their time at some percentiles, in seconds.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BlockStats {
    pub blocks: u64,
    pub p50: f64,
    pub p99: f64,
    pub p999: f64,
    pub max: f64,
}

impl Default for BlockTimes {
    fn default() -> Self {
        Self { buckets: (0..Self::BUCKETS).map(|_| AtomicU64::new(0)).collect(), max_ns: AtomicU64::new(0) }
    }
}

impl BlockTimes {
    /// 10 microseconds a bucket up to 40 ms; longer blocks share the last one.
    const BUCKET_NS: u64 = 10_000;
    const BUCKETS: usize = 4096;

    pub fn add(&self, ns: u64) {
        let i = ((ns / Self::BUCKET_NS) as usize).min(Self::BUCKETS - 1);
        self.buckets[i].fetch_add(1, Ordering::Relaxed);
        self.max_ns.fetch_max(ns, Ordering::Relaxed);
    }

    /// The blocks since the window started; `restart` starts a new one.
    pub fn read(&self, restart: bool) -> BlockStats {
        let take = |a: &AtomicU64| if restart { a.swap(0, Ordering::Relaxed) } else { a.load(Ordering::Relaxed) };
        let counts: Vec<u64> = self.buckets.iter().map(take).collect();
        let max = take(&self.max_ns);
        let blocks: u64 = counts.iter().sum();
        let at = |q: f64| {
            let rank = ((blocks as f64 * q).ceil() as u64).max(1);
            let mut seen = 0;
            for (i, c) in counts.iter().enumerate() {
                seen += c;
                if seen >= rank {
                    return ((i as u64 + 1) * Self::BUCKET_NS).min(max) as f64 / 1e9;
                }
            }
            max as f64 / 1e9
        };
        if blocks == 0 {
            return BlockStats::default();
        }
        BlockStats { blocks, p50: at(0.5), p99: at(0.99), p999: at(0.999), max: max as f64 / 1e9 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn adds_up_and_holds_the_longest_run() {
        let t = CpuTime::default();
        for ms in [3, 1] {
            let started = Instant::now() - Duration::from_millis(ms);
            t.add(started);
        }
        assert!(t.total() >= 4_000_000);
        assert!(t.last() >= 1_000_000 && t.last() < 3_000_000);
        assert!(t.take_peak() >= 3_000_000);
        assert_eq!(t.take_peak(), 0);
    }

    #[test]
    fn block_times_give_percentiles_and_start_over() {
        let b = BlockTimes::default();
        for _ in 0..990 {
            b.add(100_000);
        }
        for _ in 0..10 {
            b.add(2_000_000);
        }
        let s = b.read(true);
        assert_eq!(s.blocks, 1000);
        assert!((s.p50 - 0.000_11).abs() < 1e-9, "{s:?}");
        assert!((s.p99 - 0.000_11).abs() < 1e-9, "{s:?}");
        assert!((s.p999 - 0.002).abs() < 1e-9 && (s.max - 0.002).abs() < 1e-9, "{s:?}");
        assert_eq!(b.read(false).blocks, 0);
    }
}
