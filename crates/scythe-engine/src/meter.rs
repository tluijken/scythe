use std::sync::atomic::{AtomicU32, Ordering};

/// Stereo peak meter written by the audio thread and drained by the UI.
#[derive(Debug, Default)]
pub struct Meter {
    left: AtomicU32,
    right: AtomicU32,
    /// Time the track takes to render, its effects included.
    pub cpu: crate::perf::CpuTime,
}

fn raise(slot: &AtomicU32, value: f32) {
    let mut cur = slot.load(Ordering::Relaxed);
    while value > f32::from_bits(cur) {
        match slot.compare_exchange_weak(cur, value.to_bits(), Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(actual) => cur = actual,
        }
    }
}

impl Meter {
    pub fn record(&self, left: &[f32], right: &[f32]) {
        let peak = |b: &[f32]| b.iter().fold(0f32, |m, s| m.max(s.abs()));
        raise(&self.left, peak(left));
        raise(&self.right, peak(right));
    }

    /// Peak since the previous call.
    pub fn take(&self) -> (f32, f32) {
        (f32::from_bits(self.left.swap(0, Ordering::Relaxed)), f32::from_bits(self.right.swap(0, Ordering::Relaxed)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_max_until_taken() {
        let m = Meter::default();
        m.record(&[0.1, -0.7], &[0.2]);
        m.record(&[0.3], &[-0.1]);
        assert_eq!(m.take(), (0.7, 0.2));
        assert_eq!(m.take(), (0.0, 0.0));
    }
}
