//! The action list, keyboard shortcuts, custom actions, scripts as actions, toolbars and
//! mouse modifiers, as REAPER keeps them (`reaper-kb.ini`, `reaper-menu.ini`,
//! `reaper-mouse.ini`).

use schemars::JsonSchema;
use scythe_core::keymap::{Chord, SECTION_MAIN};
use scythe_core::menus::{self, Button, MAIN_TOOLBAR, Toolbar};
use scythe_core::mousemap::Assignment;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json as value};

use super::{ApiError, json};
use crate::Session;
use crate::actions;

/// A toolbar button.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ButtonSpec {
    /// The action it runs (a command id or named command); `-1` for a separator.
    pub action: String,
    /// Tooltip, and the label of a text button; left empty, the label the main toolbar gives the
    /// action, else its name without the category.
    #[serde(default)]
    pub text: String,
    /// An icon file from REAPER's `Data/toolbar_icons` (`toolbar_add.png`), or `text` for a
    /// text button.
    #[serde(default)]
    pub icon: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum CustomizeCommand {
    /// Every action with its command, name, keyboard shortcuts and toggle state (1 on, 0 off,
    /// -1 none). `query` keeps those whose name has all its words.
    #[serde(rename = "action.list")]
    List {
        #[serde(default)]
        query: Option<String>,
    },
    /// Runs an action: a REAPER command id (`40044`), a named command, a custom action or a
    /// script's command.
    #[serde(rename = "action.run")]
    Run { action: String },
    /// Binds a key to an action in the main section, replacing what the key ran.
    #[serde(rename = "action.shortcut.add")]
    AddShortcut {
        /// As REAPER writes it: "Ctrl+Shift+A", "F5", "NumPad +", "Space".
        key: String,
        action: String,
    },
    /// Removes a key's binding; with `disable_default`, also turns off Scythe's default
    /// shortcut on that key (REAPER's command 0).
    #[serde(rename = "action.shortcut.remove")]
    RemoveShortcut {
        key: String,
        #[serde(default)]
        disable_default: bool,
    },
    /// Adds a custom action (a macro of other actions) or replaces the one with `id`.
    #[serde(rename = "action.custom.set")]
    SetCustom {
        /// Keeps a custom action's id (without or with its `_`); a new one is made otherwise.
        #[serde(default)]
        id: Option<String>,
        name: String,
        /// The actions it runs, in order.
        actions: Vec<String>,
        /// One undo step for the whole custom action.
        #[serde(default = "yes")]
        consolidate_undo: bool,
    },
    /// Registers a Lua, EEL2 or Python script as an action (REAPER's `AddRemoveReaScript`).
    #[serde(rename = "action.script.add")]
    AddScript {
        /// Absolute, or relative to the resource path's `Scripts` folder.
        path: String,
    },
    /// Removes a custom action or script action, and its shortcuts.
    #[serde(rename = "action.remove")]
    Remove { action: String },
    /// The shortcut file as a `.ReaperKeyMap`, with custom actions and scripts.
    #[serde(rename = "action.keymap.export")]
    ExportKeymap,
    /// Merges a `.ReaperKeyMap` (or `reaper-kb.ini`) given as text.
    #[serde(rename = "action.keymap.import")]
    ImportKeymap { text: String },
    /// A toolbar's buttons: Scythe's default for the main toolbar unless customised.
    #[serde(rename = "toolbar.get")]
    GetToolbar {
        /// `main`, or a floating toolbar's number from 1 to 32.
        toolbar: String,
    },
    /// Customises a toolbar.
    #[serde(rename = "toolbar.set")]
    SetToolbar {
        /// `main`, or a floating toolbar's number from 1 to 32.
        toolbar: String,
        buttons: Vec<ButtonSpec>,
        #[serde(default)]
        title: Option<String>,
    },
    /// Back to the default toolbar.
    #[serde(rename = "toolbar.reset")]
    ResetToolbar { toolbar: String },
    /// Imports a `.ReaperMenu` or `.ReaperMenuSet` given as text.
    #[serde(rename = "toolbar.import")]
    ImportMenus { text: String },
    /// The mouse modifiers changed from the defaults, or those of one context.
    #[serde(rename = "mouse.get")]
    GetMouse {
        /// A context such as `MM_CTX_ITEM_DBLCLK`.
        #[serde(default)]
        context: Option<String>,
    },
    /// Assigns an action (or a REAPER behaviour as `"13 m"`) to a click with modifiers in a
    /// context, as `SetMouseModifier` does; no `action` resets it to the default.
    #[serde(rename = "mouse.set")]
    SetMouse {
        /// A context such as `MM_CTX_ITEM_DBLCLK` or `MM_CTX_TRACK_CLK`.
        context: String,
        /// 1 Shift, 2 Ctrl, 4 Alt, 8 Logo, added.
        #[serde(default)]
        modifiers: u8,
        #[serde(default)]
        action: Option<String>,
    },
}

fn yes() -> bool {
    true
}

fn key(text: &str) -> Result<Chord, ApiError> {
    Chord::parse(text).ok_or_else(|| ApiError::invalid(format!("not a key: \"{text}\"")))
}

/// The ini section of `main` or floating toolbar `n`.
pub fn toolbar_section(toolbar: &str) -> Result<String, ApiError> {
    let t = toolbar.trim();
    if t.eq_ignore_ascii_case("main") || t.eq_ignore_ascii_case(MAIN_TOOLBAR) {
        return Ok(MAIN_TOOLBAR.to_owned());
    }
    match t.trim_start_matches("Floating toolbar ").parse::<usize>() {
        Ok(n @ 1..=menus::FLOATING_TOOLBARS) => Ok(menus::floating_toolbar(n)),
        _ => Err(ApiError::invalid(format!("no toolbar \"{toolbar}\"; use main or 1 to 32"))),
    }
}

/// Scythe's main toolbar until it is customised.
pub fn default_main_toolbar() -> Toolbar {
    let b = |command: &str, text: &str| Button { command: command.into(), text: text.into(), icon: None, flags: None };
    Toolbar {
        section: MAIN_TOOLBAR.into(),
        title: None,
        buttons: vec![
            b("40023", "New project"),
            b("40025", "Open project"),
            b("40026", "Save project"),
            Button::separator(),
            b("40029", "Undo"),
            b("40030", "Redo"),
            Button::separator(),
            b("1157", "Snap"),
            b("1068", "Repeat"),
            b("40364", "Metronome"),
            Button::separator(),
            b("40078", "Mixer"),
            b("40605", "Actions"),
        ],
    }
}

/// The label a new button for `command` gets, on any toolbar: the default main toolbar's (so
/// "Snap" reads the same everywhere), else the action's name without its category.
pub fn button_text(session: &Session, command: &str) -> String {
    if let Some(b) = default_main_toolbar().buttons.into_iter().find(|b| b.command == command) {
        return b.text;
    }
    let name = session.action_name(command).unwrap_or_else(|| command.to_owned());
    name.split_once(": ").map_or(name.as_str(), |(_, t)| t).to_owned()
}

/// The toolbar in `section` as it shows: the customised one, else the default (an empty one
/// for a floating toolbar).
pub fn toolbar_or_default(session: &Session, section: &str) -> Toolbar {
    session.toolbar(section).unwrap_or_else(|| {
        if section == MAIN_TOOLBAR {
            default_main_toolbar()
        } else {
            Toolbar { section: section.to_owned(), title: None, buttons: Vec::new() }
        }
    })
}

fn toolbar_json(session: &Session, t: &Toolbar) -> Value {
    let buttons: Vec<Value> = t
        .buttons
        .iter()
        .map(|b| {
            value!({
                "action": b.command,
                "text": b.text,
                "icon": b.icon,
                "name": session.action_name(&b.command),
                "toggle": if b.is_separator() { -1 } else { session.toggle_state(&b.command) },
            })
        })
        .collect();
    value!({ "toolbar": t.section, "title": t.title, "customized": session.toolbar(&t.section).is_some(), "buttons": buttons })
}

pub fn execute(session: &mut Session, cmd: CustomizeCommand) -> Result<Value, ApiError> {
    match cmd {
        CustomizeCommand::List { query } => {
            let words: Vec<String> =
                query.unwrap_or_default().to_lowercase().split_whitespace().map(str::to_owned).collect();
            let list: Vec<Value> = session
                .action_list()
                .into_iter()
                .filter(|a| {
                    let name = a.name.to_lowercase();
                    words.iter().all(|w| name.contains(w) || a.command.to_lowercase().contains(w))
                })
                .map(|a| {
                    let shortcuts: Vec<String> =
                        session.shortcuts(&a.command, a.section).iter().map(Chord::describe).collect();
                    value!({
                        "command": a.command,
                        "id": session.command_id(&a.command),
                        "name": a.name,
                        "section": a.section,
                        "runs": a.runs,
                        "kind": a.kind,
                        "shortcuts": shortcuts,
                        "toggle": session.toggle_state(&a.command),
                    })
                })
                .collect();
            Ok(Value::Array(list))
        }
        CustomizeCommand::Run { action } => {
            actions::run(session, &action)?;
            Ok(value!({ "ok": true }))
        }
        CustomizeCommand::AddShortcut { key: k, action } => {
            let chord = key(&k)?;
            if session.action_name(&action).is_none() {
                return Err(ApiError::not_found(format!("no action \"{action}\"")));
            }
            session.bind_key(chord, SECTION_MAIN, &action);
            Ok(value!({ "key": chord.describe(), "action": actions::normalize(&action) }))
        }
        CustomizeCommand::RemoveShortcut { key: k, disable_default } => {
            let chord = key(&k)?;
            let removed = session.unbind_key(chord, SECTION_MAIN);
            if disable_default {
                session.disable_default_key(chord, SECTION_MAIN);
            }
            Ok(value!({ "removed": removed }))
        }
        CustomizeCommand::SetCustom { id, name, actions: members, consolidate_undo } => {
            if members.is_empty() {
                return Err(ApiError::invalid("a custom action needs at least one action"));
            }
            if let Some(missing) = members.iter().find(|m| session.action_name(m).is_none()) {
                return Err(ApiError::not_found(format!("no action \"{missing}\"")));
            }
            let command = session.define_custom_action(id, &name, members, consolidate_undo);
            Ok(value!({ "command": command, "id": session.command_id(&command) }))
        }
        CustomizeCommand::AddScript { path } => {
            let command = session.add_script_action(&path, SECTION_MAIN);
            Ok(value!({ "command": command, "id": session.command_id(&command) }))
        }
        CustomizeCommand::Remove { action } => {
            if !session.remove_named_action(&action) {
                return Err(ApiError::not_found(format!("no custom or script action \"{action}\"")));
            }
            Ok(value!({ "ok": true }))
        }
        CustomizeCommand::ExportKeymap => Ok(value!({ "text": session.keymap_text() })),
        CustomizeCommand::ImportKeymap { text } => Ok(value!({ "lines": session.import_keymap(&text) })),
        CustomizeCommand::GetToolbar { toolbar } => {
            let section = toolbar_section(&toolbar)?;
            Ok(toolbar_json(session, &toolbar_or_default(session, &section)))
        }
        CustomizeCommand::SetToolbar { toolbar, buttons, title } => {
            let section = toolbar_section(&toolbar)?;
            let mut t = session.toolbar(&section).unwrap_or(Toolbar {
                section: section.clone(),
                title: None,
                buttons: Vec::new(),
            });
            t.buttons = buttons
                .into_iter()
                .map(|b| {
                    let command = if b.action.trim() == "-1" { "-1".to_owned() } else { actions::normalize(&b.action) };
                    let mut button = Button { command, text: b.text, icon: b.icon, flags: None };
                    if button.text.is_empty() && !button.is_separator() {
                        button.text = button_text(session, &button.command);
                    }
                    button
                })
                .collect();
            if title.is_some() {
                t.title = title;
            }
            session.set_toolbar(&t);
            Ok(toolbar_json(session, &t))
        }
        CustomizeCommand::ResetToolbar { toolbar } => {
            let section = toolbar_section(&toolbar)?;
            Ok(value!({ "reset": session.reset_toolbar(&section) }))
        }
        CustomizeCommand::ImportMenus { text } => Ok(value!({ "sections": session.import_menus(&text) })),
        CustomizeCommand::GetMouse { context } => {
            let sections = session.mouse.ini.sections.iter().filter(|s| s.name.starts_with("MM_CTX_"));
            let out: Vec<Value> = sections
                .filter(|s| context.as_deref().is_none_or(|c| s.name.eq_ignore_ascii_case(c)))
                .flat_map(|s| {
                    s.entries.iter().filter_map(|(k, v)| {
                        let m: u8 = k.strip_prefix("mm_")?.parse().ok()?;
                        Some(value!({ "context": s.name, "modifiers": m, "assignment": v }))
                    })
                })
                .collect();
            Ok(json(out))
        }
        CustomizeCommand::SetMouse { context, modifiers, action } => {
            if modifiers > 15 {
                return Err(ApiError::invalid("modifiers go from 0 to 15"));
            }
            let assignment = match action.as_deref().map(Assignment::parse) {
                Some(None) => return Err(ApiError::invalid("empty action")),
                Some(Some(a)) => Some(a),
                None => None,
            };
            session.set_mouse_modifier(&context, modifiers, assignment.as_ref());
            Ok(value!({ "context": context, "modifiers": modifiers, "assignment": assignment.map(|a| a.to_text()) }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolbar_names() {
        assert_eq!(toolbar_section("main").unwrap(), MAIN_TOOLBAR);
        assert_eq!(toolbar_section("3").unwrap(), "Floating toolbar 3");
        assert!(toolbar_section("33").is_err());
    }
}
