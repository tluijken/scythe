//! The window's side of playing live: setlist files picked in dialogs, and marker commands the
//! session leaves to the app's actions.

use crate::app::ScytheApp;
use crate::palette::Action;

impl ScytheApp {
    /// Copies the setlist as text and saves it to a file.
    pub fn export_setlist(&mut self) {
        let Some(list) = self.current_setlist() else { return };
        self.ctx.copy_text(list.to_text(false));
        let Some(path) =
            rfd::FileDialog::new().add_filter("Text", &["txt"]).set_file_name(format!("{}.txt", list.name)).save_file()
        else {
            self.notify("Setlist copied");
            return;
        };
        match std::fs::write(&path, list.to_text(false)) {
            Ok(()) => self.notify(format!("Exported {} (also copied)", path.display())),
            Err(e) => self.notify(format!("Could not export: {e}")),
        }
    }

    pub fn import_setlist(&mut self) {
        let Some(path) = rfd::FileDialog::new().add_filter("Text", &["txt"]).pick_file() else { return };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                self.notify(format!("Could not import: {e}"));
                return;
            }
        };
        match self.import_setlist_text(&text).1 {
            0 => self.notify("Imported setlist"),
            missing => self.notify(format!("Imported; {missing} song(s) have no marker in this project")),
        }
    }

    /// Runs the actions of marker commands the session has none for.
    pub fn run_marker_commands(&mut self) {
        for id in self.session.take_marker_commands() {
            match Action::from_reaper_command(i64::from(id)) {
                Some(action) => action.run(self),
                None => self.notify(format!("Marker action {id} is not available in Scythe")),
            }
        }
    }
}
