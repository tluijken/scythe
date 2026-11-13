//! The thread plugins live on: REAPER's main thread, as far as plugins are concerned. Hosted
//! plugins are created and destroyed here, and their editors open, run and close here. The
//! plugin formats ask for one such thread, and frameworks hold them to it: JUCE binds its
//! message loop to the thread that created the first instance, so a plugin created on one
//! thread and shown from another blocks both on Windows.
//!
//! The thread runs a loop of its own, independent of the window's frames: on Windows a
//! message loop (plugin windows and the plugins' hidden windows get their messages from it),
//! elsewhere a wait on its queue. Other threads hand it work with [`run`], which waits, or
//! [`spawn`], which does not.

use std::any::Any;
use std::cell::RefCell;
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, Sender, channel, sync_channel};
use std::time::{Duration, Instant};

type Job = Box<dyn FnOnce() + Send>;

/// How often tickers run.
const TICK: Duration = Duration::from_millis(16);

struct Thread {
    jobs: Sender<Job>,
    /// Where a posted message wakes the thread's message wait.
    #[cfg(windows)]
    native_id: u32,
}

fn thread() -> &'static Thread {
    static THREAD: OnceLock<Thread> = OnceLock::new();
    THREAD.get_or_init(|| {
        let (jobs, rx) = channel::<Job>();
        let (ready, started) = sync_channel(1);
        std::thread::Builder::new()
            .name("scythe-plugins".into())
            .spawn(move || {
                #[cfg(windows)]
                // SAFETY: GetCurrentThreadId has no preconditions.
                let _ = ready.send(unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() });
                #[cfg(not(windows))]
                let _ = ready.send(());
                serve(rx);
            })
            .expect("spawn the plugin thread");
        #[cfg(windows)]
        let native_id = started.recv().expect("the plugin thread started");
        #[cfg(not(windows))]
        started.recv().expect("the plugin thread started");
        Thread {
            jobs,
            #[cfg(windows)]
            native_id,
        }
    })
}

thread_local! {
    static ON_PLUGIN_THREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static TICKERS: RefCell<Vec<Box<dyn FnMut() -> bool>>> = RefCell::new(Vec::new());
}

/// Whether the calling thread is the plugin thread.
pub fn is_plugin_thread() -> bool {
    ON_PLUGIN_THREAD.with(|t| t.get())
}

fn submit(job: Job) {
    let t = thread();
    if t.jobs.send(job).is_ok() {
        #[cfg(windows)]
        // SAFETY: posts an empty message to wake the plugin thread's message wait.
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                t.native_id,
                windows_sys::Win32::UI::WindowsAndMessaging::WM_APP,
                0,
                0,
            );
        }
    }
}

/// Runs `f` on the plugin thread and returns what it returns, at once when called there. A
/// panic in `f` comes back to the caller.
pub fn run<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    if is_plugin_thread() {
        return f();
    }
    let (done, result) = sync_channel::<Result<R, Box<dyn Any + Send>>>(1);
    let job: Box<dyn FnOnce() + Send + '_> = Box::new(move || {
        let _ = done.send(std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)));
    });
    // SAFETY: this function does not return before the job ran (the result is received
    // below), so what `f` borrows outlives its use on the plugin thread.
    let job: Job = unsafe { std::mem::transmute::<Box<dyn FnOnce() + Send + '_>, Job>(job) };
    submit(job);
    match result.recv().expect("the plugin thread runs every job") {
        Ok(r) => r,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// Queues `f` for the plugin thread without waiting for it.
pub fn spawn(f: impl FnOnce() + Send + 'static) {
    if is_plugin_thread() {
        f();
    } else {
        submit(Box::new(f));
    }
}

/// Calls `f` on the plugin thread about every 16 ms until it returns false. Call it from the
/// plugin thread.
pub fn add_ticker(f: impl FnMut() -> bool + 'static) {
    debug_assert!(is_plugin_thread(), "tickers are added on the plugin thread");
    TICKERS.with(|t| t.borrow_mut().push(Box::new(f)));
}

fn tick() {
    let mut tickers = TICKERS.with(|t| std::mem::take(&mut *t.borrow_mut()));
    tickers.retain_mut(|f| f());
    TICKERS.with(|t| {
        let mut t = t.borrow_mut();
        tickers.append(&mut t);
        *t = tickers;
    });
}

fn serve(jobs: Receiver<Job>) {
    ON_PLUGIN_THREAD.with(|t| t.set(true));
    let mut next_tick = Instant::now() + TICK;
    loop {
        let now = Instant::now();
        if now >= next_tick {
            tick();
            next_tick = now + TICK;
        }
        let wait = next_tick.saturating_duration_since(Instant::now());
        #[cfg(windows)]
        {
            while let Ok(job) = jobs.try_recv() {
                job();
            }
            if !windows::wait_and_dispatch(wait) {
                return;
            }
            while let Ok(job) = jobs.try_recv() {
                job();
            }
        }
        #[cfg(not(windows))]
        match jobs.recv_timeout(wait) {
            Ok(job) => {
                job();
                while let Ok(job) = jobs.try_recv() {
                    job();
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::time::Duration;

    use windows_sys::Win32::System::Threading::INFINITE;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE, PeekMessageW, QS_ALLINPUT,
        TranslateMessage, WM_QUIT,
    };

    /// Waits up to `wait` for a message or a posted job and dispatches every message waiting;
    /// false on `WM_QUIT`.
    pub fn wait_and_dispatch(wait: Duration) -> bool {
        let ms = u32::try_from(wait.as_millis()).unwrap_or(INFINITE - 1);
        // SAFETY: waits on no handles, only on this thread's message queue; then dispatches
        // the messages of this thread's windows as any message loop does.
        unsafe {
            MsgWaitForMultipleObjectsEx(0, std::ptr::null(), ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                if msg.message == WM_QUIT {
                    return false;
                }
                if !msg.hwnd.is_null() {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_runs_on_one_other_thread_and_returns() {
        assert!(!is_plugin_thread());
        let here = std::thread::current().id();
        let (there, again) = (run(|| std::thread::current().id()), run(|| std::thread::current().id()));
        assert_ne!(there, here);
        assert_eq!(there, again);
        assert!(run(is_plugin_thread));
        let borrowed = [1, 2, 3];
        assert_eq!(run(|| borrowed.iter().sum::<i32>()), 6);
        assert_eq!(run(|| run(|| 7)), 7, "nested calls run in place");
    }

    #[test]
    fn panics_come_back_to_the_caller() {
        let caught = std::panic::catch_unwind(|| run(|| panic!("plugin crashed")));
        assert!(caught.is_err());
        assert_eq!(run(|| 1), 1, "the thread keeps serving");
    }

    #[test]
    fn tickers_run_until_they_stop() {
        let (tx, rx) = channel();
        spawn(move || {
            let mut n = 0;
            add_ticker(move || {
                n += 1;
                let _ = tx.send(n);
                n < 3
            });
        });
        let ticks: Vec<i32> = rx.iter().take(3).collect();
        assert_eq!(ticks, [1, 2, 3]);
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err(), "it stopped after returning false");
    }
}
