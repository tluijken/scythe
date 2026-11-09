//! REAPER's shortcut and custom action file, `reaper-kb.ini` (and `.ReaperKeyMap` exports,
//! which use the same lines):
//!
//! ```text
//! KEY <flags> <key> <command> <section>      # comment REAPER adds
//! ACT <flags> <section> "<id>" "<name>" <command> <command> ...
//! SCR <flags> <section> <id> "<name>" <path>
//! ```
//!
//! Measured in REAPER 7.73: `KEY` flags add 1 for a virtual-key code (else `<key>` is a
//! character code), 4 Shift, 8 Ctrl, 16 Alt, 32 Logo (Super); a status byte of 0x80 or more is
//! a MIDI message and 255 a mousewheel or other special input. Navigation keys carry 0x8000 on
//! their virtual-key code (Delete is 32814); without it they are the numeric keypad's. REAPER
//! rewrites unshifted punctuation keys as character codes, sorts `KEY` lines by command (named
//! commands after REAPER's own, in the order they are defined) and section, and appends
//! `\t\t # <section> : <shortcut> : <action>` to each. Command `0` disables a default shortcut.
//! `ACT` flag 1 folds the custom action into one undo point; a custom action's toggle state is
//! its members' when they all have the same one, else -1. Named commands get command ids from
//! 53000 in the order they are defined (custom actions, then scripts).

use crate::rpp::{quote, quote_always, tokenize};

pub const SECTION_MAIN: u32 = 0;
pub const SECTION_MAIN_ALT: u32 = 100;
pub const SECTION_MIDI_EDITOR: u32 = 32060;
pub const SECTION_MIDI_EVENT_LIST: u32 = 32061;
pub const SECTION_MIDI_INLINE: u32 = 32062;
pub const SECTION_MEDIA_EXPLORER: u32 = 32063;

pub const FLAG_VIRTKEY: u32 = 1;
pub const FLAG_SHIFT: u32 = 4;
pub const FLAG_CTRL: u32 = 8;
pub const FLAG_ALT: u32 = 16;
pub const FLAG_LOGO: u32 = 32;
/// Mousewheel and other special input.
pub const FLAG_SPECIAL: u32 = 255;
/// On a virtual-key code: the navigation block's key rather than the keypad's.
pub const EXTENDED: u32 = 0x8000;
/// The first command id REAPER gives a named command.
pub const FIRST_NAMED_ID: u32 = 53000;
/// `ACT` flag: one undo point for the whole custom action.
pub const ACT_CONSOLIDATE_UNDO: u32 = 1;

/// The name REAPER gives a section in its comments and action list.
pub fn section_name(section: u32) -> &'static str {
    match section {
        SECTION_MAIN => "Main",
        SECTION_MAIN_ALT => "Main (alt recording)",
        SECTION_MIDI_EDITOR => "MIDI Editor",
        SECTION_MIDI_EVENT_LIST => "MIDI Event List Editor",
        SECTION_MIDI_INLINE => "MIDI Inline Editor",
        SECTION_MEDIA_EXPLORER => "Media Explorer",
        _ => "Unknown",
    }
}

/// A key with its modifiers (or a MIDI message, or special input) as a `KEY` line has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    pub flags: u32,
    pub key: u32,
}

/// Unshifted punctuation keys: their virtual-key code and the character REAPER writes instead.
const OEM_KEYS: [(u32, u32); 11] = [
    (186, b';' as u32),
    (187, b'=' as u32),
    (188, b',' as u32),
    (189, b'-' as u32),
    (190, b'.' as u32),
    (191, b'/' as u32),
    (192, b'`' as u32),
    (219, b'[' as u32),
    (220, b'\\' as u32),
    (221, b']' as u32),
    (222, b'\'' as u32),
];

impl Chord {
    pub fn new(flags: u32, key: u32) -> Self {
        Self { flags, key }
    }

    /// A key on the keyboard (not MIDI, not the mousewheel).
    pub fn is_keyboard(&self) -> bool {
        self.flags < 0x80
    }

    pub fn is_midi(&self) -> bool {
        (0x80..0xf0).contains(&self.flags)
    }

    /// The same key in one spelling, so two lines REAPER treats as the same shortcut compare
    /// equal: letters and digits as virtual keys, punctuation as characters, upper-case
    /// characters as Shift plus the letter. Flag 2, which REAPER ignores, is dropped.
    pub fn canonical(self) -> Self {
        if !self.is_keyboard() {
            return self;
        }
        let mods = self.flags & (FLAG_SHIFT | FLAG_CTRL | FLAG_ALT | FLAG_LOGO);
        if self.flags & FLAG_VIRTKEY != 0 {
            if let Some(&(_, c)) = OEM_KEYS.iter().find(|(vk, _)| *vk == self.key) {
                return Self::new(mods, c);
            }
            return Self::new(mods | FLAG_VIRTKEY, self.key);
        }
        match char::from_u32(self.key) {
            Some(c) if c.is_ascii_lowercase() => Self::new(mods | FLAG_VIRTKEY, c.to_ascii_uppercase() as u32),
            Some(c) if c.is_ascii_uppercase() => Self::new(mods | FLAG_VIRTKEY | FLAG_SHIFT, c as u32),
            Some(c) if c.is_ascii_digit() => Self::new(mods | FLAG_VIRTKEY, c as u32),
            _ => Self::new(mods, self.key),
        }
    }

    /// Reads a keyboard shortcut written as [`Chord::describe`] writes it ("Ctrl+Shift+A",
    /// "NumPad +", "F5"), as REAPER would store it.
    pub fn parse(text: &str) -> Option<Self> {
        let mut rest = text.trim();
        let mut flags = 0;
        loop {
            let before = rest;
            for (flag, name) in [(FLAG_CTRL, "Ctrl+"), (FLAG_ALT, "Alt+"), (FLAG_SHIFT, "Shift+"), (FLAG_LOGO, "Logo+")]
            {
                if rest.len() > name.len() && rest[..name.len()].eq_ignore_ascii_case(name) {
                    flags |= flag;
                    rest = &rest[name.len()..];
                }
            }
            if rest == before {
                break;
            }
        }
        let vk = (1..256).chain((1..256).map(|k| k | EXTENDED)).find(|&k| vk_name(k).eq_ignore_ascii_case(rest));
        if let Some(k) = vk {
            return Some(Self::new(flags | FLAG_VIRTKEY, k).canonical());
        }
        let mut chars = rest.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => Some(Self::new(flags, c as u32).canonical()),
            _ => None,
        }
    }

    /// The shortcut as REAPER's action list shows it: "Ctrl+Alt+Shift+Logo+C", "NumPad 0",
    /// "MIDI Chan 1 CC 7".
    pub fn describe(&self) -> String {
        if self.is_midi() {
            let ch = (self.flags & 0x0f) + 1;
            return match self.flags & 0xf0 {
                0x90 => format!("MIDI Chan {ch} Note {}", self.key),
                0xb0 => format!("MIDI Chan {ch} CC {}", self.key),
                0xc0 => format!("MIDI Chan {ch} PC {}", self.key),
                0xe0 => format!("MIDI Chan {ch} Pitch"),
                _ => format!("MIDI {:02X} {}", self.flags, self.key),
            };
        }
        if self.flags == FLAG_SPECIAL {
            return match self.key {
                248 => "Mousewheel".to_owned(),
                k => format!("Special {k}"),
            };
        }
        let mut out = String::new();
        for (flag, name) in [(FLAG_CTRL, "Ctrl+"), (FLAG_ALT, "Alt+"), (FLAG_SHIFT, "Shift+"), (FLAG_LOGO, "Logo+")] {
            if self.flags & flag != 0 {
                out.push_str(name);
            }
        }
        if self.flags & FLAG_VIRTKEY != 0 {
            out.push_str(&vk_name(self.key));
        } else {
            out.extend(char::from_u32(self.key));
        }
        out
    }
}

fn vk_name(key: u32) -> String {
    let extended = key & EXTENDED != 0;
    let vk = key & !EXTENDED;
    let nav = |name: &str| if extended { name.to_owned() } else { format!("NumPad {name}") };
    match vk {
        8 => "Backspace".into(),
        9 => "Tab".into(),
        13 => {
            if extended {
                "NumPad Enter".into()
            } else {
                "Enter".into()
            }
        }
        20 => "CapsLock".into(),
        27 => "ESC".into(),
        32 => "Space".into(),
        33 => nav("Page Up"),
        34 => nav("Page Down"),
        35 => nav("End"),
        36 => nav("Home"),
        37 => nav("Left"),
        38 => nav("Up"),
        39 => nav("Right"),
        40 => nav("Down"),
        45 => nav("Insert"),
        46 => nav("Delete"),
        48..=57 | 65..=90 => char::from_u32(vk).map(String::from).unwrap_or_default(),
        96..=105 => format!("NumPad {}", vk - 96),
        106 => "NumPad *".into(),
        107 => "NumPad +".into(),
        109 => "NumPad -".into(),
        110 => "NumPad .".into(),
        111 => "NumPad /".into(),
        112..=135 => format!("F{}", vk - 111),
        _ => match OEM_KEYS.iter().find(|(v, _)| *v == vk) {
            Some(&(_, c)) => char::from_u32(c).map(String::from).unwrap_or_default(),
            None => format!("Key {vk}"),
        },
    }
}

/// A shortcut: `KEY` line.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyBinding {
    pub chord: Chord,
    /// A command id (`40044`), a named command (`_SWS_ABOUT`, `_RS…`), or `0` to disable the
    /// default shortcut.
    pub command: String,
    pub section: u32,
    /// What follows `#`, as written.
    pub comment: Option<String>,
}

/// A custom action (macro): `ACT` line.
#[derive(Debug, Clone, PartialEq)]
pub struct CustomAction {
    pub flags: u32,
    pub section: u32,
    /// Without the leading `_` scripts and shortcuts use.
    pub id: String,
    pub name: String,
    pub commands: Vec<String>,
}

impl CustomAction {
    pub fn consolidate_undo(&self) -> bool {
        self.flags & ACT_CONSOLIDATE_UNDO != 0
    }

    pub fn named_command(&self) -> String {
        format!("_{}", self.id)
    }
}

/// A script registered as an action: `SCR` line.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptAction {
    pub flags: u32,
    pub section: u32,
    pub id: String,
    pub name: String,
    /// Relative to the resource path's `Scripts` directory, or absolute.
    pub path: String,
}

impl ScriptAction {
    pub fn named_command(&self) -> String {
        format!("_{}", self.id)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Line {
    Key(KeyBinding),
    Act(CustomAction),
    Scr(ScriptAction),
    /// Anything else, kept as it was.
    Other(String),
}

/// A whole `reaper-kb.ini`, in its order.
#[derive(Debug, Clone, PartialEq)]
pub struct Keymap {
    pub lines: Vec<Line>,
    /// `\r\n` as REAPER writes, or `\n` when the file read had that.
    pub crlf: bool,
}

impl Default for Keymap {
    fn default() -> Self {
        Self { lines: Vec::new(), crlf: true }
    }
}

fn parse_line(line: &str) -> Line {
    let other = || Line::Other(line.to_owned());
    let (body, comment) = match line.find('#') {
        Some(i) if line.starts_with("KEY ") => {
            let c = &line[i + 1..];
            (&line[..i], Some(c.strip_prefix(' ').unwrap_or(c).to_owned()))
        }
        _ => (line, None),
    };
    let t = tokenize(body);
    let num = |i: usize| t.get(i).and_then(|s| s.parse::<u32>().ok());
    match t.first().map(String::as_str) {
        Some("KEY") if t.len() >= 5 => match (num(1), num(2), num(4)) {
            (Some(flags), Some(key), Some(section)) => Line::Key(KeyBinding {
                chord: Chord::new(flags, key),
                command: t[3].clone(),
                section,
                comment: comment.filter(|c| !c.is_empty()),
            }),
            _ => other(),
        },
        Some("ACT") if t.len() >= 5 => match (num(1), num(2)) {
            (Some(flags), Some(section)) => Line::Act(CustomAction {
                flags,
                section,
                id: t[3].clone(),
                name: t[4].clone(),
                commands: t[5..].to_vec(),
            }),
            _ => other(),
        },
        Some("SCR") if t.len() >= 6 => match (num(1), num(2)) {
            (Some(flags), Some(section)) => {
                Line::Scr(ScriptAction { flags, section, id: t[3].clone(), name: t[4].clone(), path: t[5..].join(" ") })
            }
            _ => other(),
        },
        _ => other(),
    }
}

impl Line {
    pub fn to_text(&self) -> String {
        match self {
            Line::Key(k) => {
                let mut s = format!("KEY {} {} {} {}", k.chord.flags, k.chord.key, quote(&k.command), k.section);
                if let Some(c) = &k.comment {
                    s.push_str("\t\t # ");
                    s.push_str(c);
                }
                s
            }
            Line::Act(a) => {
                let mut s = format!("ACT {} {} {} {}", a.flags, a.section, quote_always(&a.id), quote_always(&a.name));
                for c in &a.commands {
                    s.push(' ');
                    s.push_str(&quote(c));
                }
                s
            }
            Line::Scr(sc) => {
                format!(
                    "SCR {} {} {} {} {}",
                    sc.flags,
                    sc.section,
                    quote(&sc.id),
                    quote_always(&sc.name),
                    quote(&sc.path)
                )
            }
            Line::Other(l) => l.clone(),
        }
    }
}

impl Keymap {
    pub fn parse(text: &str) -> Self {
        let crlf = text.contains("\r\n") || text.trim().is_empty();
        let lines =
            text.lines().filter(|l| !l.trim().is_empty()).map(|l| parse_line(l.trim_end_matches('\r'))).collect();
        Self { lines, crlf }
    }

    pub fn write(&self) -> String {
        let eol = if self.crlf { "\r\n" } else { "\n" };
        self.lines.iter().map(|l| format!("{}{eol}", l.to_text())).collect()
    }

    pub fn keys(&self) -> impl Iterator<Item = &KeyBinding> {
        self.lines.iter().filter_map(|l| if let Line::Key(k) = l { Some(k) } else { None })
    }

    pub fn custom_actions(&self) -> impl Iterator<Item = &CustomAction> {
        self.lines.iter().filter_map(|l| if let Line::Act(a) = l { Some(a) } else { None })
    }

    pub fn scripts(&self) -> impl Iterator<Item = &ScriptAction> {
        self.lines.iter().filter_map(|l| if let Line::Scr(s) = l { Some(s) } else { None })
    }

    /// The custom action or script with this named command (`_id` or `id`), in `section`.
    pub fn named(&self, command: &str, section: u32) -> Option<&Line> {
        let id = command.strip_prefix('_').unwrap_or(command);
        self.lines.iter().find(|l| match l {
            Line::Act(a) => a.section == section && a.id == id,
            Line::Scr(s) => s.section == section && s.id == id,
            _ => false,
        })
    }

    /// Every named command defined here, in definition order: what REAPER numbers from
    /// [`FIRST_NAMED_ID`] (custom actions first, then scripts).
    pub fn named_commands(&self) -> Vec<(u32, String)> {
        let acts = self.custom_actions().map(|a| (a.section, a.named_command()));
        let scripts = self.scripts().map(|s| (s.section, s.named_command()));
        acts.chain(scripts).collect()
    }

    /// The shortcuts of `section` that match `chord`.
    pub fn bound(&self, chord: Chord, section: u32) -> impl Iterator<Item = &KeyBinding> {
        let c = chord.canonical();
        self.keys().filter(move |k| k.section == section && k.chord.canonical() == c)
    }

    /// Where REAPER sorts a `KEY` line: by section, then command (named commands after
    /// REAPER's own, in definition order).
    fn sort_key(&self, k: &KeyBinding) -> (u32, u64) {
        let order = match k.command.parse::<u32>() {
            Ok(id) => id as u64,
            Err(_) => {
                let named = self.named_commands();
                let id = k.command.strip_prefix('_').unwrap_or(&k.command);
                let at = named.iter().position(|(_, c)| c.trim_start_matches('_') == id).unwrap_or(named.len());
                (1u64 << 32) + at as u64
            }
        };
        (k.section, order)
    }

    /// Adds a `KEY` line where REAPER would sort it, after the lines it sorts with.
    pub fn insert_key(&mut self, binding: KeyBinding) {
        let key = self.sort_key(&binding);
        let at = self
            .lines
            .iter()
            .rposition(|l| matches!(l, Line::Key(k) if self.sort_key(k) <= key))
            .map(|i| i + 1)
            .or_else(|| self.lines.iter().position(|l| matches!(l, Line::Key(k) if self.sort_key(k) > key)))
            .unwrap_or(self.lines.len());
        self.lines.insert(at, Line::Key(binding));
    }

    /// Binds `chord` in `section` to `command`, replacing what it ran there.
    pub fn bind(&mut self, chord: Chord, section: u32, command: &str, comment: Option<String>) {
        self.unbind(chord, section);
        self.insert_key(KeyBinding { chord, command: command.to_owned(), section, comment });
    }

    /// Removes the lines binding `chord` in `section`; answers how many went.
    pub fn unbind(&mut self, chord: Chord, section: u32) -> usize {
        let c = chord.canonical();
        let before = self.lines.len();
        self.lines.retain(|l| !matches!(l, Line::Key(k) if k.section == section && k.chord.canonical() == c));
        before - self.lines.len()
    }

    /// Adds or replaces (same id and section) a custom action or script, before the `KEY` lines
    /// as REAPER keeps them.
    pub fn define(&mut self, line: Line) {
        let same = |l: &Line| match (l, &line) {
            (Line::Act(a), Line::Act(b)) => a.id == b.id && a.section == b.section,
            (Line::Scr(a), Line::Scr(b)) => a.id == b.id && a.section == b.section,
            _ => false,
        };
        if let Some(i) = self.lines.iter().position(same) {
            self.lines[i] = line;
            return;
        }
        let at = match line {
            Line::Act(_) => self.lines.iter().position(|l| !matches!(l, Line::Act(_))),
            _ => self.lines.iter().position(|l| matches!(l, Line::Key(_) | Line::Other(_))),
        }
        .unwrap_or(self.lines.len());
        self.lines.insert(at, line);
    }

    /// Removes the custom action or script `command` (`_id`) of `section` and its shortcuts.
    pub fn remove_named(&mut self, command: &str, section: u32) -> bool {
        let id = command.strip_prefix('_').unwrap_or(command).to_owned();
        let before = self.lines.len();
        self.lines.retain(|l| match l {
            Line::Act(a) => !(a.section == section && a.id == id),
            Line::Scr(s) => !(s.section == section && s.id == id),
            Line::Key(k) => !(k.section == section && k.command.trim_start_matches('_') == id),
            Line::Other(_) => true,
        });
        self.lines.len() != before
    }

    /// Merges `other` (an imported `.ReaperKeyMap`): its definitions replace those with the
    /// same id, its shortcuts replace what those keys ran.
    pub fn merge(&mut self, other: &Keymap) {
        for line in &other.lines {
            match line {
                Line::Act(_) | Line::Scr(_) => self.define(line.clone()),
                Line::Key(k) => {
                    self.unbind(k.chord, k.section);
                    self.insert_key(k.clone());
                }
                Line::Other(_) => {}
            }
        }
    }
}

/// A new custom action id in REAPER's style: 32 hex digits.
pub fn new_custom_id(seed: &str) -> String {
    use std::hash::{BuildHasher, Hasher};
    let state = std::collections::hash_map::RandomState::new();
    let mut a = state.build_hasher();
    a.write(seed.as_bytes());
    let mut b = state.build_hasher();
    b.write_u64(a.finish());
    b.write(seed.as_bytes());
    format!("{:016x}{:016x}", a.finish(), b.finish())
}

/// A script's id: `RS` and 40 hex digits derived from its path, as REAPER's look (REAPER's own
/// derivation is not reproduced; the file carries the id, so either works).
pub fn script_id(path: &str, section: u32) -> String {
    let mut h: [u64; 3] = [0xcbf29ce484222325, 0x100000001b3, 0x9e3779b97f4a7c15];
    for b in path.bytes() {
        for (i, x) in h.iter_mut().enumerate() {
            *x = (*x ^ b as u64).wrapping_mul(0x100000001b3 + i as u64 * 2);
        }
    }
    let hex = format!("{:016x}{:016x}{:08x}", h[0], h[1], h[2] as u32);
    if section == SECTION_MAIN { format!("RS{hex}") } else { format!("RS{:04x}_{hex}", section & 0xffff) }
}

/// Combines member toggle states as REAPER does for a custom action: the shared state when all
/// members have the same one, else -1.
pub fn combined_toggle(states: impl IntoIterator<Item = i32>) -> i32 {
    let mut out = None;
    for s in states {
        if s < 0 || out.is_some_and(|o| o != s) {
            return -1;
        }
        out = Some(s);
    }
    out.unwrap_or(-1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTIONS: &str = include_str!("../testdata/reaper-config/reaper-kb-actions.ini");
    const KEYS: &str = include_str!("../testdata/reaper-config/reaper-kb-keys.ini");

    #[test]
    fn reaper_written_files_round_trip() {
        for text in [ACTIONS, KEYS] {
            let km = Keymap::parse(text);
            assert!(km.crlf);
            assert!(km.lines.iter().all(|l| !matches!(l, Line::Other(_))), "{:?}", km.lines);
            assert_eq!(km.write(), text);
        }
    }

    #[test]
    fn custom_actions_and_scripts_parse() {
        let km = Keymap::parse(ACTIONS);
        let acts: Vec<_> = km.custom_actions().collect();
        assert_eq!(acts.len(), 5);
        assert!(acts[0].consolidate_undo());
        assert_eq!(acts[0].commands, ["40001", "40001"]);
        assert_eq!(acts[3].commands, ["_wp12loopmetro"]);
        assert_eq!(acts[4].section, SECTION_MIDI_EDITOR);
        let scr: Vec<_> = km.scripts().collect();
        assert_eq!(scr[1].id, "RS7d3c_1ee9bb229dabffe151848d7efa3c10f748e1a1cf");
        assert_eq!(scr[0].path, "Cockos/lyrics.lua");
        // REAPER gave _a1b2… 53000, _wp12loopmetro 53001, _wp12nested 53003 (53004 counting
        // the MIDI editor's), the lyrics script 53005 after them.
        let named = km.named_commands();
        assert_eq!(named[1].1, "_wp12loopmetro");
        assert_eq!(named[5].1, "_RS1ee9bb229dabffe151848d7efa3c10f748e1a1cf");
    }

    #[test]
    fn shortcut_descriptions_match_reaper() {
        let km = Keymap::parse(&format!("{KEYS}{ACTIONS}"));
        for k in km.keys() {
            let Some(comment) = &k.comment else { continue };
            let shown = comment.split(" : ").nth(1).unwrap();
            assert_eq!(k.chord.describe(), shown, "{k:?}");
        }
        let probe = [
            (1, 65, "A"),
            (5, 65, "Shift+A"),
            (9, 65, "Ctrl+A"),
            (17, 65, "Alt+A"),
            (33, 65, "Logo+A"),
            (29, 66, "Ctrl+Alt+Shift+B"),
            (1, 112, "F1"),
            (1, 32, "Space"),
            (1, 46, "NumPad Delete"),
            (1, 37, "NumPad Left"),
            (1, 96, "NumPad 0"),
            (1, 13, "Enter"),
            (9, 13, "Ctrl+Enter"),
            (1, 33, "NumPad Page Up"),
            (1, 106, "NumPad *"),
            (1, 109, "NumPad -"),
            (1, 9, "Tab"),
            (1, 27, "ESC"),
            (1, 8, "Backspace"),
            (1, 45, "NumPad Insert"),
            (255, 248, "Mousewheel"),
            (1, 32814, "Delete"),
            (3, 46, "NumPad Delete"),
            (1, 32805, "Left"),
            (1, 32781, "NumPad Enter"),
            (5, 32808, "Shift+Down"),
            (1, 107, "NumPad +"),
            (1, 110, "NumPad ."),
            (1, 123, "F12"),
            (0, 97, "a"),
            (4, 97, "Shift+a"),
            (1, 20, "CapsLock"),
            (1, 186, ";"),
            (176, 7, "MIDI Chan 1 CC 7"),
        ];
        for (flags, key, want) in probe {
            assert_eq!(Chord::new(flags, key).describe(), want);
        }
    }

    #[test]
    fn descriptions_read_back() {
        for (flags, key) in
            [(1, 65), (29, 66), (61, 67), (1, 32814), (1, 46), (1, 107), (1, 112), (0, 59), (9, 32808), (1, 32)]
        {
            let c = Chord::new(flags, key);
            assert_eq!(Chord::parse(&c.describe()), Some(c.canonical()), "{}", c.describe());
        }
        assert_eq!(Chord::parse("ctrl+shift+a"), Some(Chord::new(13, 65)));
        assert_eq!(Chord::parse("Ctrl+"), None);
    }

    #[test]
    fn equivalent_spellings_are_one_shortcut() {
        assert_eq!(Chord::new(1, 186).canonical(), Chord::new(0, 59).canonical());
        assert_eq!(Chord::new(1, 48).canonical(), Chord::new(0, 48).canonical());
        assert_eq!(Chord::new(0, 97).canonical(), Chord::new(1, 65).canonical());
        assert_eq!(Chord::new(0, 65).canonical(), Chord::new(5, 65).canonical());
        assert_eq!(Chord::new(3, 46).canonical(), Chord::new(1, 46).canonical());
        assert_ne!(Chord::new(1, 46).canonical(), Chord::new(1, 32814).canonical());
    }

    #[test]
    fn binding_sorts_where_reaper_puts_it() {
        let mut km = Keymap::parse(ACTIONS);
        km.bind(Chord::new(1, 68), 0, "40030", None);
        km.bind(Chord::new(1, 69), 0, "_wp12trackloop", None);
        let keys: Vec<_> = km.keys().map(|k| k.command.as_str()).collect();
        assert_eq!(
            keys,
            [
                "40020",
                "0",
                "40021",
                "40030",
                "40364",
                "_wp12loopmetro",
                "_wp12trackloop",
                "_wp12nested",
                "_RS1ee9bb229dabffe151848d7efa3c10f748e1a1cf"
            ]
        );
        assert_eq!(km.unbind(Chord::new(0, 97), 0), 1);
        assert_eq!(km.unbind(Chord::new(1, 65), 0), 0);
    }

    #[test]
    fn import_replaces_and_adds() {
        let mut km = Keymap::parse(ACTIONS);
        let other = Keymap::parse(
            "ACT 0 0 \"wp12loopmetro\" \"Custom: renamed\" 1068\nKEY 1 66 40044 0\nACT 1 0 \"newone\" \"Custom: new\" 40001\n",
        );
        km.merge(&other);
        assert_eq!(km.custom_actions().count(), 6);
        assert_eq!(km.custom_actions().nth(1).unwrap().name, "Custom: renamed");
        assert_eq!(km.bound(Chord::new(1, 66), 0).map(|k| k.command.as_str()).collect::<Vec<_>>(), ["40044"]);
        assert!(km.remove_named("_newone", 0));
        assert_eq!(km.custom_actions().count(), 5);
    }

    #[test]
    fn toggle_of_a_custom_action() {
        assert_eq!(combined_toggle([0, 0]), 0);
        assert_eq!(combined_toggle([1, 1]), 1);
        assert_eq!(combined_toggle([1, 0]), -1);
        assert_eq!(combined_toggle([-1, 1]), -1);
        assert_eq!(combined_toggle([]), -1);
    }

    #[test]
    fn new_ids_look_like_reapers() {
        let id = new_custom_id("x");
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        let rs = script_id("/a/b.lua", 0);
        assert!(rs.starts_with("RS") && rs.len() == 42);
        assert_eq!(script_id("/a/b.lua", 32060)[..7], *"RS7d3c_");
        assert_eq!(script_id("/a/b.lua", 32060)[7..], rs[2..]);
    }
}
