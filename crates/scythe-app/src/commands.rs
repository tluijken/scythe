//! Running any action by its command (the window's own, custom actions, scripts, the
//! session's), the toggle states only the window knows, screensets, toolbar icons, and the
//! import and export of REAPER's keymap and menu files.

use scythe_core::ini::Ini;
use scythe_core::keymap::{Line, SECTION_MAIN};
use scythe_core::menus;

use crate::app::ScytheApp;
use crate::palette::Action;

/// Where the window sets (screensets) are kept, beside the preferences. REAPER's
/// `reaper-screensets.ini` holds window geometry Scythe has no use for, so the file is its own.
const SCREENSET_FILE: &str = "scythe-screensets.ini";

impl ScytheApp {
    /// Runs an action: a REAPER command id, a Scythe named command, a custom action or a
    /// registered script.
    pub fn run_command(&mut self, command: &str) {
        self.run_command_at(command, 0);
    }

    fn run_command_at(&mut self, command: &str, depth: usize) {
        let c = scythe_session::actions::normalize(command);
        if let Some(action) = Action::from_command(&c) {
            action.run(self);
            return;
        }
        match self.session.keymap.named(&c, SECTION_MAIN).cloned() {
            Some(Line::Act(a)) => {
                if depth > 8 {
                    self.notify(format!("{} nests too deeply", a.name));
                    return;
                }
                let mark = self.session.history.depth();
                for m in &a.commands {
                    self.run_command_at(m, depth + 1);
                }
                if a.consolidate_undo() {
                    self.session.history.consolidate(mark, &a.name);
                }
            }
            Some(Line::Scr(s)) => match self.session.script_path(&c) {
                Some(path) if path.exists() => self.run_script(&path),
                _ => self.notify(format!("Script not found: {}", s.path)),
            },
            _ => {
                if let Err(e) = scythe_session::actions::run(&mut self.session, &c) {
                    self.notify(e.message);
                }
                for c in self.session.take_host_commands() {
                    if Action::from_command(&c).is_some() || self.session.keymap.named(&c, SECTION_MAIN).is_some() {
                        self.run_command_at(&c, depth + 1);
                    }
                }
            }
        }
    }

    /// Tells the session the toggle states of the window's actions, for toolbars, scripts and
    /// surfaces.
    pub fn refresh_host_toggles(&mut self) {
        let mut states: Vec<(String, bool)> = vec![
            ("1157".into(), self.view.snap),
            ("40078".into(), self.view.show_mixer),
            ("_SCYTHE_TOGGLE_SETLIST".into(), self.view.setlist),
            ("_SCYTHE_TOGGLE_MONITOR_MIXES".into(), self.view.monitor_page),
            ("40377".into(), self.view.keyboard_open),
            ("40605".into(), self.view.actions.is_some()),
            (menus::OPEN_MAIN_TOOLBAR.to_string(), self.view.main_toolbar),
        ];
        for n in 1..=menus::FLOATING_TOOLBARS {
            if let Some(id) = menus::open_toolbar_command(n) {
                states.push((id.to_string(), self.view.toolbars.contains(&(n as u8))));
            }
        }
        for (k, v) in states {
            if self.session.host_toggles.get(&k) != Some(&v) {
                self.session.host_toggles.insert(k, v);
            }
        }
    }

    /// A toolbar icon from REAPER's `Data/toolbar_icons`, loaded once.
    pub fn toolbar_icon(&mut self, name: &str) -> Option<egui::TextureHandle> {
        if let Some(t) = self.toolbar_icons.get(name) {
            return t.clone();
        }
        let file = name.rsplit(['/', '\\']).next().unwrap_or(name);
        let file = if file.contains('.') { file.to_owned() } else { format!("{file}.png") };
        let dirs = [crate::app::reaper_resource_dir(), Some(crate::demo::data_dir())];
        let texture = dirs
            .into_iter()
            .flatten()
            .map(|d| d.join("Data").join("toolbar_icons").join(&file))
            .find(|p| p.exists())
            .and_then(|p| {
                let img = image::open(&p).ok()?.to_rgba8();
                let size = [img.width() as usize, img.height() as usize];
                let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
                Some(self.ctx.load_texture(format!("toolbar-icon-{file}"), color, egui::TextureOptions::LINEAR))
            });
        self.toolbar_icons.insert(name.to_owned(), texture.clone());
        texture
    }

    fn screenset_path() -> std::path::PathBuf {
        scythe_session::preferences_path().with_file_name(SCREENSET_FILE)
    }

    /// "Screenset: Save window set #N": which pages, windows and toolbars are shown, and the
    /// zoom.
    pub fn save_screenset(&mut self, n: u8) {
        let path = Self::screenset_path();
        let mut ini = Ini::parse(&std::fs::read_to_string(&path).unwrap_or_default());
        let v = &self.view;
        let toolbars: Vec<String> = v.toolbars.iter().map(u8::to_string).collect();
        let regions = match v.regions {
            Some(crate::ui::regions::RegionsTab::List) => "list",
            Some(crate::ui::regions::RegionsTab::Matrix) => "matrix",
            None => "",
        };
        let values = [
            ("mixer", (v.show_mixer as u8).to_string()),
            ("setlist", (v.setlist as u8).to_string()),
            ("monitor", (v.monitor_page as u8).to_string()),
            ("main_toolbar", (v.main_toolbar as u8).to_string()),
            ("toolbars", toolbars.join(",")),
            ("row_height", v.row_h.to_string()),
            ("zoom", v.px_per_sec.to_string()),
            ("keyboard", (v.keyboard_open as u8).to_string()),
            ("midi_devices", (v.midi_devices_open as u8).to_string()),
            ("actions", (v.actions.is_some() as u8).to_string()),
            ("regions", regions.to_owned()),
        ];
        let section = ini.section_mut(&format!("screenset {n}"));
        section.entries.clear();
        for (k, val) in values {
            section.set(k, &val);
        }
        match std::fs::write(&path, ini.write()) {
            Ok(()) => self.notify(format!("Saved window set {n}")),
            Err(e) => self.notify(format!("Cannot save {}: {e}", path.display())),
        }
    }

    /// "Screenset: Load window set #N".
    pub fn load_screenset(&mut self, n: u8) {
        let ini = Ini::parse(&std::fs::read_to_string(Self::screenset_path()).unwrap_or_default());
        let Some(s) = ini.section(&format!("screenset {n}")) else {
            self.notify(format!("Window set {n} has not been saved"));
            return;
        };
        let flag = |k: &str| s.get(k).map(|v| v == "1");
        let v = &mut self.view;
        v.show_mixer = flag("mixer").unwrap_or(v.show_mixer);
        v.setlist = flag("setlist").unwrap_or(v.setlist);
        v.monitor_page = flag("monitor").unwrap_or(v.monitor_page);
        v.main_toolbar = flag("main_toolbar").unwrap_or(v.main_toolbar);
        if let Some(t) = s.get("toolbars") {
            v.toolbars = t.split(',').filter_map(|n| n.trim().parse().ok()).collect();
        }
        if let Some(h) = s.get("row_height").and_then(|h| h.parse::<f32>().ok()) {
            v.row_h = h.clamp(30.0, 240.0);
        }
        if let Some(z) = s.get("zoom").and_then(|z| z.parse::<f64>().ok()) {
            v.px_per_sec = z.clamp(2.0, 2000.0);
        }
        v.keyboard_open = flag("keyboard").unwrap_or(v.keyboard_open);
        v.midi_devices_open = flag("midi_devices").unwrap_or(v.midi_devices_open);
        match flag("actions") {
            Some(true) if v.actions.is_none() => v.actions = Some(Default::default()),
            Some(false) => v.actions = None,
            _ => {}
        }
        v.regions = match s.get("regions") {
            Some("list") => Some(crate::ui::regions::RegionsTab::List),
            Some("matrix") => Some(crate::ui::regions::RegionsTab::Matrix),
            _ => None,
        };
    }

    pub fn import_keymap_dialog(&mut self) {
        let dialog = crate::app::resource_dialog("KeyMaps").add_filter("Key map", &["ReaperKeyMap", "ini"]);
        let Some(path) = dialog.pick_file() else { return };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let n = self.session.import_keymap(&text);
                self.notify(format!("Imported {n} shortcuts and actions"));
            }
            Err(e) => self.notify(format!("Cannot read {}: {e}", path.display())),
        }
    }

    pub fn export_keymap_dialog(&mut self) {
        let dialog = crate::app::resource_dialog("KeyMaps")
            .add_filter("Key map", &["ReaperKeyMap"])
            .set_file_name("Scythe.ReaperKeyMap");
        let Some(path) = dialog.save_file() else { return };
        if let Err(e) = std::fs::write(&path, self.session.keymap_text()) {
            self.notify(format!("Cannot save {}: {e}", path.display()));
        }
    }

    pub fn import_menus_dialog(&mut self) {
        let dialog =
            crate::app::resource_dialog("MenuSets").add_filter("Menus and toolbars", &["ReaperMenu", "ReaperMenuSet"]);
        let Some(path) = dialog.pick_file() else { return };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let n = self.session.import_menus(&text);
                self.notify(format!("Imported {n} menus and toolbars"));
            }
            Err(e) => self.notify(format!("Cannot read {}: {e}", path.display())),
        }
    }

    pub fn export_menus_dialog(&mut self, section: &str) {
        let toolbar = scythe_session::api::customize::toolbar_or_default(&self.session, section);
        let mut file = menus::MenuFile::default();
        file.set_toolbar(&toolbar);
        let dialog = crate::app::resource_dialog("MenuSets")
            .add_filter("Toolbar", &["ReaperMenu"])
            .set_file_name(format!("{section}.ReaperMenu"));
        let Some(path) = dialog.save_file() else { return };
        if let Err(e) = std::fs::write(&path, file.write()) {
            self.notify(format!("Cannot save {}: {e}", path.display()));
        }
    }
}
