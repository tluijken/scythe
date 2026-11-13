//! Real-time scheduling on Windows: the threads that render audio join the Multimedia Class
//! Scheduler Service (MMCSS) task "Pro Audio", as ASIO and WASAPI drivers' own threads do.
//! MMCSS raises them into the real-time priority band and reserves CPU time for them; threads
//! that render ahead of the playhead only get a raised normal priority.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use windows_sys::Win32::Foundation::ERROR_THREAD_ALREADY_IN_TASK;
use windows_sys::Win32::System::Threading::{
    AVRT_PRIORITY_CRITICAL, AvSetMmThreadCharacteristicsW, AvSetMmThreadPriority, GetCurrentThread, SetThreadPriority,
    THREAD_PRIORITY_ABOVE_NORMAL,
};

/// Kept for the device settings: on Linux the `SCHED_FIFO` priority asked for. MMCSS has no
/// numeric priority; any value above 0 asks for "Pro Audio".
pub const PRIORITY: i32 = 70;

static WANTED: AtomicI32 = AtomicI32::new(PRIORITY);

/// Sets whether audio threads ask MMCSS for real-time priority: 0 for none ("non-RT"),
/// anything else or `None` for "Pro Audio".
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
    Err("locking process memory is not supported on Windows".into())
}

pub fn hold_cpu_awake() -> Result<std::fs::File, String> {
    Err("holding the CPU out of idle states is not supported on Windows".into())
}

/// How the thread rendering audio is scheduled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Realtime {
    /// `MMCSS Pro Audio`, `MMCSS` (a task the audio driver chose) or `normal`.
    pub policy: &'static str,
    /// MMCSS's thread priority (`AVRT_PRIORITY_CRITICAL` is 2; 1 for the driver's own task,
    /// whose priority is not known), 0 when not in MMCSS.
    pub priority: i32,
    /// `mmcss`, `driver` (the audio driver's thread already was in MMCSS, as ASIO drivers do)
    /// or `none`.
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

/// Asks for real-time priority for the calling thread. The thread joins MMCSS on its first
/// call only; later calls cost a thread-local read.
pub fn request() {
    mark_audio_thread();
    join_pro_audio(true);
}

pub fn mark_audio_thread() {
    AUDIO_THREAD.with(|a| a.set(true));
}

pub fn is_audio_thread() -> bool {
    AUDIO_THREAD.with(|a| a.get())
}

/// Like [`request`], for a thread the audio thread waits on (a track render thread).
pub fn request_helper() {
    join_pro_audio(false);
}

/// For a thread that renders ahead of the playhead (anticipative FX): a raised normal
/// priority, outside MMCSS, so a thread working flat out does not starve the audio threads.
pub fn request_worker() {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    // SAFETY: changes only the calling thread's priority; the pseudo-handle needs no closing.
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL) };
}

fn join_pro_audio(reported: bool) {
    if !ENABLED.load(Ordering::Relaxed) || WANTED.load(Ordering::Relaxed) == 0 || JOINED.with(|j| j.replace(true)) {
        return;
    }
    let status = mmcss();
    if let Some(reason) = &status.reason
        && !WARNED.swap(true, Ordering::Relaxed)
    {
        eprintln!("audio: no real-time priority ({reason}); xruns are more likely under load");
    }
    if reported && let Ok(mut s) = STATUS.lock() {
        *s = Some(status);
    }
}

fn mmcss() -> Realtime {
    let task: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
    let mut index = 0u32;
    // SAFETY: `task` is a NUL-terminated UTF-16 string and `index` a valid out pointer; the
    // returned handle stays open for the thread's lifetime, which is how MMCSS keeps it.
    let handle = unsafe { AvSetMmThreadCharacteristicsW(task.as_ptr(), &mut index) };
    if handle.is_null() {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_THREAD_ALREADY_IN_TASK as i32) {
            return Realtime { policy: "MMCSS", priority: 1, via: "driver", reason: None };
        }
        let reason = format!("MMCSS Pro Audio: {error}");
        return Realtime { policy: "normal", priority: 0, via: "none", reason: Some(reason) };
    }
    // SAFETY: `handle` was just returned by AvSetMmThreadCharacteristicsW.
    unsafe { AvSetMmThreadPriority(handle, AVRT_PRIORITY_CRITICAL) };
    Realtime { policy: "MMCSS Pro Audio", priority: AVRT_PRIORITY_CRITICAL, via: "mmcss", reason: None }
}

/// How the last audio thread that asked is scheduled, once it has been looked at.
pub fn status() -> Option<Realtime> {
    STATUS.lock().ok().and_then(|s| s.clone())
}

/// The fastest cores of a hybrid CPU. Windows' scheduler already prefers the performance cores
/// for MMCSS threads, so none are singled out here.
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
    fn a_thread_joins_pro_audio_or_says_why_not() {
        let status = std::thread::spawn(mmcss).join().unwrap();
        if status.is_realtime() {
            assert_eq!((status.policy, status.via, status.reason.as_deref()), ("MMCSS Pro Audio", "mmcss", None));
        } else {
            assert!(status.reason.as_deref().is_some_and(|r| r.contains("MMCSS")), "{status:?}");
        }
    }

    #[test]
    fn requests_before_enabling_do_nothing() {
        if !ENABLED.load(Ordering::Relaxed) {
            request();
            assert_eq!(status(), None);
        }
    }
}
