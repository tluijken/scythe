//! Keyboard shortcuts as REAPER stores them: egui's keys in `reaper-kb.ini`'s encoding, and
//! which command a key runs (the shortcut file's binding, else Scythe's default).

use egui::{Key, Modifiers};
use scythe_core::keymap::{Chord, EXTENDED, FLAG_ALT, FLAG_CTRL, FLAG_SHIFT, FLAG_VIRTKEY, SECTION_MAIN};

use crate::app::ScytheApp;
use crate::palette::Action;

/// Whether  is a modifier or lock key, which egui reports as a key press of its own
/// before the key it modifies.
pub fn is_modifier(key: Key) -> bool {
    matches!(
        key,
        Key::ShiftLeft
            | Key::ShiftRight
            | Key::ControlLeft
            | Key::ControlRight
            | Key::AltLeft
            | Key::AltRight
            | Key::SuperLeft
            | Key::SuperRight
            | Key::CapsLock
            | Key::NumLock
            | Key::ScrollLock
    )
}

/// A key as REAPER's `KEY` line has it: virtual-key codes for letters, digits, function and
/// navigation keys, character codes for punctuation.
pub fn chord(key: Key, mods: Modifiers) -> Option<Chord> {
    let vk = match key {
        Key::Space => Some(32),
        Key::Enter => Some(13),
        Key::Escape => Some(27),
        Key::Tab => Some(9),
        Key::Backspace => Some(8),
        Key::Delete => Some(46 | EXTENDED),
        Key::Insert => Some(45 | EXTENDED),
        Key::Home => Some(36 | EXTENDED),
        Key::End => Some(35 | EXTENDED),
        Key::PageUp => Some(33 | EXTENDED),
        Key::PageDown => Some(34 | EXTENDED),
        Key::ArrowLeft => Some(37 | EXTENDED),
        Key::ArrowUp => Some(38 | EXTENDED),
        Key::ArrowRight => Some(39 | EXTENDED),
        Key::ArrowDown => Some(40 | EXTENDED),
        _ => None,
    };
    let name = key.name();
    let vk = vk.or_else(|| {
        let mut chars = name.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) if c.is_ascii_alphanumeric() => Some(c.to_ascii_uppercase() as u32),
            _ => name
                .strip_prefix('F')
                .and_then(|n| n.parse::<u32>().ok())
                .filter(|n| (1..=24).contains(n))
                .map(|n| 111 + n),
        }
    });
    let mut flags = 0;
    if mods.shift {
        flags |= FLAG_SHIFT;
    }
    if mods.ctrl || mods.command {
        flags |= FLAG_CTRL;
    }
    if mods.alt {
        flags |= FLAG_ALT;
    }
    if let Some(vk) = vk {
        return Some(Chord::new(flags | FLAG_VIRTKEY, vk).canonical());
    }
    let c = match key {
        Key::Minus => '-',
        Key::Equals => '=',
        Key::Plus => '+',
        Key::Comma => ',',
        Key::Period => '.',
        Key::Semicolon => ';',
        Key::Colon => ':',
        Key::Slash => '/',
        Key::Backslash => '\\',
        Key::OpenBracket => '[',
        Key::CloseBracket => ']',
        Key::Backtick => '`',
        Key::Quote => '\'',
        Key::Questionmark => '?',
        Key::Exclamationmark => '!',
        Key::Pipe => '|',
        Key::OpenCurlyBracket => '{',
        Key::CloseCurlyBracket => '}',
        _ => return None,
    };
    Some(Chord::new(flags, c as u32).canonical())
}

/// The key of an action's default shortcut.
pub fn default_chord(action: Action) -> Option<Chord> {
    action.shortcut().and_then(|s| chord(s.logical_key, s.modifiers))
}

/// Scythe's default shortcuts: command, key, action.
static DEFAULTS: std::sync::LazyLock<Vec<(String, Chord, Action)>> = std::sync::LazyLock::new(|| {
    Action::all().into_iter().filter_map(|a| Some((a.command(), default_chord(a)?, a))).collect()
});

/// What a key runs in the main window: the shortcut file's binding (`None` when it disables
/// the key), else the default action's command.
pub fn command_for(app: &ScytheApp, key: Chord) -> Option<String> {
    if app.view.midi_editor.is_some()
        && app.view.midi_state.hovered
        && app.session.key_command(key, scythe_core::keymap::SECTION_MIDI_EDITOR).is_some()
    {
        return None;
    }
    if let Some(binding) = app.session.key_command(key, SECTION_MAIN) {
        return (binding.command != "0").then(|| binding.command.clone());
    }
    let on_setlist = app.view.setlist;
    DEFAULTS.iter().find(|(_, c, a)| *c == key && a.shortcut_active(on_setlist)).map(|(cmd, ..)| cmd.clone())
}

/// The keys that run `command`: those the shortcut file binds, and its default unless the file
/// rebinds or disables that key.
pub fn keys_for(session: &scythe_session::Session, command: &str) -> Vec<Chord> {
    let mut out = session.shortcuts(command, SECTION_MAIN);
    let c = scythe_session::actions::normalize(command);
    let def = Action::from_command(&c).and_then(|a| DEFAULTS.iter().find(|(_, _, d)| *d == a)).map(|(_, k, _)| *k);
    if let Some(def) = def
        && session.key_command(def, SECTION_MAIN).is_none()
        && !out.contains(&def)
    {
        out.insert(0, def);
    }
    out
}

/// The keys that run `command`, as REAPER writes them ("Ctrl+Shift+S, F5").
pub fn keys_text(session: &scythe_session::Session, command: &str) -> String {
    keys_for(session, command).iter().map(Chord::describe).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_presses_are_not_keys_of_their_own() {
        assert!(is_modifier(Key::ControlLeft) && is_modifier(Key::AltRight) && is_modifier(Key::ShiftLeft));
        assert!(!is_modifier(Key::M) && !is_modifier(Key::F7));
    }

    #[test]
    fn keys_encode_as_reaper_stores_them() {
        let c = |k, m| chord(k, m).unwrap();
        assert_eq!(c(Key::A, Modifiers::NONE), Chord::new(1, 65));
        assert_eq!(c(Key::S, Modifiers::COMMAND | Modifiers::SHIFT), Chord::new(13, 83));
        assert_eq!(c(Key::Delete, Modifiers::NONE), Chord::new(1, 32814));
        assert_eq!(c(Key::ArrowDown, Modifiers::SHIFT), Chord::new(5, 32808));
        assert_eq!(c(Key::F5, Modifiers::ALT), Chord::new(17, 116));
        assert_eq!(c(Key::OpenBracket, Modifiers::NONE), Chord::new(0, 91));
        assert_eq!(c(Key::Num0, Modifiers::COMMAND), Chord::new(9, 48));
        assert_eq!(c(Key::Space, Modifiers::NONE).describe(), "Space");
        assert_eq!(c(Key::Z, Modifiers::COMMAND | Modifiers::SHIFT).describe(), "Ctrl+Shift+Z");
    }

    #[test]
    fn every_default_shortcut_encodes() {
        for a in Action::all() {
            if a.shortcut().is_some() {
                assert!(default_chord(a).is_some(), "{a:?}");
            }
        }
    }
}
