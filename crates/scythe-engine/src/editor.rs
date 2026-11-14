//! Plugin editors. An editor is taken from an effect once, under its lock, and then driven
//! from the UI thread on its own: the plugin formats allow their GUI calls to run alongside
//! audio processing, and holding the effect's lock for them would interrupt the audio.

use std::time::{Duration, Instant};

pub trait Editor {
    /// Embeds the editor in the native window `parent` (an X11 window id, on Windows an `HWND`,
    /// on macOS an `NSView`); returns its size in pixels.
    fn open(&mut self, parent: usize) -> Result<(u32, u32), String>;
    /// Runs the plugin's timers and file-descriptor callbacks. Returns a size the plugin asked
    /// its window to take.
    fn idle(&mut self) -> Option<(u32, u32)>;
    fn close(&mut self);
    /// The plugin closed its editor itself.
    fn closed(&self) -> bool {
        false
    }
    /// The scale of the screen the window shows on (1.0 at 96 dpi), where windows are sized in
    /// physical pixels (Windows); before `open` and whenever it changes.
    fn set_scale(&mut self, _scale: f64) {}
}

/// Timers a plugin registered with the host, fired from `due`.
#[derive(Debug, Default)]
pub struct Timers {
    timers: Vec<(u32, Duration, Instant)>,
    next_id: u32,
}

impl Timers {
    pub fn add(&mut self, period: Duration) -> u32 {
        self.next_id += 1;
        let period = period.max(Duration::from_millis(1));
        self.timers.push((self.next_id, period, Instant::now() + period));
        self.next_id
    }

    pub fn remove(&mut self, id: u32) -> bool {
        let before = self.timers.len();
        self.timers.retain(|t| t.0 != id);
        self.timers.len() != before
    }

    /// Ids of the timers that are due, rescheduling them.
    pub fn due(&mut self) -> Vec<u32> {
        let now = Instant::now();
        let mut out = Vec::new();
        for (id, period, next) in &mut self.timers {
            if *next <= now {
                out.push(*id);
                *next = now + *period;
            }
        }
        out
    }
}

#[cfg(unix)]
pub(crate) const POLL_READ: i16 = libc::POLLIN | libc::POLLPRI;
#[cfg(unix)]
pub(crate) const POLL_ERROR: i16 = libc::POLLERR | libc::POLLHUP;
#[cfg(windows)]
pub(crate) const POLL_READ: i16 = 1;
#[cfg(windows)]
pub(crate) const POLL_ERROR: i16 = 2;

/// Plugins register descriptors only through CLAP's POSIX extension, which Windows lacks.
#[cfg(windows)]
pub fn ready_fds(_fds: &[i32]) -> Vec<(i32, i16)> {
    Vec::new()
}

/// File descriptors that are readable (or in error) right now, without blocking.
#[cfg(unix)]
pub fn ready_fds(fds: &[i32]) -> Vec<(i32, i16)> {
    if fds.is_empty() {
        return Vec::new();
    }
    let mut polls: Vec<libc::pollfd> =
        fds.iter().map(|&fd| libc::pollfd { fd, events: libc::POLLIN | libc::POLLPRI, revents: 0 }).collect();
    // SAFETY: `polls` is a valid array of pollfd for the duration of the call; timeout 0.
    let n = unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, 0) };
    if n <= 0 {
        return Vec::new();
    }
    polls.into_iter().filter(|p| p.revents != 0).map(|p| (p.fd, p.revents)).collect()
}
