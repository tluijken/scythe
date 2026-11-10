//! Real-time scheduling for the threads that render audio: the device callback and the silent
//! clock. With an `rtprio` limit (or as root) the thread gets `SCHED_FIFO` directly; without
//! one it asks RealtimeKit over the system D-Bus, as PipeWire does. Promotion happens on a
//! helper thread, so the audio thread only hands over its id and never waits; when neither
//! works the audio plays on at normal priority and the reason is printed once.
//!
//! RealtimeKit grants priority only under a CPU-time limit (`RLIMIT_RTTIME`). Its soft limit
//! is kept below the hard one, so a block that runs past it raises `SIGXCPU`, which drops the
//! promoted threads back to normal priority, instead of reaching the hard limit's `SIGKILL`.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Mutex, OnceLock};

/// `SCHED_FIFO` priority asked for directly: above PipeWire's and JACK's clients, below
/// the IRQ threads of a real-time kernel.
const PRIORITY: i32 = 70;

/// The `SCHED_FIFO` priority to ask for (REAPER's "RT priority"); 0 keeps audio threads at
/// normal priority.
static WANTED: AtomicI32 = AtomicI32::new(PRIORITY);

/// Sets the priority audio threads ask for from now on: REAPER's ALSA "RT priority" (1-99),
/// 0 for none ("non-RT"), `None` for Scythe's default.
pub fn set_priority(priority: Option<u32>) {
    WANTED.store(priority.map_or(PRIORITY, |p| p.min(99) as i32), Ordering::Relaxed);
}

/// Locks the process's memory in RAM (REAPER's "Lock process memory with mlockall()"), so
/// no page the audio touches is swapped out.
pub fn lock_memory() -> Result<(), String> {
    // SAFETY: mlockall takes flags only.
    if unsafe { libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

/// Holds `/dev/cpu_dma_latency` at 0 while kept (REAPER's "Disable power management"): the
/// CPU stays out of deep idle states that wake up too slowly for small blocks.
pub fn hold_cpu_awake() -> Result<std::fs::File, String> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/cpu_dma_latency")
        .map_err(|e| format!("/dev/cpu_dma_latency: {e}"))?;
    file.write_all(&0i32.to_ne_bytes()).map_err(|e| format!("/dev/cpu_dma_latency: {e}"))?;
    Ok(file)
}

/// How the thread rendering audio is scheduled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Realtime {
    /// `SCHED_FIFO`, `SCHED_RR` or `SCHED_OTHER`.
    pub policy: &'static str,
    /// Its real-time priority, 0 under `SCHED_OTHER`.
    pub priority: i32,
    /// `direct` (an rtprio limit), `rtkit`, `host` (the audio system's thread already was
    /// real-time) or `none`.
    pub via: &'static str,
    /// Why there is no real-time priority.
    pub reason: Option<String>,
}

impl Realtime {
    pub fn is_realtime(&self) -> bool {
        self.priority > 0
    }
}

/// Threads to promote, and whether each is the one [`status`] reports on.
static PROMOTER: OnceLock<SyncSender<(libc::pid_t, bool)>> = OnceLock::new();
static STATUS: Mutex<Option<Realtime>> = Mutex::new(None);

/// Threads promoted so far (audio threads and the track threads helping them), demoted
/// together when the CPU-time limit is hit.
static PROMOTED: [AtomicI32; 64] = [const { AtomicI32::new(0) }; 64];
static NEXT_SLOT: AtomicUsize = AtomicUsize::new(0);
static DEMOTED: AtomicBool = AtomicBool::new(false);

/// Starts promoting the threads that [`request`] names. Without it (tests, offline renders)
/// audio threads keep their normal priority.
pub fn enable() {
    PROMOTER.get_or_init(|| {
        catch_cpu_limit();
        let (tx, rx) = sync_channel(PROMOTED.len());
        std::thread::Builder::new()
            .name("scythe-realtime".into())
            .spawn(move || promoter(rx))
            .expect("spawn real-time promotion thread");
        tx
    });
}

/// Asks for real-time priority for the calling thread. Never blocks or allocates, so the audio
/// callback can call it.
pub fn request() {
    if WANTED.load(Ordering::Relaxed) == 0 {
        return;
    }
    if let Some(tx) = PROMOTER.get() {
        let _ = tx.try_send((current_tid(), true));
    }
}

/// Like [`request`], for a thread the audio thread waits on (a track render thread): at the
/// same priority, nothing that would not preempt the audio thread holds it up.
pub fn request_helper() {
    if WANTED.load(Ordering::Relaxed) == 0 {
        return;
    }
    if let Some(tx) = PROMOTER.get() {
        let _ = tx.try_send((current_tid(), false));
    }
}

/// How the last thread that asked is scheduled, once it has been looked at.
pub fn status() -> Option<Realtime> {
    let status = STATUS.lock().ok().and_then(|s| s.clone())?;
    if DEMOTED.load(Ordering::Relaxed) {
        return Some(Realtime {
            policy: "SCHED_OTHER",
            priority: 0,
            via: "none",
            reason: Some("demoted after a block ran past the real-time CPU-time limit".into()),
        });
    }
    Some(status)
}

fn catch_cpu_limit() {
    // SAFETY: the handler only makes async-signal-safe system calls and reads atomics.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_cpu_limit as *const () as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGXCPU, &action, std::ptr::null_mut());
    }
}

extern "C" fn on_cpu_limit(_: libc::c_int) {
    // The kernel would SIGKILL a thread that keeps running real-time up to the hard limit;
    // under normal priority the limit no longer applies.
    let param = libc::sched_param { sched_priority: 0 };
    for slot in &PROMOTED {
        let tid = slot.load(Ordering::Relaxed);
        if tid > 0 {
            // SAFETY: sched_setscheduler is a plain system call; a thread that is gone gives ESRCH.
            unsafe { libc::sched_setscheduler(tid, libc::SCHED_OTHER | libc::SCHED_RESET_ON_FORK, &param) };
        }
    }
    DEMOTED.store(true, Ordering::Relaxed);
}

fn remember_promoted(tid: libc::pid_t) {
    let slot = NEXT_SLOT.fetch_add(1, Ordering::Relaxed) % PROMOTED.len();
    PROMOTED[slot].store(tid, Ordering::Relaxed);
    DEMOTED.store(false, Ordering::Relaxed);
}

fn promoter(rx: Receiver<(libc::pid_t, bool)>) {
    let warned = AtomicBool::new(false);
    // Helpers wait for an audio thread to be promoted first: RealtimeKit grants a user only so
    // many threads and requests at a time, and the audio thread must not miss out.
    let mut waiting: Vec<libc::pid_t> = Vec::new();
    let mut audio_promoted = false;
    while let Ok((tid, reported)) = rx.recv() {
        if !reported && !audio_promoted {
            waiting.push(tid);
            continue;
        }
        let status = promote(tid);
        if matches!(status.via, "direct" | "rtkit") {
            remember_promoted(tid);
        }
        if reported && status.is_realtime() && !audio_promoted {
            audio_promoted = true;
            for helper in waiting.drain(..) {
                if matches!(promote(helper).via, "direct" | "rtkit") {
                    remember_promoted(helper);
                }
            }
        }
        if let Some(reason) = &status.reason
            && !warned.swap(true, Ordering::Relaxed)
        {
            eprintln!("audio: no real-time priority ({reason}); xruns are more likely under load");
        }
        if reported && let Ok(mut s) = STATUS.lock() {
            *s = Some(status);
        }
    }
}

fn current_tid() -> libc::pid_t {
    // SAFETY: gettid has no preconditions.
    unsafe { libc::syscall(libc::SYS_gettid) as libc::pid_t }
}

fn scheduling(tid: libc::pid_t) -> (&'static str, i32) {
    // SAFETY: both calls only read the scheduling of thread `tid`; a stale tid gives -1.
    let policy = unsafe { libc::sched_getscheduler(tid) };
    let mut param = libc::sched_param { sched_priority: 0 };
    unsafe { libc::sched_getparam(tid, &mut param) };
    let name = match policy & !libc::SCHED_RESET_ON_FORK {
        libc::SCHED_FIFO => "SCHED_FIFO",
        libc::SCHED_RR => "SCHED_RR",
        _ => "SCHED_OTHER",
    };
    (name, param.sched_priority)
}

fn promote(tid: libc::pid_t) -> Realtime {
    let (policy, priority) = scheduling(tid);
    if priority > 0 {
        return Realtime { policy, priority, via: "host", reason: None };
    }
    let direct = match set_fifo(tid, rtprio_limit()) {
        Ok(()) => {
            let (policy, priority) = scheduling(tid);
            return Realtime { policy, priority, via: "direct", reason: None };
        }
        Err(e) => e,
    };
    match rtkit(tid) {
        Ok(()) => {
            let (policy, priority) = scheduling(tid);
            Realtime { policy, priority, via: "rtkit", reason: None }
        }
        Err(rtkit) => {
            Realtime { policy, priority, via: "none", reason: Some(format!("{direct}; RealtimeKit: {rtkit}")) }
        }
    }
}

/// The soft `RLIMIT_RTPRIO`, raised to the hard limit first.
fn rtprio_limit() -> libc::rlim_t {
    let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: getrlimit and setrlimit only read and write `limit`.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_RTPRIO, &mut limit) != 0 {
            return 0;
        }
        if limit.rlim_cur < limit.rlim_max {
            let raised = libc::rlimit { rlim_cur: limit.rlim_max, ..limit };
            if libc::setrlimit(libc::RLIMIT_RTPRIO, &raised) == 0 {
                limit = raised;
            }
        }
    }
    limit.rlim_cur
}

fn set_fifo(tid: libc::pid_t, limit: libc::rlim_t) -> Result<(), String> {
    // SAFETY: geteuid has no preconditions.
    let root = unsafe { libc::geteuid() } == 0;
    let wanted = WANTED.load(Ordering::Relaxed);
    let priority = if root { wanted } else { wanted.min(limit.min(99) as i32) };
    if priority <= 0 {
        return Err("rtprio limit is 0".into());
    }
    let param = libc::sched_param { sched_priority: priority };
    // SAFETY: sched_setscheduler reads `param` and changes only thread `tid` of this process.
    if unsafe { libc::sched_setscheduler(tid, libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK, &param) } != 0 {
        return Err(format!("SCHED_FIFO {priority}: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}

/// Asks RealtimeKit for its highest priority for thread `tid`. RealtimeKit only serves
/// processes whose hard `RLIMIT_RTTIME` is at most its own limit.
fn rtkit(tid: libc::pid_t) -> Result<(), String> {
    let err = |e: zbus::Error| e.to_string();
    let conn = zbus::blocking::Connection::system().map_err(err)?;
    let kit = zbus::blocking::Proxy::new(
        &conn,
        "org.freedesktop.RealtimeKit1",
        "/org/freedesktop/RealtimeKit1",
        "org.freedesktop.RealtimeKit1",
    )
    .map_err(err)?;
    let max_priority: i32 = kit.get_property("MaxRealtimePriority").map_err(err)?;
    let max_rttime: i64 = kit.get_property("RTTimeUSecMax").map_err(err)?;
    if max_priority <= 0 || max_rttime <= 0 {
        return Err(format!("it allows priority {max_priority}, {max_rttime} µs"));
    }
    let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: getrlimit and setrlimit only read and write `limit`.
    unsafe {
        libc::getrlimit(libc::RLIMIT_RTTIME, &mut limit);
        let new = rttime_limit(limit.rlim_max, max_rttime as libc::rlim_t);
        if (new.rlim_cur, new.rlim_max) != (limit.rlim_cur, limit.rlim_max)
            && libc::setrlimit(libc::RLIMIT_RTTIME, &new) != 0
        {
            return Err(format!("RLIMIT_RTTIME: {}", std::io::Error::last_os_error()));
        }
    }
    // SAFETY: getpid has no preconditions.
    let pid = unsafe { libc::getpid() } as u64;
    kit.call_method("MakeThreadRealtimeWithPID", &(pid, tid as u64, max_priority as u32)).map_err(err)?;
    Ok(())
}

/// The `RLIMIT_RTTIME` to run under: a hard limit RealtimeKit accepts, and a soft one at three
/// quarters of it so the soft limit's SIGXCPU comes well before the hard limit's SIGKILL.
fn rttime_limit(hard: libc::rlim_t, kit_max: libc::rlim_t) -> libc::rlimit {
    let hard = hard.min(kit_max);
    libc::rlimit { rlim_cur: hard / 4 * 3, rlim_max: hard }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_is_promoted_or_says_why_not() {
        let status = std::thread::spawn(|| promote(current_tid())).join().unwrap();
        if status.is_realtime() {
            assert!(status.policy != "SCHED_OTHER" && status.reason.is_none(), "{status:?}");
            assert!(["direct", "rtkit"].contains(&status.via), "{status:?}");
        } else {
            assert_eq!((status.policy, status.via), ("SCHED_OTHER", "none"));
            assert!(status.reason.as_deref().is_some_and(|r| r.contains("RealtimeKit")), "{status:?}");
        }
        let (policy, priority) = scheduling(current_tid());
        assert_eq!((policy, priority), ("SCHED_OTHER", 0), "only the thread asked for is promoted");
    }

    #[test]
    fn the_soft_cpu_limit_stays_below_the_hard_one() {
        let unlimited = rttime_limit(libc::RLIM_INFINITY, 200_000);
        assert_eq!((unlimited.rlim_cur, unlimited.rlim_max), (150_000, 200_000));
        let lower = rttime_limit(40_000, 200_000);
        assert_eq!((lower.rlim_cur, lower.rlim_max), (30_000, 40_000));
    }

    #[test]
    fn requests_before_enabling_do_nothing() {
        if PROMOTER.get().is_none() {
            request();
            assert_eq!(status(), None);
        }
    }
}
