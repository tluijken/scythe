//! Mouse modifiers as REAPER keeps them in `reaper-mouse.ini` (and `.ReaperMouseMap`
//! exports): a section per context changed from the defaults (`[MM_CTX_ITEM_DBLCLK]`), with
//! `mm_<modifiers>=<assignment>`. Modifiers add 1 for Shift, 2 Ctrl, 4 Alt, 8 Logo (Super).
//!
//! Measured in REAPER 7.73 (`SetMouseModifier`, then the file it wrote): an assignment is a
//! built-in behaviour (`13 m`), a command id (`40001 c`) or a named command as written
//! (`_wp12loop`). The `[hasimported]` section lists contexts as `MM_CTX_…=1`; it is kept.

use crate::ini::Ini;

pub const SHIFT: u8 = 1;
pub const CTRL: u8 = 2;
pub const ALT: u8 = 4;
pub const LOGO: u8 = 8;

/// The contexts Scythe's window has where a click or double click can run an action, with
/// REAPER's description of each.
pub const CLICK_CONTEXTS: [(&str, &str); 7] = [
    ("MM_CTX_ITEM_CLK", "Media item left click"),
    ("MM_CTX_ITEM_DBLCLK", "Media item double click"),
    ("MM_CTX_TRACK_CLK", "Track left click"),
    ("MM_CTX_TRACK_DBLCLK", "Track double click"),
    ("MM_CTX_TCP_DBLCLK", "Track control panel double click"),
    ("MM_CTX_RULER_CLK", "Ruler left click"),
    ("MM_CTX_RULER_DBLCLK", "Ruler double click"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Assignment {
    /// A built-in mouse behaviour, by REAPER's number.
    Behavior(u32),
    /// An action: a command id or named command.
    Action(String),
}

impl Assignment {
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if let Some(n) = text.strip_suffix(" m") {
            return n.trim().parse().ok().map(Self::Behavior);
        }
        let cmd = text.strip_suffix(" c").unwrap_or(text).trim();
        (!cmd.is_empty()).then(|| Self::Action(cmd.to_owned()))
    }

    /// As `GetMouseModifier` returns it and the file stores it.
    pub fn to_text(&self) -> String {
        match self {
            Self::Behavior(n) => format!("{n} m"),
            Self::Action(c) if c.parse::<u32>().is_ok() => format!("{c} c"),
            Self::Action(c) => c.clone(),
        }
    }
}

/// `reaper-mouse.ini`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MouseMap {
    pub ini: Ini,
}

impl MouseMap {
    pub fn parse(text: &str) -> Self {
        Self { ini: Ini::parse(text) }
    }

    pub fn write(&self) -> String {
        self.ini.write()
    }

    /// What `modifiers` (0-15) do in `context`, when changed from the default.
    pub fn get(&self, context: &str, modifiers: u8) -> Option<Assignment> {
        Assignment::parse(self.ini.section(context)?.get(&format!("mm_{modifiers}"))?)
    }

    /// Sets (or with `None` resets to the default) an assignment.
    pub fn set(&mut self, context: &str, modifiers: u8, assignment: Option<&Assignment>) {
        let key = format!("mm_{modifiers}");
        match assignment {
            Some(a) => self.ini.section_mut(context).set(&key, &a.to_text()),
            None => {
                if let Some(s) = self.ini.sections.iter_mut().find(|s| s.name.eq_ignore_ascii_case(context)) {
                    s.remove(&key);
                    if s.entries.is_empty() {
                        self.ini.remove_section(context);
                    }
                }
            }
        }
    }

    /// Resets a whole context to the defaults.
    pub fn reset_context(&mut self, context: &str) -> bool {
        self.ini.remove_section(context)
    }

    /// The action a click in `context` with `modifiers` runs, if one is assigned.
    pub fn action(&self, context: &str, modifiers: u8) -> Option<String> {
        match self.get(context, modifiers)? {
            Assignment::Action(c) => Some(c),
            Assignment::Behavior(_) => None,
        }
    }
}

/// The modifier number for these keys.
pub fn modifiers(shift: bool, ctrl: bool, alt: bool, logo: bool) -> u8 {
    shift as u8 * SHIFT + ctrl as u8 * CTRL + alt as u8 * ALT + logo as u8 * LOGO
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAPER: &str = include_str!("../testdata/reaper-config/reaper-mouse.ini");

    #[test]
    fn reaper_written_file_round_trips() {
        let mut m = MouseMap::parse(REAPER);
        assert_eq!(m.write(), REAPER);
        assert_eq!(m.get("MM_CTX_ITEM", 1), Some(Assignment::Behavior(13)));
        assert_eq!(m.get("MM_CTX_ITEM_DBLCLK", 0), Some(Assignment::Action("40001".into())));
        assert_eq!(m.get("MM_CTX_TRACK_CLK", 2), Some(Assignment::Action("_wp12loop".into())));
        assert_eq!(m.action("MM_CTX_TRACK_CLK", CTRL).as_deref(), Some("_wp12loop"));
        assert_eq!(m.get("MM_CTX_ITEM", 0), None);
        let a = m.get("MM_CTX_ITEM_DBLCLK", 0).unwrap();
        m.set("MM_CTX_ITEM_DBLCLK", 0, Some(&a));
        assert_eq!(m.write(), REAPER);
    }

    #[test]
    fn setting_and_resetting() {
        let mut m = MouseMap::parse(REAPER);
        m.set("MM_CTX_RULER_DBLCLK", modifiers(false, false, true, false), Some(&Assignment::Action("1068".into())));
        assert!(m.write().ends_with("[MM_CTX_RULER_DBLCLK]\nmm_4=1068 c\n\n"));
        m.set("MM_CTX_RULER_DBLCLK", 4, None);
        assert_eq!(m.write(), REAPER);
        assert!(m.reset_context("MM_CTX_ITEM"));
        assert_eq!(m.get("MM_CTX_ITEM", 1), None);
    }
}
