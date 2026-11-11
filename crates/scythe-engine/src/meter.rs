use std::sync::atomic::{AtomicI64, AtomicU32, AtomicUsize, Ordering};

/// Stereo peak meter written by the audio thread and drained by the UI.
#[derive(Debug, Default)]
pub struct Meter {
    left: AtomicU32,
    right: AtomicU32,
    /// Time the track takes to render, its effects included.
    pub cpu: crate::perf::CpuTime,
    /// Peaks of blocks rendered ahead of the playhead (anticipative FX), held until the
    /// callback plays their frames so the meter moves with what is heard.
    ahead: AheadPeaks,
}

/// Peaks waiting for their frames to be played: the worker adds at `head`, the callback
/// releases from `tail`.
#[derive(Debug)]
struct AheadPeaks {
    slots: [AheadPeak; AHEAD_SLOTS],
    head: AtomicUsize,
    tail: AtomicUsize,
}

impl Default for AheadPeaks {
    fn default() -> Self {
        Self {
            slots: std::array::from_fn(|_| AheadPeak::default()),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }
}

#[derive(Debug, Default)]
struct AheadPeak {
    /// The chunk it was rendered in.
    serial: AtomicI64,
    left: AtomicU32,
    right: AtomicU32,
}

/// Blocks rendered ahead a meter holds: a render-ahead of a second in 512-frame chunks.
const AHEAD_SLOTS: usize = 128;

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

    /// Like [`Meter::record`] for a block rendered ahead of the playhead in chunk `serial`:
    /// it shows once [`Meter::release`] has passed that chunk.
    pub fn record_ahead(&self, serial: i64, left: &[f32], right: &[f32]) {
        let q = &self.ahead;
        let head = q.head.load(Ordering::Relaxed);
        if head - q.tail.load(Ordering::Acquire) >= AHEAD_SLOTS {
            self.record(left, right);
            return;
        }
        let peak = |b: &[f32]| b.iter().fold(0f32, |m, s| m.max(s.abs()));
        let slot = &q.slots[head % AHEAD_SLOTS];
        slot.serial.store(serial, Ordering::Relaxed);
        slot.left.store(peak(left).to_bits(), Ordering::Relaxed);
        slot.right.store(peak(right).to_bits(), Ordering::Relaxed);
        q.head.store(head + 1, Ordering::Release);
    }

    /// Shows the peaks of the chunks up to `serial`, which the callback has played (or
    /// dropped).
    pub fn release(&self, serial: i64) {
        let q = &self.ahead;
        let head = q.head.load(Ordering::Acquire);
        let mut tail = q.tail.load(Ordering::Relaxed);
        while tail < head {
            let slot = &q.slots[tail % AHEAD_SLOTS];
            if slot.serial.load(Ordering::Relaxed) > serial {
                break;
            }
            raise(&self.left, f32::from_bits(slot.left.load(Ordering::Relaxed)));
            raise(&self.right, f32::from_bits(slot.right.load(Ordering::Relaxed)));
            tail += 1;
        }
        q.tail.store(tail, Ordering::Release);
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

    #[test]
    fn peaks_rendered_ahead_show_when_played() {
        let m = Meter::default();
        m.record_ahead(1, &[0.5], &[0.25]);
        m.record_ahead(2, &[0.9], &[0.1]);
        m.release(0);
        assert_eq!(m.take(), (0.0, 0.0));
        m.release(1);
        assert_eq!(m.take(), (0.5, 0.25));
        m.release(5);
        assert_eq!(m.take(), (0.9, 0.1));
        assert_eq!(m.take(), (0.0, 0.0));
    }
}
