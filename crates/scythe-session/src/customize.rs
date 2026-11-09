//! Keyboard shortcuts, custom actions, scripts as actions, toolbars and mouse modifiers, kept
//! in REAPER's files beside the preferences (`reaper-kb.ini`, `reaper-menu.ini`,
//! `reaper-mouse.ini`) so REAPER's own can be copied in and out.

use std::path::PathBuf;

use scythe_core::keymap::{self, Chord, CustomAction, KeyBinding, Keymap, Line, ScriptAction};
use scythe_core::menus::{MenuFile, Toolbar};
use scythe_core::mousemap::{Assignment, MouseMap};

use crate::Session;

pub const MENU_FILE: &str = "reaper-menu.ini";
pub const MOUSE_FILE: &str = "reaper-mouse.ini";

impl Session {
    fn config_file(&self, name: &str) -> Option<PathBuf> {
        Some(self.prefs_file.as_ref()?.with_file_name(name))
    }

    /// Reads the toolbars and mouse modifiers beside the preferences.
    pub(crate) fn load_customizations(&mut self) {
        let read = |path: Option<PathBuf>| path.and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
        self.menus = MenuFile::parse(&read(self.config_file(MENU_FILE)));
        self.mouse = MouseMap::parse(&read(self.config_file(MOUSE_FILE)));
    }

    fn save_config(&mut self, name: &str, text: String) {
        let Some(path) = self.config_file(name) else { return };
        if let Err(e) = std::fs::write(&path, text) {
            self.notify(format!("Cannot save {}: {e}", path.display()));
        }
        self.sync();
    }

    fn keymap_changed(&mut self) {
        self.save_action_bindings();
        self.sync();
    }

    /// The keyboard shortcuts of `command` in `section`.
    pub fn shortcuts(&self, command: &str, section: u32) -> Vec<Chord> {
        let c = crate::actions::normalize(command);
        self.keymap
            .keys()
            .filter(|k| k.section == section && k.chord.is_keyboard() && k.command.eq_ignore_ascii_case(&c))
            .map(|k| k.chord)
            .collect()
    }

    /// Binds a key to `command` in `section`, replacing what the key ran there.
    pub fn bind_key(&mut self, chord: Chord, section: u32, command: &str) {
        let command = crate::actions::normalize(command);
        let name = self.action_name(&command).unwrap_or_default();
        let comment = format!("{} : {} : {name}", keymap::section_name(section), chord.describe());
        self.keymap.bind(chord, section, &command, Some(comment));
        self.keymap_changed();
    }

    /// Removes what `chord` runs in `section`; answers whether it ran anything.
    pub fn unbind_key(&mut self, chord: Chord, section: u32) -> bool {
        let removed = self.keymap.unbind(chord, section) > 0;
        if removed {
            self.keymap_changed();
        }
        removed
    }

    /// Disables Scythe's default shortcut on `chord` (REAPER's command 0).
    pub fn disable_default_key(&mut self, chord: Chord, section: u32) {
        let comment = format!("{} : {} : DISABLED DEFAULT", keymap::section_name(section), chord.describe());
        self.keymap.bind(chord, section, "0", Some(comment));
        self.keymap_changed();
    }

    /// Adds or replaces a custom action; answers its named command (`_id`).
    pub fn define_custom_action(
        &mut self,
        id: Option<String>,
        name: &str,
        commands: Vec<String>,
        consolidate_undo: bool,
    ) -> String {
        let id = id.map(|i| i.trim_start_matches('_').to_owned()).unwrap_or_else(|| keymap::new_custom_id(name));
        let name = if name.starts_with("Custom: ") { name.to_owned() } else { format!("Custom: {name}") };
        let flags = if consolidate_undo { keymap::ACT_CONSOLIDATE_UNDO } else { 0 };
        let commands = commands.iter().map(|c| crate::actions::normalize(c)).collect();
        let act = CustomAction { flags, section: keymap::SECTION_MAIN, id, name, commands };
        let command = act.named_command();
        self.keymap.define(Line::Act(act));
        self.keymap_changed();
        command
    }

    /// Registers a script as an action, as `AddRemoveReaScript` does; answers its command.
    pub fn add_script_action(&mut self, path: &str, section: u32) -> String {
        if let Some(existing) = self.keymap.scripts().find(|s| s.path == path && s.section == section) {
            return existing.named_command();
        }
        let file = std::path::Path::new(path).file_name().map_or(path.to_owned(), |f| f.to_string_lossy().into_owned());
        let script = ScriptAction {
            flags: 4,
            section,
            id: keymap::script_id(path, section),
            name: format!("Custom: {file}"),
            path: path.to_owned(),
        };
        let command = script.named_command();
        self.keymap.define(Line::Scr(script));
        self.keymap_changed();
        command
    }

    /// Removes a custom action or script and its shortcuts.
    pub fn remove_named_action(&mut self, command: &str) -> bool {
        let removed = self.keymap.remove_named(command, keymap::SECTION_MAIN);
        if removed {
            self.learn
                .bindings
                .retain(|b| !b.action.trim_start_matches('_').eq_ignore_ascii_case(command.trim_start_matches('_')));
            self.keymap_changed();
        }
        removed
    }

    /// The script a script action runs, resolved against the resource path's `Scripts`.
    pub fn script_path(&self, command: &str) -> Option<PathBuf> {
        let Line::Scr(s) = self.keymap.named(command, keymap::SECTION_MAIN)? else { return None };
        let path = PathBuf::from(&s.path);
        if path.is_absolute() {
            return Some(path);
        }
        Some(self.prefs_file.as_ref()?.with_file_name("Scripts").join(path))
    }

    /// Merges a `.ReaperKeyMap`; answers how many lines came in.
    pub fn import_keymap(&mut self, text: &str) -> usize {
        let mut other = Keymap::parse(text);
        let mut learned = crate::learn::LearnState::default();
        learned.take_from(&mut other);
        for b in learned.bindings {
            self.learn.bindings.retain(|x| x.trigger != b.trigger);
            self.learn.bindings.push(b);
        }
        let n = other.lines.len();
        self.keymap.merge(&other);
        self.keymap_changed();
        n
    }

    /// The customised toolbar in `section`, else none.
    pub fn toolbar(&self, section: &str) -> Option<Toolbar> {
        self.menus.toolbar(section)
    }

    pub fn set_toolbar(&mut self, toolbar: &Toolbar) {
        self.menus.set_toolbar(toolbar);
        let text = self.menus.write();
        self.save_config(MENU_FILE, text);
    }

    pub fn reset_toolbar(&mut self, section: &str) -> bool {
        let removed = self.menus.reset(section);
        let text = self.menus.write();
        self.save_config(MENU_FILE, text);
        removed
    }

    /// Imports a `.ReaperMenu` or `.ReaperMenuSet`; answers how many menus and toolbars came.
    pub fn import_menus(&mut self, text: &str) -> usize {
        let n = self.menus.import(&MenuFile::parse(text));
        let text = self.menus.write();
        self.save_config(MENU_FILE, text);
        n
    }

    /// `SetMouseModifier`: `None` resets to the default.
    pub fn set_mouse_modifier(&mut self, context: &str, modifiers: u8, assignment: Option<&Assignment>) {
        self.mouse.set(context, modifiers, assignment);
        let text = self.mouse.write();
        self.save_config(MOUSE_FILE, text);
    }

    pub fn reset_mouse_context(&mut self, context: &str) {
        self.mouse.reset_context(context);
        let text = self.mouse.write();
        self.save_config(MOUSE_FILE, text);
    }

    /// The keyboard shortcuts `chord` runs in `section`, first the file's own.
    pub fn key_command(&self, chord: Chord, section: u32) -> Option<&KeyBinding> {
        self.keymap.bound(chord, section).next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        let pool = std::sync::Arc::new(scythe_engine::MediaPool::with_resample(
            scythe_core::render_settings::ResampleMode::PLAYBACK,
        ));
        Session::new(scythe_engine::Engine::start_offline(pool, 48_000))
    }

    #[test]
    fn custom_action_runs_its_members_as_one_undo_step() {
        let mut s = session();
        s.project.add_track("a");
        s.selected_track = Some(s.project.tracks[0].id);
        let id = s.define_custom_action(None, "mute and solo", vec!["6".into(), "7".into()], true);
        assert!(id.starts_with('_') && id.len() == 33);
        assert_eq!(s.action_name(&id).as_deref(), Some("Custom: mute and solo"));
        crate::actions::run(&mut s, &id).unwrap();
        assert!(s.project.tracks[0].muted && s.project.tracks[0].solo);
        assert_eq!(s.history.undo_label(), Some("Custom: mute and solo"));
        s.undo();
        assert!(!s.project.tracks[0].muted && !s.project.tracks[0].solo);
        assert_eq!(s.toggle_state(&id), 0);
        crate::actions::run(&mut s, "6").unwrap();
        assert_eq!(s.toggle_state(&id), -1);
        assert_eq!(s.command_id(&id), keymap::FIRST_NAMED_ID);
        assert_eq!(s.command_for_id(keymap::FIRST_NAMED_ID).as_deref(), Some(id.as_str()));
    }

    #[test]
    fn without_consolidation_each_member_is_a_step() {
        let mut s = session();
        s.project.add_track("a");
        s.selected_track = Some(s.project.tracks[0].id);
        let id = s.define_custom_action(Some("two".into()), "Custom: two", vec!["6".into(), "7".into()], false);
        assert_eq!(id, "_two");
        crate::actions::run(&mut s, "_two").unwrap();
        assert_eq!(s.history.undo_label(), Some("Toggle solo"));
    }

    #[test]
    fn window_actions_wait_for_the_window() {
        let mut s = session();
        assert!(crate::actions::run(&mut s, "40001").is_err());
        s.host_actions = true;
        crate::actions::run(&mut s, "40001").unwrap();
        let script = s.add_script_action("/x/y.lua", 0);
        crate::actions::run(&mut s, &script).unwrap();
        let mixed = s.define_custom_action(None, "mixed", vec!["40001".into(), "1068".into()], true);
        crate::actions::run(&mut s, &mixed).unwrap();
        assert_eq!(s.take_host_commands(), vec!["40001".to_owned(), script.clone(), mixed]);
        assert_eq!(s.script_path(&script), Some(PathBuf::from("/x/y.lua")));
        assert!(s.remove_named_action(&script));
        assert!(s.script_path(&script).is_none());
    }

    #[test]
    fn keys_bind_and_import() {
        let mut s = session();
        let chord = Chord::parse("Ctrl+Shift+M").unwrap();
        s.bind_key(chord, 0, "40364");
        assert_eq!(s.key_command(chord, 0).map(|k| k.command.as_str()), Some("40364"));
        assert_eq!(s.shortcuts("40364", 0), vec![chord]);
        assert!(
            s.keymap_text().contains("KEY 13 77 40364 0\t\t # Main : Ctrl+Shift+M : Options: Toggle metronome\r\n")
        );
        assert_eq!(s.import_keymap("KEY 13 77 1068 0\nKEY 176 7 40364 0\n"), 1);
        assert_eq!(s.key_command(chord, 0).map(|k| k.command.as_str()), Some("1068"));
        assert_eq!(s.learn.bindings.len(), 1);
        assert!(s.unbind_key(chord, 0));
        assert!(!s.unbind_key(chord, 0));
    }
}
