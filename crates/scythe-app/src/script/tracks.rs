//! Tracks, their sends, effects and envelopes.

use scythe_core::automation_item::{AutomationItem, AutomationPool, Scale, capture};
use scythe_core::model::GlobalAutomation;
use scythe_core::rpp::{Chunk, Node, line_key, quote, tokenize};
use scythe_core::{Color, EnvPoint, Envelope, EnvelopeKind, HwOut, PointShape, Project, Send, SendMode, TrackId};

use super::{Args, Env, Ret, State, Val, index, ret, track};

pub(super) fn call(s: &mut State, name: &str, a: &Args) -> Option<Ret> {
    Some(match name {
        "CountTracks" => ret(Val::Int(s.project.tracks.len() as i64)),
        "GetTrack" => {
            ret(index(s.project.tracks.len(), a.int(1)).map_or(Val::Nil, |i| Val::Track(s.project.tracks[i].id)))
        }
        "GetMasterTrack" => ret(Val::Track(s.project.master.id)),
        "InsertTrackAtIndex" => {
            s.project.add_track("");
            let t = s.project.tracks.pop().unwrap();
            let at = (a.int(0).max(0) as usize).min(s.project.tracks.len());
            s.project.tracks.insert(at, t);
            s.changed = true;
            ret(Val::Nil)
        }
        "DeleteTrack" => {
            if let Some(t) = a.get(0).track().filter(|&t| t != s.project.master.id) {
                s.project.remove_track(t);
                s.ctx.fx.remove(&t);
                if s.sel_tracks.contains(&t) {
                    s.sel_tracks.retain(|&x| x != t);
                    s.sel_changed = true;
                }
                s.changed = true;
            }
            ret(Val::Nil)
        }
        "GetTrackName" => match a.get(0).track() {
            Some(t) if s.project.track(t).is_some() || t == s.project.master.id => {
                (Val::Bool(true), vec![Val::Str(track_name(&s.project, t))])
            }
            _ => (Val::Bool(false), vec![Val::Str(String::new())]),
        },
        "GetSetMediaTrackInfo_String" => {
            let (key, value, set) = (a.text(1), a.text(2), a.bool(3));
            let Some(t) = track(&mut s.project, a.get(0).track()) else {
                return Some((Val::Bool(false), vec![Val::Str(String::new())]));
            };
            let got = match key.as_str() {
                "P_NAME" if set => Some(std::mem::replace(&mut t.name, value.clone())).map(|_| value),
                "P_NAME" => Some(t.name.clone()),
                "GUID" if set => Some(std::mem::replace(&mut t.guid, value.clone())).map(|_| value),
                "GUID" => Some(t.guid.clone()),
                k => k.strip_prefix("P_EXT:").map(|k| ext_value(&mut t.extra, k, set.then_some(value))),
            };
            s.changed |= set && got.is_some();
            (Val::Bool(got.is_some()), vec![Val::Str(got.unwrap_or_default())])
        }
        "GetMediaTrackInfo_Value" => ret(track_value(s, a.get(0).track(), &a.text(1))),
        "SetMediaTrackInfo_Value" => ret(Val::Bool(set_track_value(s, a.get(0).track(), &a.text(1), a.num(2)))),
        "GetTrackColor" => {
            ret(Val::Int(track(&mut s.project, a.get(0).track()).map_or(0, |t| color_to_native(t.color))))
        }
        "SetTrackColor" => {
            let c = a.int(1);
            if let Some(t) = track(&mut s.project, a.get(0).track()) {
                t.color = Some(Color::from_hex((c & 0xff_ffff) as u32));
                s.changed = true;
            }
            ret(Val::Nil)
        }
        "GetParentTrack" => {
            let parents = s.project.folder_parents();
            let parent = a.get(0).track().and_then(|t| s.project.track_index(t)).and_then(|i| parents[i]);
            ret(parent.map_or(Val::Nil, |i| Val::Track(s.project.tracks[i].id)))
        }
        "GetTrackDepth" => {
            let levels = s.project.folder_levels();
            ret(Val::Int(a.get(0).track().and_then(|t| s.project.track_index(t)).map_or(0, |i| levels[i] as i64)))
        }
        "GetTrackUIVolPan" => match track(&mut s.project, a.get(0).track()) {
            Some(t) => (Val::Bool(true), vec![Val::Num(t.volume), Val::Num(t.pan)]),
            None => (Val::Bool(false), vec![Val::Nil, Val::Nil]),
        },
        "GetSetTrackGroupMembership" | "GetSetTrackGroupMembershipHigh" | "GetSetTrackGroupMembershipEx" => {
            let (offset, at) = match name {
                "GetSetTrackGroupMembershipEx" => (a.int(2), 3),
                "GetSetTrackGroupMembershipHigh" => (32, 2),
                _ => (0, 2),
            };
            let (mask, value) = (a.int(at) as u32 as u64, a.int(at + 1) as u32 as u64);
            let param = scythe_core::groups::GroupParam::from_name(&a.text(1));
            let shift = if offset >= 32 { 32 } else { 0 };
            let Some((t, param)) = track(&mut s.project, a.get(0).track()).zip(param) else {
                return Some(ret(Val::Int(0)));
            };
            let m = &mut t.groups.0[param.index()];
            let old = (*m >> shift) as u32;
            let new = (*m & !(mask << shift)) | ((value & mask) << shift);
            if new != *m {
                *m = new;
                s.changed = true;
            }
            ret(Val::Int(old as i64))
        }
        "CSurf_OnVolumeChange" | "CSurf_OnVolumeChangeEx" | "SetTrackUIVolume" => {
            let gang = match name {
                "CSurf_OnVolumeChangeEx" => a.bool(3),
                "SetTrackUIVolume" => a.int(4) & 1 == 0,
                _ => true,
            };
            ret(Val::Num(ui_level(s, a.get(0).track(), Level::Volume, a.num(1), a.bool(2), gang)))
        }
        "CSurf_OnPanChange" | "CSurf_OnPanChangeEx" | "SetTrackUIPan" => {
            let gang = match name {
                "CSurf_OnPanChangeEx" => a.bool(3),
                "SetTrackUIPan" => a.int(4) & 1 == 0,
                _ => true,
            };
            ret(Val::Num(ui_level(s, a.get(0).track(), Level::Pan, a.num(1), a.bool(2), gang)))
        }
        "CSurf_OnWidthChange" | "CSurf_OnWidthChangeEx" | "SetTrackUIWidth" => {
            let gang = match name {
                "CSurf_OnWidthChangeEx" => a.bool(3),
                "SetTrackUIWidth" => a.int(4) & 1 == 0,
                _ => true,
            };
            ret(Val::Num(ui_level(s, a.get(0).track(), Level::Width, a.num(1), a.bool(2), gang)))
        }
        "CSurf_OnMuteChange"
        | "CSurf_OnMuteChangeEx"
        | "CSurf_OnSoloChange"
        | "CSurf_OnSoloChangeEx"
        | "CSurf_OnRecArmChange"
        | "CSurf_OnRecArmChangeEx" => {
            let gang = !name.ends_with("Ex") || a.bool(2);
            let kind = match name {
                n if n.contains("Mute") => Switch::Mute,
                n if n.contains("Solo") => Switch::Solo,
                _ => Switch::Arm,
            };
            let on = ui_switch(s, a.get(0).track(), kind, a.int(1), gang);
            ret(Val::Bool(on.unwrap_or(false)))
        }
        "SetTrackUIMute" | "SetTrackUISolo" | "SetTrackUIRecArm" | "SetTrackUIPolarity" => {
            let kind = match name {
                "SetTrackUIMute" => Switch::Mute,
                "SetTrackUISolo" => Switch::Solo,
                "SetTrackUIRecArm" => Switch::Arm,
                _ => Switch::Polarity,
            };
            // REAPER 7.73 solos ignoring routing when given 2 here.
            let (value, mode) = match (kind, a.int(1)) {
                (Switch::Solo, 2) => (1, Some(scythe_core::SoloMode::IgnoreRouting)),
                (_, v) => (v, None),
            };
            let on = ui_switch(s, a.get(0).track(), kind, value, a.int(2) & 1 == 0);
            if let (Some(m), Some(t)) = (mode, track(&mut s.project, a.get(0).track())) {
                t.solo_mode = m;
            }
            ret(Val::Int(on.unwrap_or(false) as i64))
        }
        "CountSelectedTracks" => ret(Val::Int(selected_tracks(s, false).len() as i64)),
        "CountSelectedTracks2" => ret(Val::Int(selected_tracks(s, a.bool(1)).len() as i64)),
        "GetSelectedTrack" | "GetSelectedTrack2" => {
            let list = selected_tracks(s, name == "GetSelectedTrack2" && a.bool(2));
            ret(index(list.len(), a.int(1)).map_or(Val::Nil, |i| Val::Track(list[i])))
        }
        "IsTrackSelected" => ret(Val::Bool(a.get(0).track().is_some_and(|t| s.sel_tracks.contains(&t)))),
        "SetTrackSelected" => {
            if let Some(t) = a.get(0).track() {
                select_track(s, t, a.bool(1));
            }
            ret(Val::Nil)
        }
        // The folder modes: 0 keeps the folders around the place as they are, 1 makes the
        // track before it a folder holding the moved tracks, 2 extends the folder that track
        // closes.
        "ReorderSelectedTracks" => {
            let sel = selected_tracks(s, false);
            let before = a.int(0).max(0) as usize;
            let moving = s.project.with_children(&sel);
            let (min, max) = s.project.drop_levels(&moving, before);
            let levels = s.project.folder_levels();
            let prev = (0..before.min(levels.len())).rev().find(|i| moving.binary_search(i).is_err());
            let level = match a.int(1) {
                1 => max,
                2 => prev.filter(|&p| s.project.tracks[p].folder_depth < 0).map_or(min, |p| levels[p]),
                _ => min,
            };
            if !sel.is_empty() {
                s.project.move_tracks(&sel, before, level);
                s.changed = true;
            }
            ret(Val::Bool(!sel.is_empty()))
        }
        "SetOnlyTrackSelected" => {
            if let Some(t) = a.get(0).track() {
                s.sel_tracks = vec![t];
                s.sel_changed = true;
            }
            ret(Val::Nil)
        }
        "Track_GetPeakInfo" => ret(Val::Num(0.0)),

        // Sends: category < 0 lists the track's receives, 0 its sends, > 0 its hardware
        // outputs (the master has only those).
        "CreateTrackSend" => {
            let dest = a.get(1).track().filter(|d| s.project.track(*d).is_some());
            let Some(t) = track(&mut s.project, a.get(0).track()) else { return Some(ret(Val::Int(-1))) };
            let n = match dest {
                Some(dest) => {
                    // REAPER's CreateTrackSend carries all MIDI (I_MIDIFLAGS 0).
                    t.sends.push(Send { midi_flags: 0, ..Send::new(dest) });
                    t.sends.len()
                }
                None => {
                    t.hw_outs.push(HwOut::default());
                    t.hw_outs.len()
                }
            };
            s.changed = true;
            ret(Val::Int(n as i64 - 1))
        }
        "RemoveTrackSend" => {
            let ok = match route_at(&mut s.project, a.get(0).track(), a.int(1), a.int(2)) {
                Some(Route::Send(src, i)) => {
                    s.project.track_mut(src).unwrap().sends.remove(i);
                    true
                }
                Some(Route::Hw(t, i)) => {
                    track(&mut s.project, Some(t)).unwrap().hw_outs.remove(i);
                    true
                }
                None => false,
            };
            s.changed |= ok;
            ret(Val::Bool(ok))
        }
        "GetTrackNumSends" => {
            let (t, cat) = (a.get(0).track(), a.int(1));
            let n = (0..).take_while(|&i| route_at(&mut s.project, t, cat, i).is_some()).count();
            ret(Val::Int(n as i64))
        }
        "GetTrackSendInfo_Value" => {
            let Some(route) = route_at(&mut s.project, a.get(0).track(), a.int(1), a.int(2)) else {
                return Some(ret(Val::Num(0.0)));
            };
            let key = a.text(3);
            let v = |x: f64| ret(Val::Num(x));
            match route {
                Route::Send(src, i) => {
                    let send = &s.project.track(src).unwrap().sends[i];
                    let raw = |i: usize| send.raw.as_ref()?.get(i)?.split(':').next()?.parse::<f64>().ok();
                    match key.as_str() {
                        "B_MUTE" => v(send.muted as u8 as f64),
                        "D_VOL" => v(send.volume),
                        "D_PAN" => v(send.pan),
                        "I_SRCCHAN" => v(send.src_channels as f64),
                        "I_MIDIFLAGS" => v(send.midi_flags as f64),
                        "I_DSTCHAN" => v(send.dst_channels as f64),
                        "I_SENDMODE" => v(send.mode.code() as f64),
                        "B_MONO" => v(send.mono as u8 as f64),
                        "B_PHASE" => v(send.phase as u8 as f64),
                        "D_PANLAW" => v(raw(10).unwrap_or(-1.0)),
                        "I_AUTOMODE" => v(-1.0),
                        "P_DESTTRACK" => ret(Val::Track(send.dest)),
                        "P_SRCTRACK" => ret(Val::Track(src)),
                        _ => v(0.0),
                    }
                }
                Route::Hw(t, i) => {
                    let o = &track(&mut s.project, Some(t)).unwrap().hw_outs[i];
                    match key.as_str() {
                        "B_MUTE" => v(o.muted as u8 as f64),
                        "D_VOL" => v(o.volume),
                        "D_PAN" => v(o.pan),
                        "I_SRCCHAN" => v(o.src_channels as f64),
                        "I_DSTCHAN" => v(o.dst_channels as f64),
                        "I_SENDMODE" => v(o.mode.code() as f64),
                        "B_PHASE" => v(o.phase as u8 as f64),
                        "D_PANLAW" => v(o.pan_law),
                        "I_AUTOMODE" => v(-1.0),
                        "P_SRCTRACK" => ret(Val::Track(t)),
                        _ => v(0.0),
                    }
                }
            }
        }
        "SetTrackSendInfo_Value" => {
            let Some(route) = route_at(&mut s.project, a.get(0).track(), a.int(1), a.int(2)) else {
                return Some(ret(Val::Bool(false)));
            };
            let v = a.num(4);
            let ok = match route {
                Route::Send(src, i) => {
                    let send = &mut s.project.track_mut(src).unwrap().sends[i];
                    match a.text(3).as_str() {
                        "B_MUTE" => send.muted = v != 0.0,
                        "D_VOL" => send.volume = v.max(0.0),
                        "D_PAN" => send.pan = v.clamp(-1.0, 1.0),
                        "I_SRCCHAN" => send.src_channels = v as i64,
                        "I_MIDIFLAGS" => send.midi_flags = v as i64,
                        "I_DSTCHAN" => send.dst_channels = v as i64,
                        "I_SENDMODE" => send.mode = SendMode::from_code(v as i64),
                        "B_MONO" => send.mono = v != 0.0,
                        "B_PHASE" => send.phase = v != 0.0,
                        _ => return Some(ret(Val::Bool(false))),
                    }
                    true
                }
                Route::Hw(t, i) => {
                    let o = &mut track(&mut s.project, Some(t)).unwrap().hw_outs[i];
                    match a.text(3).as_str() {
                        "B_MUTE" => o.muted = v != 0.0,
                        "D_VOL" => o.volume = v.max(0.0),
                        "D_PAN" => o.pan = v.clamp(-1.0, 1.0),
                        "I_SRCCHAN" => o.src_channels = v as i64,
                        "I_DSTCHAN" => o.dst_channels = v as i64,
                        "I_SENDMODE" => o.mode = SendMode::from_code(v as i64),
                        "B_PHASE" => o.phase = v != 0.0,
                        "D_PANLAW" => o.pan_law = v,
                        _ => return Some(ret(Val::Bool(false))),
                    }
                    true
                }
            };
            s.changed |= ok;
            ret(Val::Bool(ok))
        }
        "GetTrackSendName" | "GetTrackReceiveName" => {
            let cat = if name == "GetTrackSendName" { 0 } else { -1 };
            match send_at(&s.project, a.get(0).track(), cat, a.int(1)) {
                Some((src, i)) => {
                    let other = if cat == 0 { s.project.track(src).unwrap().sends[i].dest } else { src };
                    (Val::Bool(true), vec![Val::Str(track_name(&s.project, other))])
                }
                None => (Val::Bool(false), vec![Val::Str(String::new())]),
            }
        }

        _ => return fx(s, name, a).or_else(|| envelopes(s, name, a)),
    })
}

/// A track's name as REAPER reports it: "MASTER", or "Track N" when it has none.
pub(super) fn track_name(p: &Project, t: TrackId) -> String {
    if t == p.master.id {
        return "MASTER".to_owned();
    }
    match p.track_index(t) {
        Some(i) if p.tracks[i].name.is_empty() => format!("Track {}", i + 1),
        Some(i) => p.tracks[i].name.clone(),
        None => String::new(),
    }
}

pub(super) fn color_to_native(c: Option<Color>) -> i64 {
    c.map_or(0, |c| c.to_hex() as i64 | 0x100_0000)
}

pub(super) fn color_from_native(v: i64) -> Option<Color> {
    (v & 0x100_0000 != 0).then(|| Color::from_hex((v & 0xff_ffff) as u32))
}

/// Gets, or sets when `set` is given, a value in the `<EXT` chunk of `extra`, where REAPER keeps
/// `P_EXT:` values.
pub(super) fn ext_value(extra: &mut Vec<Node>, key: &str, set: Option<String>) -> String {
    let at = extra.iter().position(|n| matches!(n, Node::Chunk(c) if c.name() == "EXT"));
    let found = at.and_then(|i| match &extra[i] {
        Node::Chunk(c) => c.children.iter().position(|n| matches!(n, Node::Line(l) if line_key(l.trim()) == key)),
        Node::Line(_) => None,
    });
    let Some(value) = set else {
        let Some((Node::Chunk(c), j)) = at.map(|i| &extra[i]).zip(found) else { return String::new() };
        let Node::Line(l) = &c.children[j] else { return String::new() };
        return tokenize(l).get(1).cloned().unwrap_or_default();
    };
    let i = at.unwrap_or_else(|| {
        extra.push(Node::Chunk(Chunk::new("EXT", &[])));
        extra.len() - 1
    });
    if let Node::Chunk(c) = &mut extra[i] {
        let line = Node::Line(format!("{key} {}", quote(&value)));
        match found {
            Some(j) => c.children[j] = line,
            None => c.children.push(line),
        }
    }
    value
}

/// The selected tracks in track order, the master first when asked for.
fn selected_tracks(s: &State, master: bool) -> Vec<TrackId> {
    let m = s.project.master.id;
    let master = (master && s.sel_tracks.contains(&m)).then_some(m);
    master.into_iter().chain(s.project.tracks.iter().map(|t| t.id).filter(|t| s.sel_tracks.contains(t))).collect()
}

pub(super) fn select_track(s: &mut State, t: TrackId, on: bool) {
    if on != s.sel_tracks.contains(&t) {
        if on {
            s.sel_tracks.push(t);
        } else {
            s.sel_tracks.retain(|&x| x != t);
        }
        s.sel_changed = true;
    }
}

fn track_value(s: &mut State, t: Option<TrackId>, key: &str) -> Val {
    let number = t.and_then(|t| s.project.track_index(t)).map_or(-1.0, |i| i as f64 + 1.0);
    let parent = t.and_then(|t| s.project.track_index(t)).and_then(|i| s.project.folder_parents()[i]);
    let parent = parent.map_or(Val::Nil, |i| Val::Track(s.project.tracks[i].id));
    let selected = t.is_some_and(|t| s.sel_tracks.contains(&t));
    let Some(t) = track(&mut s.project, t) else { return Val::Num(0.0) };
    Val::Num(match key {
        "D_VOL" => t.volume,
        "D_PAN" => t.pan,
        "B_MUTE" => t.muted as u8 as f64,
        "I_SOLO" => {
            if t.solo {
                t.solo_mode.code(t.solo_safe) as f64
            } else {
                0.0
            }
        }
        "B_SOLO_DEFEAT" => t.solo_defeat as u8 as f64,
        "B_PHASE" => t.phase as u8 as f64,
        "I_PANMODE" => t.pan_mode.map_or(-1.0, |m| m.code() as f64),
        "D_WIDTH" => t.width,
        "D_DUALPANL" => t.dual_pan[0],
        "D_DUALPANR" => t.dual_pan[1],
        "I_RECARM" => t.armed as u8 as f64,
        "I_RECINPUT" => t.rec_input as f64,
        "I_RECMON" => match t.monitor_mode() {
            scythe_core::recording::MonitorMode::Off => 0.0,
            scythe_core::recording::MonitorMode::On => 1.0,
            scythe_core::recording::MonitorMode::Auto => 2.0,
        },
        "I_RECMODE" => t.rec_mode.code() as f64,
        "I_RECMODE_FLAGS" => t.rec_tap.code() as f64,
        "I_RECMONITEMS" => t.monitor_items as u8 as f64,
        "B_AUTO_RECARM" => t.auto_arm as u8 as f64,
        "I_NUMFIXEDLANES" => t.lane_count() as f64,
        "B_MAINSEND" => t.main_send as u8 as f64,
        "C_MAINSEND_OFFS" => t.main_send_offset as f64,
        "I_FOLDERDEPTH" => t.folder_depth as f64,
        "I_AUTOMODE" => t.auto_mode.rpp_code() as f64,
        "I_NCHAN" => t.channels as f64,
        "IP_TRACKNUMBER" => number,
        "I_SELECTED" => selected as u8 as f64,
        "I_CUSTOMCOLOR" => color_to_native(t.color) as f64,
        "D_PANLAW" => t.pan_law,
        "I_FREEMODE" => {
            if t.fixed_lanes {
                2.0
            } else {
                0.0
            }
        }
        "I_MIDIHWOUT" => t.midi_out.map_or(-1.0, |(d, c)| (d as i64 * 32 + c as i64) as f64),
        "P_PARTRACK" => return parent,
        "P_PROJECT" => return Val::Proj,
        _ => 0.0,
    })
}

fn set_track_value(s: &mut State, id: Option<TrackId>, key: &str, v: f64) -> bool {
    if key == "I_SELECTED" {
        if let Some(t) = id.filter(|&t| s.project.track(t).is_some() || t == s.project.master.id) {
            select_track(s, t, v != 0.0);
        }
        return true;
    }
    let Some(t) = track(&mut s.project, id) else { return false };
    match key {
        "D_VOL" => t.volume = v.max(0.0),
        "D_PAN" => t.pan = v.clamp(-1.0, 1.0),
        "B_MUTE" => t.muted = v != 0.0,
        // REAPER 7.73 takes 1 and 2 here; anything else, safe solo (5, 6) included, unsolos.
        "I_SOLO" => match v as i64 {
            1 => (t.solo, t.solo_mode) = (true, scythe_core::SoloMode::IgnoreRouting),
            2 => (t.solo, t.solo_mode) = (true, scythe_core::SoloMode::InPlace),
            _ => t.solo = false,
        },
        "B_SOLO_DEFEAT" => t.solo_defeat = v != 0.0,
        "B_PHASE" => t.phase = v != 0.0,
        "I_PANMODE" => t.pan_mode = (v >= 0.0).then(|| scythe_core::fader::PanMode::from_code(v as i64)),
        "D_WIDTH" => t.width = v.clamp(-1.0, 1.0),
        "D_DUALPANL" => t.dual_pan[0] = v.clamp(-1.0, 1.0),
        "D_DUALPANR" => t.dual_pan[1] = v.clamp(-1.0, 1.0),
        "I_RECARM" => t.armed = v != 0.0,
        "I_RECINPUT" => t.rec_input = v as i64,
        "I_RECMON" => {
            (t.monitor, t.monitor_auto) = (v != 0.0, v as i64 == 2);
        }
        "I_RECMODE" => t.rec_mode = scythe_core::recording::RecordMode::from_code(v as i64),
        "I_RECMODE_FLAGS" => t.rec_tap = scythe_core::recording::RecordTap::from_code(v as i64),
        "I_RECMONITEMS" => t.monitor_items = v != 0.0,
        "B_AUTO_RECARM" => t.auto_arm = v != 0.0,
        "B_MAINSEND" => t.main_send = v != 0.0,
        "C_MAINSEND_OFFS" => t.main_send_offset = v.max(0.0) as u32,
        "I_FOLDERDEPTH" => t.folder_depth = v as i32,
        "I_AUTOMODE" => t.auto_mode = scythe_core::AutoMode::from_rpp(v as i64),
        "I_NCHAN" => t.channels = ((v.max(2.0) as u32).min(128) + 1) & !1,
        "I_CUSTOMCOLOR" => t.color = color_from_native(v as i64),
        "D_PANLAW" => t.pan_law = v,
        "I_FREEMODE" => t.set_fixed_lanes(v as i64 == 2),
        "I_MIDIHWOUT" => {
            t.midi_out = (v >= 0.0).then_some(((v as i64 >> 5) as u8, (v as i64 & 31) as u8));
        }
        _ => return false,
    }
    s.changed = true;
    true
}

/// The send a category and index name: its source track and index in that track's sends.
enum Route {
    /// A send: its source track and index in that track's sends.
    Send(TrackId, usize),
    /// A hardware output of a track or the master.
    Hw(TrackId, usize),
}

/// What a send category and index name, hardware outputs (category > 0) included.
fn route_at(p: &mut Project, t: Option<TrackId>, category: i64, i: i64) -> Option<Route> {
    if category > 0 {
        let i = usize::try_from(i).ok()?;
        let id = t?;
        return track(p, Some(id)).filter(|tr| i < tr.hw_outs.len()).map(|_| Route::Hw(id, i));
    }
    send_at(p, t, category, i).map(|(src, k)| Route::Send(src, k))
}

fn send_at(p: &Project, t: Option<TrackId>, category: i64, i: i64) -> Option<(TrackId, usize)> {
    let t = t?;
    let i = usize::try_from(i).ok()?;
    match category {
        0 => p.track(t).filter(|tr| i < tr.sends.len()).map(|_| (t, i)),
        c if c < 0 => p
            .tracks
            .iter()
            .flat_map(|src| src.sends.iter().enumerate().filter(|(_, x)| x.dest == t).map(|(k, _)| (src.id, k)))
            .nth(i),
        _ => None,
    }
}

// ---- effects -------------------------------------------------------------------------------

fn fx(s: &mut State, name: &str, a: &Args) -> Option<Ret> {
    let t = a.get(0).track();
    let slot_index = |s: &mut State, i: i64| track(&mut s.project, t).and_then(|tr| index(tr.fx.len(), i));
    Some(match name {
        "TrackFX_GetCount" => ret(Val::Int(track(&mut s.project, t).map_or(0, |t| t.fx.len() as i64))),
        "TrackFX_Show" | "TrackFX_SetOpen" => {
            let (Some(id), Some(i)) = (t, slot_index(s, a.int(1))) else { return Some(ret(Val::Nil)) };
            let flag = if name == "TrackFX_SetOpen" { if a.bool(2) { 3 } else { 2 } } else { a.int(2) };
            s.ctx.open_fx.retain(|o| *o != (id, i) || flag == 0 || flag == 1);
            if flag == 3 {
                s.ctx.open_fx.push((id, i));
            }
            s.out.fx_windows.push((id, i, flag));
            ret(Val::Nil)
        }
        "TrackFX_GetOpen" => {
            let open = t.zip(slot_index(s, a.int(1))).is_some_and(|(id, i)| s.ctx.open_fx.contains(&(id, i)));
            ret(Val::Bool(open))
        }
        "TrackFX_GetFloatingWindow" => {
            // The window's handle only says whether it is open: no script can draw into it.
            let open =
                t.zip(slot_index(s, a.int(1))).and_then(|(id, i)| s.ctx.open_fx.iter().position(|o| *o == (id, i)));
            ret(open.map_or(Val::Nil, |k| Val::Int(0x5c7e_0000 + k as i64)))
        }
        "TrackFX_GetFXName" => {
            let name = slot_index(s, a.int(1)).map(|i| track(&mut s.project, t).unwrap().fx[i].name.clone());
            (Val::Bool(name.is_some()), vec![Val::Str(name.unwrap_or_default())])
        }
        "TrackFX_GetEnabled" => {
            let on = slot_index(s, a.int(1)).map(|i| track(&mut s.project, t).unwrap().fx[i].enabled);
            ret(Val::Bool(on.unwrap_or(false)))
        }
        "TrackFX_SetEnabled" => {
            if let Some(i) = slot_index(s, a.int(1)) {
                track(&mut s.project, t).unwrap().fx[i].enabled = a.bool(2);
                s.changed = true;
            }
            ret(Val::Nil)
        }
        "TrackFX_GetOffline" => {
            let offline = slot_index(s, a.int(1)).is_some_and(|i| {
                track(&mut s.project, t).unwrap().fx[i].raw.iter().any(|n| {
                    matches!(n, Node::Line(l) if line_key(l.trim()) == "BYPASS" && tokenize(l).get(2).is_some_and(|v| v == "1"))
                })
            });
            ret(Val::Bool(offline))
        }
        "TrackFX_GetInstrument" => {
            let live = t.and_then(|t| s.ctx.fx.get(&t));
            let i = live.and_then(|l| l.iter().position(|f| f.as_ref().is_some_and(|f| f.is_instrument())));
            ret(Val::Int(i.map_or(-1, |i| i as i64)))
        }
        "TrackFX_AddByName" | "TrackFX_GetByName" => {
            let query = a.text(1);
            let instantiate = if name == "TrackFX_GetByName" { a.bool(2) as i64 } else { a.int(3) };
            let Some(tr) = track(&mut s.project, t) else { return Some(ret(Val::Int(-1))) };
            let found = tr.fx.iter().position(|f| fx_name_matches(&f.name, &query));
            if instantiate == 0 || (instantiate > 0 && found.is_some()) {
                return Some(ret(Val::Int(found.map_or(-1, |i| i as i64))));
            }
            let Some(slot) = s.ctx.services.as_ref().and_then(|sv| sv.plugin(&query)) else {
                return Some(ret(Val::Int(-1)));
            };
            let tr = track(&mut s.project, t).unwrap();
            let at = if instantiate <= -1000 { ((-instantiate - 1000) as usize).min(tr.fx.len()) } else { tr.fx.len() };
            tr.fx.insert(at, slot);
            let id = tr.id;
            if let Some(live) = s.ctx.fx.get_mut(&id) {
                live.insert(at.min(live.len()), None);
            }
            s.changed = true;
            ret(Val::Int(at as i64))
        }
        "TrackFX_Delete" => {
            let Some(i) = slot_index(s, a.int(1)) else { return Some(ret(Val::Bool(false))) };
            let tr = track(&mut s.project, t).unwrap();
            tr.fx.remove(i);
            let id = tr.id;
            if let Some(live) = s.ctx.fx.get_mut(&id).filter(|l| i < l.len()) {
                live.remove(i);
            }
            s.changed = true;
            ret(Val::Bool(true))
        }
        "TrackFX_CopyToTrack" => {
            let (dest, moved) = (a.get(2).track(), a.bool(4));
            let Some(i) = slot_index(s, a.int(1)) else { return Some(ret(Val::Nil)) };
            if track(&mut s.project, dest).is_none() {
                return Some(ret(Val::Nil));
            }
            let src = track(&mut s.project, t).unwrap();
            let src_id = src.id;
            let slot = if moved { src.fx.remove(i) } else { src.fx[i].clone() };
            let instance = match s.ctx.fx.get_mut(&src_id).filter(|l| i < l.len()) {
                Some(l) if moved => l.remove(i),
                _ => None,
            };
            let to = track(&mut s.project, dest).unwrap();
            let at = (a.int(3).max(0) as usize).min(to.fx.len());
            to.fx.insert(at, slot);
            let dest_id = to.id;
            if let Some(live) = s.ctx.fx.get_mut(&dest_id) {
                live.insert(at.min(live.len()), instance);
            }
            s.changed = true;
            ret(Val::Nil)
        }
        "TrackFX_GetNumParams" => ret(Val::Int(live_params(s, t, a.int(1)).map_or(0, |p| p.len() as i64))),
        "TrackFX_GetParam" | "TrackFX_GetParamEx" => {
            let p = live_params(s, t, a.int(1)).and_then(|p| index(p.len(), a.int(2)).map(|i| p[i].clone()));
            let Some(p) = p else {
                let outs = if name == "TrackFX_GetParam" { 2 } else { 3 };
                return Some((Val::Num(0.0), vec![Val::Num(0.0); outs]));
            };
            let mut outs = vec![Val::Num(p.min), Val::Num(p.max)];
            if name == "TrackFX_GetParamEx" {
                outs.push(Val::Num((p.min + p.max) / 2.0));
            }
            (Val::Num(p.value), outs)
        }
        "TrackFX_GetParamNormalized" => {
            let p = live_params(s, t, a.int(1)).and_then(|p| index(p.len(), a.int(2)).map(|i| p[i].clone()));
            ret(Val::Num(p.map_or(0.0, |p| {
                let span = p.max - p.min;
                if span.abs() > 1e-12 { (p.value - p.min) / span } else { 0.0 }
            })))
        }
        "TrackFX_SetParam" | "TrackFX_SetParamNormalized" => {
            let (fx, param) = (a.int(1), a.int(2));
            let p = live_params(s, t, fx).and_then(|p| index(p.len(), param).map(|i| p[i].clone()));
            let (Some(p), Some(live)) = (p, live_fx(s, t, fx)) else { return Some(ret(Val::Bool(false))) };
            let v =
                if name == "TrackFX_SetParam" { a.num(3) } else { p.min + a.num(3).clamp(0.0, 1.0) * (p.max - p.min) };
            live.set_param(param as usize, v.clamp(p.min.min(p.max), p.max.max(p.min)));
            s.out.fx_touched = true;
            ret(Val::Bool(true))
        }
        "TrackFX_GetFormattedParamValue" | "TrackFX_GetParamName" => {
            let p = live_params(s, t, a.int(1)).and_then(|p| index(p.len(), a.int(2)).map(|i| p[i].clone()));
            let text = p.map(|p| if name == "TrackFX_GetParamName" { p.name } else { p.text });
            (Val::Bool(text.is_some()), vec![Val::Str(text.unwrap_or_default())])
        }
        "TrackFX_GetPreset" => {
            let Some(i) = slot_index(s, a.int(1)) else {
                return Some((Val::Bool(false), vec![Val::Str(String::new())]));
            };
            let preset = preset_name(&track(&mut s.project, t).unwrap().fx[i]);
            (Val::Bool(!preset.is_empty()), vec![Val::Str(preset)])
        }
        "TrackFX_GetPresetIndex" => {
            let Some(i) = slot_index(s, a.int(1)) else { return Some((Val::Int(-1), vec![Val::Int(0)])) };
            let slot = &track(&mut s.project, t).unwrap().fx[i];
            let presets = presets(s.ctx.resource_dir.as_deref(), slot);
            let current = preset_name(slot);
            let at = presets.iter().position(|p| p.name == current);
            (Val::Int(at.map_or(-1, |i| i as i64)), vec![Val::Int(presets.len() as i64)])
        }
        "TrackFX_SetPreset" | "TrackFX_SetPresetByIndex" | "TrackFX_NavigatePresets" => {
            let Some(i) = slot_index(s, a.int(1)) else { return Some(ret(Val::Bool(false))) };
            let dir = s.ctx.resource_dir.clone();
            let slot = &mut track(&mut s.project, t).unwrap().fx[i];
            let presets = presets(dir.as_deref(), slot);
            let chosen = match name {
                "TrackFX_SetPreset" => presets.iter().position(|p| p.name == a.text(2)),
                "TrackFX_SetPresetByIndex" => index(presets.len(), a.int(2)),
                _ if presets.is_empty() => None,
                _ => {
                    let n = presets.len() as i64;
                    let current = presets.iter().position(|p| p.name == preset_name(slot)).map_or(-1, |i| i as i64);
                    let next = if current < 0 && a.int(2) < 0 { n - 1 } else { current + a.int(2) };
                    Some(next.rem_euclid(n) as usize)
                }
            };
            let Some(p) = chosen.map(|i| &presets[i]) else { return Some(ret(Val::Bool(false))) };
            let applied = crate::fx_preset::apply(slot, &p.data, &p.name);
            s.changed |= applied;
            ret(Val::Bool(applied))
        }
        _ => return None,
    })
}

/// Whether REAPER's FX lookup by name finds `query` in a plugin called `full`: a
/// case-insensitive match of the name, with or without its `VST3:`-style prefix.
pub(crate) fn fx_name_matches(full: &str, query: &str) -> bool {
    let (full, query) = (full.to_lowercase(), query.to_lowercase());
    let query = match query.split_once(':') {
        Some((kind, rest)) if !kind.contains(' ') && full.starts_with(&format!("{kind}:")) => rest.trim().to_owned(),
        Some((kind, _)) if !kind.contains(' ') => return false,
        _ => query,
    };
    !query.is_empty() && full.contains(query.trim())
}

fn live_fx(s: &State, t: Option<TrackId>, fx: i64) -> Option<std::rc::Rc<dyn super::LiveFx>> {
    let list = s.ctx.fx.get(&t?)?;
    list.get(usize::try_from(fx).ok()?)?.clone()
}

fn live_params(s: &State, t: Option<TrackId>, fx: i64) -> Option<Vec<super::FxParam>> {
    Some(live_fx(s, t, fx)?.params())
}

/// The preset the slot was last set to (`PRESETNAME`).
fn preset_name(slot: &scythe_core::FxSlot) -> String {
    slot.raw
        .iter()
        .find_map(|n| match n {
            Node::Line(l) if line_key(l.trim()) == "PRESETNAME" => tokenize(l).get(1).cloned(),
            _ => None,
        })
        .unwrap_or_default()
}

/// The presets in the plugin's REAPER preset file.
fn presets(resource_dir: Option<&std::path::Path>, slot: &scythe_core::FxSlot) -> Vec<crate::fx_preset::Preset> {
    let (Some(dir), Some(file)) = (resource_dir, crate::fx_preset::file_name(slot)) else { return Vec::new() };
    let text = std::fs::read_to_string(dir.join("presets").join(file)).unwrap_or_default();
    crate::fx_preset::PresetFile::parse(&text).presets
}

// ---- envelopes -----------------------------------------------------------------------------

/// REAPER's names for the envelopes Scythe has.
fn env_name(kind: EnvelopeKind) -> &'static str {
    match kind {
        EnvelopeKind::Volume => "Volume",
        EnvelopeKind::VolumePreFx => "Volume (Pre-FX)",
        EnvelopeKind::Trim => "Trim Volume",
        EnvelopeKind::Pan => "Pan",
        EnvelopeKind::Mute => "Mute",
        EnvelopeKind::Width => "Width",
        EnvelopeKind::DualPanLeft => "Pan (Left)",
        EnvelopeKind::DualPanRight => "Pan (Right)",
        EnvelopeKind::Pitch => "Pitch",
    }
}

/// An envelope the script holds, when it exists.
pub(super) fn envelope(p: &mut Project, e: Option<Env>) -> Option<&mut Envelope> {
    match e? {
        Env::Track(t, kind) => track(p, Some(t))?.envelopes.iter_mut().find(|e| e.kind == kind),
        Env::Take(i, k, kind) => p
            .tracks
            .iter_mut()
            .find_map(|t| t.item_mut(i))?
            .takes
            .get_mut(k)?
            .envelopes
            .iter_mut()
            .find(|e| e.kind == kind),
    }
}

fn fader_scaled(env: &Envelope) -> bool {
    env.fader_scaled
}

pub(super) use scythe_core::fader::{envelope_gain as scale_from_fader, envelope_position as scale_to_fader};

/// A point as the API sees it: (time, value, shape, tension, selected).
fn point_out(env: &Envelope, p: &EnvPoint) -> (f64, f64, i64, f64, bool) {
    let value = if fader_scaled(env) { scale_to_fader(p.value) } else { p.value };
    let tension = if let PointShape::Bezier(t) = p.shape { t } else { 0.0 };
    (p.time, value, p.shape.rpp_code(), tension, point_selected(p))
}

fn point_selected(p: &EnvPoint) -> bool {
    p.raw.get(5).is_some_and(|v| v.parse::<i64>().is_ok_and(|v| v & 1 != 0))
}

fn set_point_selected(p: &mut EnvPoint, on: bool) {
    if p.raw.is_empty() {
        p.raw = vec!["PT".into(), "0".into(), "0".into(), "0".into()];
    }
    if p.raw.len() < 6 {
        p.raw.resize(6, "0".into());
    }
    p.raw[5] = if on { "1" } else { "0" }.into();
}

fn envelopes(s: &mut State, name: &str, a: &Args) -> Option<Ret> {
    let e = a.get(0).env();
    // The `Ex` forms take an automation item index first; -1 is the envelope itself.
    let ex = name.ends_with("Ex") && name.contains("Envelope");
    let (base, ai) = if ex { (1, a.int(1)) } else { (0, -1) };
    if ex && ai >= 0 {
        return Some(item_points(s, name, a, e, ai as usize));
    }
    Some(match name {
        "CountTrackEnvelopes" => {
            ret(Val::Int(track(&mut s.project, a.get(0).track()).map_or(0, |t| t.envelopes.len() as i64)))
        }
        "GetTrackEnvelope" => {
            let i = a.int(1);
            let Some(t) = track(&mut s.project, a.get(0).track()) else { return Some(ret(Val::Nil)) };
            ret(index(t.envelopes.len(), i).map_or(Val::Nil, |i| Val::Env(Env::Track(t.id, t.envelopes[i].kind))))
        }
        "GetTrackEnvelopeByName" | "GetTrackEnvelopeByChunkName" => {
            let want = a.text(1);
            let Some(t) = track(&mut s.project, a.get(0).track()) else { return Some(ret(Val::Nil)) };
            let found = t.envelopes.iter().find(|e| {
                if name == "GetTrackEnvelopeByChunkName" {
                    want.trim_start_matches('<').eq_ignore_ascii_case(e.kind.chunk_name())
                } else {
                    want.eq_ignore_ascii_case(env_name(e.kind))
                }
            });
            ret(found.map_or(Val::Nil, |e| Val::Env(Env::Track(t.id, e.kind))))
        }
        "CountTakeEnvelopes" | "GetTakeEnvelope" | "GetTakeEnvelopeByName" => {
            let Some((item, k)) = a.get(0).take() else { return Some(ret(Val::Nil)) };
            let take = s.project.tracks.iter().find_map(|t| t.item(item)).and_then(|i| i.takes.get(k));
            let kinds: Vec<EnvelopeKind> =
                take.map(|t| t.envelopes.iter().map(|e| e.kind).collect()).unwrap_or_default();
            let env = |kind: EnvelopeKind| Val::Env(Env::Take(item, k, kind));
            match name {
                "CountTakeEnvelopes" => ret(Val::Int(kinds.len() as i64)),
                "GetTakeEnvelope" => ret(index(kinds.len(), a.int(1)).map_or(Val::Nil, |i| env(kinds[i]))),
                _ => {
                    ret(kinds.into_iter().find(|&k| a.text(1).eq_ignore_ascii_case(env_name(k))).map_or(Val::Nil, env))
                }
            }
        }
        "GetEnvelopeName" => match envelope(&mut s.project, e) {
            Some(env) => (Val::Bool(true), vec![Val::Str(env_name(env.kind).to_owned())]),
            None => (Val::Bool(false), vec![Val::Str(String::new())]),
        },
        "GetEnvelopeScalingMode" => ret(Val::Int(envelope(&mut s.project, e).map_or(0, |e| fader_scaled(e) as i64))),
        "ScaleFromEnvelopeMode" => ret(Val::Num(if a.int(0) == 1 { scale_from_fader(a.num(1)) } else { a.num(1) })),
        "ScaleToEnvelopeMode" => ret(Val::Num(if a.int(0) == 1 { scale_to_fader(a.num(1)) } else { a.num(1) })),
        "CountEnvelopePoints" | "CountEnvelopePointsEx" => {
            ret(Val::Int(envelope(&mut s.project, e).map_or(0, |e| e.points.len() as i64)))
        }
        "GetEnvelopePoint" | "GetEnvelopePointEx" => {
            let i = a.int(base + 1);
            match envelope(&mut s.project, e)
                .and_then(|env| index(env.points.len(), i).map(|i| point_out(env, &env.points[i])))
            {
                Some((time, value, shape, tension, sel)) => (
                    Val::Bool(true),
                    vec![Val::Num(time), Val::Num(value), Val::Int(shape), Val::Num(tension), Val::Bool(sel)],
                ),
                None => {
                    (Val::Bool(false), vec![Val::Num(0.0), Val::Num(0.0), Val::Int(0), Val::Num(0.0), Val::Bool(false)])
                }
            }
        }
        "SetEnvelopePoint" | "SetEnvelopePointEx" => {
            let i = a.int(base + 1);
            let at = |k: usize| base + 2 + k;
            let Some(env) = envelope(&mut s.project, e) else { return Some(ret(Val::Bool(false))) };
            let Some(i) = index(env.points.len(), i) else { return Some(ret(Val::Bool(false))) };
            let scaled = fader_scaled(env);
            let p = &mut env.points[i];
            if let Some(t) = a.opt(at(0)) {
                p.time = t;
            }
            if let Some(v) = a.opt(at(1)) {
                p.value = if scaled { scale_from_fader(v) } else { v };
            }
            if a.opt(at(2)).is_some() || a.opt(at(3)).is_some() {
                let shape = a.opt(at(2)).map_or(p.shape.rpp_code(), |v| v as i64);
                let tension = a.opt(at(3)).unwrap_or(if let PointShape::Bezier(t) = p.shape { t } else { 0.0 });
                p.shape = PointShape::from_rpp(shape, tension);
            }
            if !a.get(at(4)).is_nil() {
                set_point_selected(p, a.bool(at(4)));
            }
            // REAPER hands the point back through the optional parameters.
            let p = p.clone();
            let (time, value, shape, tension, sel) = point_out(env, &p);
            if !a.bool(at(5)) {
                env.sort();
            }
            s.changed = true;
            let outs = vec![Val::Num(time), Val::Num(value), Val::Int(shape), Val::Num(tension), Val::Bool(sel)];
            (Val::Bool(true), outs.into_iter().chain([Val::Bool(a.bool(at(5)))]).collect())
        }
        "InsertEnvelopePoint" | "InsertEnvelopePointEx" => {
            let at = |k: usize| base + 1 + k;
            let Some(env) = envelope(&mut s.project, e) else { return Some(ret(Val::Bool(false))) };
            let value = if fader_scaled(env) { scale_from_fader(a.num(at(1))) } else { a.num(at(1)) };
            let mut p = EnvPoint {
                shape: PointShape::from_rpp(a.int(at(2)), a.num(at(3))),
                ..EnvPoint::new(a.num(at(0)), value)
            };
            if a.bool(at(4)) {
                set_point_selected(&mut p, true);
            }
            env.points.push(p);
            if !a.bool(at(5)) {
                env.sort();
            }
            s.changed = true;
            (Val::Bool(true), vec![Val::Bool(a.bool(at(5)))])
        }
        "DeleteEnvelopePointEx" => {
            let i = a.int(2);
            let Some(env) = envelope(&mut s.project, e) else { return Some(ret(Val::Bool(false))) };
            let Some(i) = index(env.points.len(), i) else { return Some(ret(Val::Bool(false))) };
            env.points.remove(i);
            s.changed = true;
            ret(Val::Bool(true))
        }
        "DeleteEnvelopePointRange" | "DeleteEnvelopePointRangeEx" => {
            let (t0, t1) = (a.num(base + 1), a.num(base + 2));
            let Some(env) = envelope(&mut s.project, e) else { return Some(ret(Val::Bool(false))) };
            let before = env.points.len();
            // REAPER 7.73 leaves the first point when the range covers them all.
            let first = env.points.first().cloned();
            env.points.retain(|p| p.time < t0 || p.time >= t1);
            if env.points.is_empty() {
                env.points.extend(first);
            }
            s.changed |= env.points.len() != before;
            ret(Val::Bool(true))
        }
        "GetEnvelopePointByTime" | "GetEnvelopePointByTimeEx" => {
            let t = a.num(base + 1);
            let i = envelope(&mut s.project, e).map_or(0, |env| env.points.partition_point(|p| p.time <= t));
            ret(Val::Int(i as i64 - 1))
        }
        "Envelope_Evaluate" => {
            let (t, rate, n) = (a.num(1), a.num(2).max(1.0), a.int(3).max(1));
            let Some(env) = envelope(&mut s.project, e) else {
                return Some((Val::Int(0), vec![Val::Num(0.0); 4]));
            };
            // Fader-scaled envelopes interpolate in fader position, the values scripts see.
            let scaled = fader_scaled(env);
            let raw = |v: f64| if scaled { scale_to_fader(v) } else { v };
            let neutral = raw(env.kind.neutral());
            let env = env.clone();
            let at = |t: f64| env.value_at(t).map(raw).unwrap_or(neutral);
            let step = 1.0 / rate;
            let (v0, v1, v2, v3) = (at(t), at(t + step), at(t + 2.0 * step), at(t + 3.0 * step));
            let d1 = v1 - v0;
            let d2 = v2 - 2.0 * v1 + v0;
            let d3 = v3 - 3.0 * v2 + 3.0 * v1 - v0;
            (Val::Int(n), vec![Val::Num(v0), Val::Num(d1), Val::Num(d2), Val::Num(d3)])
        }
        "Envelope_SortPoints" | "Envelope_SortPointsEx" => match envelope(&mut s.project, e) {
            Some(env) => {
                env.sort();
                ret(Val::Bool(true))
            }
            None => ret(Val::Bool(false)),
        },
        "CountAutomationItems" => ret(Val::Int(envelope(&mut s.project, e).map_or(0, |e| e.items.len() as i64))),
        "InsertAutomationItem" => {
            let (pool, pos, len) = (a.int(1), a.num(2).max(0.0), a.num(3));
            let timeline = s.project.tempo.timeline();
            let Some(env) = envelope(&mut s.project, e) else { return Some(ret(Val::Int(-1))) };
            if len.is_nan() || len <= 0.0 {
                return Some(ret(Val::Int(-1)));
            }
            let (length_qn, points) = capture(env, pos, len, &timeline);
            let n = env.items.len();
            let id = if pool >= 0 {
                // A pool that does not exist yet starts empty.
                if !s.project.automation_pools.iter().any(|p| p.id == pool) {
                    s.project.automation_pools.push(AutomationPool::new(pool, length_qn));
                }
                pool
            } else {
                let id = s.project.next_pool_id();
                s.project.automation_pools.push(AutomationPool { points, ..AutomationPool::new(id, length_qn) });
                id
            };
            if let Some(env) = envelope(&mut s.project, e) {
                env.items.push(AutomationItem::new(id, pos, len));
            }
            s.project.resolve_automation_items();
            s.changed = true;
            ret(Val::Int(n as i64))
        }
        "GetSetAutomationItemInfo" => ret(Val::Num(item_info(s, e, a))),
        "GetSetAutomationItemInfo_String" => {
            let (i, key, value, set) = (a.int(1), a.text(2), a.text(3), a.bool(4));
            let pool = envelope(&mut s.project, e).and_then(|env| index(env.items.len(), i).map(|i| env.items[i].pool));
            let found = pool.and_then(|id| s.project.automation_pools.iter_mut().find(|p| p.id == id));
            match (found, key.as_str()) {
                (Some(p), "P_POOL_NAME") => {
                    if set {
                        p.name = value;
                        s.changed = true;
                    }
                    (Val::Bool(true), vec![Val::Str(p.name.clone())])
                }
                _ => (Val::Bool(false), vec![Val::Str(String::new())]),
            }
        }
        "GetGlobalAutomationOverride" => ret(Val::Int(GlobalAutomation::rpp_code(s.project.global_automation))),
        "SetGlobalAutomationOverride" => {
            s.project.global_automation = GlobalAutomation::from_rpp(a.int(0));
            s.changed = true;
            ret(Val::Nil)
        }
        _ => return None,
    })
}

/// How an automation item shows its pool to scripts, as REAPER 7.73's `…Ex` point calls do:
/// every pass of the pool inside the item, in project seconds, with the item's baseline and
/// amplitude applied, in the envelope's raw values (fader positions when fader-scaled).
struct ItemMap {
    item: AutomationItem,
    q0: f64,
    offset_qn: f64,
    rate: f64,
    scale: Scale,
    timeline: scythe_core::tempo::Timeline,
}

impl ItemMap {
    fn new(project: &Project, env: &Envelope, item: &AutomationItem) -> Self {
        let timeline = project.tempo.timeline();
        let q0 = timeline.qn_at(item.position);
        let offset_qn = timeline.qn_at(item.position + item.start_offset) - q0;
        let rate = if item.rate > 0.0 { item.rate } else { 1.0 };
        Self { item: item.clone(), q0, offset_qn, rate, scale: Scale::of(env), timeline }
    }

    fn raw(&self, n: f64) -> f64 {
        match self.scale {
            Scale::Fader => n * scale_to_fader(2.0),
            s => s.denormalize(n),
        }
    }

    fn normalized(&self, v: f64) -> f64 {
        match self.scale {
            Scale::Fader => v / scale_to_fader(2.0),
            s => s.normalize(v),
        }
    }

    /// The pool time a project time plays, in the pass that covers it.
    fn pool_time(&self, t: f64, length_qn: f64) -> f64 {
        let qn = (self.timeline.qn_at(t) - self.q0) * self.rate + self.offset_qn;
        if self.item.looped && length_qn > 0.0 { qn.rem_euclid(length_qn) } else { qn }
    }

    fn time_of(&self, qn: f64) -> f64 {
        self.timeline.time_at_qn(self.q0 + (qn - self.offset_qn) / self.rate)
    }

    /// (pool point index, project seconds, raw value) for every point the item plays.
    fn visible(&self, pool: &AutomationPool) -> Vec<(usize, f64, f64)> {
        let (start, end) = (self.item.position, self.item.end());
        let shown = |n: f64| self.raw(self.item.baseline + self.item.amplitude * (n - 0.5));
        let len = pool.length_qn;
        let passes: Vec<f64> = if self.item.looped && len > 0.0 {
            let first = (self.offset_qn / len).floor();
            let last = ((self.pool_time_unwrapped(end)) / len).ceil();
            (first as i64..=last as i64).map(|k| k as f64 * len).take(10_000).collect()
        } else {
            vec![0.0]
        };
        passes
            .into_iter()
            .flat_map(|base| pool.points.iter().enumerate().map(move |(i, p)| (i, base + p.time, p.value)))
            .map(|(i, qn, n)| (i, self.time_of(qn), shown(n)))
            .filter(|&(_, t, _)| t >= start - 1e-9 && t < end - 1e-9)
            .collect()
    }

    fn pool_time_unwrapped(&self, t: f64) -> f64 {
        (self.timeline.qn_at(t) - self.q0) * self.rate + self.offset_qn
    }

    /// A raw value as the pool stores it, undoing the item's baseline and amplitude.
    fn stored(&self, v: f64) -> f64 {
        let n = self.normalized(v);
        if self.item.amplitude.abs() > 1e-9 { 0.5 + (n - self.item.baseline) / self.item.amplitude } else { 0.5 }
    }
}

/// The `…Ex` point calls on automation item `ai` of the envelope: they read and edit its pool.
fn item_points(s: &mut State, name: &str, a: &Args, e: Option<Env>, ai: usize) -> Ret {
    let found = envelope(&mut s.project, e).and_then(|env| env.items.get(ai).cloned().map(|i| (env.clone(), i)));
    let Some((env, item)) = found else { return ret(Val::Bool(false)) };
    let map = ItemMap::new(&s.project, &env, &item);
    let Some(pool) = s.project.automation_pools.iter_mut().find(|p| p.id == item.pool) else {
        return ret(Val::Bool(false));
    };
    let visible = map.visible(pool);
    let empty = || vec![Val::Num(0.0), Val::Num(0.0), Val::Int(0), Val::Num(0.0), Val::Bool(false)];
    let out = |pool: &AutomationPool, (i, t, v): (usize, f64, f64)| {
        let p = &pool.points[i];
        let tension = if let PointShape::Bezier(t) = p.shape { t } else { 0.0 };
        vec![Val::Num(t), Val::Num(v), Val::Int(p.shape.rpp_code()), Val::Num(tension), Val::Bool(false)]
    };
    let sort = |pool: &mut AutomationPool| pool.points.sort_by(|a, b| a.time.total_cmp(&b.time));
    let result = match name {
        "CountEnvelopePointsEx" => return ret(Val::Int(visible.len() as i64)),
        "GetEnvelopePointEx" => match index(visible.len(), a.int(2)) {
            Some(k) => return (Val::Bool(true), out(pool, visible[k])),
            None => return (Val::Bool(false), empty()),
        },
        "GetEnvelopePointByTimeEx" => {
            let t = a.num(2);
            return ret(Val::Int(visible.partition_point(|&(_, pt, _)| pt <= t + 1e-9) as i64 - 1));
        }
        "InsertEnvelopePointEx" => {
            let qn = map.pool_time(a.num(2), pool.length_qn);
            let shape = PointShape::from_rpp(a.int(4), a.num(5));
            pool.points.push(EnvPoint { shape, ..EnvPoint::new(qn, map.stored(a.num(3))) });
            if !a.bool(7) {
                sort(pool);
            }
            (Val::Bool(true), vec![Val::Bool(a.bool(7))])
        }
        "SetEnvelopePointEx" => {
            let Some(k) = index(visible.len(), a.int(2)) else { return ret(Val::Bool(false)) };
            let (i, t, v) = visible[k];
            let p = &mut pool.points[i];
            if let Some(t) = a.opt(3) {
                p.time = map.pool_time(t, 0.0).rem_euclid(pool.length_qn.max(f64::MIN_POSITIVE));
            }
            p.value = map.stored(a.opt(4).unwrap_or(v));
            if a.opt(5).is_some() || a.opt(6).is_some() {
                let tension = a.opt(6).unwrap_or(if let PointShape::Bezier(t) = p.shape { t } else { 0.0 });
                p.shape = PointShape::from_rpp(a.opt(5).map_or(p.shape.rpp_code(), |v| v as i64), tension);
            }
            let outs = out(pool, (i, a.opt(3).unwrap_or(t), a.opt(4).unwrap_or(v)));
            if !a.bool(8) {
                sort(pool);
            }
            (Val::Bool(true), outs.into_iter().chain([Val::Bool(a.bool(8))]).collect())
        }
        "DeleteEnvelopePointEx" => match index(visible.len(), a.int(2)) {
            Some(k) => {
                pool.points.remove(visible[k].0);
                ret(Val::Bool(true))
            }
            None => return ret(Val::Bool(false)),
        },
        "DeleteEnvelopePointRangeEx" => {
            let (from, to) = (a.num(2), a.num(3));
            let mut gone: Vec<usize> =
                visible.iter().filter(|&&(_, t, _)| t >= from && t < to).map(|&(i, _, _)| i).collect();
            gone.sort_unstable();
            gone.dedup();
            for i in gone.into_iter().rev() {
                pool.points.remove(i);
            }
            ret(Val::Bool(true))
        }
        "Envelope_SortPointsEx" => {
            sort(pool);
            ret(Val::Bool(true))
        }
        _ => return ret(Val::Nil),
    };
    s.changed = true;
    s.project.resolve_automation_items();
    result
}

/// `GetSetAutomationItemInfo`: reads or sets one of an automation item's values.
fn item_info(s: &mut State, e: Option<Env>, a: &Args) -> f64 {
    let (i, key, value, set) = (a.int(1), a.text(2), a.num(3), a.bool(4));
    let Some(env) = envelope(&mut s.project, e) else { return 0.0 };
    let Some(i) = index(env.items.len(), i) else { return 0.0 };
    let item = &mut env.items[i];
    let pool = item.pool;
    let got = match key.as_str() {
        "D_POOL_ID" => {
            if set {
                item.pool = value as i64;
            }
            item.pool as f64
        }
        "D_POSITION" => set_or(&mut item.position, set, value),
        "D_LENGTH" => set_or(&mut item.length, set, value.max(0.0)),
        "D_STARTOFFS" => set_or(&mut item.start_offset, set, value),
        "D_PLAYRATE" => set_or(&mut item.rate, set && value > 0.0, value),
        "D_BASELINE" => set_or(&mut item.baseline, set, value.clamp(0.0, 1.0)),
        "D_AMPLITUDE" => set_or(&mut item.amplitude, set, value.clamp(-1.0, 1.0)),
        "D_LOOPSRC" => {
            if set {
                item.looped = value != 0.0;
            }
            item.looped as i64 as f64
        }
        "D_UISEL" => {
            if set {
                item.selected = value != 0.0;
            }
            item.selected as i64 as f64
        }
        "D_POOL_QNLEN" => {
            let Some(p) = s.project.automation_pools.iter_mut().find(|p| p.id == pool) else { return 0.0 };
            if set && value > 0.0 {
                p.length_qn = value;
            }
            let len = p.length_qn;
            s.changed |= set;
            s.project.resolve_automation_items();
            return len;
        }
        _ => return 0.0,
    };
    if set {
        s.changed = true;
        s.project.resolve_automation_items();
    }
    got
}

fn set_or(field: &mut f64, set: bool, value: f64) -> f64 {
    if set {
        *field = value;
    }
    *field
}

#[cfg(test)]
mod tests {
    use super::super::tests::{api, project, state};
    use super::*;

    #[test]
    fn fader_scaling_matches_reaper() {
        // ScaleToEnvelopeMode(1, x) in REAPER 7.73.
        for (amp, pos) in [(1.0, 716.21785031261), (0.5, 592.44867591629), (2.0, 852.77445440699)] {
            assert!((scale_to_fader(amp) - pos).abs() < 1e-6, "{amp}: {}", scale_to_fader(amp));
            assert!((scale_from_fader(pos) - amp).abs() < 1e-9);
        }
        assert!((scale_from_fader(500.0) - 0.27991910429543).abs() < 1e-9);
    }

    #[test]
    fn sends_and_receives() {
        let p = project();
        let (a, b) = (Val::Track(p.tracks[0].id), Val::Track(p.tracks[1].id));
        let mut s = state(p);
        assert_eq!(api(&mut s, "CreateTrackSend", &[a.clone(), b.clone()]).0, Val::Int(0));
        assert_eq!(api(&mut s, "GetTrackNumSends", &[a.clone(), Val::Int(0)]).0, Val::Int(1));
        assert_eq!(api(&mut s, "GetTrackNumSends", &[b.clone(), Val::Int(-1)]).0, Val::Int(1));
        let set = [a.clone(), Val::Int(0), Val::Int(0), Val::Str("D_VOL".into()), Val::Num(0.5)];
        assert_eq!(api(&mut s, "SetTrackSendInfo_Value", &set).0, Val::Bool(true));
        let get = [b.clone(), Val::Int(-1), Val::Int(0), Val::Str("D_VOL".into())];
        assert_eq!(api(&mut s, "GetTrackSendInfo_Value", &get).0, Val::Num(0.5));
        let dest = [a.clone(), Val::Int(0), Val::Int(0), Val::Str("P_DESTTRACK".into())];
        assert_eq!(api(&mut s, "GetTrackSendInfo_Value", &dest).0, b);
        assert_eq!(api(&mut s, "GetTrackSendName", &[a.clone(), Val::Int(0)]).1, [Val::Str("Bass".into())]);
        assert_eq!(api(&mut s, "RemoveTrackSend", &[b, Val::Int(-1), Val::Int(0)]).0, Val::Bool(true));
        assert!(s.project.tracks[0].sends.is_empty());
    }

    #[test]
    fn hardware_outputs() {
        let p = project();
        let (a, master) = (Val::Track(p.tracks[0].id), Val::Track(p.master.id));
        let mut s = state(p);
        assert_eq!(api(&mut s, "CreateTrackSend", &[a.clone(), Val::Nil]).0, Val::Int(0));
        assert_eq!(api(&mut s, "GetTrackNumSends", &[a.clone(), Val::Int(1)]).0, Val::Int(1));
        assert_eq!(api(&mut s, "GetTrackNumSends", &[a.clone(), Val::Int(0)]).0, Val::Int(0));
        for (key, v) in [("I_DSTCHAN", 1026.0), ("I_SENDMODE", 3.0), ("D_VOL", 0.5)] {
            let set = [a.clone(), Val::Int(1), Val::Int(0), Val::Str(key.into()), Val::Num(v)];
            assert_eq!(api(&mut s, "SetTrackSendInfo_Value", &set).0, Val::Bool(true));
            let get = [a.clone(), Val::Int(1), Val::Int(0), Val::Str(key.into())];
            assert_eq!(api(&mut s, "GetTrackSendInfo_Value", &get).0, Val::Num(v));
        }
        let o = &s.project.tracks[0].hw_outs[0];
        assert_eq!((o.label(), o.mode), ("Out 3 (mono)".to_owned(), SendMode::PostFx));
        assert_eq!(api(&mut s, "GetTrackNumSends", &[master.clone(), Val::Int(1)]).0, Val::Int(1));
        assert_eq!(api(&mut s, "RemoveTrackSend", &[master, Val::Int(1), Val::Int(0)]).0, Val::Bool(true));
        assert!(s.project.master.hw_outs.is_empty());
    }

    #[test]
    fn envelope_points_in_fader_scaling() {
        let mut p = project();
        p.tracks[0].envelope_mut(EnvelopeKind::Volume);
        let tr = Val::Track(p.tracks[0].id);
        let mut s = state(p);
        let env = api(&mut s, "GetTrackEnvelopeByName", &[tr, Val::Str("volume".into())]).0;
        assert!(matches!(env, Val::Env(_)));
        assert_eq!(api(&mut s, "GetEnvelopeScalingMode", std::slice::from_ref(&env)).0, Val::Int(1));
        let insert = |t: f64, v: f64| {
            [env.clone(), Val::Num(t), Val::Num(v), Val::Int(0), Val::Num(0.0), Val::Bool(false), Val::Bool(true)]
        };
        api(&mut s, "InsertEnvelopePoint", &insert(3.0, 716.0));
        api(&mut s, "InsertEnvelopePoint", &insert(1.0, 0.0));
        api(&mut s, "Envelope_SortPoints", std::slice::from_ref(&env));
        let pts = &s.project.tracks[0].envelopes[0].points;
        assert_eq!(pts.iter().map(|p| p.time).collect::<Vec<_>>(), [1.0, 3.0]);
        assert!((pts[1].value - 0.998842898261).abs() < 1e-9, "stored as amplitude");
        let (ok, outs) = api(&mut s, "GetEnvelopePoint", &[env.clone(), Val::Int(1)]);
        assert_eq!(ok, Val::Bool(true));
        assert!((outs[1].num() - 716.0).abs() < 1e-9);
        let (_, outs) = api(&mut s, "Envelope_Evaluate", &[env.clone(), Val::Num(2.0), Val::Num(48000.0), Val::Int(1)]);
        assert!((outs[0].num() - 358.0).abs() < 1e-6, "linear between fader positions: {:?}", outs[0]);
        assert_eq!(api(&mut s, "GetEnvelopePointByTime", &[env.clone(), Val::Num(2.0)]).0, Val::Int(0));
        api(&mut s, "DeleteEnvelopePointRange", &[env.clone(), Val::Num(2.5), Val::Num(4.0)]);
        assert_eq!(api(&mut s, "CountEnvelopePoints", &[env]).0, Val::Int(1));
    }

    struct Gain(std::cell::Cell<f64>);

    impl super::super::LiveFx for Gain {
        fn params(&self) -> Vec<super::super::FxParam> {
            let value = self.0.get();
            vec![super::super::FxParam {
                name: "Gain".into(),
                min: -24.0,
                max: 24.0,
                value,
                text: format!("{value:.1} dB"),
            }]
        }

        fn set_param(&self, _: usize, value: f64) {
            self.0.set(value);
        }

        fn is_instrument(&self) -> bool {
            false
        }
    }

    #[test]
    fn fx_windows_open_and_close_as_trackfx_show_says() {
        let mut p = project();
        p.tracks[0].fx.push(scythe_core::FxSlot { name: "VST: ReaEQ (Cockos)".into(), enabled: true, raw: Vec::new() });
        let id = p.tracks[0].id;
        let mut s = super::super::State::new(&p, super::super::ScriptContext::default());
        let (tr, fx) = (Val::Track(id), Val::Int(0));
        assert_eq!(api(&mut s, "TrackFX_GetOpen", &[tr.clone(), fx.clone()]).0, Val::Bool(false));
        assert_eq!(api(&mut s, "TrackFX_GetFloatingWindow", &[tr.clone(), fx.clone()]).0, Val::Nil);
        api(&mut s, "TrackFX_Show", &[tr.clone(), fx.clone(), Val::Int(3)]);
        assert_eq!(api(&mut s, "TrackFX_GetOpen", &[tr.clone(), fx.clone()]).0, Val::Bool(true));
        assert_ne!(api(&mut s, "TrackFX_GetFloatingWindow", &[tr.clone(), fx.clone()]).0, Val::Nil);
        api(&mut s, "TrackFX_SetOpen", &[tr.clone(), fx.clone(), Val::Bool(false)]);
        assert_eq!(api(&mut s, "TrackFX_GetOpen", &[tr.clone(), fx.clone()]).0, Val::Bool(false));
        api(&mut s, "TrackFX_Show", &[tr.clone(), Val::Int(5), Val::Int(3)]);
        assert_eq!(s.out.fx_windows, [(id, 0, 3), (id, 0, 2)], "no slot 5");
    }

    #[test]
    fn fx_parameters_reach_the_running_effect() {
        let mut p = project();
        p.tracks[0].fx.push(scythe_core::FxSlot { name: "VST: Gain (Test)".into(), enabled: true, raw: Vec::new() });
        let id = p.tracks[0].id;
        let gain = std::rc::Rc::new(Gain(std::cell::Cell::new(0.0)));
        let mut ctx = super::super::ScriptContext::default();
        ctx.fx.insert(id, vec![Some(gain.clone() as std::rc::Rc<dyn super::super::LiveFx>)]);
        let mut s = super::super::State::new(&p, ctx);
        let (tr, fx, param) = (Val::Track(id), Val::Int(0), Val::Int(0));
        assert_eq!(api(&mut s, "TrackFX_GetNumParams", &[tr.clone(), fx.clone()]).0, Val::Int(1));
        assert_eq!(
            api(&mut s, "TrackFX_SetParamNormalized", &[tr.clone(), fx.clone(), param.clone(), Val::Num(0.75)]).0,
            Val::Bool(true)
        );
        assert_eq!(gain.0.get(), 12.0);
        let (v, range) = api(&mut s, "TrackFX_GetParam", &[tr.clone(), fx.clone(), param.clone()]);
        assert_eq!((v, range), (Val::Num(12.0), vec![Val::Num(-24.0), Val::Num(24.0)]));
        let text = api(&mut s, "TrackFX_GetFormattedParamValue", &[tr.clone(), fx.clone(), param.clone()]).1;
        assert_eq!(text, [Val::Str("12.0 dB".into())]);
        assert_eq!(api(&mut s, "TrackFX_GetParamName", &[tr.clone(), fx.clone(), param]).1, [Val::Str("Gain".into())]);
        assert_eq!(
            api(&mut s, "TrackFX_GetByName", &[tr.clone(), Val::Str("gain".into()), Val::Bool(false)]).0,
            Val::Int(0)
        );
        assert!(s.out.fx_touched);
        assert_eq!(api(&mut s, "TrackFX_Delete", &[tr, fx]).0, Val::Bool(true));
        assert!(s.ctx.fx[&id].is_empty(), "the running effects follow the slots");
    }

    #[test]
    fn fx_names_match_as_reaper_looks_them_up() {
        assert!(fx_name_matches("VST: ReaEQ (Cockos)", "ReaEQ"));
        assert!(fx_name_matches("VST: ReaEQ (Cockos)", "vst: reaeq"));
        assert!(!fx_name_matches("VST: ReaEQ (Cockos)", "VST3: ReaEQ"));
        assert!(!fx_name_matches("VST: ReaEQ (Cockos)", "ReaComp"));
    }

    #[test]
    fn ext_values_live_in_an_ext_chunk() {
        let mut extra = Vec::new();
        assert_eq!(ext_value(&mut extra, "mine", None), "");
        ext_value(&mut extra, "mine", Some("a b".into()));
        ext_value(&mut extra, "mine", Some("c d".into()));
        assert_eq!(ext_value(&mut extra, "mine", None), "c d");
        assert_eq!(extra.len(), 1);
    }

    /// Values from the same calls in REAPER 7.73 (`GetSetAutomationItemInfo`,
    /// `GetEnvelopePointEx` on a looped pooled copy, `Envelope_Evaluate`).
    #[test]
    fn automation_items_as_reaper_shows_them() {
        let mut p = project();
        let pan = p.tracks[0].envelope_mut(EnvelopeKind::Pan);
        pan.points = vec![EnvPoint::new(0.0, 0.2), EnvPoint::new(8.0, -0.4)];
        let tr = Val::Track(p.tracks[0].id);
        let mut s = state(p);
        let env = api(&mut s, "GetTrackEnvelopeByName", &[tr, Val::Str("Pan".into())]).0;
        let call = |s: &mut State, name: &str, args: &[Val]| {
            let mut all = vec![env.clone()];
            all.extend_from_slice(args);
            api(s, name, &all)
        };
        assert_eq!(call(&mut s, "InsertAutomationItem", &[Val::Int(-1), Val::Num(1.0), Val::Num(2.0)]).0, Val::Int(0));
        call(&mut s, "DeleteEnvelopePointRangeEx", &[Val::Int(0), Val::Num(0.0), Val::Num(100.0)]);
        for (t, v, shape) in [(1.0, -1.0, 0), (2.0, 1.0, 2), (2.5, 0.5, 0)] {
            let pt = [
                Val::Int(0),
                Val::Num(t),
                Val::Num(v),
                Val::Int(shape),
                Val::Num(0.0),
                Val::Bool(false),
                Val::Bool(true),
            ];
            call(&mut s, "InsertEnvelopePointEx", &pt);
        }
        call(&mut s, "Envelope_SortPointsEx", &[Val::Int(0)]);
        let pool = call(
            &mut s,
            "GetSetAutomationItemInfo",
            &[Val::Int(0), Val::Str("D_POOL_ID".into()), Val::Num(0.0), Val::Bool(false)],
        )
        .0;
        assert_eq!(
            call(&mut s, "InsertAutomationItem", &[Val::Int(pool.num() as i64), Val::Num(4.0), Val::Num(3.0)]).0,
            Val::Int(1)
        );
        for (key, v) in [("D_PLAYRATE", 1.5), ("D_STARTOFFS", 0.25), ("D_BASELINE", 0.4), ("D_AMPLITUDE", -0.5)] {
            call(
                &mut s,
                "GetSetAutomationItemInfo",
                &[Val::Int(1), Val::Str(key.into()), Val::Num(v), Val::Bool(true)],
            );
        }
        assert_eq!(call(&mut s, "CountEnvelopePointsEx", &[Val::Int(1)]).0, Val::Int(6));
        let (_, outs) = call(&mut s, "GetEnvelopePointEx", &[Val::Int(1), Val::Int(1)]);
        assert!((outs[0].num() - 4.8333).abs() < 1e-4 && (outs[1].num() + 0.45).abs() < 1e-9, "{outs:?}");
        for (t, want) in [(1.25, -0.5), (2.0, 1.0), (4.5, -0.7), (5.25, 0.175), (7.5, -0.3625)] {
            let (_, outs) = call(&mut s, "Envelope_Evaluate", &[Val::Num(t), Val::Num(48000.0), Val::Int(1)]);
            assert!((outs[0].num() - want).abs() < 1e-4, "{t}: {outs:?}");
        }
    }
}

// ---- control surface style edits -----------------------------------------------------------

#[derive(Clone, Copy)]
enum Level {
    Volume,
    Pan,
    Width,
}

/// Sets (or with `relative` moves: volume in dB, pan and width by the amount) a track's level
/// the way a control surface does, carrying it over to group followers when `gang` is set.
/// Answers the new value.
fn ui_level(s: &mut State, id: Option<TrackId>, level: Level, value: f64, relative: bool, gang: bool) -> f64 {
    let index = id.and_then(|t| s.project.track_index(t));
    let Some(t) = track(&mut s.project, id) else { return 0.0 };
    let new = match (level, relative) {
        (Level::Volume, true) => t.volume * scythe_core::units::db_to_gain(value),
        (Level::Volume, false) => value.max(0.0),
        (Level::Pan, true) => (t.pan + value).clamp(-1.0, 1.0),
        (Level::Pan, false) => value.clamp(-1.0, 1.0),
        (Level::Width, true) => (t.width + value).clamp(-1.0, 1.0),
        (Level::Width, false) => value.clamp(-1.0, 1.0),
    };
    match (index, gang, level) {
        (Some(i), true, Level::Volume) => s.project.set_volume_grouped(i, new),
        (Some(i), true, Level::Pan) => s.project.set_pan_grouped(i, new),
        (Some(i), true, Level::Width) => s.project.set_width_grouped(i, new),
        (_, _, Level::Volume) => t.volume = new,
        (_, _, Level::Pan) => t.pan = new,
        (_, _, Level::Width) => t.width = new,
    }
    s.changed = true;
    new
}

#[derive(Clone, Copy, PartialEq)]
enum Switch {
    Mute,
    Solo,
    Arm,
    Polarity,
}

/// Sets a track switch (negative toggles), carrying it over to group followers when `gang` is
/// set. Answers the new state, `None` without a track.
fn ui_switch(s: &mut State, id: Option<TrackId>, kind: Switch, value: i64, gang: bool) -> Option<bool> {
    let index = id.and_then(|t| s.project.track_index(t));
    let t = track(&mut s.project, id)?;
    let current = match kind {
        Switch::Mute => t.muted,
        Switch::Solo => t.solo,
        Switch::Arm => t.armed,
        Switch::Polarity => t.phase,
    };
    let on = if value < 0 { !current } else { value != 0 };
    if kind == Switch::Solo && on {
        t.solo_mode = scythe_core::SoloMode::InPlace;
    }
    match (index.filter(|_| gang), kind) {
        (Some(i), Switch::Mute) => s.project.set_mute_grouped(i, on),
        (Some(i), Switch::Solo) => s.project.set_solo_grouped(i, on),
        (Some(i), Switch::Arm) => s.project.set_arm_grouped(i, on),
        (Some(i), Switch::Polarity) => s.project.set_phase_grouped(i, on),
        (None, Switch::Mute) => t.muted = on,
        (None, Switch::Solo) => t.solo = on,
        (None, Switch::Arm) => t.armed = on,
        (None, Switch::Polarity) => t.phase = on,
    }
    s.changed = true;
    Some(on)
}
