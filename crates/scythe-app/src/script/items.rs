//! Items, their takes and sources, and the MIDI in MIDI takes.

use scythe_core::midi::{MUTED, MidiEvent, MidiSource, SELECTED};
use scythe_core::model::MIN_ITEM_LENGTH;
use scythe_core::rpp::Chunk;
use scythe_core::{Item, ItemId, Project, Source, Take, TrackId};

use super::tracks::{ext_value, select_track};
use super::{Args, Ret, State, Val, index, ret, track};

fn item_mut(p: &mut Project, id: Option<ItemId>) -> Option<&mut Item> {
    let id = id?;
    p.tracks.iter_mut().find_map(|t| t.item_mut(id))
}

fn take_mut(p: &mut Project, take: Option<(ItemId, usize)>) -> Option<&mut Take> {
    let (item, k) = take?;
    item_mut(p, Some(item))?.takes.get_mut(k)
}

/// Every item, in track order.
fn all_items(p: &Project) -> Vec<ItemId> {
    p.tracks.iter().flat_map(|t| t.items.iter().map(|i| i.id)).collect()
}

/// The selected items in track order.
fn selected_items(s: &State) -> Vec<ItemId> {
    all_items(&s.project).into_iter().filter(|i| s.sel_items.contains(i)).collect()
}

fn select_item(s: &mut State, id: ItemId, on: bool) {
    if on != s.sel_items.contains(&id) {
        if on {
            s.sel_items.push(id);
        } else {
            s.sel_items.retain(|&x| x != id);
        }
        s.sel_changed = true;
    }
}

pub(super) fn call(s: &mut State, name: &str, a: &Args) -> Option<Ret> {
    let item = a.get(0).item();
    Some(match name {
        "CountMediaItems" => ret(Val::Int(all_items(&s.project).len() as i64)),
        "GetMediaItem" => {
            let items = all_items(&s.project);
            ret(index(items.len(), a.int(1)).map_or(Val::Nil, |i| Val::Item(items[i])))
        }
        "CountTrackMediaItems" | "GetTrackNumMediaItems" => {
            ret(Val::Int(track(&mut s.project, a.get(0).track()).map_or(0, |t| t.items.len() as i64)))
        }
        "GetTrackMediaItem" => {
            let i = a.int(1);
            let found =
                track(&mut s.project, a.get(0).track()).and_then(|t| index(t.items.len(), i).map(|i| t.items[i].id));
            ret(found.map_or(Val::Nil, Val::Item))
        }
        "GetMediaItem_Track" | "GetMediaItemTrack" => {
            ret(item.and_then(|i| s.project.find_item(i)).map_or(Val::Nil, |(t, _)| Val::Track(t)))
        }
        "CountSelectedMediaItems" => ret(Val::Int(selected_items(s).len() as i64)),
        "GetSelectedMediaItem" => {
            let list = selected_items(s);
            ret(index(list.len(), a.int(1)).map_or(Val::Nil, |i| Val::Item(list[i])))
        }
        "IsMediaItemSelected" => ret(Val::Bool(item.is_some_and(|i| s.sel_items.contains(&i)))),
        "SetMediaItemSelected" => {
            if let Some(i) = item.filter(|&i| s.project.find_item(i).is_some()) {
                select_item(s, i, a.bool(1));
            }
            ret(Val::Nil)
        }
        "SelectAllMediaItems" => {
            let on = a.bool(1);
            let all = if on { all_items(&s.project) } else { Vec::new() };
            if all != s.sel_items {
                s.sel_items = all;
                s.sel_changed = true;
            }
            ret(Val::Nil)
        }
        "GetMediaItemInfo_Value" => {
            let selected = item.is_some_and(|i| s.sel_items.contains(&i));
            let Some((tid, it)) = item.and_then(|i| s.project.find_item(i)) else { return Some(ret(Val::Num(0.0))) };
            let number = s.project.track(tid).and_then(|t| t.items.iter().position(|x| x.id == it.id));
            ret(Val::Num(match a.text(1).as_str() {
                "D_POSITION" => it.position,
                "D_LENGTH" => it.length,
                "B_MUTE" | "B_MUTE_ACTUAL" => it.muted as u8 as f64,
                "D_VOL" => it.volume,
                "B_LOOPSRC" => it.looped as u8 as f64,
                "C_BEATATTACHMODE" => it.timebase.map_or(-1.0, |b| b.rpp_code() as f64),
                "D_FADEINLEN" => it.fade_in.length,
                "D_FADEOUTLEN" => it.fade_out.length,
                "D_FADEINLEN_AUTO" => it.fade_in.auto_length,
                "D_FADEOUTLEN_AUTO" => it.fade_out.auto_length,
                "C_FADEINSHAPE" => it.fade_in.shape as f64,
                "C_FADEOUTSHAPE" => it.fade_out.shape as f64,
                "D_FADEINDIR" => it.fade_in.curve,
                "D_FADEOUTDIR" => it.fade_out.curve,
                "I_CURTAKE" => it.active_take as f64,
                "I_FIXEDLANE" => it.lane() as f64,
                "B_UISEL" => selected as u8 as f64,
                "IP_ITEMNUMBER" => number.map_or(-1.0, |n| n as f64),
                "P_TRACK" => return Some(ret(Val::Track(tid))),
                _ => 0.0,
            }))
        }
        "SetMediaItemInfo_Value" => {
            let (key, v) = (a.text(1), a.num(2));
            if key == "B_UISEL" {
                if let Some(i) = item.filter(|&i| s.project.find_item(i).is_some()) {
                    select_item(s, i, v != 0.0);
                }
                return Some(ret(Val::Bool(item.is_some())));
            }
            let Some(it) = item_mut(&mut s.project, item) else { return Some(ret(Val::Bool(false))) };
            match key.as_str() {
                "D_POSITION" => it.position = v.max(0.0),
                "D_LENGTH" => it.length = v.max(MIN_ITEM_LENGTH),
                "B_MUTE" => it.muted = v != 0.0,
                "D_VOL" => it.volume = v.max(0.0),
                "B_LOOPSRC" => it.looped = v != 0.0,
                "C_BEATATTACHMODE" => it.timebase = scythe_core::timebase::Timebase::from_rpp(v as i64),
                "D_FADEINLEN" => it.fade_in.length = v.max(0.0),
                "D_FADEOUTLEN" => it.fade_out.length = v.max(0.0),
                "D_FADEINLEN_AUTO" => it.fade_in.auto_length = v,
                "D_FADEOUTLEN_AUTO" => it.fade_out.auto_length = v,
                "C_FADEINSHAPE" => it.fade_in.shape = v.clamp(0.0, 6.0) as u8,
                "C_FADEOUTSHAPE" => it.fade_out.shape = v.clamp(0.0, 6.0) as u8,
                "D_FADEINDIR" => it.fade_in.curve = v.clamp(-1.0, 1.0),
                "D_FADEOUTDIR" => it.fade_out.curve = v.clamp(-1.0, 1.0),
                "I_CURTAKE" => it.active_take = (v.max(0.0) as usize).min(it.takes.len().saturating_sub(1)),
                _ => return Some(ret(Val::Bool(false))),
            }
            s.changed = true;
            ret(Val::Bool(true))
        }
        "GetSetMediaItemInfo_String" => {
            let (key, value, set) = (a.text(1), a.text(2), a.bool(3));
            let Some(it) = item_mut(&mut s.project, item) else {
                return Some((Val::Bool(false), vec![Val::Str(String::new())]));
            };
            let got = match key.as_str() {
                "GUID" if set => Some(std::mem::replace(&mut it.guid, value.clone())).map(|_| value),
                "GUID" => Some(it.guid.clone()),
                k => k.strip_prefix("P_EXT:").map(|k| ext_value(&mut it.extra, k, set.then_some(value))),
            };
            s.changed |= set && got.is_some();
            (Val::Bool(got.is_some()), vec![Val::Str(got.unwrap_or_default())])
        }
        "SetMediaItemPosition" | "SetMediaItemLength" => {
            let v = a.num(1);
            let Some(it) = item_mut(&mut s.project, item) else { return Some(ret(Val::Bool(false))) };
            if name == "SetMediaItemPosition" {
                it.position = v.max(0.0);
            } else {
                it.length = v.max(MIN_ITEM_LENGTH);
            }
            s.changed = true;
            ret(Val::Bool(true))
        }
        "AddMediaItemToTrack" => {
            let Some(t) = a.get(0).track().and_then(|t| s.project.track_mut(t)) else { return Some(ret(Val::Nil)) };
            let mut it = Item::new(0.0, 0.0, Take::from_file(Default::default()));
            it.takes.clear();
            let id = it.id;
            t.items.push(it);
            s.changed = true;
            ret(Val::Item(id))
        }
        "CreateNewMIDIItemInProj" => {
            let (mut start, mut end) = (a.num(1), a.num(2));
            if a.bool(3) {
                let tl = s.project.tempo.timeline();
                (start, end) = (tl.time_at_qn(start), tl.time_at_qn(end));
            }
            if end <= start {
                return Some(ret(Val::Nil));
            }
            let tl = s.project.tempo.timeline();
            let length_qn = tl.qn_at(end) - tl.qn_at(start);
            let Some(t) = a.get(0).track().and_then(|t| s.project.track_mut(t)) else { return Some(ret(Val::Nil)) };
            let midi = MidiSource {
                ticks_per_qn: 960,
                events: Vec::new(),
                long: Vec::new(),
                length_ticks: (length_qn * 960.0).round() as u64,
                cc_interp: 32,
            };
            let take = Take {
                name: String::new(),
                source: Source::Raw(midi.to_chunk()),
                ..Take::from_file(Default::default())
            };
            let it = Item::new(start, end - start, take);
            let id = it.id;
            t.items.push(it);
            s.changed = true;
            ret(Val::Item(id))
        }
        "SplitMediaItem" => {
            let Some((tid, _)) = item.and_then(|i| s.project.find_item(i)) else { return Some(ret(Val::Nil)) };
            let t = s.project.track_mut(tid).unwrap();
            let Some(right) = t.item_mut(item.unwrap()).and_then(|i| i.split_at(a.num(1))) else {
                return Some(ret(Val::Nil));
            };
            let right_id = right.id;
            t.items.push(right);
            s.changed = true;
            ret(Val::Item(right_id))
        }
        "DeleteTrackMediaItem" => {
            let item = a.get(1).item();
            let Some(t) = track(&mut s.project, a.get(0).track()) else { return Some(ret(Val::Bool(false))) };
            let before = t.items.len();
            t.items.retain(|i| Some(i.id) != item);
            let removed = t.items.len() != before;
            s.changed |= removed;
            if let Some(i) = item.filter(|_| removed) {
                select_item(s, i, false);
            }
            ret(Val::Bool(removed))
        }
        "MoveMediaItemToTrack" => {
            let (Some(i), Some(t)) = (item, a.get(1).track()) else { return Some(ret(Val::Bool(false))) };
            let moved = s.project.move_item_to_track(i, t);
            s.changed |= moved;
            ret(Val::Bool(moved))
        }
        _ => return takes(s, name, a).or_else(|| midi(s, name, a)),
    })
}

/// REAPER's name for the kind of source a file is.
fn file_type(path: &std::path::Path) -> String {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "wav" | "w64" | "bwf" => "WAVE".into(),
        "aif" | "aiff" => "AIFF".into(),
        "mp3" => "MP3".into(),
        "flac" => "FLAC".into(),
        "ogg" => "VORBIS".into(),
        "opus" => "OPUS".into(),
        "wv" => "WAVPACK".into(),
        "rpp" => "RPP_PROJECT".into(),
        "mid" | "midi" => "MIDI".into(),
        "mp4" | "mov" | "mkv" | "webm" | "avi" => "VIDEO".into(),
        "" => "WAVE".into(),
        other => other.to_ascii_uppercase(),
    }
}

/// The handle of a source, the same one each time a script asks for an equal source.
fn source_handle(s: &mut State, src: &Source) -> usize {
    match s.sources.iter().position(|x| x == src) {
        Some(i) => i,
        None => {
            s.sources.push(src.clone());
            s.sources.len() - 1
        }
    }
}

fn is_midi(src: &Source) -> bool {
    matches!(src, Source::Raw(c) if c.args().first().is_some_and(|k| k == "MIDI"))
}

fn takes(s: &mut State, name: &str, a: &Args) -> Option<Ret> {
    let item = a.get(0).item();
    let take = a.get(0).take();
    Some(match name {
        "CountTakes" | "GetMediaItemNumTakes" => {
            ret(Val::Int(item_mut(&mut s.project, item).map_or(0, |i| i.takes.len() as i64)))
        }
        "GetTake" | "GetMediaItemTake" => {
            let k = a.int(1);
            let n = item_mut(&mut s.project, item).map_or(0, |i| i.takes.len());
            // A negative index means the active take, as REAPER's does.
            let k = if k < 0 { item_mut(&mut s.project, item).map(|i| i.active_take) } else { index(n, k) };
            ret(match (item, k) {
                (Some(i), Some(k)) if k < n => Val::Take(i, k),
                _ => Val::Nil,
            })
        }
        "GetActiveTake" => {
            let active = item_mut(&mut s.project, item).filter(|i| !i.takes.is_empty()).map(|i| (i.id, i.active_take));
            ret(active.map_or(Val::Nil, |(i, k)| Val::Take(i, k)))
        }
        "SetActiveTake" => {
            if let Some((i, k)) = take
                && let Some(it) = item_mut(&mut s.project, Some(i)).filter(|it| k < it.takes.len())
            {
                it.active_take = k;
                s.changed = true;
            }
            ret(Val::Nil)
        }
        "AddTakeToMediaItem" => {
            let Some(it) = item_mut(&mut s.project, item) else { return Some(ret(Val::Nil)) };
            it.takes.push(Take { name: String::new(), source: Source::Empty, ..Take::from_file(Default::default()) });
            let k = it.takes.len() - 1;
            let id = it.id;
            s.changed = true;
            ret(Val::Take(id, k))
        }
        "GetMediaItemTake_Item" => {
            ret(take.filter(|&t| take_mut(&mut s.project, Some(t)).is_some()).map_or(Val::Nil, |(i, _)| Val::Item(i)))
        }
        "GetMediaItemTake_Track" => {
            let t =
                take.filter(|&t| take_mut(&mut s.project, Some(t)).is_some()).and_then(|(i, _)| s.project.find_item(i));
            ret(t.map_or(Val::Nil, |(t, _)| Val::Track(t)))
        }
        "GetTakeName" => ret(take_mut(&mut s.project, take).map_or(Val::Nil, |t| Val::Str(t.name.clone()))),
        "GetSetMediaItemTakeInfo_String" => {
            let (key, value, set) = (a.text(1), a.text(2), a.bool(3));
            let Some(t) = take_mut(&mut s.project, take) else {
                return Some((Val::Bool(false), vec![Val::Str(String::new())]));
            };
            let got = match key.as_str() {
                "P_NAME" if set => Some(std::mem::replace(&mut t.name, value.clone())).map(|_| value),
                "P_NAME" => Some(t.name.clone()),
                k => k.strip_prefix("P_EXT:").map(|k| ext_value(&mut t.extra, k, set.then_some(value))),
            };
            s.changed |= set && got.is_some();
            (Val::Bool(got.is_some()), vec![Val::Str(got.unwrap_or_default())])
        }
        "GetMediaItemTakeInfo_Value" => {
            let key = a.text(1);
            let Some(t) = take_mut(&mut s.project, take) else { return Some(ret(Val::Num(0.0))) };
            let (item, k) = take.unwrap();
            ret(match key.as_str() {
                "D_STARTOFFS" => Val::Num(t.source_offset),
                "D_VOL" => Val::Num(t.volume),
                "D_PAN" => Val::Num(t.pan),
                "D_PLAYRATE" => Val::Num(t.playrate),
                "D_PITCH" => Val::Num(t.pitch),
                "B_PPITCH" => Val::Num(t.preserve_pitch as u8 as f64),
                "I_CHANMODE" => Val::Num(t.channel_mode.code() as f64),
                "IP_TAKENUMBER" => Val::Num(k as f64),
                "P_ITEM" => Val::Item(item),
                "P_TRACK" => s.project.find_item(item).map_or(Val::Nil, |(t, _)| Val::Track(t)),
                "P_SOURCE" => {
                    let src = t.source.clone();
                    Val::Src(source_handle(s, &src))
                }
                _ => Val::Num(0.0),
            })
        }
        "SetMediaItemTakeInfo_Value" => {
            let (key, v) = (a.text(1), a.num(2));
            let Some(t) = take_mut(&mut s.project, take) else { return Some(ret(Val::Bool(false))) };
            match key.as_str() {
                "D_STARTOFFS" => t.source_offset = v,
                "D_VOL" => t.volume = v,
                "D_PAN" => t.pan = v.clamp(-1.0, 1.0),
                "D_PLAYRATE" => t.playrate = v.max(1e-6),
                "D_PITCH" => t.pitch = v,
                "B_PPITCH" => t.preserve_pitch = v != 0.0,
                "I_CHANMODE" => t.channel_mode = scythe_core::ChannelMode::from_code(v as i64),
                _ => return Some(ret(Val::Bool(false))),
            }
            s.changed = true;
            ret(Val::Bool(true))
        }
        "TakeIsMIDI" => ret(Val::Bool(take_mut(&mut s.project, take).is_some_and(|t| is_midi(&t.source)))),
        "GetTakeNumStretchMarkers" => {
            ret(Val::Int(take_mut(&mut s.project, take).map_or(0, |t| t.stretch_markers.len() as i64)))
        }
        "GetTakeStretchMarker" => {
            let m = take_mut(&mut s.project, take)
                .and_then(|t| index(t.stretch_markers.len(), a.int(1)).map(|i| (i, t.stretch_markers[i])));
            match m {
                Some((i, m)) => (Val::Int(i as i64), vec![Val::Num(m.pos), Val::Num(m.src)]),
                None => (Val::Int(-1), vec![Val::Num(0.0), Val::Num(0.0)]),
            }
        }
        "SetTakeStretchMarker" => {
            let (idx, pos, src) = (a.int(1), a.num(2), a.opt(3));
            let Some(t) = take_mut(&mut s.project, take) else { return Some(ret(Val::Int(-1))) };
            let rate = if t.playrate > 0.0 { t.playrate } else { 1.0 };
            let src = src.unwrap_or_else(|| t.source_pos(pos / rate));
            let at = if idx < 0 {
                t.stretch_markers.push(scythe_core::stretch::StretchMarker::new(pos, src));
                t.stretch_markers.len() - 1
            } else if let Some(i) = index(t.stretch_markers.len(), idx) {
                t.stretch_markers[i] = scythe_core::stretch::StretchMarker { pos, src, ..t.stretch_markers[i] };
                i
            } else {
                return Some(ret(Val::Int(-1)));
            };
            let placed = t.stretch_markers[at];
            t.stretch_markers.sort_by(|x, y| x.pos.total_cmp(&y.pos));
            let i = t.stretch_markers.iter().position(|m| *m == placed).unwrap_or(at);
            s.changed = true;
            ret(Val::Int(i as i64))
        }
        "DeleteTakeStretchMarkers" => {
            let (idx, count) = (a.int(1), a.opt(2).map_or(1, |c| c.max(0.0) as usize));
            let Some(t) = take_mut(&mut s.project, take) else { return Some(ret(Val::Int(0))) };
            let Some(i) = index(t.stretch_markers.len(), idx) else { return Some(ret(Val::Int(0))) };
            let end = (i + count).min(t.stretch_markers.len());
            t.stretch_markers.drain(i..end);
            s.changed |= end > i;
            ret(Val::Int((end - i) as i64))
        }
        "GetTakeStretchMarkerSlope" => {
            let slope = take_mut(&mut s.project, take)
                .and_then(|t| index(t.stretch_markers.len(), a.int(1)).map(|i| t.stretch_markers[i].slope));
            ret(Val::Num(slope.unwrap_or(0.0)))
        }
        "SetTakeStretchMarkerSlope" => {
            let (idx, slope) = (a.int(1), a.num(2));
            let Some(t) = take_mut(&mut s.project, take) else { return Some(ret(Val::Bool(false))) };
            let Some(i) = index(t.stretch_markers.len(), idx) else { return Some(ret(Val::Bool(false))) };
            t.stretch_markers[i].slope = slope.clamp(-1.0, 1.0);
            s.changed = true;
            ret(Val::Bool(true))
        }
        "GetMediaItemTake_Source" => {
            let Some(src) = take_mut(&mut s.project, take).map(|t| t.source.clone()) else {
                return Some(ret(Val::Nil));
            };
            ret(Val::Src(source_handle(s, &src)))
        }
        "SetMediaItemTake_Source" => {
            let src = a.get(1).src().and_then(|i| s.sources.get(i).cloned());
            match (src, take_mut(&mut s.project, take)) {
                (Some(src), Some(t)) => {
                    t.source = src;
                    s.changed = true;
                    ret(Val::Bool(true))
                }
                _ => ret(Val::Bool(false)),
            }
        }
        "PCM_Source_CreateFromFile" => {
            let path = a.text(0);
            if path.is_empty() {
                return Some(ret(Val::Nil));
            }
            ret(Val::Src(source_handle(s, &Source::File(path.into()))))
        }
        "PCM_Source_Destroy" => ret(Val::Nil),
        "GetMediaSourceFileName" | "GetMediaSourceType" | "GetMediaSourceLength" => {
            let Some(src) = a.get(0).src().and_then(|i| s.sources.get(i).cloned()) else {
                return Some(match name {
                    "GetMediaSourceLength" => (Val::Num(0.0), vec![Val::Bool(false)]),
                    _ => (Val::Nil, vec![Val::Str(String::new())]),
                });
            };
            match name {
                "GetMediaSourceFileName" => {
                    let file = src.file().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                    (Val::Nil, vec![Val::Str(file)])
                }
                "GetMediaSourceType" => {
                    let kind = match &src {
                        Source::File(p) => file_type(p),
                        Source::Empty => "EMPTY".into(),
                        Source::Section(_) => "SECTION".into(),
                        Source::Raw(c) => c.args().first().cloned().unwrap_or_default(),
                    };
                    (Val::Nil, vec![Val::Str(kind)])
                }
                _ => match &src {
                    Source::Raw(c) if is_midi(&src) => {
                        let qn = MidiSource::parse(c).map_or(0.0, |m| m.length_qn());
                        (Val::Num(qn), vec![Val::Bool(true)])
                    }
                    Source::Section(sec) if sec.length > 0.0 => (Val::Num(sec.length), vec![Val::Bool(false)]),
                    _ => {
                        let length = src.file().and_then(|f| s.ctx.services.as_ref()?.media_length(f));
                        (Val::Num(length.unwrap_or(0.0)), vec![Val::Bool(false)])
                    }
                },
            }
        }
        _ => return None,
    })
}

// ---- MIDI ----------------------------------------------------------------------------------

/// A MIDI take's source and the chunk it came from.
fn midi_of(p: &mut Project, take: Option<(ItemId, usize)>) -> Option<(MidiSource, Chunk)> {
    let Source::Raw(c) = &take_mut(p, take)?.source else { return None };
    Some((MidiSource::parse(c)?, c.clone()))
}

/// Stores the events in time order: the RPP format has no other, so `noSort` changes nothing.
fn store_midi(s: &mut State, take: Option<(ItemId, usize)>, src: &MidiSource, chunk: &Chunk) {
    let mut src = src.clone();
    sort_midi(&mut src);
    if let Some(t) = take_mut(&mut s.project, take) {
        t.source = Source::Raw(src.write_chunk(chunk));
        s.changed = true;
    }
}

fn is_note_on(e: &MidiEvent) -> bool {
    e.bytes[0] & 0xf0 == 0x90 && e.bytes[2] > 0
}

fn is_note_off(e: &MidiEvent) -> bool {
    e.bytes[0] & 0xf0 == 0x80 || (e.bytes[0] & 0xf0 == 0x90 && e.bytes[2] == 0)
}

/// REAPER's end-of-source marker: all notes off at the very end.
fn is_end_marker(src: &MidiSource, e: &MidiEvent) -> bool {
    e.tick == src.length_ticks && e.bytes[0] & 0xf0 == 0xb0 && e.bytes[1] == 0x7b
}

/// The notes as event indices: each note-on with the note-off that ends it.
fn notes(src: &MidiSource) -> Vec<(usize, Option<usize>)> {
    let mut used = vec![false; src.events.len()];
    let mut out = Vec::new();
    for (i, e) in src.events.iter().enumerate() {
        if !is_note_on(e) {
            continue;
        }
        let off = (i + 1..src.events.len()).find(|&j| {
            let o = &src.events[j];
            !used[j] && is_note_off(o) && o.bytes[0] & 0x0f == e.bytes[0] & 0x0f && o.bytes[1] == e.bytes[1]
        });
        if let Some(j) = off {
            used[j] = true;
        }
        out.push((i, off));
    }
    out
}

/// The events REAPER counts as CC events: channel messages other than notes.
fn ccs(src: &MidiSource) -> Vec<usize> {
    (0..src.events.len())
        .filter(|&i| {
            let e = &src.events[i];
            matches!(e.bytes[0] & 0xf0, 0xa0..=0xe0) && !is_end_marker(src, e)
        })
        .collect()
}

/// Puts the events in time order, note-offs before note-ons at the same tick, and moves the
/// end marker past the last event.
fn sort_midi(src: &mut MidiSource) {
    let marker = src.events.iter().position(|e| is_end_marker(src, e)).map(|i| src.events.remove(i));
    let last = src.events.iter().map(|e| e.tick).max().unwrap_or(0);
    src.length_ticks = src.length_ticks.max(last);
    src.events.sort_by_key(|e| (e.tick, is_note_on(e)));
    let mut marker = marker.unwrap_or(MidiEvent { tick: 0, bytes: [0xb0, 0x7b, 0], len: 3, flags: 0, shape: None });
    marker.tick = src.length_ticks;
    src.events.push(marker);
}

fn set_flag(e: &mut MidiEvent, flag: u8, on: bool) {
    if on {
        e.flags |= flag;
    } else {
        e.flags &= !flag;
    }
}

fn event_len(status: u8) -> u8 {
    if matches!(status & 0xf0, 0xc0 | 0xd0) { 2 } else { 3 }
}

/// A take's placement: its item's start in quarter notes, its offset in quarter notes, its
/// play rate and the source's ticks per quarter note.
fn placement(s: &mut State, take: Option<(ItemId, usize)>) -> Option<(f64, f64, f64, f64)> {
    let tpq = midi_of(&mut s.project, take).map_or(960.0, |(m, _)| m.ticks_per_qn as f64);
    let tl = s.project.tempo.timeline();
    let (item, k) = take?;
    let (_, it) = s.project.find_item(item)?;
    let t = it.takes.get(k)?;
    let rate = if t.playrate > 0.0 { t.playrate } else { 1.0 };
    let offset_qn = t.source_offset * tl.bpm_at(it.position) / 60.0;
    Some((tl.qn_at(it.position), offset_qn, rate, tpq))
}

fn midi(s: &mut State, name: &str, a: &Args) -> Option<Ret> {
    let take = a.get(0).take();
    let tick = |v: f64| v.max(0.0).round() as u64;
    Some(match name {
        "MIDI_CountEvts" => {
            let Some((src, _)) = midi_of(&mut s.project, take) else {
                return Some((Val::Int(0), vec![Val::Int(0); 3]));
            };
            let (n, c, x) = (notes(&src).len() as i64, ccs(&src).len() as i64, src.long.len() as i64);
            (Val::Int(n + c + x), vec![Val::Int(n), Val::Int(c), Val::Int(x)])
        }
        "MIDI_GetNote" => {
            let note = midi_of(&mut s.project, take).and_then(|(src, _)| {
                let list = notes(&src);
                let (on, off) = list.get(usize::try_from(a.int(1)).ok()?).copied()?;
                let e = &src.events[on];
                let end = off.map_or(src.length_ticks, |j| src.events[j].tick);
                Some(vec![
                    Val::Bool(e.flags & SELECTED != 0),
                    Val::Bool(e.muted()),
                    Val::Num(e.tick as f64),
                    Val::Num(end as f64),
                    Val::Int((e.bytes[0] & 0x0f) as i64),
                    Val::Int(e.bytes[1] as i64),
                    Val::Int(e.bytes[2] as i64),
                ])
            });
            match note {
                Some(outs) => (Val::Bool(true), outs),
                None => (
                    Val::Bool(false),
                    vec![
                        Val::Bool(false),
                        Val::Bool(false),
                        Val::Num(0.0),
                        Val::Num(0.0),
                        Val::Int(0),
                        Val::Int(0),
                        Val::Int(0),
                    ],
                ),
            }
        }
        "MIDI_SetNote" => {
            let Some((mut src, chunk)) = midi_of(&mut s.project, take) else { return Some(ret(Val::Bool(false))) };
            let Some((on, off)) = notes(&src).get(a.int(1).max(0) as usize).copied().filter(|_| a.int(1) >= 0) else {
                return Some(ret(Val::Bool(false)));
            };
            // Absent or negative values leave a property as it is.
            let given = |i: usize| a.opt(i).filter(|v| *v >= 0.0);
            let both: Vec<usize> = std::iter::once(on).chain(off).collect();
            for &i in &both {
                let e = &mut src.events[i];
                if !a.get(2).is_nil() {
                    set_flag(e, SELECTED, a.bool(2));
                }
                if !a.get(3).is_nil() {
                    set_flag(e, MUTED, a.bool(3));
                }
                if let Some(c) = given(6) {
                    e.bytes[0] = e.bytes[0] & 0xf0 | (c as u8 & 0x0f);
                }
                if let Some(k) = given(7) {
                    e.bytes[1] = (k as u8).min(127);
                }
            }
            if let Some(t) = given(4) {
                src.events[on].tick = tick(t);
            }
            if let (Some(t), Some(j)) = (given(5), off) {
                src.events[j].tick = tick(t);
            }
            if let Some(v) = given(8) {
                src.events[on].bytes[2] = (v as u8).clamp(1, 127);
            }
            if !a.bool(9) {
                sort_midi(&mut src);
            }
            store_midi(s, take, &src, &chunk);
            ret(Val::Bool(true))
        }
        "MIDI_InsertNote" => {
            let Some((mut src, chunk)) = midi_of(&mut s.project, take) else { return Some(ret(Val::Bool(false))) };
            let flags = if a.bool(1) { SELECTED } else { 0 } | if a.bool(2) { MUTED } else { 0 };
            let (start, end) = (tick(a.num(3)), tick(a.num(4)));
            let (ch, key, vel) = ((a.int(5) & 0x0f) as u8, a.int(6).clamp(0, 127) as u8, a.int(7).clamp(1, 127) as u8);
            src.events.push(MidiEvent { tick: start, bytes: [0x90 | ch, key, vel], len: 3, flags, shape: None });
            src.events.push(MidiEvent { tick: end.max(start), bytes: [0x80 | ch, key, 0], len: 3, flags, shape: None });
            if !a.bool(8) {
                sort_midi(&mut src);
            }
            store_midi(s, take, &src, &chunk);
            ret(Val::Bool(true))
        }
        "MIDI_DeleteNote" => {
            let Some((mut src, chunk)) = midi_of(&mut s.project, take) else { return Some(ret(Val::Bool(false))) };
            let Some((on, off)) = notes(&src).get(a.int(1).max(0) as usize).copied().filter(|_| a.int(1) >= 0) else {
                return Some(ret(Val::Bool(false)));
            };
            if let Some(j) = off {
                src.events.remove(j);
            }
            src.events.remove(on);
            store_midi(s, take, &src, &chunk);
            ret(Val::Bool(true))
        }
        "MIDI_GetCC" => {
            let cc = midi_of(&mut s.project, take).and_then(|(src, _)| {
                let i = *ccs(&src).get(usize::try_from(a.int(1)).ok()?)?;
                let e = &src.events[i];
                Some(vec![
                    Val::Bool(e.flags & SELECTED != 0),
                    Val::Bool(e.muted()),
                    Val::Num(e.tick as f64),
                    Val::Int((e.bytes[0] & 0xf0) as i64),
                    Val::Int((e.bytes[0] & 0x0f) as i64),
                    Val::Int(e.bytes[1] as i64),
                    Val::Int(if e.len > 2 { e.bytes[2] as i64 } else { 0 }),
                ])
            });
            match cc {
                Some(outs) => (Val::Bool(true), outs),
                None => (
                    Val::Bool(false),
                    vec![
                        Val::Bool(false),
                        Val::Bool(false),
                        Val::Num(0.0),
                        Val::Int(0),
                        Val::Int(0),
                        Val::Int(0),
                        Val::Int(0),
                    ],
                ),
            }
        }
        "MIDI_SetCC" => {
            let Some((mut src, chunk)) = midi_of(&mut s.project, take) else { return Some(ret(Val::Bool(false))) };
            let Some(i) = ccs(&src).get(a.int(1).max(0) as usize).copied().filter(|_| a.int(1) >= 0) else {
                return Some(ret(Val::Bool(false)));
            };
            let given = |i: usize| a.opt(i).filter(|v| *v >= 0.0);
            let e = &mut src.events[i];
            if !a.get(2).is_nil() {
                set_flag(e, SELECTED, a.bool(2));
            }
            if !a.get(3).is_nil() {
                set_flag(e, MUTED, a.bool(3));
            }
            if let Some(t) = given(4) {
                e.tick = tick(t);
            }
            if let Some(m) = given(5).filter(|m| (0xa0..=0xef).contains(&(*m as i64))) {
                e.bytes[0] = m as u8 & 0xf0 | e.bytes[0] & 0x0f;
                e.len = event_len(e.bytes[0]);
            }
            if let Some(c) = given(6) {
                e.bytes[0] = e.bytes[0] & 0xf0 | (c as u8 & 0x0f);
            }
            if let Some(v) = given(7) {
                e.bytes[1] = (v as u8).min(127);
            }
            if let Some(v) = given(8) {
                e.bytes[2] = (v as u8).min(127);
            }
            if !a.bool(9) {
                sort_midi(&mut src);
            }
            store_midi(s, take, &src, &chunk);
            ret(Val::Bool(true))
        }
        "MIDI_InsertCC" => {
            let Some((mut src, chunk)) = midi_of(&mut s.project, take) else { return Some(ret(Val::Bool(false))) };
            let status = (a.int(4) as u8 & 0xf0).max(0xa0) | (a.int(5) as u8 & 0x0f);
            if status & 0xf0 == 0xf0 {
                return Some(ret(Val::Bool(false)));
            }
            let flags = if a.bool(1) { SELECTED } else { 0 } | if a.bool(2) { MUTED } else { 0 };
            let bytes = [status, a.int(6).clamp(0, 127) as u8, a.int(7).clamp(0, 127) as u8];
            src.events.push(MidiEvent { tick: tick(a.num(3)), bytes, len: event_len(status), flags, shape: None });
            sort_midi(&mut src);
            store_midi(s, take, &src, &chunk);
            ret(Val::Bool(true))
        }
        "MIDI_DeleteCC" => {
            let Some((mut src, chunk)) = midi_of(&mut s.project, take) else { return Some(ret(Val::Bool(false))) };
            let Some(i) = ccs(&src).get(a.int(1).max(0) as usize).copied().filter(|_| a.int(1) >= 0) else {
                return Some(ret(Val::Bool(false)));
            };
            src.events.remove(i);
            store_midi(s, take, &src, &chunk);
            ret(Val::Bool(true))
        }
        "MIDI_Sort" => {
            if let Some((mut src, chunk)) = midi_of(&mut s.project, take) {
                sort_midi(&mut src);
                store_midi(s, take, &src, &chunk);
            }
            ret(Val::Nil)
        }
        "MIDI_DisableSort" => ret(Val::Nil),
        "MIDI_GetPPQPosFromProjTime" | "MIDI_GetPPQPosFromProjQN" => {
            let Some((item_qn, offset_qn, rate, tpq)) = placement(s, take) else { return Some(ret(Val::Num(0.0))) };
            let qn = if name.ends_with("Time") { s.project.tempo.timeline().qn_at(a.num(1)) } else { a.num(1) };
            ret(Val::Num(((qn - item_qn) * rate + offset_qn) * tpq))
        }
        "MIDI_GetProjTimeFromPPQPos" | "MIDI_GetProjQNFromPPQPos" => {
            let Some((item_qn, offset_qn, rate, tpq)) = placement(s, take) else { return Some(ret(Val::Num(0.0))) };
            let qn = item_qn + (a.num(1) / tpq - offset_qn) / rate;
            ret(Val::Num(if name == "MIDI_GetProjTimeFromPPQPos" {
                s.project.tempo.timeline().time_at_qn(qn)
            } else {
                qn
            }))
        }
        _ => return None,
    })
}

/// Makes an item selected or not, for commands run on the selection.
pub(super) fn set_selected(s: &mut State, id: ItemId, on: bool) {
    select_item(s, id, on);
}

/// The selected items' ids, in track order.
pub(super) fn selection(s: &State) -> Vec<ItemId> {
    selected_items(s)
}

/// Selects every track (or none).
pub(super) fn select_all_tracks(s: &mut State, on: bool) {
    let ids: Vec<TrackId> = s.project.tracks.iter().map(|t| t.id).collect();
    for t in ids {
        select_track(s, t, on);
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{api, project, state};
    use super::*;

    #[test]
    fn midi_notes_and_ccs_as_reaper_numbers_them() {
        let p = project();
        let tr = Val::Track(p.tracks[0].id);
        let mut s = state(p);
        let item = api(&mut s, "CreateNewMIDIItemInProj", &[tr, Val::Num(1.0), Val::Num(3.0), Val::Nil]).0;
        let take = api(&mut s, "GetActiveTake", std::slice::from_ref(&item)).0;
        assert_eq!(api(&mut s, "TakeIsMIDI", std::slice::from_ref(&take)).0, Val::Bool(true));
        let note = |sel, mute, a: f64, b: f64, ch, key, vel, nosort| {
            vec![
                take.clone(),
                Val::Bool(sel),
                Val::Bool(mute),
                Val::Num(a),
                Val::Num(b),
                Val::Int(ch),
                Val::Int(key),
                Val::Int(vel),
                Val::Bool(nosort),
            ]
        };
        api(&mut s, "MIDI_InsertNote", &note(true, false, 0.0, 480.0, 0, 60, 100, true));
        api(&mut s, "MIDI_InsertNote", &note(false, true, 960.0, 1920.0, 1, 64, 90, true));
        api(&mut s, "MIDI_InsertNote", &note(false, false, 480.0, 960.0, 0, 62, 80, false));
        let cc = |sel, ppq: f64, msg, ch, a, b| {
            vec![
                take.clone(),
                Val::Bool(sel),
                Val::Bool(false),
                Val::Num(ppq),
                Val::Int(msg),
                Val::Int(ch),
                Val::Int(a),
                Val::Int(b),
            ]
        };
        api(&mut s, "MIDI_InsertCC", &cc(false, 240.0, 0xb0, 0, 7, 100));
        api(&mut s, "MIDI_InsertCC", &cc(true, 100.0, 0xe0, 2, 0, 64));
        let (n, counts) = api(&mut s, "MIDI_CountEvts", std::slice::from_ref(&take));
        assert_eq!((n, counts), (Val::Int(5), vec![Val::Int(3), Val::Int(2), Val::Int(0)]));
        let get = |s: &mut State, i| api(s, "MIDI_GetNote", &[take.clone(), Val::Int(i)]);
        let (ok, outs) = get(&mut s, 2);
        assert_eq!(ok, Val::Bool(true));
        assert_eq!(
            outs,
            [
                Val::Bool(false),
                Val::Bool(true),
                Val::Num(960.0),
                Val::Num(1920.0),
                Val::Int(1),
                Val::Int(64),
                Val::Int(90)
            ]
        );
        assert_eq!(get(&mut s, 1).1[5], Val::Int(62));
        let (_, outs) = api(&mut s, "MIDI_GetCC", &[take.clone(), Val::Int(0)]);
        assert_eq!(
            outs,
            [Val::Bool(true), Val::Bool(false), Val::Num(100.0), Val::Int(224), Val::Int(2), Val::Int(0), Val::Int(64)]
        );
        let set = vec![
            take.clone(),
            Val::Int(0),
            Val::Nil,
            Val::Nil,
            Val::Nil,
            Val::Nil,
            Val::Nil,
            Val::Int(72),
            Val::Nil,
            Val::Bool(false),
        ];
        api(&mut s, "MIDI_SetNote", &set);
        assert_eq!(get(&mut s, 0).1[5], Val::Int(72));
        api(&mut s, "MIDI_DeleteNote", &[take.clone(), Val::Int(1)]);
        api(&mut s, "MIDI_DeleteCC", &[take.clone(), Val::Int(0)]);
        assert_eq!(api(&mut s, "MIDI_CountEvts", std::slice::from_ref(&take)).0, Val::Int(3));
        // The source is written as REAPER writes it: `e` for selected, `Em` for muted events.
        let Source::Raw(c) = &s.project.tracks[0].items[1].takes[0].source else { panic!() };
        let text = scythe_core::rpp::write(c);
        assert!(text.contains("e 0 90 48 64") && text.contains("Em 480 91 40 5a"), "{text}");
        assert_eq!(api(&mut s, "MIDI_GetPPQPosFromProjTime", &[take.clone(), Val::Num(1.5)]).0, Val::Num(960.0));
        assert_eq!(api(&mut s, "MIDI_GetProjTimeFromPPQPos", &[take, Val::Num(960.0)]).0, Val::Num(1.5));
    }

    #[test]
    fn items_takes_and_sources() {
        let p = project();
        let tr = Val::Track(p.tracks[1].id);
        let mut s = state(p);
        let item = api(&mut s, "AddMediaItemToTrack", &[tr]).0;
        assert_eq!(api(&mut s, "GetActiveTake", std::slice::from_ref(&item)).0, Val::Nil);
        let take = api(&mut s, "AddTakeToMediaItem", std::slice::from_ref(&item)).0;
        assert_eq!(api(&mut s, "GetActiveTake", std::slice::from_ref(&item)).0, take);
        let src = api(&mut s, "PCM_Source_CreateFromFile", &[Val::Str("/x/kick.wav".into())]).0;
        assert_eq!(api(&mut s, "SetMediaItemTake_Source", &[take.clone(), src.clone()]).0, Val::Bool(true));
        assert_eq!(api(&mut s, "GetMediaItemTake_Source", std::slice::from_ref(&take)).0, src);
        assert_eq!(api(&mut s, "GetMediaSourceType", std::slice::from_ref(&src)).1, [Val::Str("WAVE".into())]);
        assert_eq!(api(&mut s, "GetMediaSourceFileName", &[src]).1, [Val::Str("/x/kick.wav".into())]);
        let name = [take.clone(), Val::Str("P_NAME".into()), Val::Str("Kick".into()), Val::Bool(true)];
        api(&mut s, "GetSetMediaItemTakeInfo_String", &name);
        assert_eq!(api(&mut s, "GetTakeName", &[take]).0, Val::Str("Kick".into()));
        api(&mut s, "SetMediaItemSelected", &[item.clone(), Val::Bool(true)]);
        assert_eq!(api(&mut s, "CountSelectedMediaItems", &[Val::Proj]).0, Val::Int(1));
        assert_eq!(api(&mut s, "GetMediaItemInfo_Value", &[item, Val::Str("B_UISEL".into())]).0, Val::Num(1.0));
        assert!(s.sel_changed);
    }
}
