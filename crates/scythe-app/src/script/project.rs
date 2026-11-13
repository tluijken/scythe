//! The project as a whole: transport, time selection, tempo, markers, project settings,
//! extended state, commands, undo and the general functions.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use scythe_core::rpp::{Chunk, Node, line_key, quote, tokenize};
use scythe_core::timebase::PlayRate;
use scythe_core::{Marker, Project, TimeRange, TrackId};

use super::items::{select_all_tracks, selection};
use super::tracks::{color_from_native, color_to_native};
use super::{Args, Ret, State, Val, index, ret};

pub(super) fn call(s: &mut State, name: &str, a: &Args) -> Option<Ret> {
    Some(match name {
        "ShowConsoleMsg" => {
            s.out.console.push_str(&a.text(0));
            ret(Val::Nil)
        }
        "ClearConsole" => {
            s.out.console.clear();
            ret(Val::Nil)
        }
        "GetAppVersion" => ret(Val::Str("7.73/scythe".to_owned())),
        "GetOS" => ret(Val::Str(
            match std::env::consts::OS {
                "windows" => "Win64",
                "macos" if cfg!(target_arch = "aarch64") => "macOS-arm64",
                "macos" => "OSX64",
                _ => "Other",
            }
            .to_owned(),
        )),
        "GetResourcePath" => {
            ret(Val::Str(s.ctx.resource_dir.as_ref().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default()))
        }
        "GetExePath" => {
            let exe = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_string_lossy().into_owned()));
            ret(Val::Str(exe.unwrap_or_default()))
        }
        "time_precise" => ret(Val::Num(
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64()),
        )),
        "ShowMessageBox" => {
            let (text, title, kind) = (a.text(0), a.text(1), a.int(2));
            let pressed = match &s.ctx.services {
                Some(sv) => sv.message_box(&text, &title, kind),
                // Without a window the first button counts as pressed.
                None => [1, 1, 3, 6, 6, 4].get(kind as usize).copied().unwrap_or(1),
            };
            ret(Val::Int(pressed))
        }
        "GetUserInputs" => {
            let n = a.int(1).clamp(0, 16) as usize;
            let (title, extra) = split_extra(&a.text(2));
            let captions = csv(&title, n);
            let values = csv(&a.text(3), n);
            let _ = extra;
            let answer = s.ctx.services.as_ref().and_then(|sv| sv.user_inputs(&a.text(0), &captions, &values));
            match answer {
                Some(v) => (Val::Bool(true), vec![Val::Str(v.join(","))]),
                None => (Val::Bool(false), vec![Val::Str(a.text(3))]),
            }
        }
        "ValidatePtr" | "ValidatePtr2" => {
            let (p, kind) = if name == "ValidatePtr" { (a.get(0), a.text(1)) } else { (a.get(1), a.text(2)) };
            ret(Val::Bool(valid(s, p) && super::value_kind(p).is_some_and(|k| k.pointer_type() == kind)))
        }
        "DB2SLIDER" => ret(Val::Num(scythe_core::fader::db_to_slider(a.num(0)))),
        "SLIDER2DB" => ret(Val::Num(scythe_core::fader::slider_to_db(a.num(0)))),
        "mkvolstr" => (Val::Nil, vec![Val::Str(volume_text(a.num(1)))]),
        "mkpanstr" => (Val::Nil, vec![Val::Str(pan_text(a.num(1)))]),
        "ColorToNative" => ret(Val::Int((a.int(0) & 255) << 16 | (a.int(1) & 255) << 8 | a.int(2) & 255)),
        "ColorFromNative" => {
            let c = a.int(0);
            (Val::Nil, vec![Val::Int(c >> 16 & 255), Val::Int(c >> 8 & 255), Val::Int(c & 255)])
        }
        "format_timestr" => (Val::Nil, vec![Val::Str(scythe_core::tempo::format_time(a.num(0)))]),
        "format_timestr_pos" => (Val::Nil, vec![Val::Str(format_pos(&s.project, a.num(0), a.int(2)))]),
        "parse_timestr" => ret(Val::Num(parse_time(&a.text(0)))),
        "parse_timestr_pos" => ret(Val::Num(parse_pos(&s.project, &a.text(0), a.int(1)))),
        "GetExtState" => ret(Val::Str(ext_state(s, |e| e.get(&a.text(0), &a.text(1)).unwrap_or_default()))),
        "HasExtState" => ret(Val::Bool(ext_state(s, |e| e.get(&a.text(0), &a.text(1)).is_some()))),
        "SetExtState" => {
            let (section, key, value, persist) = (a.text(0), a.text(1), a.text(2), a.bool(3));
            ext_state(s, |e| e.set(&section, &key, Some(value), persist));
            ret(Val::Nil)
        }
        "DeleteExtState" => {
            let (section, key, persist) = (a.text(0), a.text(1), a.bool(2));
            ext_state(s, |e| e.set(&section, &key, None, persist));
            ret(Val::Nil)
        }

        // Undo and refreshes.
        "Undo_EndBlock" | "Undo_OnStateChange" => {
            s.out.undo = Some(a.text(0));
            ret(Val::Nil)
        }
        "Undo_EndBlock2" | "Undo_OnStateChange2" => {
            s.out.undo = Some(a.text(1));
            ret(Val::Nil)
        }
        "Undo_CanUndo2" => ret(s.ctx.undo.clone().map_or(Val::Nil, Val::Str)),
        "Undo_CanRedo2" => ret(s.ctx.redo.clone().map_or(Val::Nil, Val::Str)),
        "Undo_DoUndo2" | "Undo_DoRedo2" => {
            let (label, id) = if name == "Undo_DoUndo2" { (&s.ctx.undo, 40029) } else { (&s.ctx.redo, 40030) };
            let can = label.is_some();
            if can {
                s.out.commands.push(id);
            }
            ret(Val::Int(can as i64))
        }
        "MarkProjectDirty" => {
            s.out.dirty = true;
            ret(Val::Nil)
        }
        "IsProjectDirty" => ret(Val::Int((s.ctx.dirty || s.changed || s.out.dirty) as i64)),
        "GetProjectStateChangeCount" => ret(Val::Int(s.changed as i64)),

        // Project.
        "EnumProjects" => match a.int(0) {
            -1 | 0 => (Val::Proj, vec![Val::Str(project_file(&s.project))]),
            _ => (Val::Nil, vec![Val::Str(String::new())]),
        },
        "GetProjectName" => {
            let name = s.project.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
            (Val::Nil, vec![Val::Str(name.unwrap_or_default())])
        }
        "GetProjectPath" | "GetProjectPathEx" => (Val::Nil, vec![Val::Str(project_path(&s.project))]),
        "GetProjectLength" => ret(Val::Num(s.project.length())),
        "GetSetProjectInfo" => ret(Val::Num(project_info(s, &a.text(1), a.bool(3).then(|| a.num(2))))),
        "GetSetProjectInfo_String" => {
            let set = a.bool(3).then(|| a.text(2));
            match project_text(s, &a.text(1), set) {
                Some(v) => (Val::Bool(true), vec![Val::Str(v)]),
                None => (Val::Bool(false), vec![Val::Str(String::new())]),
            }
        }
        "GetSetProjectAuthor" => {
            let set = a.bool(1).then(|| a.text(2));
            (Val::Nil, vec![Val::Str(project_text(s, "PROJECT_AUTHOR", set).unwrap_or_default())])
        }
        "GetSetProjectNotes" => {
            let set = a.bool(1).then(|| a.text(2));
            (Val::Nil, vec![Val::Str(notes(s, set))])
        }
        "SetProjExtState" => {
            let n = proj_ext_set(s, &a.text(1), &a.text(2), &a.text(3));
            ret(Val::Int(n as i64))
        }
        "GetProjExtState" => {
            match proj_ext(&s.project, &a.text(1)).into_iter().find(|(k, _)| k.eq_ignore_ascii_case(&a.text(2))) {
                Some((_, v)) => (Val::Int(1), vec![Val::Str(v)]),
                None => (Val::Int(0), vec![Val::Str(String::new())]),
            }
        }
        "EnumProjExtState" => {
            match proj_ext(&s.project, &a.text(1)).into_iter().nth(a.int(2).max(0) as usize).filter(|_| a.int(2) >= 0) {
                Some((k, v)) => (Val::Bool(true), vec![Val::Str(k), Val::Str(v)]),
                None => (Val::Bool(false), vec![Val::Str(String::new()), Val::Str(String::new())]),
            }
        }

        // Transport and cursor.
        "GetCursorPosition" | "GetCursorPositionEx" => ret(Val::Num(s.cursor())),
        "GetSetProjectGrid" => {
            if a.bool(1) {
                s.project.editing.grid.set_api(a.num(2), a.int(3), a.num(4));
                s.changed = true;
            }
            let (division, mode, swing) = s.project.editing.grid.api();
            // REAPER 7.73 answers 3327 here whatever the grid.
            (Val::Int(3327), vec![Val::Num(division), Val::Int(mode), Val::Num(swing)])
        }
        "SnapToGrid" => ret(Val::Num(s.project.editing.grid.snap(&s.project.tempo, a.num(1)))),
        "ApplyNudge" => {
            use scythe_core::edit::nudge::{Nudge, NudgeContext, NudgeUnit, NudgeWhat};
            let (flags, unit) = (a.int(1), a.int(3));
            let (Some(what), true) = (NudgeWhat::from_code(a.int(2)), (0..NudgeUnit::COUNT as i64).contains(&unit))
            else {
                return Some(ret(Val::Bool(false)));
            };
            let nudge = Nudge {
                set: flags & 1 != 0,
                snap: flags & 2 != 0,
                what,
                unit: NudgeUnit(unit as u8),
                value: a.num(4),
                reverse: a.bool(5),
                copies: a.int(6).max(1) as u32,
            };
            let items = selection(s);
            let mut ctx = NudgeContext { cursor: s.cursor(), pixel: 0.01 };
            let selected = nudge.apply(&mut s.project, &items, &mut ctx);
            if what == NudgeWhat::EditCursor {
                s.out.cursor = Some(ctx.cursor);
            } else if selected != items {
                s.sel_items = selected;
                s.sel_changed = true;
            }
            s.changed = true;
            ret(Val::Bool(true))
        }
        "SetEditCurPos" => {
            s.out.cursor = Some(a.num(0).max(0.0));
            ret(Val::Nil)
        }
        "MoveEditCursor" => {
            s.out.cursor = Some((s.cursor() + a.num(0)).max(0.0));
            ret(Val::Nil)
        }
        "GetPlayState" => ret(Val::Int(s.playing() as i64)),
        "GetPlayPosition" | "GetPlayPosition2" | "GetPlayPositionEx" | "GetPlayPosition2Ex" => {
            ret(Val::Num(s.ctx.play_position))
        }
        "OnPlayButton" => {
            s.out.play = Some(true);
            ret(Val::Nil)
        }
        "OnStopButton" => {
            s.out.play = Some(false);
            ret(Val::Nil)
        }
        "GetSet_LoopTimeRange" | "GetSet_LoopTimeRange2" => {
            let b = (name == "GetSet_LoopTimeRange2") as usize;
            if a.bool(b) {
                let (start, end) = (a.num(b + 2), a.num(b + 3));
                s.project.time_selection = (end > start).then(|| TimeRange::new(start, end));
                s.changed = true;
            }
            let r = s.project.time_selection.unwrap_or(TimeRange { start: 0.0, end: 0.0 });
            (Val::Nil, vec![Val::Num(r.start), Val::Num(r.end)])
        }
        "GetSetRepeat" | "GetSetRepeatEx" => {
            let v = a.int((name == "GetSetRepeatEx") as usize);
            let on = match v {
                0 => false,
                1 => true,
                2 => !s.project.loop_enabled,
                _ => s.project.loop_enabled,
            };
            if on != s.project.loop_enabled {
                s.project.loop_enabled = on;
                s.changed = true;
            }
            ret(Val::Int(on as i64))
        }

        _ => return tempo(s, name, a).or_else(|| markers(s, name, a)),
    })
}

/// Whether a pointer still names something in the project.
fn valid(s: &mut State, p: &Val) -> bool {
    match p {
        Val::Track(t) => *t == s.project.master.id || s.project.track(*t).is_some(),
        Val::Item(i) => s.project.find_item(*i).is_some(),
        Val::Take(i, k) => s.project.find_item(*i).is_some_and(|(_, it)| *k < it.takes.len()),
        Val::Env(_) => super::tracks::call(s, "GetEnvelopeName", &Args(std::slice::from_ref(p)))
            .is_some_and(|r| r.0 == Val::Bool(true)),
        Val::Src(i) => *i < s.sources.len(),
        Val::Proj => true,
        _ => false,
    }
}

/// `GetUserInputs` captions may end in `,extrawidth=…` and the like; those are not captions.
fn split_extra(captions: &str) -> (String, String) {
    match captions.find(",extrawidth=").or_else(|| captions.find(",separator=")) {
        Some(i) => (captions[..i].to_owned(), captions[i + 1..].to_owned()),
        None => (captions.to_owned(), String::new()),
    }
}

/// `n` comma-separated fields, padded with empty ones.
fn csv(text: &str, n: usize) -> Vec<String> {
    let mut v: Vec<String> = if text.is_empty() { Vec::new() } else { text.split(',').map(str::to_owned).collect() };
    v.resize(n.max(v.len().min(n)), String::new());
    v.truncate(n);
    v
}

/// `mkvolstr`: "-6.02dB", "-12.0dB", "+6.02dB", as REAPER 7.73 writes them; "-inf dB" for
/// silence.
fn volume_text(v: f64) -> String {
    if v <= 0.0 {
        return "-inf dB".to_owned();
    }
    let db = 20.0 * v.log10();
    if db <= -150.0 {
        return "-inf dB".to_owned();
    }
    let decimals = if db.abs() < 10.0 {
        2
    } else if db.abs() < 100.0 {
        1
    } else {
        0
    };
    let text = format!("{db:.decimals$}");
    let zero = text.trim_start_matches('-').chars().all(|c| c == '0' || c == '.');
    match (zero, db > 0.0) {
        (true, _) => format!("{}dB", text.trim_start_matches('-')),
        (false, true) => format!("+{text}dB"),
        (false, false) => format!("{text}dB"),
    }
}

/// `mkpanstr`: "center", "50%L", "25%R".
fn pan_text(p: f64) -> String {
    let pct = (p * 100.0).round() as i64;
    match pct {
        0 => "center".to_owned(),
        n if n < 0 => format!("{}%L", -n),
        n => format!("{n}%R"),
    }
}

/// `format_timestr_pos`: 0 time, 1 and 2 measures.beats, 3 seconds, 4 samples, 5
/// hours:minutes:seconds:frames, 8 frames; -1 is the project default, time.
fn format_pos(p: &Project, t: f64, mode: i64) -> String {
    let rate = p.sample_rate.unwrap_or(44_100) as f64;
    match mode {
        1 | 2 => p.tempo.format_bar_beat(t),
        3 => format!("{t:.3}"),
        4 => format!("{}", (t * rate).round() as i64),
        5 => {
            let frames = (t * 30.0).round() as i64;
            let (h, m, s, f) = (frames / 108_000, frames / 1800 % 60, frames / 30 % 60, frames % 30);
            format!("{h:02}:{m:02}:{s:02}:{f:02}")
        }
        8 => format!("{}", (t * 30.0).round() as i64),
        _ => scythe_core::tempo::format_time(t),
    }
}

/// `parse_timestr`: `[[h:]m:]s.frac`.
fn parse_time(text: &str) -> f64 {
    text.trim().split(':').fold(0.0, |acc, part| acc * 60.0 + part.trim().parse::<f64>().unwrap_or(0.0))
}

/// `parse_timestr_pos`: as `format_timestr_pos` writes positions.
fn parse_pos(p: &Project, text: &str, mode: i64) -> f64 {
    match mode {
        1 | 2 => {
            let mut parts = text.trim().split('.').map(|v| v.parse::<f64>().unwrap_or(0.0));
            let (bar, beat, frac) =
                (parts.next().unwrap_or(1.0), parts.next().unwrap_or(1.0), parts.next().unwrap_or(0.0));
            let tl = p.tempo.timeline();
            let start = tl.bar_start(bar as i64);
            let (_, denom) = p.tempo.time_sig_at(start);
            let qn = tl.qn_at(start) + (beat - 1.0 + frac / 100.0) * 4.0 / denom.max(1) as f64;
            tl.time_at_qn(qn)
        }
        3 => text.trim().parse().unwrap_or(0.0),
        4 => text.trim().parse::<f64>().unwrap_or(0.0) / p.sample_rate.unwrap_or(44_100) as f64,
        5 => {
            let parts: Vec<f64> = text.trim().split(':').map(|v| v.parse().unwrap_or(0.0)).collect();
            let f = parts.last().copied().unwrap_or(0.0) / 30.0;
            parse_time(&parts[..parts.len().saturating_sub(1)].iter().map(f64::to_string).collect::<Vec<_>>().join(":"))
                + f
        }
        8 => text.trim().parse::<f64>().unwrap_or(0.0) / 30.0,
        _ => parse_time(text),
    }
}

fn project_file(p: &Project) -> String {
    p.path.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The project's folder, or where an unsaved project's media goes.
fn project_path(p: &Project) -> String {
    let dir = p.path.as_ref().and_then(|p| p.parent().map(PathBuf::from));
    let dir = dir.or_else(|| scythe_core::paths::home().map(|h| h.join("Documents").join("REAPER Media")));
    dir.map(|d| d.to_string_lossy().into_owned()).unwrap_or_default()
}

/// A project-level line kept as it was read (`TITLE`, `RENDER_FILE`…): its first value.
fn extra_line(p: &Project, key: &str) -> Option<String> {
    p.extra.iter().find_map(|n| match n {
        Node::Line(l) if line_key(l.trim()) == key => Some(tokenize(l).get(1).cloned().unwrap_or_default()),
        _ => None,
    })
}

fn set_extra_line(p: &mut Project, key: &str, value: &str) {
    let line = Node::Line(format!("{key} {}", quote(value)));
    match p.extra.iter().position(|n| matches!(n, Node::Line(l) if line_key(l.trim()) == key)) {
        Some(i) => p.extra[i] = line,
        None => p.extra.insert(0, line),
    }
}

fn render_fmt(p: &Project, i: usize) -> Option<f64> {
    p.extra.iter().find_map(|n| match n {
        Node::Line(l) if line_key(l.trim()) == "RENDER_FMT" => tokenize(l).get(i)?.parse().ok(),
        _ => None,
    })
}

/// `GetSetProjectInfo` for the settings Scythe has.
fn project_info(s: &mut State, key: &str, set: Option<f64>) -> f64 {
    let p = &mut s.project;
    match (key, set) {
        ("PROJECT_SRATE", Some(v)) => {
            p.sample_rate = (v > 0.0).then_some(v as u32);
            s.changed = true;
            v
        }
        ("PROJECT_SRATE", None) => p.sample_rate.unwrap_or(44_100) as f64,
        ("PROJECT_SRATE_USE", Some(v)) => {
            if v == 0.0 {
                p.sample_rate = None;
            } else if p.sample_rate.is_none() {
                p.sample_rate = Some(44_100);
            }
            s.changed = true;
            v
        }
        ("PROJECT_SRATE_USE", None) => p.sample_rate.is_some() as u8 as f64,
        ("RENDER_CHANNELS", None) => render_fmt(p, 2).unwrap_or(2.0),
        ("RENDER_SRATE", None) => render_fmt(p, 3).unwrap_or(0.0),
        ("RENDER_BOUNDSFLAG", None) => {
            scythe_core::render_settings::RenderSettings::from_project(p).bounds.code() as f64
        }
        ("RENDER_SETTINGS", None) => scythe_core::render_settings::RenderSettings::from_project(p).source.0 as f64,
        ("RENDER_TAILMS", None) => 1000.0,
        _ => 0.0,
    }
}

/// `GetSetProjectInfo_String` for the settings Scythe has; `None` for other keys.
fn project_text(s: &mut State, key: &str, set: Option<String>) -> Option<String> {
    if let Some(n) = key.strip_prefix("TRACK_GROUP_NAME:") {
        let g = n.trim().parse::<usize>().ok().filter(|g| (1..=scythe_core::groups::GROUP_COUNT).contains(g))? - 1;
        if let Some(v) = set {
            if v.is_empty() {
                s.project.group_names.remove(&g);
            } else {
                s.project.group_names.insert(g, v.clone());
            }
            s.changed = true;
            return Some(v);
        }
        return Some(s.project.group_name(g).unwrap_or_default().to_owned());
    }
    let line = match key {
        "PROJECT_TITLE" => "TITLE",
        "PROJECT_AUTHOR" => "AUTHOR",
        "RENDER_FILE" => "RENDER_FILE",
        "RENDER_PATTERN" => "RENDER_PATTERN",
        "RECORD_PATH" => "RECORD_PATH",
        "PROJECT_NAME" => {
            return Some(
                s.project
                    .path
                    .as_ref()
                    .and_then(|p| p.file_stem())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            );
        }
        _ => return None,
    };
    if let Some(v) = set {
        set_extra_line(&mut s.project, line, &v);
        s.changed = true;
        return Some(v);
    }
    Some(
        extra_line(&s.project, line)
            .unwrap_or_else(|| if line == "RECORD_PATH" { "Media".into() } else { String::new() }),
    )
}

/// The project notes (`<NOTES` with `|`-prefixed lines), set first when `set` is given.
fn notes(s: &mut State, set: Option<String>) -> String {
    let at = s.project.extra.iter().position(|n| matches!(n, Node::Chunk(c) if c.name() == "NOTES"));
    if let Some(text) = set {
        let mut c = Chunk::new("NOTES", &["0", "2"]);
        c.children = text.split('\n').map(|l| Node::Line(format!("|{l}"))).collect();
        match at {
            Some(i) => s.project.extra[i] = Node::Chunk(c),
            None => s.project.extra.push(Node::Chunk(c)),
        }
        s.changed = true;
        return text;
    }
    let Some(Node::Chunk(c)) = at.map(|i| &s.project.extra[i]) else { return String::new() };
    let lines: Vec<&str> = c
        .children
        .iter()
        .filter_map(|n| match n {
            Node::Line(l) => Some(l.trim_start().strip_prefix('|').unwrap_or(l.trim_start())),
            Node::Chunk(_) => None,
        })
        .collect();
    lines.join("\n")
}

// ---- project extended state ----------------------------------------------------------------

/// The keys and values a project keeps for `ext` (`<EXTSTATE`).
fn proj_ext(p: &Project, ext: &str) -> Vec<(String, String)> {
    p.ext_state_section(ext)
}

/// `SetProjExtState`: an empty key clears `ext`, an empty value removes the key. Returns how
/// many keys `ext` holds.
fn proj_ext_set(s: &mut State, ext: &str, key: &str, value: &str) -> usize {
    if key.is_empty() {
        for (k, _) in s.project.ext_state_section(ext) {
            s.project.set_ext_state(ext, &k, None);
        }
    } else {
        s.project.set_ext_state(ext, key, Some(value));
    }
    s.changed = true;
    s.project.ext_state_section(ext).len()
}

// ---- extended state ------------------------------------------------------------------------

/// `GetExtState` and friends: values kept for the whole session, those set to persist also in
/// a file like REAPER's `reaper-extstate.ini`.
#[derive(Default)]
pub(super) struct ExtState {
    file: Option<PathBuf>,
    /// Section, then key: the value and whether it persists.
    values: BTreeMap<String, BTreeMap<String, (String, bool)>>,
}

impl ExtState {
    fn load(file: Option<PathBuf>) -> Self {
        let mut values: BTreeMap<String, BTreeMap<String, (String, bool)>> = BTreeMap::new();
        let text = file.as_ref().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default();
        let mut section = None;
        for line in text.lines() {
            let line = line.trim();
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = Some(name.to_owned());
            } else if let (Some(sec), Some((k, v))) = (&section, line.split_once('=')) {
                values.entry(sec.clone()).or_default().insert(k.to_owned(), (v.to_owned(), true));
            }
        }
        Self { file, values }
    }

    fn get(&self, section: &str, key: &str) -> Option<String> {
        self.values.get(section)?.get(key).map(|(v, _)| v.clone())
    }

    fn set(&mut self, section: &str, key: &str, value: Option<String>, persist: bool) {
        let entries = self.values.entry(section.to_owned()).or_default();
        let was_persistent = entries.get(key).is_some_and(|(_, p)| *p);
        match value {
            Some(v) => {
                entries.insert(key.to_owned(), (v, persist));
            }
            None => {
                entries.remove(key);
            }
        }
        if persist || was_persistent {
            self.save();
        }
    }

    fn save(&self) {
        let Some(file) = &self.file else { return };
        let mut text = String::new();
        for (section, entries) in &self.values {
            let kept: Vec<_> = entries.iter().filter(|(_, (_, p))| *p).collect();
            if kept.is_empty() {
                continue;
            }
            text += &format!("[{section}]\n");
            for (k, (v, _)) in kept {
                text += &format!("{k}={}\n", v.replace(['\n', '\r'], " "));
            }
            text.push('\n');
        }
        let _ = file.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(file, text));
    }
}

static EXT_STATE: Mutex<Option<ExtState>> = Mutex::new(None);

fn ext_state<R>(s: &State, f: impl FnOnce(&mut ExtState) -> R) -> R {
    let mut guard = EXT_STATE.lock().unwrap_or_else(|e| e.into_inner());
    let file = s.ctx.ext_state_file.clone();
    if guard.as_ref().is_none_or(|e| e.file != file) {
        *guard = Some(ExtState::load(file));
    }
    f(guard.as_mut().unwrap())
}

// ---- commands ------------------------------------------------------------------------------

/// `Main_OnCommand`: runs the REAPER actions that edit what the script holds; others are left
/// for the app, which runs those it has once the script's changes are in
/// (`ScriptOutcome::commands`).
pub(super) fn command(s: &mut State, id: i64) {
    let selected_tracks: Vec<TrackId> =
        s.project.tracks.iter().map(|t| t.id).filter(|t| s.sel_tracks.contains(t)).collect();
    match id {
        // Transport.
        1007 => s.out.play = Some(true),
        1016 | 1008 => s.out.play = Some(false),
        40044 | 40073 => s.out.play = Some(!s.playing()),
        1068 => {
            s.project.loop_enabled ^= true;
            s.changed = true;
        }
        40042 => s.out.cursor = Some(0.0),
        40043 => s.out.cursor = Some(s.project.length()),
        // Tracks.
        40001 | 40702 => {
            let at = match id {
                40001 => selected_tracks
                    .last()
                    .and_then(|t| s.project.track_index(*t))
                    .map_or(s.project.tracks.len(), |i| i + 1),
                _ => s.project.tracks.len(),
            };
            let new = s.project.add_track("");
            let t = s.project.tracks.pop().unwrap();
            s.project.tracks.insert(at, t);
            s.sel_tracks = vec![new];
            s.sel_changed = true;
            s.changed = true;
        }
        40005 => {
            for t in &selected_tracks {
                s.project.remove_track(*t);
                s.ctx.fx.remove(t);
            }
            s.sel_tracks.retain(|t| !selected_tracks.contains(t));
            s.sel_changed |= !selected_tracks.is_empty();
            s.changed |= !selected_tracks.is_empty();
        }
        6 | 7 | 9 => {
            for t in s.project.tracks.iter_mut().filter(|t| selected_tracks.contains(&t.id)) {
                match id {
                    6 => t.muted ^= true,
                    7 => t.solo ^= true,
                    _ => t.armed ^= true,
                }
            }
            s.changed |= !selected_tracks.is_empty();
        }
        14 => {
            s.project.master.muted ^= true;
            s.changed = true;
        }
        40340 => {
            s.project.tracks.iter_mut().for_each(|t| t.solo = false);
            s.changed = true;
        }
        40296 | 40297 => select_all_tracks(s, id == 40296),
        40406 | 40407 => {
            let kind = if id == 40406 { scythe_core::EnvelopeKind::Volume } else { scythe_core::EnvelopeKind::Pan };
            for t in s.project.tracks.iter_mut().filter(|t| selected_tracks.contains(&t.id)) {
                let env = t.envelope_mut(kind);
                // REAPER gives a new envelope a point at the start, at the track's setting.
                if env.points.is_empty() {
                    env.points.push(scythe_core::EnvPoint::new(0.0, kind.neutral()));
                }
                env.visible ^= true;
            }
            s.changed |= !selected_tracks.is_empty();
        }
        // Items.
        40012 | 40757 | 40759 | 43178 | 40186 => {
            let at = if s.playing() { s.ctx.play_position } else { s.cursor() };
            let items = selection(s);
            let right = s.project.split_items(&items, at);
            if id == 40012 && !right.is_empty() {
                s.sel_items = right;
                s.sel_changed = true;
            }
            s.changed = true;
        }
        40175 | 40719 | 40720 => {
            let items = selection(s);
            for it in s.project.tracks.iter_mut().flat_map(|t| t.items.iter_mut()).filter(|i| items.contains(&i.id)) {
                it.muted = match id {
                    40719 => true,
                    40720 => false,
                    _ => !it.muted,
                };
            }
            s.changed = true;
        }
        40182 | 40289 => {
            let all: Vec<_> = if id == 40182 {
                s.project.tracks.iter().flat_map(|t| t.items.iter().map(|i| i.id)).collect()
            } else {
                Vec::new()
            };
            s.sel_items = all;
            s.sel_changed = true;
        }
        // Markers and the time selection.
        40157 => {
            let at = if s.playing() { s.ctx.play_position } else { s.cursor() };
            s.project.add_marker(at, "");
            s.changed = true;
        }
        40020 | 40635 => {
            s.project.time_selection = None;
            s.changed = true;
        }
        // Tab-to-transient needs the media, which the app has.
        40375 | 40376 => s.out.commands.push(id),
        _ if u32::try_from(id).is_ok_and(scythe_core::edit::actions::handles) => {
            let none = scythe_core::edit::actions::no_transients;
            let items = selection(s);
            let mut ctx = scythe_core::edit::actions::EditContext::new(items.clone(), s.cursor(), &none);
            scythe_core::edit::actions::run(&mut s.project, &mut ctx, id as u32);
            if ctx.items != items {
                s.sel_items = ctx.items;
                s.sel_changed = true;
            }
            s.changed = true;
        }
        _ => s.out.commands.push(id),
    }
}

// ---- tempo ---------------------------------------------------------------------------------

fn tempo(s: &mut State, name: &str, a: &Args) -> Option<Ret> {
    let tl = s.project.tempo.timeline();
    let sig_at = |t: f64| s.project.tempo.time_sig_at(t);
    Some(match name {
        "Master_GetTempo" => ret(Val::Num(s.project.tempo.bpm_at(0.0))),
        "Master_GetPlayRate" => ret(Val::Num(s.project.playrate.rate)),
        "CSurf_OnPlayRateChange" => {
            s.project.playrate.rate = a.num(0).clamp(PlayRate::MIN, PlayRate::MAX);
            s.changed = true;
            ret(Val::Nil)
        }
        "SetCurrentBPM" => {
            s.project.change_tempo(|t| t.set_base_bpm(a.num(1).clamp(1.0, 960.0)));
            s.changed = true;
            ret(Val::Nil)
        }
        "TimeMap2_timeToQN" => ret(Val::Num(tl.qn_at(a.num(1)))),
        "TimeMap2_QNToTime" => ret(Val::Num(tl.time_at_qn(a.num(1)))),
        "TimeMap_timeToQN" => ret(Val::Num(tl.qn_at(a.num(0)))),
        "TimeMap_QNToTime" => ret(Val::Num(tl.time_at_qn(a.num(0)))),
        "TimeMap_GetDividedBpmAtTime" | "TimeMap2_GetDividedBpmAtTime" => {
            let t = a.num((name == "TimeMap2_GetDividedBpmAtTime") as usize);
            ret(Val::Num(tl.bpm_at(t) * sig_at(t).1 as f64 / 4.0))
        }
        "TimeMap_GetTimeSigAtTime" => {
            let t = a.num(1);
            let (num, denom) = sig_at(t);
            (Val::Nil, vec![Val::Int(num as i64), Val::Int(denom as i64), Val::Num(tl.bpm_at(t))])
        }
        "GetProjectTimeSignature2" => {
            let (num, _) = sig_at(0.0);
            (Val::Nil, vec![Val::Num(s.project.tempo.bpm_at(0.0)), Val::Num(num as f64)])
        }
        "TimeMap2_timeToBeats" => {
            let t = a.num(1);
            let bar = tl.bar_beat(t).bar;
            let start = tl.bar_start(bar);
            let (num, denom) = sig_at(start);
            let qn = tl.qn_at(t);
            let beats = (qn - tl.qn_at(start)) * denom as f64 / 4.0;
            (Val::Num(beats), vec![Val::Int(bar - 1), Val::Int(num as i64), Val::Num(qn), Val::Int(denom as i64)])
        }
        "TimeMap2_beatsToTime" => {
            let beats = a.num(1);
            match a.opt(2) {
                Some(m) => {
                    let start = tl.bar_start(m as i64 + 1);
                    let (_, denom) = sig_at(start);
                    ret(Val::Num(tl.time_at_qn(tl.qn_at(start) + beats * 4.0 / denom.max(1) as f64)))
                }
                None => ret(Val::Num(tl.time_at_qn(beats))),
            }
        }
        "TimeMap_GetMeasureInfo" => {
            let bar = a.int(1) + 1;
            let (t0, t1) = (tl.bar_start(bar), tl.bar_start(bar + 1));
            let (num, denom) = sig_at(t0);
            (
                Val::Num(t0),
                vec![
                    Val::Num(tl.qn_at(t0)),
                    Val::Num(tl.qn_at(t1)),
                    Val::Int(num as i64),
                    Val::Int(denom as i64),
                    Val::Num(tl.bpm_at(t0)),
                ],
            )
        }
        "TimeMap2_GetNextChangeTime" => {
            let t = a.num(1);
            let next =
                s.project.tempo.points.iter().map(|p| p.time).filter(|&x| x > t + 1e-9).fold(f64::INFINITY, f64::min);
            ret(Val::Num(if next.is_finite() { next } else { -1.0 }))
        }
        "CountTempoTimeSigMarkers" => ret(Val::Int(s.project.tempo.points.len() as i64)),
        "GetTempoTimeSigMarker" => {
            let Some(p) = index(s.project.tempo.points.len(), a.int(1)).map(|i| s.project.tempo.points[i].clone())
            else {
                return Some((
                    Val::Bool(false),
                    vec![
                        Val::Num(0.0),
                        Val::Int(0),
                        Val::Num(0.0),
                        Val::Num(0.0),
                        Val::Int(0),
                        Val::Int(0),
                        Val::Bool(false),
                    ],
                ));
            };
            let bar = tl.bar_beat(p.time).bar;
            let start = tl.bar_start(bar);
            let (_, denom) = sig_at(start);
            let beat = (tl.qn_at(p.time) - tl.qn_at(start)) * denom as f64 / 4.0;
            let (num, den) = p.time_sig.map_or((-1, -1), |(n, d)| (n as i64, d as i64));
            (
                Val::Bool(true),
                vec![
                    Val::Num(p.time),
                    Val::Int(bar - 1),
                    Val::Num(beat),
                    Val::Num(p.bpm),
                    Val::Int(num),
                    Val::Int(den),
                    Val::Bool(p.linear),
                ],
            )
        }
        "SetTempoTimeSigMarker" | "AddTempoTimeSigMarker" => {
            let (i, mut time, bpm, num, denom, linear) = if name == "AddTempoTimeSigMarker" {
                (-1, a.num(1), a.num(2), a.int(3), a.int(4), a.bool(5))
            } else {
                (a.int(1), a.num(2), a.num(5), a.int(6), a.int(7), a.bool(8))
            };
            if time < 0.0 && name == "SetTempoTimeSigMarker" {
                let (bar, beat) = (a.int(3).max(0) + 1, a.num(4));
                let start = tl.bar_start(bar);
                let (_, d) = sig_at(start);
                time = tl.time_at_qn(tl.qn_at(start) + beat * 4.0 / d.max(1) as f64);
            }
            let sig = (num > 0 && denom > 0).then_some((num as u32, denom as u32));
            if sig.is_some() {
                // A time signature starts a measure: the marker moves to the next bar line.
                let bb = tl.bar_beat(time);
                if bb.beat != 1 || bb.fraction > 1e-6 {
                    time = tl.bar_start(bb.bar + 1);
                }
            }
            let point = scythe_core::tempo::TempoPoint {
                linear,
                time_sig: sig,
                ..scythe_core::tempo::TempoPoint::new(time.max(0.0), bpm.max(1.0))
            };
            let Some(k) = (i >= 0).then(|| index(s.project.tempo.points.len(), i)) else {
                s.project.change_tempo(|tempo| {
                    if tempo.points.is_empty() && time > 0.0 {
                        tempo.points.push(scythe_core::tempo::TempoPoint {
                            time_sig: Some((tempo.beats_per_bar, tempo.beat_unit)),
                            ..scythe_core::tempo::TempoPoint::new(0.0, tempo.bpm)
                        });
                    }
                    tempo.points.push(point);
                    tempo.settle();
                });
                s.changed = true;
                return Some(ret(Val::Bool(true)));
            };
            let Some(k) = k else { return Some(ret(Val::Bool(false))) };
            s.project.change_tempo(|tempo| {
                let raw = std::mem::take(&mut tempo.points[k].raw);
                tempo.points[k] = scythe_core::tempo::TempoPoint { raw, ..point };
                tempo.settle();
            });
            s.changed = true;
            ret(Val::Bool(true))
        }
        "DeleteTempoTimeSigMarker" => match index(s.project.tempo.points.len(), a.int(1)) {
            Some(i) => {
                s.project.change_tempo(|tempo| {
                    tempo.points.remove(i);
                });
                s.changed = true;
                ret(Val::Bool(true))
            }
            None => ret(Val::Bool(false)),
        },
        _ => return None,
    })
}

// ---- markers and regions -------------------------------------------------------------------

fn marker_color(c: i64) -> Option<scythe_core::Color> {
    color_from_native(c)
}

fn next_index(p: &Project, region: bool) -> u32 {
    p.markers.iter().filter(|m| m.region_end.is_some() == region).map(|m| m.index).max().unwrap_or(0) + 1
}

fn sort_markers(p: &mut Project) {
    p.markers.sort_by(|a, b| a.position.total_cmp(&b.position));
}

/// Sets a marker's fields: an empty name, a zero color or a negative index leave theirs as
/// they are (flag 1 of `SetProjectMarker4` clears the name).
#[allow(clippy::too_many_arguments)]
fn update_marker(m: &mut Marker, region: bool, pos: f64, end: f64, name: &str, color: i64, clear_name: bool) {
    m.position = pos;
    m.region_end = region.then_some(end.max(pos));
    if clear_name {
        m.name.clear();
    } else if !name.is_empty() {
        m.name = name.to_owned();
    }
    if color != 0 {
        m.color = marker_color(color);
    }
}

fn enum_marker(p: &Project, i: i64) -> Ret {
    match index(p.markers.len(), i).map(|k| &p.markers[k]) {
        Some(m) => (
            Val::Int(i + 1),
            vec![
                Val::Bool(m.region_end.is_some()),
                Val::Num(m.position),
                Val::Num(m.region_end.unwrap_or(0.0)),
                Val::Str(m.name.clone()),
                Val::Int(m.index as i64),
                Val::Int(color_to_native(m.color)),
            ],
        ),
        None => (
            Val::Int(0),
            vec![Val::Bool(false), Val::Num(0.0), Val::Num(0.0), Val::Str(String::new()), Val::Int(0), Val::Int(0)],
        ),
    }
}

fn markers(s: &mut State, name: &str, a: &Args) -> Option<Ret> {
    Some(match name {
        "AddProjectMarker" | "AddProjectMarker2" => {
            let region = a.bool(1);
            let want = a.int(5);
            let index = if want > 0 { want as u32 } else { next_index(&s.project, region) };
            let color = if name == "AddProjectMarker2" { marker_color(a.int(6)) } else { None };
            s.project.markers.push(Marker {
                index,
                position: a.num(2),
                name: a.text(4),
                color,
                region_end: region.then_some(a.num(3).max(a.num(2))),
                render_matrix: Vec::new(),
                lines: Default::default(),
            });
            sort_markers(&mut s.project);
            s.changed = true;
            ret(Val::Int(index as i64))
        }
        "CountProjectMarkers" => {
            let regions = s.project.markers.iter().filter(|m| m.region_end.is_some()).count() as i64;
            let total = s.project.markers.len() as i64;
            (Val::Int(total), vec![Val::Int(total - regions), Val::Int(regions)])
        }
        "EnumProjectMarkers" | "EnumProjectMarkers2" | "EnumProjectMarkers3" => {
            let i = a.int((name != "EnumProjectMarkers") as usize);
            let (v, mut outs) = enum_marker(&s.project, i);
            if name != "EnumProjectMarkers3" {
                outs.pop();
            }
            (v, outs)
        }
        "SetProjectMarker" | "SetProjectMarker2" | "SetProjectMarker3" | "SetProjectMarker4" => {
            let b = (name != "SetProjectMarker") as usize;
            let (number, region) = (a.int(b), a.bool(b + 1));
            let color = if matches!(name, "SetProjectMarker3" | "SetProjectMarker4") { a.int(b + 5) } else { 0 };
            let clear = name == "SetProjectMarker4" && a.int(b + 6) & 1 != 0;
            let found =
                s.project.markers.iter_mut().find(|m| m.index as i64 == number && m.region_end.is_some() == region);
            let Some(m) = found else { return Some(ret(Val::Bool(false))) };
            update_marker(m, region, a.num(b + 2), a.num(b + 3), &a.text(b + 4), color, clear);
            sort_markers(&mut s.project);
            s.changed = true;
            ret(Val::Bool(true))
        }
        "SetProjectMarkerByIndex" | "SetProjectMarkerByIndex2" => {
            let Some(k) = index(s.project.markers.len(), a.int(1)) else { return Some(ret(Val::Bool(false))) };
            let clear = name == "SetProjectMarkerByIndex2" && a.int(8) & 1 != 0;
            let m = &mut s.project.markers[k];
            update_marker(m, a.bool(2), a.num(3), a.num(4), &a.text(6), a.int(7), clear);
            if a.int(5) >= 0 {
                m.index = a.int(5) as u32;
            }
            sort_markers(&mut s.project);
            s.changed = true;
            ret(Val::Bool(true))
        }
        "DeleteProjectMarker" => {
            let (number, region) = (a.int(1), a.bool(2));
            let at =
                s.project.markers.iter().position(|m| m.index as i64 == number && m.region_end.is_some() == region);
            match at {
                Some(i) => {
                    s.project.markers.remove(i);
                    s.changed = true;
                    ret(Val::Bool(true))
                }
                None => ret(Val::Bool(false)),
            }
        }
        "DeleteProjectMarkerByIndex" => match index(s.project.markers.len(), a.int(1)) {
            Some(i) => {
                s.project.markers.remove(i);
                s.changed = true;
                ret(Val::Bool(true))
            }
            None => ret(Val::Bool(false)),
        },
        "GetLastMarkerAndCurRegion" => {
            let t = a.num(1);
            let ms = &s.project.markers;
            let marker = ms
                .iter()
                .enumerate()
                .filter(|(_, m)| m.region_end.is_none() && m.position <= t)
                .map(|(i, _)| i as i64)
                .next_back();
            let region = ms
                .iter()
                .enumerate()
                .filter(|(_, m)| m.region_end.is_some_and(|e| m.position <= t && t < e))
                .map(|(i, _)| i as i64)
                .next_back();
            (Val::Nil, vec![Val::Int(marker.unwrap_or(-1)), Val::Int(region.unwrap_or(-1))])
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::super::tests::{api, project, state};
    use super::*;

    fn s(v: &str) -> Val {
        Val::Str(v.into())
    }

    #[test]
    fn markers_and_regions_as_reaper_keeps_them() {
        let mut st = state(project());
        let red = 0xff0000 | 0x100_0000;
        let add2 = |pos: f64, end: f64, region: bool, name: &str, want: i64, color: i64| {
            vec![Val::Proj, Val::Bool(region), Val::Num(pos), Val::Num(end), s(name), Val::Int(want), Val::Int(color)]
        };
        assert_eq!(api(&mut st, "AddProjectMarker2", &add2(2.0, 0.0, false, "M", -1, red)).0, Val::Int(1));
        assert_eq!(api(&mut st, "AddProjectMarker2", &add2(1.0, 4.0, true, "R", -1, 0)).0, Val::Int(1));
        assert_eq!(api(&mut st, "AddProjectMarker", &add2(0.5, 0.0, false, "First", 5, 0)[..6]).0, Val::Int(5));
        let (n, outs) = api(&mut st, "EnumProjectMarkers3", &[Val::Proj, Val::Int(2)]);
        assert_eq!(n, Val::Int(3));
        assert_eq!(outs, [Val::Bool(false), Val::Num(2.0), Val::Num(0.0), s("M"), Val::Int(1), Val::Int(33_488_896)]);
        assert_eq!(
            api(&mut st, "GetLastMarkerAndCurRegion", &[Val::Proj, Val::Num(2.5)]).1,
            [Val::Int(2), Val::Int(1)]
        );
        let set = [Val::Proj, Val::Int(1), Val::Bool(false), Val::Num(2.5), Val::Num(0.0), s("M2"), Val::Int(0)];
        assert_eq!(api(&mut st, "SetProjectMarker3", &set).0, Val::Bool(true));
        let by_index =
            [Val::Proj, Val::Int(0), Val::Bool(false), Val::Num(0.75), Val::Num(0.0), Val::Int(5), s(""), Val::Int(0)];
        api(&mut st, "SetProjectMarkerByIndex", &by_index);
        assert_eq!(api(&mut st, "EnumProjectMarkers3", &[Val::Proj, Val::Int(0)]).1[3], s("First"));
        assert_eq!(api(&mut st, "EnumProjectMarkers3", &[Val::Proj, Val::Int(2)]).1[5], Val::Int(33_488_896));
        assert_eq!(api(&mut st, "DeleteProjectMarker", &[Val::Proj, Val::Int(1), Val::Bool(true)]).0, Val::Bool(true));
        assert_eq!(api(&mut st, "CountProjectMarkers", &[Val::Proj]).1, [Val::Int(2), Val::Int(0)]);
    }

    #[test]
    fn tempo_markers_and_measures() {
        let mut st = state(Project::default());
        let set = |t: f64, bpm: f64, num: i64, den: i64, linear: bool| {
            [
                Val::Proj,
                Val::Int(-1),
                Val::Num(t),
                Val::Int(-1),
                Val::Num(-1.0),
                Val::Num(bpm),
                Val::Int(num),
                Val::Int(den),
                Val::Bool(linear),
            ]
        };
        api(&mut st, "SetTempoTimeSigMarker", &set(2.0, 90.0, 0, 0, false));
        api(&mut st, "SetTempoTimeSigMarker", &set(4.0, 100.0, 3, 4, true));
        // REAPER 7.73 gives the same for this map (a marker at 0 is added for the old tempo).
        assert_eq!(api(&mut st, "CountTempoTimeSigMarkers", &[Val::Proj]).0, Val::Int(3));
        let (_, m) = api(&mut st, "GetTempoTimeSigMarker", &[Val::Proj, Val::Int(2)]);
        assert!((m[0].num() - 4.666_666_666_666_667).abs() < 1e-9, "{m:?}");
        assert_eq!(m[1..], [Val::Int(2), m[2].clone(), Val::Num(100.0), Val::Int(3), Val::Int(4), Val::Bool(true)]);
        assert!(m[2].num().abs() < 1e-9);
        let (t, info) = api(&mut st, "TimeMap_GetMeasureInfo", &[Val::Proj, Val::Int(2)]);
        assert!((t.num() - 4.666_666_666_666_667).abs() < 1e-9);
        assert_eq!(info[2..4], [Val::Int(3), Val::Int(4)]);
        assert!((info[0].num() - 8.0).abs() < 1e-9 && (info[1].num() - 11.0).abs() < 1e-9, "{info:?}");
        let (beats, outs) = api(&mut st, "TimeMap2_timeToBeats", &[Val::Proj, Val::Num(5.0)]);
        assert!((beats.num() - 0.555_555_555_555_6).abs() < 1e-6, "{beats:?}");
        assert_eq!((outs[0].clone(), outs[1].clone(), outs[3].clone()), (Val::Int(2), Val::Int(3), Val::Int(4)));
        let t = api(&mut st, "TimeMap2_beatsToTime", &[Val::Proj, Val::Num(1.0), Val::Int(2)]).0;
        assert!((t.num() - 5.266_666_666_666_667).abs() < 1e-9, "{t:?}");
        assert_eq!(api(&mut st, "TimeMap_GetDividedBpmAtTime", &[Val::Num(3.0)]).0, Val::Num(90.0));
        let next = api(&mut st, "TimeMap2_GetNextChangeTime", &[Val::Proj, Val::Num(0.0)]).0;
        assert_eq!(next, Val::Num(2.0));
    }

    #[test]
    fn strings_as_reaper_formats_them() {
        let mut st = state(Project::default());
        let one = |st: &mut State, f: &str, args: &[Val]| api(st, f, args).1[0].clone();
        assert_eq!(one(&mut st, "mkvolstr", &[s(""), Val::Num(0.5)]), s("-6.02dB"));
        assert_eq!(one(&mut st, "mkvolstr", &[s(""), Val::Num(0.0)]), s("-inf dB"));
        assert_eq!(one(&mut st, "mkvolstr", &[s(""), Val::Num(0.25)]), s("-12.0dB"));
        assert_eq!(one(&mut st, "mkvolstr", &[s(""), Val::Num(1.0)]), s("0.00dB"));
        assert_eq!(one(&mut st, "mkvolstr", &[s(""), Val::Num(2.0)]), s("+6.02dB"));
        assert_eq!(one(&mut st, "mkpanstr", &[s(""), Val::Num(-0.5)]), s("50%L"));
        assert_eq!(one(&mut st, "mkpanstr", &[s(""), Val::Num(0.0)]), s("center"));
        assert_eq!(one(&mut st, "format_timestr", &[Val::Num(75.5), s("")]), s("1:15.500"));
        let pos = |st: &mut State, mode| one(st, "format_timestr_pos", &[Val::Num(75.5), s(""), Val::Int(mode)]);
        assert_eq!((pos(&mut st, 3), pos(&mut st, 5)), (s("75.500"), s("00:01:15:15")));
        assert_eq!(api(&mut st, "parse_timestr", &[s("1:15.5")]).0, Val::Num(75.5));
        assert_eq!(api(&mut st, "ColorToNative", &[Val::Int(1), Val::Int(2), Val::Int(3)]).0, Val::Int(66051));
    }

    #[test]
    fn project_ext_state_is_stored_in_the_project() {
        let mut st = state(project());
        let set = |k: &str, v: &str| [Val::Proj, s("ScytheTest"), s(k), s(v)];
        assert_eq!(api(&mut st, "SetProjExtState", &set("key1", "value 1")).0, Val::Int(1));
        assert_eq!(api(&mut st, "SetProjExtState", &set("key2", "v2")).0, Val::Int(2));
        let get = api(&mut st, "GetProjExtState", &[Val::Proj, s("scythetest"), s("KEY1")]);
        assert_eq!(get, (Val::Int(1), vec![s("value 1")]));
        let text = scythe_core::rpp::project_to_rpp(&st.project);
        assert!(
            text.contains("<EXTSTATE") && text.contains("<SCYTHETEST") && text.contains("KEY1 \"value 1\""),
            "{text}"
        );
        let (ok, kv) = api(&mut st, "EnumProjExtState", &[Val::Proj, s("ScytheTest"), Val::Int(1)]);
        assert_eq!((ok, kv), (Val::Bool(true), vec![s("KEY2"), s("v2")]));
        api(&mut st, "SetProjExtState", &set("", ""));
        assert_eq!(api(&mut st, "EnumProjExtState", &[Val::Proj, s("ScytheTest"), Val::Int(0)]).0, Val::Bool(false));
    }

    #[test]
    fn ext_state_persists_to_a_file() {
        let dir = std::env::temp_dir().join(format!("scythe-extstate-{}", std::process::id()));
        let file = dir.join("reaper-extstate.ini");
        let ctx = super::super::ScriptContext { ext_state_file: Some(file.clone()), ..Default::default() };
        let mut st = State::new(&project(), ctx);
        api(&mut st, "SetExtState", &[s("ScytheExtTest"), s("kept"), s("yes"), Val::Bool(true)]);
        api(&mut st, "SetExtState", &[s("ScytheExtTest"), s("temp"), s("t"), Val::Bool(false)]);
        assert_eq!(api(&mut st, "GetExtState", &[s("ScytheExtTest"), s("temp")]).0, s("t"));
        assert_eq!(api(&mut st, "HasExtState", &[s("ScytheExtTest"), s("nope")]).0, Val::Bool(false));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "[ScytheExtTest]\nkept=yes\n\n");
        api(&mut st, "DeleteExtState", &[s("ScytheExtTest"), s("kept"), Val::Bool(true)]);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn commands_edit_what_the_script_holds_or_go_to_the_app() {
        let p = project();
        let (guitar, item) = (p.tracks[0].id, p.tracks[0].items[0].id);
        let mut st = State::new(
            &p,
            super::super::ScriptContext {
                selected_tracks: vec![guitar],
                selected_items: vec![item],
                cursor: 2.0,
                ..Default::default()
            },
        );
        api(&mut st, "Main_OnCommand", &[Val::Int(40012), Val::Int(0)]);
        assert_eq!(st.project.tracks[0].items.len(), 2, "split at the cursor");
        api(&mut st, "Main_OnCommand", &[Val::Int(6), Val::Int(0)]);
        assert!(st.project.tracks[0].muted);
        api(&mut st, "Main_OnCommand", &[Val::Int(40001), Val::Int(0)]);
        assert_eq!(st.project.tracks.len(), 3);
        assert_eq!(st.sel_tracks, [st.project.tracks[1].id], "the new track follows the selected one");
        api(&mut st, "Main_OnCommand", &[Val::Int(40029), Val::Int(0)]);
        api(&mut st, "Main_OnCommand", &[Val::Int(99999), Val::Int(0)]);
        assert_eq!(st.out.commands, [40029, 99999]);
    }

    /// `ReorderSelectedTracks` results of REAPER 7.73 on T1 (folder of T2, T3) and T4.
    #[test]
    fn reorder_selected_tracks_matches_reaper() {
        let depths =
            |st: &State| st.project.tracks.iter().map(|t| (t.name.clone(), t.folder_depth)).collect::<Vec<_>>();
        let named = |v: &[(&str, i32)]| v.iter().map(|(n, d)| ((*n).to_owned(), *d)).collect::<Vec<_>>();
        for (mode, want) in [
            (0, [("T1", 1), ("T2", 0), ("T3", -1), ("T4", 0)]),
            (1, [("T1", 1), ("T2", 0), ("T3", 1), ("T4", -2)]),
            (2, [("T1", 1), ("T2", 0), ("T3", 0), ("T4", -1)]),
        ] {
            let mut p = scythe_core::Project::default();
            for (name, depth) in [("T1", 1), ("T2", 0), ("T3", -1), ("T4", 0)] {
                let id = p.add_track(name);
                p.track_mut(id).unwrap().folder_depth = depth;
            }
            let t4 = p.tracks[3].id;
            let mut st =
                State::new(&p, super::super::ScriptContext { selected_tracks: vec![t4], ..Default::default() });
            api(&mut st, "ReorderSelectedTracks", &[Val::Int(3), Val::Int(mode)]);
            assert_eq!(depths(&st), named(&want), "mode {mode}");
        }
    }

    /// REAPER 7.73 moves the same items this way after the same calls (Lua probe of D_POSITION,
    /// D_LENGTH and D_PLAYRATE with the project timebase on beats).
    #[test]
    fn tempo_edits_move_items_by_their_timebase() {
        let rpp = "<REAPER_PROJECT 0.1 \"7.73\" 0\n  TIMELOCKMODE 1\n  TEMPOENVLOCKMODE 1\n  TEMPO 120 4 4\n  <TEMPOENVEX\n    PT 0 120 1\n    PT 4 120 1\n  >\n  <TRACK\n    <ITEM\n      POSITION 2\n      LENGTH 1\n    >\n    <ITEM\n      POSITION 2\n      LENGTH 1\n      BEAT 0\n    >\n    <ITEM\n      POSITION 2\n      LENGTH 1\n      BEAT 2\n    >\n  >\n>\n";
        let set = |i: i64, time: f64, bpm: f64, linear: bool| {
            let sig = if i == 0 { 4 } else { 0 };
            [
                Val::Proj,
                Val::Int(i),
                Val::Num(time),
                Val::Int(-1),
                Val::Num(-1.0),
                Val::Num(bpm),
                Val::Int(sig),
                Val::Int(sig),
                Val::Bool(linear),
            ]
        };
        let items = |st: &mut State| -> Vec<[f64; 3]> {
            (0..3)
                .map(|n| {
                    let item = api(st, "GetTrackMediaItem", &[Val::Track(st.project.tracks[0].id), Val::Int(n)]).0;
                    let take = api(st, "GetActiveTake", std::slice::from_ref(&item)).0;
                    let get =
                        |st: &mut State, k: &str| api(st, "GetMediaItemInfo_Value", &[item.clone(), s(k)]).0.num();
                    let rate = api(st, "GetMediaItemTakeInfo_Value", &[take, s("D_PLAYRATE")]).0.num();
                    [get(st, "D_POSITION"), get(st, "D_LENGTH"), rate]
                })
                .collect()
        };
        let mut st = state(scythe_core::rpp::project_from_rpp(rpp).unwrap());
        api(&mut st, "SetTempoTimeSigMarker", &set(0, 0.0, 60.0, false));
        assert_eq!(items(&mut st), [[4.0, 2.0, 0.5], [2.0, 1.0, 1.0], [4.0, 1.0, 1.0]]);
        let (_, m) = api(&mut st, "GetTempoTimeSigMarker", &[Val::Proj, Val::Int(1)]);
        assert_eq!(m[0], Val::Num(8.0));

        let mut st = state(scythe_core::rpp::project_from_rpp(rpp).unwrap());
        api(&mut st, "SetTempoTimeSigMarker", &set(0, 0.0, 120.0, true));
        api(&mut st, "SetTempoTimeSigMarker", &set(1, 4.0, 60.0, false));
        let got = items(&mut st);
        for (g, want) in got[0].iter().zip([2.343145751, 1.656854249, 0.603553391]) {
            assert!((g - want).abs() < 1e-8, "{got:?}");
        }
    }
}
