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
}
