//! Toolbars (and menus) as REAPER keeps them customised in `reaper-menu.ini` and exports
//! them as `.ReaperMenu`/`.ReaperMenuSet`: one section per toolbar (`[Main toolbar]`,
//! `[Floating toolbar 1]`…) with `item_N=<command> <text>`, `icon_N=<file or text icon>` and a
//! `title`. Item `-1` is a separator. Keys Scythe does not read are kept.

use crate::ini::{Ini, Section};

pub const MAIN_TOOLBAR: &str = "Main toolbar";
/// How many floating toolbars REAPER has.
pub const FLOATING_TOOLBARS: usize = 32;
/// REAPER's "Toolbar: Open/close main toolbar".
pub const OPEN_MAIN_TOOLBAR: u32 = 41651;

/// The section of floating toolbar `n` (from 1).
pub fn floating_toolbar(n: usize) -> String {
    format!("Floating toolbar {n}")
}

/// REAPER's "Toolbar: Open/close toolbar N" command id, N from 1 to 32.
pub fn open_toolbar_command(n: usize) -> Option<u32> {
    match n {
        1..=8 => Some(41679 + n as u32 - 1),
        9..=16 => Some(41936 + n as u32 - 9),
        17..=32 => Some(42713 + n as u32 - 17),
        _ => None,
    }
}

/// The floating toolbar a "Toolbar: Open/close toolbar N" command opens.
pub fn toolbar_for_command(id: u32) -> Option<usize> {
    (1..=FLOATING_TOOLBARS).find(|&n| open_toolbar_command(n) == Some(id))
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Button {
    /// A command id or named command; `-1` for a separator.
    pub command: String,
    /// The tooltip, and the label of a text icon.
    pub text: String,
    /// An icon file under `Data/toolbar_icons` (`toolbar_add.png`), or a text icon (`text`,
    /// `text_wide`).
    pub icon: Option<String>,
    /// `tbf_N`, kept as written.
    pub flags: Option<String>,
}

impl Button {
    pub fn is_separator(&self) -> bool {
        self.command == "-1"
    }

    pub fn separator() -> Self {
        Self { command: "-1".into(), ..Self::default() }
    }

    /// The label shown on the button: the icon's name without `toolbar_` and `.png`, or the
    /// text for a text icon or no icon.
    pub fn label(&self) -> String {
        match self.icon.as_deref() {
            Some(icon) if !icon.starts_with("text") => {
                icon.trim_end_matches(".png").trim_start_matches("toolbar_").replace('_', " ")
            }
            _ => self.text.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Toolbar {
    pub section: String,
    pub title: Option<String>,
    pub buttons: Vec<Button>,
}

fn indexed(key: &str, prefix: &str) -> Option<usize> {
    key.strip_prefix(prefix)?.parse().ok()
}

impl Toolbar {
    pub fn from_section(s: &Section) -> Self {
        let mut buttons: Vec<Button> = Vec::new();
        let slot = |buttons: &mut Vec<Button>, i: usize| {
            if buttons.len() <= i {
                buttons.resize(i + 1, Button::default());
            }
        };
        for (k, v) in &s.entries {
            if let Some(i) = indexed(k, "item_") {
                slot(&mut buttons, i);
                let (cmd, text) = v.split_once(' ').unwrap_or((v, ""));
                buttons[i].command = cmd.to_owned();
                buttons[i].text = text.to_owned();
            } else if let Some(i) = indexed(k, "icon_") {
                slot(&mut buttons, i);
                buttons[i].icon = Some(v.clone());
            } else if let Some(i) = indexed(k, "tbf_") {
                slot(&mut buttons, i);
                buttons[i].flags = Some(v.clone());
            }
        }
        buttons.retain(|b| !b.command.is_empty());
        Self { section: s.name.clone(), title: s.get("title").map(str::to_owned), buttons }
    }

    /// Writes the toolbar into `s`, keeping the keys it does not describe.
    pub fn write_section(&self, s: &mut Section) {
        s.entries.retain(|(k, _)| {
            indexed(k, "item_").is_none()
                && indexed(k, "icon_").is_none()
                && indexed(k, "tbf_").is_none()
                && k != "title"
        });
        for (i, b) in self.buttons.iter().enumerate() {
            if let Some(icon) = &b.icon {
                s.entries.push((format!("icon_{i}"), icon.clone()));
            }
        }
        for (i, b) in self.buttons.iter().enumerate() {
            let item = if b.text.is_empty() { b.command.clone() } else { format!("{} {}", b.command, b.text) };
            s.entries.push((format!("item_{i}"), item));
        }
        for (i, b) in self.buttons.iter().enumerate() {
            if let Some(f) = &b.flags {
                s.entries.push((format!("tbf_{i}"), f.clone()));
            }
        }
        if let Some(t) = &self.title {
            s.entries.push(("title".into(), t.clone()));
        }
    }
}

/// `reaper-menu.ini`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MenuFile {
    pub ini: Ini,
}

impl MenuFile {
    pub fn parse(text: &str) -> Self {
        Self { ini: Ini::parse(text) }
    }

    pub fn write(&self) -> String {
        self.ini.write()
    }

    /// The customised toolbar in `section`, if there is one.
    pub fn toolbar(&self, section: &str) -> Option<Toolbar> {
        self.ini.section(section).map(Toolbar::from_section)
    }

    pub fn set_toolbar(&mut self, toolbar: &Toolbar) {
        toolbar.write_section(self.ini.section_mut(&toolbar.section));
    }

    /// Back to the default toolbar.
    pub fn reset(&mut self, section: &str) -> bool {
        self.ini.remove_section(section)
    }

    /// Merges an imported `.ReaperMenu` or `.ReaperMenuSet`: each of its sections replaces
    /// the one of the same name. Answers how many came in.
    pub fn import(&mut self, other: &MenuFile) -> usize {
        for s in &other.ini.sections {
            *self.ini.section_mut(&s.name) = s.clone();
        }
        other.ini.sections.len()
    }

    /// The sections named, as a `.ReaperMenuSet` (or `.ReaperMenu` for one).
    pub fn export(&self, sections: &[&str]) -> String {
        let ini = Ini {
            sections: self
                .ini
                .sections
                .iter()
                .filter(|s| sections.iter().any(|n| n.eq_ignore_ascii_case(&s.name)))
                .cloned()
                .collect(),
        };
        ini.write()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MENU: &str = "[Main toolbar]\nicon_0=toolbar_new.png\nicon_1=text_wide\nitem_0=40023 New project\nitem_1=1068 Repeat\nitem_2=-1\ntbf_1=4\ntitle=Main toolbar\n\n[Floating toolbar 1]\nitem_0=_SWS_ABOUT About SWS\ncustomized=1\n\n";

    #[test]
    fn toolbars_round_trip() {
        let mut m = MenuFile::parse(MENU);
        assert_eq!(m.write(), MENU);
        let main = m.toolbar(MAIN_TOOLBAR).unwrap();
        assert_eq!(main.buttons.len(), 3);
        assert_eq!(main.buttons[0].command, "40023");
        assert_eq!(main.buttons[0].label(), "new");
        assert_eq!(main.buttons[1].label(), "Repeat");
        assert!(main.buttons[2].is_separator());
        assert_eq!(main.buttons[1].flags.as_deref(), Some("4"));
        m.set_toolbar(&main);
        assert_eq!(m.write(), MENU);
        let floating = m.toolbar(&floating_toolbar(1)).unwrap();
        assert_eq!(floating.buttons[0].command, "_SWS_ABOUT");
        let mut f = floating.clone();
        f.buttons.push(Button { command: "40364".into(), text: "Metronome".into(), icon: None, flags: None });
        m.set_toolbar(&f);
        assert!(
            m.write()
                .contains("[Floating toolbar 1]\ncustomized=1\nitem_0=_SWS_ABOUT About SWS\nitem_1=40364 Metronome\n")
        );
    }

    #[test]
    fn toolbar_commands() {
        assert_eq!(open_toolbar_command(1), Some(41679));
        assert_eq!(open_toolbar_command(8), Some(41686));
        assert_eq!(open_toolbar_command(9), Some(41936));
        assert_eq!(open_toolbar_command(16), Some(41943));
        assert_eq!(open_toolbar_command(17), Some(42713));
        assert_eq!(open_toolbar_command(32), Some(42728));
        assert_eq!(toolbar_for_command(41940), Some(13));
        assert_eq!(toolbar_for_command(41651), None);
    }

    #[test]
    fn import_and_export() {
        let mut m = MenuFile::parse(MENU);
        let set = m.export(&[MAIN_TOOLBAR]);
        assert!(set.starts_with("[Main toolbar]\n") && !set.contains("Floating"));
        let other =
            MenuFile::parse("[Floating toolbar 1]\nitem_0=40044 Play\n\n[Floating toolbar 2]\nitem_0=1016 Stop\n\n");
        assert_eq!(m.import(&other), 2);
        assert_eq!(m.toolbar(&floating_toolbar(1)).unwrap().buttons[0].command, "40044");
        assert!(m.toolbar(&floating_toolbar(2)).is_some());
        assert!(m.reset(&floating_toolbar(2)));
        assert!(m.toolbar(&floating_toolbar(2)).is_none());
    }
}
