//! Whether a file dialog showed, for Scythe and the ScyEQ-Pro plugin. rfd answers no file both
//! after a cancel and when, on Linux, it has nothing to show: no file chooser portal and no
//! zenity. [`show`] tells the two apart.

#[cfg(all(unix, not(target_os = "macos")))]
mod probe;

#[cfg(all(unix, not(target_os = "macos")))]
pub use probe::available;

/// What to tell the user when no file dialog can open.
pub const NO_DIALOG: &str =
    "No file dialog can open: install a desktop file chooser portal (xdg-desktop-portal) or zenity.";

/// How a file dialog ended.
#[derive(Debug, PartialEq)]
pub enum Shown<T> {
    Picked(T),
    Cancelled,
    /// None showed: on Linux, neither a file chooser portal nor zenity was found, and rfd gave up
    /// at once.
    NoDialog,
}

impl<T> Shown<T> {
    pub fn picked(self) -> Option<T> {
        match self {
            Shown::Picked(t) => Some(t),
            Shown::Cancelled | Shown::NoDialog => None,
        }
    }
}

/// Runs `dialog`, an rfd call that blocks until the dialog closes, and says how it ended.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn show<T>(dialog: impl FnOnce() -> Option<T>) -> Shown<T> {
    let found = available();
    let started = std::time::Instant::now();
    let picked = dialog();
    outcome(found, picked, started.elapsed())
}

/// Runs `dialog`, an rfd call that blocks until the dialog closes, and says how it ended.
#[cfg(not(all(unix, not(target_os = "macos"))))]
pub fn show<T>(dialog: impl FnOnce() -> Option<T>) -> Shown<T> {
    dialog().map_or(Shown::Cancelled, Shown::Picked)
}

/// How a dialog that took `took` ended, `found` saying whether a file chooser was found before.
/// The search can miss one that works, so its answer alone opens or blocks nothing: no file means
/// no dialog only when none was found and rfd gave up at once. A missing portal and zenity fail
/// within milliseconds; a person needs longer than this to see a dialog and cancel it.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn outcome<T>(found: bool, picked: Option<T>, took: std::time::Duration) -> Shown<T> {
    const AT_ONCE: std::time::Duration = std::time::Duration::from_millis(1500);
    match picked {
        Some(t) => Shown::Picked(t),
        None if !found && took < AT_ONCE => Shown::NoDialog,
        None => Shown::Cancelled,
    }
}

#[cfg(all(test, unix, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn a_dialog_without_a_file_counts_as_missing_only_when_none_was_found_and_it_ended_at_once() {
        use std::time::Duration;
        let file = || Some("/music/reference.wav");
        let (quick, slow) = (Duration::from_millis(30), Duration::from_secs(10));
        assert_eq!(outcome(true, None::<&str>, quick), Shown::Cancelled, "found, then cancelled");
        assert_eq!(outcome(false, file(), quick), Shown::Picked("/music/reference.wav"));
        assert_eq!(outcome(false, None::<&str>, quick), Shown::NoDialog);
        assert_eq!(outcome(false, None::<&str>, slow), Shown::Cancelled, "a person closed a dialog the probe missed");
    }
}
