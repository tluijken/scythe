//! Scythe's file dialogs, which say so when none can open: on Linux rfd answers no file both
//! after a cancel and when it has neither a file chooser portal nor zenity to show.

use std::cell::Cell;
use std::path::PathBuf;

use scythe_chooser::Shown;

/// rfd's dialogs through [`picked`].
pub trait Ask {
    fn ask_file(self) -> Option<PathBuf>;
    fn ask_files(self) -> Option<Vec<PathBuf>>;
    fn ask_save_file(self) -> Option<PathBuf>;
    fn ask_folder(self) -> Option<PathBuf>;
}

impl Ask for rfd::FileDialog {
    fn ask_file(self) -> Option<PathBuf> {
        picked(|| self.pick_file())
    }

    fn ask_files(self) -> Option<Vec<PathBuf>> {
        picked(|| self.pick_files())
    }

    fn ask_save_file(self) -> Option<PathBuf> {
        picked(|| self.save_file())
    }

    fn ask_folder(self) -> Option<PathBuf> {
        picked(|| self.pick_folder())
    }
}

thread_local! {
    static MISSED: Cell<bool> = const { Cell::new(false) };
    #[cfg(test)]
    static FAKE: Cell<Option<Fake>> = const { Cell::new(None) };
}

/// What `dialog`, an rfd call, gave. When no dialog could show, [`missed`] says so once.
pub fn picked<T>(dialog: impl FnOnce() -> Option<T>) -> Option<T> {
    let shown = show(dialog);
    if matches!(shown, Shown::NoDialog) {
        MISSED.set(true);
    }
    shown.picked()
}

/// How `dialog`, an rfd call, ended.
pub fn show<T>(dialog: impl FnOnce() -> Option<T>) -> Shown<T> {
    #[cfg(test)]
    match FAKE.get() {
        Some(Fake::NoDialog) => return Shown::NoDialog,
        Some(Fake::Cancelled) => return Shown::Cancelled,
        None => {}
    }
    scythe_chooser::show(dialog)
}

/// Whether a dialog asked for through [`picked`] could not show since the last call.
pub fn missed() -> bool {
    MISSED.replace(false)
}

/// How dialogs on this thread end instead of showing.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub enum Fake {
    NoDialog,
    Cancelled,
}

#[cfg(test)]
pub fn fake(answer: Option<Fake>) {
    FAKE.set(answer);
}
