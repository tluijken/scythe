//! Actions, shortcuts, toolbars and mouse modifiers (`Main_OnCommand` with custom actions,
//! `NamedCommandLookup`, toggle states…), state chunks, and general functions scripts call
//! often (`genGuid`, `AnyTrackSolo`, `file_exists`…).
//!
//! Measured in REAPER 7.73 (agent probes, kept as `tests/fixtures/reascript`): named commands
//! need their `_`; a numeric string looks up as itself; `ReverseNamedCommandLookup` gives nil
//! for REAPER's own actions; a custom action's toggle state is its members' when they agree;
//! `SetToggleCommandState` refuses a script that is not an action; `SetTrackStateChunk` with
//! text that is not a track answers false but empties the track's items and envelopes.

use std::rc::Rc;

use scythe_core::keymap::combined_toggle;
use scythe_core::mousemap::Assignment;
use scythe_core::rpp;

use super::{Args, Ret, State, Val, project, ret, tracks};

pub(super) fn call(s: &mut State, name: &str, a: &Args) -> Option<Ret> {
    let table = s.ctx.actions.clone();
    Some(match name {
        "Main_OnCommand" | "Main_OnCommandEx" => {
            run(s, a.int(0), 0);
            ret(Val::Nil)
        }
        "NamedCommandLookup" => {
            let name = a.text(0);
            let name = name.trim();
            let id = match name.parse::<i64>() {
                Ok(n) => n.max(0),
                Err(_) if name.starts_with('_') => table
                    .actions
                    .iter()
                    .find(|(_, named, _, _)| named.eq_ignore_ascii_case(name))
                    .map_or(0, |(id, ..)| i64::from(*id)),
                Err(_) => 0,
            };
            ret(Val::Int(id))
        }
        "ReverseNamedCommandLookup" => {
            let id = a.int(0);
            let named = table.actions.iter().find(|(i, n, _, _)| i64::from(*i) == id && !n.is_empty());
            ret(named.map_or(Val::Nil, |(_, n, _, _)| Val::Str(n.trim_start_matches('_').to_owned())))
        }
        "kbd_getTextFromCmd" => {
            let id = a.int(0);
            let name = table.actions.iter().find(|(i, ..)| i64::from(*i) == id).map(|(_, _, n, _)| n.clone());
            ret(Val::Str(name.unwrap_or_default()))
        }
        // A section handle is its id plus one, so 0 stays the null section REAPER's calls refuse.
        "SectionFromUniqueID" => ret(Val::Int(a.int(0).max(0) + 1)),
        "CountActionShortcuts" => {
            let keys = shortcuts(&table, a.int(0), a.int(1));
            ret(Val::Int(keys.len() as i64))
        }
        "GetActionShortcutDesc" => match shortcuts(&table, a.int(0), a.int(1)).get(a.int(2).max(0) as usize) {
            Some(d) if a.int(2) >= 0 => (Val::Bool(true), vec![Val::Str(d.clone())]),
            _ => (Val::Bool(false), vec![Val::Str(String::new())]),
        },
        "GetToggleCommandState" => ret(Val::Int(toggle(s, a.int(0), 0).into())),
        "GetToggleCommandStateEx" => ret(Val::Int(toggle(s, a.int(1), 0).into())),
        "SetToggleCommandState" => {
            let id = a.int(1);
            let script = table.actions.iter().any(|(i, n, ..)| i64::from(*i) == id && n.starts_with("_RS"));
            if script {
                let state = a.int(2) as i32;
                s.out.toggle_states.push((id as u32, state));
                Rc::make_mut(&mut s.ctx.actions).toggles.insert(id as u32, state);
            }
            ret(Val::Bool(script))
        }
        "RefreshToolbar" | "RefreshToolbar2" => ret(Val::Nil),
        "AddRemoveReaScript" => {
            let (add, section, path) = (a.bool(0), a.int(1).max(0) as u32, a.text(2));
            if path.is_empty() {
                return Some(ret(Val::Int(0)));
            }
            s.out.script_actions.push((add, section, path));
            let id = if add {
                let t = Rc::make_mut(&mut s.ctx.actions);
                t.next_id += 1;
                t.next_id - 1
            } else {
                1
            };
            ret(Val::Int(id.into()))
        }
        "GetMouseModifier" => {
            let action = table.mouse.get(&a.text(0), a.int(1).clamp(0, 15) as u8).map(|m| m.to_text());
            (Val::Nil, vec![Val::Str(action.unwrap_or_default())])
        }
        "SetMouseModifier" => {
            let (context, flag, action) = (a.text(0), a.int(1), a.text(2));
            let assignment = if action == "-1" { None } else { Assignment::parse(&action) };
            if (0..16).contains(&flag) {
                let action = assignment.as_ref().map(|a| match a {
                    Assignment::Action(c) => c.clone(),
                    Assignment::Behavior(_) => a.to_text(),
                });
                s.out.mouse_modifiers.push((context.clone(), flag as u8, action));
                Rc::make_mut(&mut s.ctx.actions).mouse.set(&context, flag as u8, assignment.as_ref());
            }
            ret(Val::Nil)
        }
        "get_action_context" => (
            Val::Bool(false),
            vec![
                Val::Str(table.script_file.clone()),
                Val::Int(0),
                Val::Int(table.script_command.into()),
                Val::Int(-1),
                Val::Int(-1),
                Val::Num(-1.0),
                Val::Str(String::new()),
            ],
        ),

        // State chunks.
        "GetTrackStateChunk" => match a.get(0).track().and_then(|t| rpp::track_state_chunk(&s.project, t)) {
            Some(text) => (Val::Bool(true), vec![Val::Str(text)]),
            None => (Val::Bool(false), vec![Val::Str(String::new())]),
        },
        "SetTrackStateChunk" => {
            let Some(t) = a.get(0).track().filter(|t| s.project.track(*t).is_some()) else {
                return Some(ret(Val::Bool(false)));
            };
            let ok = rpp::set_track_state_chunk(&mut s.project, t, &a.text(1));
            if !ok && let Some(track) = s.project.track_mut(t) {
                track.items.clear();
                track.envelopes.clear();
            }
            s.changed = true;
            ret(Val::Bool(ok))
        }
        "GetItemStateChunk" => match a.get(0).item().and_then(|i| s.project.find_item(i)) {
            Some((_, item)) => (Val::Bool(true), vec![Val::Str(rpp::item_state_chunk(item))]),
            None => (Val::Bool(false), vec![Val::Str(String::new())]),
        },
        "SetItemStateChunk" => {
            let Some(id) = a.get(0).item().filter(|i| s.project.find_item(*i).is_some()) else {
                return Some(ret(Val::Bool(false)));
            };
            if let Some(new) = rpp::item_from_state_chunk(&a.text(1), id)
                && let Some(item) = s.project.tracks.iter_mut().find_map(|t| t.item_mut(id))
            {
                *item = new;
                s.changed = true;
            }
            ret(Val::Bool(true))
        }
        "GetEnvelopeStateChunk" => match tracks::envelope(&mut s.project, a.get(0).env()) {
            Some(env) => (Val::Bool(true), vec![Val::Str(rpp::envelope_state_chunk(env))]),
            None => (Val::Bool(false), vec![Val::Str(String::new())]),
        },
        "SetEnvelopeStateChunk" => {
            let text = a.text(1);
            let Some(env) = tracks::envelope(&mut s.project, a.get(0).env()) else {
                return Some(ret(Val::Bool(false)));
            };
            match rpp::envelope_from_state_chunk(env.kind, &text) {
                Some(new) => {
                    *env = new;
                    s.changed = true;
                    ret(Val::Bool(true))
                }
                None => ret(Val::Bool(false)),
            }
        }

        // General.
        "genGuid" => (Val::Nil, vec![Val::Str(scythe_core::model::new_guid())]),
        "guidToString" | "stringToGuid" => (Val::Nil, vec![Val::Str(a.text(0))]),
        "AnyTrackSolo" => ret(Val::Bool(s.project.tracks.iter().any(|t| t.solo))),
        "SoloAllTracks" => {
            let on = a.int(0) != 0;
            s.project.tracks.iter_mut().for_each(|t| t.solo = on);
            s.changed = true;
            ret(Val::Nil)
        }
        "MuteAllTracks" => {
            let on = a.bool(0);
            s.project.tracks.iter_mut().for_each(|t| t.muted = on);
            s.changed = true;
            ret(Val::Nil)
        }
        "ClearAllRecArmed" => {
            s.project.tracks.iter_mut().for_each(|t| t.armed = false);
            s.changed = true;
            ret(Val::Nil)
        }
        "GetTrackGUID" => {
            let guid = a.get(0).track().and_then(|t| {
                if s.project.master.id == t {
                    Some(s.project.master.guid.clone())
                } else {
                    s.project.track(t).map(|t| t.guid.clone())
                }
            });
            ret(Val::Str(guid.unwrap_or_default()))
        }
        "GetNumTracks" => ret(Val::Int(s.project.tracks.len() as i64)),
        "GetLastTouchedTrack" => {
            let t = s.sel_tracks.iter().copied().find(|t| s.project.track(*t).is_some());
            ret(t.map_or(Val::Nil, Val::Track))
        }
        "IsTrackVisible" => ret(Val::Bool(a.get(0).track().is_some())),
        "GetMasterMuteSoloFlags" => {
            let m = &s.project.master;
            ret(Val::Int(m.muted as i64 | (m.solo as i64) << 1))
        }
        "file_exists" => ret(Val::Bool(std::path::Path::new(&a.text(0)).exists())),
        "RecursiveCreateDirectory" => ret(Val::Int(std::fs::create_dir_all(a.text(0)).is_ok() as i64)),
        "EnumerateFiles" | "EnumerateSubdirectories" => {
            let dirs = name == "EnumerateSubdirectories";
            let mut names: Vec<String> = std::fs::read_dir(a.text(0))
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() == dirs))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            let i = a.int(1);
            ret(usize::try_from(i).ok().and_then(|i| names.get(i).cloned()).map_or(Val::Nil, Val::Str))
        }
        "SetEditCurPos2" => {
            s.out.cursor = Some(a.num(1).max(0.0));
            ret(Val::Nil)
        }
        "OnPauseButton" => {
            s.out.commands.push(i64::from(scythe_core::marker_action::PAUSE));
            ret(Val::Nil)
        }
        "Main_SaveProject" => {
            s.out.commands.push(if a.bool(1) { 40022 } else { 40026 });
            ret(Val::Nil)
        }
        "GetPlayStateEx" => return project::call(s, "GetPlayState", &Args(&[])),
        "MB" => return project::call(s, "ShowMessageBox", a),
        "Undo_OnStateChangeEx" => return project::call(s, "Undo_OnStateChange", &Args(&a.0[..1.min(a.0.len())])),
        "Undo_OnStateChangeEx2" => return project::call(s, "Undo_OnStateChange", &Args(a.0.get(1..2).unwrap_or(&[]))),
        _ => return None,
    })
}

/// `Main_OnCommand`: a custom action runs its members in turn; scripts and other named
/// commands are left to the app.
fn run(s: &mut State, id: i64, depth: usize) {
    let members = u32::try_from(id).ok().and_then(|id| s.ctx.actions.custom.get(&id).cloned());
    match members {
        Some(members) if depth < 8 => members.into_iter().for_each(|m| run(s, i64::from(m), depth + 1)),
        Some(_) => {}
        None if id >= i64::from(scythe_core::keymap::FIRST_NAMED_ID) => s.out.commands.push(id),
        None => project::command(s, id),
    }
}

/// The toggle state of command `id` as the script left things.
fn toggle(s: &State, id: i64, depth: usize) -> i32 {
    let Ok(id) = u32::try_from(id) else { return -1 };
    if let Some(members) = s.ctx.actions.custom.get(&id) {
        if depth > 8 {
            return -1;
        }
        return combined_toggle(members.iter().map(|m| toggle(s, i64::from(*m), depth + 1)));
    }
    match id {
        1068 => s.project.loop_enabled as i32,
        40364 => s.project.metronome.enabled() as i32,
        _ => match scythe_core::edit::actions::toggle_state(&s.project, id) {
            Some(on) => on as i32,
            None => s.ctx.actions.toggles.get(&id).copied().unwrap_or(-1),
        },
    }
}

fn shortcuts(table: &super::ActionTable, section: i64, id: i64) -> Vec<String> {
    match (u32::try_from(section - 1), u32::try_from(id)) {
        (Ok(section), Ok(id)) => table.shortcuts.get(&(section, id)).cloned().unwrap_or_default(),
        _ => Vec::new(),
    }
}
