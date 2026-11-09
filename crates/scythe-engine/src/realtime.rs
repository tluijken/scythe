//! Real-time scheduling for the threads that render audio: the device callback and the silent
//! clock. With an `rtprio` limit (or as root) the thread gets `SCHED_FIFO` directly; without
//! one it asks RealtimeKit over the system D-Bus, as PipeWire does. Promotion happens on a
//! helper thread, so the audio thread only hands over its id and never waits; when neither
//! works the audio plays on at normal priority and the reason is printed once.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Mutex, OnceLock};

/// `SCHED_FIFO` priority asked for directly: above PipeWire's and JACK's clients, below
/// the IRQ threads of a real-time kernel.
const PRIORITY: i32 = 70;

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

static PROMOTER: OnceLock<SyncSender<libc::pid_t>> = OnceLock::new();
static STATUS: Mutex<Option<Realtime>> = Mutex::new(None);

/// Starts promoting the threads that [`request`] names. Without it (tests, offline renders)
/// audio threads keep their normal priority.
pub fn enable() {
    PROMOTER.get_or_init(|| {
        let (tx, rx) = sync_channel(4);
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
    if let Some(tx) = PROMOTER.get() {
        let _ = tx.try_send(current_tid());
    }
}

/// How the last thread that asked is scheduled, once it has been looked at.
pub fn status() -> Option<Realtime> {
    STATUS.lock().ok().and_then(|s| s.clone())
}

fn promoter(rx: Receiver<libc::pid_t>) {
    let warned = AtomicBool::new(false);
    while let Ok(tid) = rx.recv() {
        let status = promote(tid);
        if let Some(reason) = &status.reason
            && !warned.swap(true, Ordering::Relaxed)
        {
            eprintln!("audio: no real-time priority ({reason}); xruns are more likely under load");
        }
        if let Ok(mut s) = STATUS.lock() {
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
    let priority = if root { PRIORITY } else { PRIORITY.min(limit.min(99) as i32) };
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
/// processes whose `RLIMIT_RTTIME` is at most its own limit; the soft limit is set equal to the
/// hard one, as a soft limit below it would send SIGXCPU, ending the process on one long block.
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
        let max = max_rttime as libc::rlim_t;
        if limit.rlim_max == libc::RLIM_INFINITY || limit.rlim_max > max || limit.rlim_cur != limit.rlim_max {
            let capped = limit.rlim_max.min(max);
            let new = libc::rlimit { rlim_cur: capped, rlim_max: capped };
            if libc::setrlimit(libc::RLIMIT_RTTIME, &new) != 0 {
                return Err(format!("RLIMIT_RTTIME: {}", std::io::Error::last_os_error()));
            }
        }
    }
    // SAFETY: getpid has no preconditions.
    let pid = unsafe { libc::getpid() } as u64;
    kit.call_method("MakeThreadRealtimeWithPID", &(pid, tid as u64, max_priority as u32)).map_err(err)?;
    Ok(())
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
    fn requests_before_enabling_do_nothing() {
        if PROMOTER.get().is_none() {
            request();
            assert_eq!(status(), None);
        }
    }
}
