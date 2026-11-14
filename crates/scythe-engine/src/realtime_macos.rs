//! Real-time scheduling on macOS: threads that render audio get Mach's time-constraint policy,
//! the band Core Audio's own I/O threads run in. The device callback already runs on such a
//! thread; the threads it waits on are put there; threads that render ahead of the playhead
//! only get the user-interactive QoS class.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// Kept for the device settings: on Linux the `SCHED_FIFO` priority asked for. The
/// time-constraint policy has no numeric priority; any value above 0 asks for it.
pub const PRIORITY: i32 = 70;

/// The time constraint asked for, in nanoseconds. The block length is not known when a thread
/// asks, so these fit a 512-frame block at 48 kHz; the scheduler uses them as hints, and a
/// thread that blocks well within its computation keeps the policy at any block length.
const PERIOD_NS: u64 = 10_000_000;
const COMPUTATION_NS: u64 = 5_000_000;

static WANTED: AtomicI32 = AtomicI32::new(PRIORITY);

/// Sets whether audio threads ask for real-time scheduling: 0 for none ("non-RT"), anything
/// else or `None` for the time-constraint policy.
pub fn set_priority(priority: Option<u32>) {
    WANTED.store(priority.map_or(PRIORITY, |p| p.min(99) as i32), Ordering::Relaxed);
    if priority == Some(0)
        && let Ok(mut s) = STATUS.lock()
    {
        *s = Some(Realtime {
            policy: "normal",
            priority: 0,
            via: "none",
            reason: Some("RT priority is set to 0 (non-RT) in the audio device settings".into()),
        });
    }
}

pub fn lock_memory() -> Result<(), String> {
    Err("locking process memory is not supported on macOS".into())
}

pub fn hold_cpu_awake() -> Result<std::fs::File, String> {
    Err("holding the CPU out of idle states is not supported on macOS".into())
}

/// How the thread rendering audio is scheduled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Realtime {
    /// `time constraint` or `normal`.
    pub policy: &'static str,
    /// 1 under the time-constraint policy, which has no numeric priority; 0 otherwise.
    pub priority: i32,
    /// `mach` (Scythe set the policy), `driver` (Core Audio's thread already had it) or `none`.
    pub via: &'static str,
    /// Why there is no real-time priority.
    pub reason: Option<String>,
}

impl Realtime {
    pub fn is_realtime(&self) -> bool {
        self.priority > 0
    }
}

static STATUS: Mutex<Option<Realtime>> = Mutex::new(None);
static ENABLED: AtomicBool = AtomicBool::new(false);
static WARNED: AtomicBool = AtomicBool::new(false);

/// Starts promoting the threads that [`request`] names. Without it (tests, offline renders)
/// audio threads keep their normal priority.
pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

thread_local! {
    static AUDIO_THREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static JOINED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Asks for real-time scheduling for the calling thread. The policy is set on the first call
/// only; later calls cost a thread-local read.
pub fn request() {
    mark_audio_thread();
    join(true);
}

pub fn mark_audio_thread() {
    AUDIO_THREAD.with(|a| a.set(true));
}

pub fn is_audio_thread() -> bool {
    AUDIO_THREAD.with(|a| a.get())
}

/// Like [`request`], for a thread the audio thread waits on (a track render thread).
pub fn request_helper() {
    join(false);
}

/// For a thread that renders ahead of the playhead (anticipative FX): the highest QoS class
/// outside real time, so a thread working flat out does not starve the audio threads.
pub fn request_worker() {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    // SAFETY: changes only the calling thread's QoS class.
    unsafe { libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0) };
}

fn join(reported: bool) {
    if !ENABLED.load(Ordering::Relaxed) || WANTED.load(Ordering::Relaxed) == 0 || JOINED.with(|j| j.replace(true)) {
        return;
    }
    let status = time_constraint();
    if let Some(reason) = &status.reason
        && !WARNED.swap(true, Ordering::Relaxed)
    {
        eprintln!("audio: no real-time priority ({reason}); xruns are more likely under load");
    }
    if reported && let Ok(mut s) = STATUS.lock() {
        *s = Some(status);
    }
}

fn has_time_constraint(thread: libc::thread_t) -> bool {
    let mut policy = libc::thread_time_constraint_policy { period: 0, computation: 0, constraint: 0, preemptible: 0 };
    let mut count = libc::THREAD_TIME_CONSTRAINT_POLICY_COUNT;
    let mut default = 0;
    // SAFETY: `policy` is the struct this flavor fills and `count` its size in words.
    let result = unsafe {
        libc::thread_policy_get(
            thread,
            libc::THREAD_TIME_CONSTRAINT_POLICY as _,
            (&raw mut policy).cast(),
            &mut count,
            &mut default,
        )
    };
    result == libc::KERN_SUCCESS && default == 0
}

fn time_constraint() -> Realtime {
    // SAFETY: the calling thread's port; pthread_mach_thread_np adds no reference to release.
    let thread = unsafe { libc::pthread_mach_thread_np(libc::pthread_self()) };
    if has_time_constraint(thread) {
        return Realtime { policy: "time constraint", priority: 1, via: "driver", reason: None };
    }
    let mut timebase = mach2::mach_time::mach_timebase_info::default();
    // SAFETY: `timebase` is a valid out pointer.
    unsafe { mach2::mach_time::mach_timebase_info(&mut timebase) };
    let ticks = |ns: u64| (ns * timebase.denom.max(1) as u64 / timebase.numer.max(1) as u64) as u32;
    let mut policy = libc::thread_time_constraint_policy {
        period: ticks(PERIOD_NS),
        computation: ticks(COMPUTATION_NS),
        constraint: ticks(PERIOD_NS),
        preemptible: 1,
    };
    // SAFETY: `policy` is the struct this flavor takes and the count its size in words.
    let result = unsafe {
        libc::thread_policy_set(
            thread,
            libc::THREAD_TIME_CONSTRAINT_POLICY as _,
            (&raw mut policy).cast(),
            libc::THREAD_TIME_CONSTRAINT_POLICY_COUNT,
        )
    };
    if result != libc::KERN_SUCCESS {
        let reason = format!("thread_policy_set(THREAD_TIME_CONSTRAINT_POLICY) returned {result}");
        return Realtime { policy: "normal", priority: 0, via: "none", reason: Some(reason) };
    }
    Realtime { policy: "time constraint", priority: 1, via: "mach", reason: None }
}

/// How the last audio thread that asked is scheduled, once it has been looked at.
pub fn status() -> Option<Realtime> {
    STATUS.lock().ok().and_then(|s| s.clone())
}

/// The fastest cores of a hybrid CPU. macOS already runs time-constraint threads on the
/// performance cores, so none are singled out here.
pub struct FastCpus {
    pub cpus: Vec<usize>,
    pub cores: usize,
    pub hybrid: bool,
}

pub fn fast_cpus() -> Option<&'static FastCpus> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_gets_the_time_constraint_policy() {
        let status = std::thread::spawn(time_constraint).join().unwrap();
        assert_eq!((status.policy, status.via, status.reason.as_deref()), ("time constraint", "mach", None));
    }

    #[test]
    fn requests_before_enabling_do_nothing() {
        if !ENABLED.load(Ordering::Relaxed) {
            request();
            assert_eq!(status(), None);
        }
    }
}
