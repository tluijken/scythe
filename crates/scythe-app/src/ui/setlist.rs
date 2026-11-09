//! The setlist page: the songs found in the project's markers, the setlists made from them,
//! and a stage view big enough to read from behind a drum kit.

use egui::{Align2, Color32, CornerRadius, Id, Key, Rect, Sense, Stroke, Ui, pos2, vec2};
use scythe_core::setlist::{Setlist, Song, SongEnd};

use super::GAP;
use super::widgets::{self, Icon, icon_button, icon_button_with_id, paint_icon, switch, text_pill};
use crate::app::ScytheApp;
use crate::palette::{Action, shortcut_text};
use crate::theme::{self, ACCENT, BG, GREEN, PANEL, RAISED, RAISED_HI, TEXT, TEXT_DIM, TEXT_MID};

const STAGE_H: f32 = 236.0;
const ROW_H: f32 = 44.0;
const HEAD_H: f32 = 40.0;

/// A song row picked up for dropping into the setlist, or a setlist row being moved.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Drag {
    Song(usize),
    Entry(usize),
}

pub fn show(app: &mut ScytheApp, ui: &mut Ui, rect: Rect) {
    let songs = app.songs();
    let sets = app.setlists();
    let list = sets.lists.get(app.live.list).cloned();

    let stage = Rect::from_min_size(rect.min, vec2(rect.width(), STAGE_H));
    stage_view(app, ui, stage, &songs, list.as_ref());

    let body = Rect::from_min_max(pos2(rect.left(), stage.bottom() + GAP), rect.max);
    let songs_w = (body.width() * 0.4).clamp(260.0, 440.0);
    let songs_rect = Rect::from_min_size(body.min, vec2(songs_w, body.height()));
    let list_rect = Rect::from_min_max(pos2(songs_rect.right() + GAP, body.top()), body.max);
    let drag_id = Id::unique("setlist-drag");
    let drag: Option<Drag> = ui.data(|d| d.get_temp(drag_id));
    song_list(app, ui, songs_rect, &songs, drag_id);
    setlist_panel(app, ui, list_rect, &songs, list.as_ref(), sets.auto_advance, drag, drag_id);
    if ui.input(|i| i.pointer.any_released()) {
        ui.data_mut(|d| d.remove::<Drag>(drag_id));
    }
}

fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn plural(n: usize, noun: &str) -> String {
    if n == 1 { format!("1 {noun}") } else { format!("{n} {noun}s") }
}

fn card(ui: &Ui, rect: Rect) {
    ui.painter().rect_filled(rect, CornerRadius::same(18), PANEL);
}

fn stage_view(app: &mut ScytheApp, ui: &mut Ui, rect: Rect, songs: &[Song], list: Option<&Setlist>) {
    card(ui, rect);
    let painter = ui.painter().clone();
    let inner = rect.shrink2(vec2(24.0, 20.0));
    let playing = app.is_playing();
    let pos = app.position();
    let resolved = list.map(|l| l.resolve(songs)).unwrap_or_default();
    let status = list.map(|l| app.live.player.status(l, songs, pos, playing));
    let entry_song = |e: Option<usize>| e.and_then(|e| resolved.get(e).copied().flatten()).map(|s| &songs[s]);
    let now = if playing {
        songs.iter().find(|s| s.contains(pos))
    } else {
        entry_song(status.as_ref().and_then(|s| s.current))
    };
    let next = entry_song(status.as_ref().and_then(|s| s.next));

    let buttons_w = 64.0 * 2.0 + 88.0 + 16.0 * 2.0;
    let main_w = (inner.width() - buttons_w - 40.0).max(120.0);

    let (label, color) = if playing { ("NOW PLAYING", GREEN) } else { ("READY", ACCENT) };
    painter.text(inner.left_top(), Align2::LEFT_TOP, label, theme::semibold(12.0), color);
    let title = now.map_or_else(|| "No song".to_owned(), |s| s.name.clone());
    let title = widgets::truncate(ui, &title, &theme::semibold(40.0), main_w);
    painter.text(inner.left_top() + vec2(0.0, 18.0), Align2::LEFT_TOP, title, theme::semibold(40.0), TEXT);
    if let Some(entry) = status.as_ref().and_then(|s| s.current)
        && let Some(l) = list
    {
        let of = format!("{} of {} · {} · {}", entry + 1, l.songs.len(), l.name, clock(l.duration(songs)));
        let of = widgets::truncate(ui, &of, &theme::regular(14.0), main_w);
        painter.text(inner.left_top() + vec2(0.0, 68.0), Align2::LEFT_TOP, of, theme::regular(14.0), TEXT_DIM);
    }

    if let Some(song) = now {
        let bar = Rect::from_min_size(inner.left_top() + vec2(0.0, 100.0), vec2(main_w, 10.0));
        painter.rect_filled(bar, CornerRadius::same(5), RAISED_HI);
        let into = if playing { (pos - song.start).clamp(0.0, song.duration()) } else { 0.0 };
        let frac = if song.duration() > 0.0 { (into / song.duration()) as f32 } else { 0.0 };
        let filled = Rect::from_min_size(bar.min, vec2(bar.width() * frac, bar.height()));
        painter.rect_filled(filled, CornerRadius::same(5), if playing { GREEN } else { ACCENT });
        for (_, at) in &song.sections {
            let x = bar.left() + bar.width() * ((at - song.start) / song.duration().max(1e-9)) as f32;
            painter.line_segment([pos2(x, bar.top() - 4.0), pos2(x, bar.bottom() + 4.0)], Stroke::new(2.0, TEXT_DIM));
        }
        let times = pos2(bar.left(), bar.bottom() + 8.0);
        painter.text(times, Align2::LEFT_TOP, clock(into), theme::mono(22.0), TEXT);
        let remaining = format!("-{}", clock(song.duration() - into));
        painter.text(pos2(bar.right(), times.y), Align2::RIGHT_TOP, remaining, theme::mono(22.0), TEXT_MID);
        if song.end_kind != SongEnd::StopMarker {
            let note = "no stop marker";
            painter.text(pos2(bar.center().x, times.y + 4.0), Align2::CENTER_TOP, note, theme::regular(12.0), TEXT_DIM);
        }
    }

    let auto = app.setlists().auto_advance;
    let next_label = if auto { "NEXT UP · cued when this song stops" } else { "NEXT UP" };
    painter.text(inner.left_bottom() - vec2(0.0, 46.0), Align2::LEFT_TOP, next_label, theme::semibold(12.0), TEXT_DIM);
    let next_name = match next {
        Some(s) => format!("{}  ·  {}", s.name, clock(s.duration())),
        None => "End of setlist".to_owned(),
    };
    let next_name = widgets::truncate(ui, &next_name, &theme::semibold(22.0), main_w);
    painter.text(inner.left_bottom(), Align2::LEFT_BOTTOM, next_name, theme::semibold(22.0), TEXT_MID);

    let cy = inner.center().y;
    let mut x = inner.right() - buttons_w;
    let ctx = ui.ctx().clone();
    let kbd = |a| shortcut_text(&ctx, a);
    let prev_tip = format!("Previous song ({})", kbd(Action::SetlistPrevious));
    if icon_button(
        ui,
        Rect::from_center_size(pos2(x + 32.0, cy), vec2(64.0, 64.0)),
        Icon::SkipBack,
        RAISED,
        TEXT,
        24.0,
        &prev_tip,
    )
    .clicked()
    {
        app.setlist_step(false);
    }
    x += 64.0 + 16.0;
    let (icon, tip) = if playing {
        (Icon::Stop, format!("Stop ({})", kbd(Action::Stop)))
    } else {
        (Icon::Play, format!("Play song ({})", kbd(Action::SetlistPlay)))
    };
    let bg = if playing { theme::PINK } else { ACCENT };
    if icon_button(ui, Rect::from_center_size(pos2(x + 44.0, cy), vec2(88.0, 88.0)), icon, bg, BG, 34.0, &tip).clicked()
    {
        if playing {
            app.stop();
        } else {
            app.setlist_play();
        }
    }
    x += 88.0 + 16.0;
    let next_tip = format!("Next song ({})", kbd(Action::SetlistNext));
    if icon_button(
        ui,
        Rect::from_center_size(pos2(x + 32.0, cy), vec2(64.0, 64.0)),
        Icon::SkipForward,
        RAISED,
        TEXT,
        24.0,
        &next_tip,
    )
    .clicked()
    {
        app.setlist_step(true);
    }
}

/// Vertical scroll offset for a list, stored per id and driven by the wheel over `area`.
fn scroll(ui: &Ui, id: Id, area: Rect, content_h: f32) -> f32 {
    let max = (content_h - area.height()).max(0.0);
    let mut offset: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(0.0);
    if ui.ctx().input(|i| i.pointer.hover_pos()).is_some_and(|p| area.contains(p)) {
        offset -= ui.ctx().input(|i| i.smooth_scroll_delta.y);
    }
    offset = offset.clamp(0.0, max);
    ui.data_mut(|d| d.insert_temp(id, offset));
    offset
}

fn song_list(app: &mut ScytheApp, ui: &mut Ui, rect: Rect, songs: &[Song], drag_id: Id) {
    card(ui, rect);
    let painter = ui.painter().clone();
    let inner = rect.shrink2(vec2(14.0, 14.0));
    painter.text(inner.left_top() + vec2(4.0, 12.0), Align2::LEFT_CENTER, "Songs", theme::semibold(15.0), TEXT);
    let found = format!("{} from markers", songs.len());
    painter.text(inner.right_top() + vec2(-4.0, 12.0), Align2::RIGHT_CENTER, found, theme::mono(11.0), TEXT_DIM);
    let area = Rect::from_min_max(pos2(inner.left(), inner.top() + HEAD_H), inner.max);
    if songs.is_empty() {
        let msg = "Add a marker at the start of each song\nand a !1016 marker where it should stop.";
        painter.text(area.center_top() + vec2(0.0, 40.0), Align2::CENTER_TOP, msg, theme::regular(13.0), TEXT_DIM);
        return;
    }
    let offset = scroll(ui, Id::unique("setlist-songs-scroll"), area, songs.len() as f32 * ROW_H);
    let clip = painter.with_clip_rect(area);
    let playing_at = app.is_playing().then(|| app.position());
    for (i, song) in songs.iter().enumerate() {
        let row = Rect::from_min_size(
            pos2(area.left(), area.top() + i as f32 * ROW_H - offset),
            vec2(area.width(), ROW_H - 4.0),
        );
        if row.bottom() < area.top() || row.top() > area.bottom() {
            continue;
        }
        let resp = ui.interact(row.intersect(area), Id::unique("setlist-song").with(i), Sense::click_and_drag());
        let live = playing_at.is_some_and(|t| song.contains(t));
        // By position rather than `hovered`, which the row loses while its own buttons are pressed.
        let hovered = ui.rect_contains_pointer(row.intersect(area));
        let bg = if hovered { RAISED_HI } else { RAISED };
        clip.rect_filled(row, CornerRadius::same(10), bg);
        let badge = Rect::from_center_size(row.left_center() + vec2(22.0, 0.0), vec2(30.0, 22.0));
        clip.rect_filled(badge, CornerRadius::same(6), if live { GREEN } else { RAISED_HI });
        clip.text(
            badge.center(),
            Align2::CENTER_CENTER,
            song.number.to_string(),
            theme::mono(11.0),
            if live { BG } else { TEXT_MID },
        );
        let dur = clock(song.duration());
        let dur_w = widgets::text_width(ui, &dur, theme::mono(12.0));
        let buttons_w = if hovered { 64.0 } else { 0.0 };
        let name_w = row.width() - 48.0 - dur_w - 20.0 - buttons_w;
        let name = widgets::truncate(ui, &song.name, &theme::regular(14.0), name_w);
        clip.text(row.left_center() + vec2(46.0, -7.0), Align2::LEFT_CENTER, name, theme::regular(14.0), TEXT);
        let mut sub = clock(song.start);
        if !song.sections.is_empty() {
            sub.push_str(&format!(" · {}", plural(song.sections.len(), "cue")));
        }
        clip.text(row.left_center() + vec2(46.0, 9.0), Align2::LEFT_CENTER, sub, theme::mono(10.0), TEXT_DIM);
        clip.text(
            row.right_center() - vec2(12.0 + buttons_w, 0.0),
            Align2::RIGHT_CENTER,
            dur,
            theme::mono(12.0),
            TEXT_MID,
        );
        if hovered {
            let add = Rect::from_center_size(row.right_center() - vec2(22.0, 0.0), vec2(28.0, 28.0));
            let play = Rect::from_center_size(row.right_center() - vec2(54.0, 0.0), vec2(28.0, 28.0));
            let id = Id::unique("setlist-song-buttons").with(i);
            if icon_button_with_id(ui, id.with("play"), play, Icon::Play, RAISED_HI, TEXT, 12.0, "Play this song")
                .clicked()
            {
                app.play_song(song);
            }
            if icon_button_with_id(ui, id.with("add"), add, Icon::Plus, ACCENT, BG, 14.0, "Add to the setlist")
                .clicked()
            {
                app.add_to_setlist(song);
            }
        }
        if resp.double_clicked() {
            app.play_song(song);
        }
        if resp.drag_started() {
            ui.data_mut(|d| d.insert_temp(drag_id, Drag::Song(i)));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn setlist_panel(
    app: &mut ScytheApp,
    ui: &mut Ui,
    rect: Rect,
    songs: &[Song],
    list: Option<&Setlist>,
    auto_advance: bool,
    drag: Option<Drag>,
    drag_id: Id,
) {
    card(ui, rect);
    let painter = ui.painter().clone();
    let inner = rect.shrink2(vec2(14.0, 14.0));
    let names: Vec<String> = app.setlists().lists.into_iter().map(|l| l.name).collect();

    // Setlist tabs, then the actions on the current one.
    let mut x = inner.left();
    let top = inner.top();
    let rename_id = Id::unique("setlist-rename");
    // Focus is asked for once, when renaming starts, so a click elsewhere can end it.
    let focus_id = rename_id.with("focus");
    let mut renaming: Option<String> = ui.data(|d| d.get_temp(rename_id));
    for (i, name) in names.iter().enumerate() {
        let font = theme::semibold(13.0);
        let active = i == app.live.list;
        let shown = renaming.as_deref().filter(|_| active).unwrap_or(name);
        let w = widgets::text_width(ui, shown, font.clone()).min(180.0) + 28.0;
        if x + w > inner.right() - 40.0 && i > 0 {
            break;
        }
        let r = Rect::from_min_size(pos2(x, top), vec2(w, 28.0));
        if active && let Some(text) = renaming.as_mut() {
            let edit = ui.put(r, egui::TextEdit::singleline(text).font(font).margin(vec2(8.0, 5.0)));
            if ui.data_mut(|d| d.remove_temp::<bool>(focus_id)).is_some() {
                edit.request_focus();
            }
            if ui.input(|i| i.key_pressed(Key::Escape)) {
                renaming = None;
            } else if edit.lost_focus() || ui.input(|i| i.key_pressed(Key::Enter)) {
                let new = text.trim().to_owned();
                renaming = None;
                let at = app.live.list;
                app.rename_setlist(at, &new);
            }
        } else {
            let label = widgets::truncate(ui, name, &font, 180.0);
            let (bg, fg) = if active { (ACCENT, BG) } else { (RAISED, TEXT_MID) };
            let resp = text_pill(ui, r, &label, font, bg, fg).on_hover_text("Double-click to rename");
            if resp.clicked() && !active {
                app.choose_setlist(i);
            }
            if resp.double_clicked() {
                app.choose_setlist(i);
                renaming = Some(name.clone());
                ui.data_mut(|d| d.insert_temp(focus_id, true));
            }
        }
        x += w + 6.0;
    }
    if icon_button(
        ui,
        Rect::from_min_size(pos2(x, top), vec2(28.0, 28.0)),
        Icon::Plus,
        RAISED,
        TEXT,
        14.0,
        "New setlist",
    )
    .clicked()
    {
        app.new_setlist(None, Vec::new());
    }
    match &renaming {
        Some(r) => {
            ui.data_mut(|d| d.insert_temp(rename_id, r.clone()));
        }
        None => ui.data_mut(|d| d.remove::<String>(rename_id)),
    }

    let tools_top = top + 36.0;
    let mut bx = inner.left();
    let mut tool = |ui: &mut Ui, label: &str, tip: &str| {
        let w = widgets::text_width(ui, label, theme::regular(12.0)) + 20.0;
        let r = Rect::from_min_size(pos2(bx, tools_top), vec2(w, 24.0));
        bx += w + 6.0;
        text_pill(ui, r, label, theme::regular(12.0), RAISED, TEXT_MID).on_hover_text(tip).clicked()
    };
    if list.is_some() {
        if tool(ui, "Rename", "Rename this setlist") {
            ui.data_mut(|d| d.insert_temp(rename_id, list.map(|l| l.name.clone()).unwrap_or_default()));
            ui.data_mut(|d| d.insert_temp(focus_id, true));
        }
        if tool(ui, "Duplicate", "Copy this setlist") {
            let at = app.live.list;
            app.duplicate_setlist(at, None);
        }
        if tool(ui, "Delete", "Delete this setlist") {
            let at = app.live.list;
            app.delete_setlist(at);
        }
        if tool(ui, "Export", "Save this setlist as text and copy it to the clipboard") {
            app.export_setlist();
        }
    }
    if tool(ui, "Import", "Add a setlist from a text file: a title line, then one song name per line") {
        app.import_setlist();
    }

    // Options along the bottom.
    let foot = Rect::from_min_max(pos2(inner.left(), inner.bottom() - 28.0), inner.max);
    let sw = Rect::from_min_size(foot.left_center() - vec2(0.0, 11.0), vec2(40.0, 22.0));
    if switch(ui, sw, auto_advance, ACCENT)
        .on_hover_text("Cue the next song when one stops at its stop marker")
        .clicked()
    {
        app.set_auto_advance(!auto_advance);
    }
    painter.text(
        sw.right_center() + vec2(8.0, 0.0),
        Align2::LEFT_CENTER,
        "Auto-advance",
        theme::regular(13.0),
        TEXT_MID,
    );
    let ma = app.engine.marker_actions();
    let sw2 = Rect::from_min_size(sw.right_top() + vec2(120.0, 0.0), vec2(40.0, 22.0));
    if switch(ui, sw2, ma, ACCENT).on_hover_text("Run the actions of !markers (!1016 stops) during playback").clicked()
    {
        Action::ToggleMarkerActions.run(app);
    }
    painter.text(
        sw2.right_center() + vec2(8.0, 0.0),
        Align2::LEFT_CENTER,
        "Marker actions",
        theme::regular(13.0),
        TEXT_MID,
    );
    if let Some(l) = list {
        let total = format!("{} · {}", plural(l.songs.len(), "song"), clock(l.duration(songs)));
        let room =
            foot.right() - (sw2.right() + 8.0 + widgets::text_width(ui, "Marker actions", theme::regular(13.0)) + 16.0);
        if widgets::text_width(ui, &total, theme::mono(12.0)) <= room {
            painter.text(foot.right_center(), Align2::RIGHT_CENTER, total, theme::mono(12.0), TEXT_DIM);
        }
    }

    let area = Rect::from_min_max(pos2(inner.left(), tools_top + HEAD_H), pos2(inner.right(), foot.top() - 8.0));
    let Some(list) = list else {
        let msg = "No setlist yet. Press + to make one,\nthen add songs from the list on the left.";
        painter.text(area.center_top() + vec2(0.0, 40.0), Align2::CENTER_TOP, msg, theme::regular(13.0), TEXT_DIM);
        return;
    };
    let resolved = list.resolve(songs);
    let offset = scroll(
        ui,
        Id::unique("setlist-entries-scroll").with(app.live.list),
        area,
        (list.songs.len() + 1) as f32 * ROW_H,
    );
    let clip = painter.with_clip_rect(area);
    let pointer = ui.ctx().input(|i| i.pointer.interact_pos());
    let hovering_area = pointer.is_some_and(|p| area.contains(p));
    // Where a dragged song or entry would land.
    let drop_at = pointer
        .filter(|_| drag.is_some() && hovering_area)
        .map(|p| (((p.y - area.top() + offset) / ROW_H).round().max(0.0) as usize).min(list.songs.len()));
    let current = app.live.player.entry;
    let playing = app.is_playing();
    let mut remove = None;
    for (i, entry) in list.songs.iter().enumerate() {
        let row = Rect::from_min_size(
            pos2(area.left(), area.top() + i as f32 * ROW_H - offset),
            vec2(area.width(), ROW_H - 4.0),
        );
        if row.bottom() < area.top() || row.top() > area.bottom() {
            continue;
        }
        let resp = ui.interact(
            row.intersect(area),
            Id::unique("setlist-entry").with((app.live.list, i)),
            Sense::click_and_drag(),
        );
        let song = resolved[i].map(|s| &songs[s]);
        let is_current = i == current;
        let bg = if drag == Some(Drag::Entry(i)) {
            BG
        } else if resp.hovered() {
            RAISED_HI
        } else {
            RAISED
        };
        clip.rect_filled(row, CornerRadius::same(10), bg);
        if is_current {
            let mark = Rect::from_min_size(row.min + vec2(0.0, 8.0), vec2(4.0, row.height() - 16.0));
            clip.rect_filled(mark, CornerRadius::same(2), if playing { GREEN } else { ACCENT });
        }
        clip.text(
            row.left_center() + vec2(26.0, 0.0),
            Align2::RIGHT_CENTER,
            format!("{}", i + 1),
            theme::mono(14.0),
            TEXT_DIM,
        );
        let (name, color) = match song {
            Some(_) => (entry.name.clone(), TEXT),
            None => (format!("{} (no marker)", entry.name), TEXT_DIM),
        };
        let dur = song.map(|s| clock(s.duration())).unwrap_or_default();
        let name = widgets::truncate(ui, &name, &theme::semibold(15.0), row.width() - 140.0);
        clip.text(row.left_center() + vec2(40.0, 0.0), Align2::LEFT_CENTER, name, theme::semibold(15.0), color);
        clip.text(row.right_center() - vec2(48.0, 0.0), Align2::RIGHT_CENTER, dur, theme::mono(12.0), TEXT_MID);
        let close = Rect::from_center_size(row.right_center() - vec2(20.0, 0.0), vec2(26.0, 26.0));
        if ui.rect_contains_pointer(row.intersect(area)) {
            let id = Id::unique("setlist-entry-remove").with((app.live.list, i));
            if icon_button_with_id(
                ui,
                id,
                close,
                Icon::Close,
                Color32::TRANSPARENT,
                TEXT_DIM,
                12.0,
                "Remove from the setlist",
            )
            .clicked()
            {
                remove = Some(i);
            }
        } else {
            paint_icon(
                ui,
                Rect::from_center_size(close.center(), vec2(14.0, 14.0)),
                Icon::Setlist,
                theme::with_alpha(TEXT_DIM, 90),
                14.0,
            );
        }
        if resp.double_clicked() {
            app.setlist_play_entry(i);
        } else if resp.clicked() {
            app.setlist_select(i);
        }
        if resp.drag_started() {
            ui.data_mut(|d| d.insert_temp(drag_id, Drag::Entry(i)));
        }
    }
    if let Some(at) = drop_at {
        let y = area.top() + at as f32 * ROW_H - offset - 2.0;
        clip.line_segment([pos2(area.left() + 8.0, y), pos2(area.right() - 8.0, y)], Stroke::new(2.0, ACCENT));
    }
    if list.songs.is_empty() && drop_at.is_none() {
        let msg = "Drag songs here, or press + on a song";
        clip.text(area.center_top() + vec2(0.0, 30.0), Align2::CENTER_TOP, msg, theme::regular(13.0), TEXT_DIM);
    }

    let released = ui.input(|i| i.pointer.any_released());
    let at_list = app.live.list;
    if let (true, Some(d), Some(to)) = (released, drag, drop_at) {
        match d {
            Drag::Song(s) => {
                let song = songs[s].clone();
                app.edit_setlists("Add song to setlist", |sets| {
                    let to = to.min(sets.lists[at_list].songs.len());
                    sets.lists[at_list].songs.insert(to, scythe_core::setlist::SongRef::to(&song));
                });
            }
            Drag::Entry(from) if to != from && to != from + 1 => {
                let to = if to > from { to - 1 } else { to };
                app.edit_setlists("Reorder setlist", |sets| {
                    let songs = &mut sets.lists[at_list].songs;
                    let moved = songs.remove(from);
                    songs.insert(to, moved);
                });
                if app.live.player.entry == from {
                    app.live.player.entry = to;
                }
            }
            Drag::Entry(_) => {}
        }
    }
    if let Some(i) = remove {
        app.edit_setlists("Remove song from setlist", |sets| {
            sets.lists[at_list].songs.remove(i);
        });
    }
}
