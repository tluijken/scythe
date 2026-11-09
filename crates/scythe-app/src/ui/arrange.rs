use egui::{Align2, Color32, CornerRadius, Id, Mesh, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2};
use scythe_core::groups::GroupedControl;
use scythe_core::units::format_db;
use scythe_core::{EnvelopeKind, ItemId, TimeRange, TrackId};
use scythe_engine::MediaState;

use crate::app::{ScytheApp, Selection};
use crate::theme::{
    self, ACCENT, BG, LANE, LANE_SELECTED, PANEL, RAISED, RAISED_HI, TEXT, TEXT_DIM, TEXT_MID, track_color, with_alpha,
};
use crate::ui::widgets::{self, Icon, text_pill};

pub(super) const HEADER_W: f32 = 236.0;
const RULER_H: f32 = 52.0;
/// Track header layout needs this much height for its button row.
const FULL_ROW_H: f32 = 56.0;
const ROW_GAP: f32 = 4.0;

/// Maps between project time and screen x for the lanes.
#[derive(Clone, Copy)]
struct TimeAxis {
    left: f32,
    start: f64,
    pps: f64,
}

impl TimeAxis {
    fn x(&self, t: f64) -> f32 {
        self.left + ((t - self.start) * self.pps) as f32
    }

    fn t(&self, x: f32) -> f64 {
        self.start + (x - self.left) as f64 / self.pps
    }
}

pub fn show(app: &mut ScytheApp, ui: &mut Ui, rect: Rect) {
    ui.painter().rect_filled(rect, CornerRadius::same(18), PANEL);
    let inner = rect.shrink(8.0);
    let lanes_left = inner.left() + HEADER_W + 8.0;
    let lanes_w = (inner.right() - lanes_left).max(50.0);

    // Window managers often resize right after the first frame, so a fit stays pending until
    // the lane width has been stable for a frame.
    if let Some(range) = app.view.fit {
        app.view.px_per_sec = lanes_w as f64 / range.length().max(0.1);
        app.view.start = range.start;
        let last: Option<f32> = ui.data(|d| d.get_temp(Id::unique("fit-width")));
        if last == Some(lanes_w) {
            app.view.fit = None;
        }
        ui.data_mut(|d| d.insert_temp(Id::unique("fit-width"), lanes_w));
        ui.ctx().request_repaint();
    }
    let ruler = Rect::from_min_max(inner.min, pos2(inner.right(), inner.top() + RULER_H));
    let rows = Rect::from_min_max(pos2(inner.left(), ruler.bottom() + 6.0), inner.max);

    handle_scroll(app, ui, inner, lanes_left, rows);
    follow_playhead(app, lanes_w);
    let axis = TimeAxis { left: lanes_left, start: app.view.start, pps: app.view.px_per_sec };

    ruler_header(app, ui, Rect::from_min_max(ruler.min, pos2(lanes_left - 8.0, ruler.bottom())));
    ruler_timeline(app, ui, Rect::from_min_max(pos2(lanes_left, ruler.top()), ruler.max), axis);

    let clip = ui.painter().with_clip_rect(rows);
    track_rows(app, ui, rows, axis, &clip);

    // Loop shading and playhead span the ruler and the rows.
    let span = Rect::from_min_max(pos2(lanes_left, ruler.top() + 26.0), rows.max);
    let lane_clip = ui.painter().with_clip_rect(Rect::from_min_max(pos2(lanes_left, ruler.top()), inner.max));
    if app.project.loop_enabled
        && let Some(sel) = app.project.time_selection
    {
        let r = Rect::from_min_max(pos2(axis.x(sel.start), rows.top()), pos2(axis.x(sel.end), rows.bottom()));
        lane_clip.rect_filled(r, CornerRadius::same(8), with_alpha(ACCENT, 18));
    }
    let px = axis.x(app.position());
    if px >= lanes_left - 1.0 && px <= inner.right() {
        lane_clip.rect_filled(
            Rect::from_min_max(pos2(px - 1.0, span.top()), pos2(px + 1.0, span.bottom())),
            CornerRadius::same(2),
            ACCENT,
        );
        lane_clip.circle_filled(pos2(px, span.top() + 6.0), 6.0, ACCENT);
    }
    if app.is_playing() {
        ui.ctx().request_repaint();
    }
}

fn handle_scroll(app: &mut ScytheApp, ui: &Ui, area: Rect, lanes_left: f32, rows: Rect) {
    let hover = ui.ctx().input(|i| i.pointer.hover_pos());
    let Some(pos) = hover.filter(|p| area.contains(*p)) else { return };
    let (scroll, zoom, mods) = ui.ctx().input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.modifiers));
    if zoom != 1.0 && pos.x >= lanes_left {
        let axis = TimeAxis { left: lanes_left, start: app.view.start, pps: app.view.px_per_sec };
        let anchor = axis.t(pos.x);
        app.view.px_per_sec = (app.view.px_per_sec * zoom as f64).clamp(2.0, 2000.0);
        app.view.start = (anchor - (pos.x - lanes_left) as f64 / app.view.px_per_sec).max(0.0);
        return;
    }
    if scroll == egui::Vec2::ZERO {
        return;
    }
    let content_h = app.project.tracks.len() as f32 * (app.view.row_h + ROW_GAP);
    let max_scroll = (content_h - rows.height()).max(0.0);
    if mods.shift || scroll.x.abs() > scroll.y.abs() || max_scroll == 0.0 {
        let dx = if scroll.x != 0.0 { scroll.x } else { scroll.y };
        app.view.start = (app.view.start - dx as f64 / app.view.px_per_sec).max(0.0);
    } else {
        app.view.track_scroll = (app.view.track_scroll - scroll.y).clamp(0.0, max_scroll);
    }
}

/// Pages the view when the playhead runs off the right edge, like REAPER's auto-scroll.
fn follow_playhead(app: &mut ScytheApp, lanes_w: f32) {
    if !app.is_playing() {
        return;
    }
    let visible = lanes_w as f64 / app.view.px_per_sec;
    let pos = app.position();
    if pos > app.view.start + visible * 0.95 || pos < app.view.start {
        app.view.start = (pos - visible * 0.05).max(0.0);
    }
}

fn ruler_header(app: &mut ScytheApp, ui: &mut Ui, rect: Rect) {
    let label = "Add track";
    let w = widgets::text_width(ui, label, theme::semibold(12.0)) + 40.0;
    let top = Rect::from_min_max(rect.min, pos2(rect.right(), rect.center().y));
    crate::ui::editing::switches(app, ui, top);
    let rect = Rect::from_min_max(pos2(rect.left(), rect.center().y), rect.max);
    ui.painter().text(rect.left_center() + vec2(10.0, 0.0), Align2::LEFT_CENTER, "Tracks", theme::semibold(15.0), TEXT);
    let btn = Rect::from_min_size(pos2(rect.right() - 8.0 - w, rect.center().y - 11.0), vec2(w, 22.0));
    let resp = ui.allocate_rect(btn, Sense::click()).on_hover_text("Insert new track (Ctrl+T)");
    let bg = if resp.hovered() { RAISED_HI } else { RAISED };
    ui.painter().rect_filled(btn, CornerRadius::same(255), bg);
    widgets::paint_icon(
        ui,
        Rect::from_center_size(btn.left_center() + vec2(18.0, 0.0), vec2(14.0, 14.0)),
        Icon::Plus,
        TEXT,
        14.0,
    );
    ui.painter().text(btn.left_center() + vec2(30.0, 0.0), Align2::LEFT_CENTER, label, theme::semibold(12.0), TEXT);
    if resp.clicked() {
        app.add_track();
    }
}

/// Bars per grid line so that lines stay at least ~28 px apart.
fn grid_step(bar_px: f32) -> i64 {
    let mut step = 1;
    while bar_px * (step as f32) < 28.0 {
        step *= 2;
    }
    step
}

/// Snap-grid lines between bar lines, when they are at least 10 px apart.
fn sub_lines(app: &ScytheApp, axis: TimeAxis, right: f32) -> Vec<f64> {
    let p = &app.project;
    let grid = &p.editing.grid;
    let px = grid.step_seconds(&p.tempo, axis.start.max(0.0)) * axis.pps;
    if px < 10.0 {
        return Vec::new();
    }
    grid.lines_between(&p.tempo, axis.start.max(0.0), axis.t(right), 600)
}

/// Bar numbers and start times of the grid lines visible between `axis.start` and `right`.
fn bar_lines(tempo: &scythe_core::TempoMap, axis: TimeAxis, right: f32) -> Vec<(i64, f64)> {
    let tl = tempo.timeline();
    let step = grid_step((tempo.bar_seconds_at(axis.start.max(0.0)) * axis.pps) as f32);
    let first = tl.bar_beat(axis.start.max(0.0)).bar;
    let end = axis.t(right);
    let mut out = Vec::new();
    let mut bar = first - (first - 1).rem_euclid(step);
    loop {
        let t = tl.bar_start(bar);
        if t > end || out.len() > 4000 {
            break;
        }
        out.push((bar, t));
        bar += step;
    }
    out
}

fn ruler_timeline(app: &mut ScytheApp, ui: &mut Ui, rect: Rect, axis: TimeAxis) {
    let painter = ui.painter().with_clip_rect(rect);
    for (bar, t) in bar_lines(&app.project.tempo, axis, rect.right()) {
        let x = axis.x(t);
        painter.text(
            pos2(x + 2.0, rect.bottom() - 8.0),
            Align2::LEFT_CENTER,
            bar.to_string(),
            theme::mono(10.0),
            TEXT_DIM,
        );
    }

    // Tempo changes, labelled unless they crowd the previous label.
    let mut last_label = f32::NEG_INFINITY;
    for (i, pt) in app.project.tempo.points.iter().enumerate() {
        let x = axis.x(pt.time);
        if x < rect.left() - 40.0 || x > rect.right() {
            continue;
        }
        let prev = i.checked_sub(1).map(|j| &app.project.tempo.points[j]);
        let changed = prev.is_none_or(|p| (p.bpm - pt.bpm).abs() > 1e-6 || p.time_sig != pt.time_sig || p.linear);
        if !changed {
            continue;
        }
        painter.vline(x, (rect.bottom() - 18.0)..=(rect.bottom() - 2.0), Stroke::new(1.0, theme::YELLOW));
        if x - last_label > 46.0 {
            let mut label = format!("{}", (pt.bpm * 10.0).round() / 10.0);
            if let Some((n, d)) = pt.time_sig {
                label.push_str(&format!(" {n}/{d}"));
            }
            painter.text(
                pos2(x + 3.0, rect.bottom() - 20.0),
                Align2::LEFT_CENTER,
                label,
                theme::mono(9.0),
                theme::YELLOW,
            );
            last_label = x;
        }
    }

    if let Some(sel) = app.project.time_selection {
        let color = if app.project.loop_enabled { ACCENT } else { with_alpha(TEXT_DIM, 160) };
        let r =
            Rect::from_min_max(pos2(axis.x(sel.start), rect.top() + 28.0), pos2(axis.x(sel.end), rect.top() + 32.0));
        painter.rect_filled(r, CornerRadius::same(255), color);
    }

    let markers = &app.project.markers;
    let mut pills: Vec<(usize, Rect)> = Vec::new();
    for (i, m) in markers.iter().enumerate() {
        let x = axis.x(m.position);
        // A pill never runs into the next marker; when squeezed it shrinks to its colour dot.
        let room = markers.get(i + 1).map_or(f32::INFINITY, |n| axis.x(n.position) - x - 4.0);
        let label = if m.name.is_empty() { m.index.to_string() } else { m.name.clone() };
        let dot = m.color.map_or(TEXT_DIM, |c| Color32::from_rgb(c.r, c.g, c.b));
        let font = theme::semibold(11.0);
        let label = widgets::truncate(ui, &label, &font, room - 32.0);
        if label.chars().filter(|c| *c != '…').count() < 2 {
            painter.circle_filled(pos2(x + 4.0, rect.top() + 13.0), 3.0, dot);
            pills.push((i, Rect::from_center_size(pos2(x + 4.0, rect.top() + 13.0), vec2(10.0, 14.0))));
        } else {
            let w = (widgets::text_width(ui, &label, font.clone()) + 32.0).min(room);
            let pill = Rect::from_min_size(pos2(x, rect.top() + 2.0), vec2(w, 22.0));
            pills.push((i, pill));
            painter.rect_filled(pill, CornerRadius::same(255), RAISED);
            painter.circle_filled(pill.left_center() + vec2(11.0, 0.0), 3.0, dot);
            painter.text(pill.left_center() + vec2(20.0, 0.0), Align2::LEFT_CENTER, label, font, TEXT);
        }
        if let Some(end) = m.region_end {
            painter.hline(x..=axis.x(end), rect.top() + 25.0, Stroke::new(1.0, dot));
        }
    }

    let resp = ui.interact(rect, Id::unique("ruler"), Sense::click_and_drag());
    let bypass = ui.ctx().input(|i| i.modifiers.shift);
    if let Some(p) = resp.interact_pointer_pos() {
        let t = app.snap(axis.t(p.x).max(0.0), bypass);
        if resp.drag_started() {
            ui.data_mut(|d| d.insert_temp(Id::unique("ruler-anchor"), t));
        }
        if resp.dragged() {
            let anchor: f64 = ui.data(|d| d.get_temp(Id::unique("ruler-anchor"))).unwrap_or(t);
            let range = TimeRange::new(anchor, t);
            app.set_loop_range((!range.is_empty()).then_some(range));
        }
        if resp.clicked() {
            app.set_cursor(t);
            crate::ui::toolbars::mouse_action(app, ui, "MM_CTX_RULER_CLK");
        }
    }
    if resp.drag_stopped() {
        app.end_gesture();
    }
    if resp.double_clicked() && !crate::ui::toolbars::mouse_action(app, ui, "MM_CTX_RULER_DBLCLK") {
        app.edit("Clear loop points", |p| p.time_selection = None);
    }
    resp.on_hover_text("Click to move the cursor · drag to set loop points · double-click to clear");

    // Marker pills sit on top of the ruler: click jumps, double-click renames, right-click deletes.
    for (i, pill) in pills {
        let r = ui
            .interact(pill, Id::unique(("marker", i)), Sense::click())
            .on_hover_text("Click to jump · double-click to rename · right-click to delete");
        let Some(m) = app.project.markers.get(i).cloned() else { continue };
        if r.clicked() {
            app.set_cursor(m.position);
        }
        if r.double_clicked() {
            app.view.renaming_marker = Some((i, m.name.clone()));
            ui.memory_mut(|mem| mem.request_focus(Id::unique("marker-rename")));
        }
        if r.secondary_clicked() {
            app.edit("Delete marker", |p| {
                p.markers.remove(i);
            });
            break;
        }
    }
    if let Some((i, mut name)) = app.view.renaming_marker.take()
        && let Some(m) = app.project.markers.get(i).cloned()
    {
        let r = Rect::from_min_size(pos2(axis.x(m.position), rect.top() + 2.0), vec2(180.0, 22.0));
        ui.painter().rect_filled(r, CornerRadius::same(255), RAISED_HI);
        let field = egui::TextEdit::singleline(&mut name)
            .id(Id::unique("marker-rename"))
            .frame(egui::Frame::NONE)
            .font(theme::semibold(11.0));
        let edit = ui.put(r.shrink2(vec2(10.0, 2.0)), field);
        let cancel = ui.input(|inp| inp.key_pressed(egui::Key::Escape));
        if edit.lost_focus() && !cancel {
            app.edit("Rename marker", |p| p.markers[i].name = name.clone());
        } else if !cancel {
            app.view.renaming_marker = Some((i, name));
        }
    }
}

const RAZOR_ANCHOR: &str = "razor-anchor";

fn razor_dragging(ui: &Ui) -> bool {
    ui.data(|d| d.get_temp::<Pos2>(Id::unique(RAZOR_ANCHOR))).is_some()
}

/// Razor edits: Alt+drag anywhere on the lanes, over items too, marks the same time range on
/// every track it crosses; Shift+Alt adds to the existing areas. Read from the raw pointer, so
/// it works whichever widget is under it.
fn razor_drag(app: &mut ScytheApp, ui: &Ui, lanes: Rect, axis: TimeAxis, rows: Rect, row_h: f32, scroll: f32) {
    let id = Id::unique(RAZOR_ANCHOR);
    let (pressed, down, alt, shift, ctrl, pos) = ui.input(|i| {
        (
            i.pointer.primary_pressed(),
            i.pointer.primary_down(),
            i.modifiers.alt,
            i.modifiers.shift,
            i.modifiers.command,
            i.pointer.interact_pos(),
        )
    });
    if pressed
        && alt
        && let Some(p) = pos.filter(|p| lanes.contains(*p))
    {
        ui.data_mut(|d| d.insert_temp(id, p));
        ui.data_mut(|d| d.insert_temp(id.with("base"), if shift { app.razor.clone() } else { Vec::new() }));
    }
    let Some(anchor) = ui.data(|d| d.get_temp::<Pos2>(id)) else { return };
    if let Some(p) = pos {
        let (t0, t1) =
            (app.snap(axis.t(anchor.x.min(p.x)).max(0.0), false), app.snap(axis.t(anchor.x.max(p.x)), false));
        let row_of = |y: f32| ((y - rows.top() + scroll) / (row_h + ROW_GAP)).floor().max(0.0) as usize;
        let (first, last) = (row_of(anchor.y.min(p.y)), row_of(anchor.y.max(p.y)));
        let mut areas: Vec<(TrackId, TimeRange)> = ui.data(|d| d.get_temp(id.with("base"))).unwrap_or_default();
        if t1 > t0 {
            let range = TimeRange::new(t0, t1);
            areas.extend(
                app.project
                    .tracks
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| (first..=last).contains(i))
                    .map(|(_, t)| (t.id, range)),
            );
        }
        if ctrl {
            // Ctrl+Alt marks the tracks' visible envelopes instead of their items.
            app.razor_envelopes = areas
                .iter()
                .flat_map(|&(t, r)| {
                    let kinds: Vec<_> = app
                        .project
                        .track(t)
                        .map(|tr| tr.envelopes.iter().filter(|e| e.visible).map(|e| e.kind).collect())
                        .unwrap_or_default();
                    kinds.into_iter().map(move |k| (t, k, r))
                })
                .collect();
            app.razor.clear();
        } else {
            app.razor = areas;
            app.razor_envelopes.clear();
        }
    }
    if !down {
        ui.data_mut(|d| d.remove::<Pos2>(id));
        if !app.razor.is_empty() {
            app.multi.clear();
            app.sel.item = None;
        }
    }
}

fn track_rows(app: &mut ScytheApp, ui: &mut Ui, rows: Rect, axis: TimeAxis, clip: &egui::Painter) {
    let levels = app.project.folder_levels();
    let any_solo = app.project.any_solo();
    let routing = app.project.solo_routing();
    let grid = bar_lines(&app.project.tempo, axis, rows.right());
    let subs = if app.project.editing.grid.lines { sub_lines(app, axis, rows.right()) } else { Vec::new() };
    let n = app.project.tracks.len();
    let scroll = app.view.track_scroll;
    let row_h = app.view.row_h;
    let row_rect = |i: usize| {
        let top = rows.top() + i as f32 * (row_h + ROW_GAP) - scroll;
        Rect::from_min_size(pos2(rows.left(), top), vec2(rows.width(), row_h))
    };

    // Marquee selection: dragging on empty lane space. Registered first, so items and lanes
    // (which only take clicks) stay on top.
    let mut marquee_rect = None;
    let lanes_area = Rect::from_min_max(pos2(axis.left, rows.top()), rows.max);
    let marquee = ui.interact(lanes_area, Id::unique("lanes-marquee"), Sense::drag());
    let anchor_id = Id::unique("marquee-anchor");
    razor_drag(app, ui, lanes_area, axis, rows, row_h, scroll);
    if marquee.drag_started()
        && !razor_dragging(ui)
        && !crate::ui::comp::swiping(ui)
        && let Some(p) = marquee.interact_pointer_pos()
    {
        ui.data_mut(|d| d.insert_temp(anchor_id, p));
    }
    let anchor: Option<Pos2> = ui.data(|d| d.get_temp(anchor_id));
    if let (Some(a), Some(p)) = (anchor, marquee.interact_pointer_pos())
        && (marquee.dragged() || marquee.drag_stopped())
    {
        let r = Rect::from_two_pos(a, p);
        if marquee.dragged() {
            marquee_rect = Some(r);
        }
        if marquee.drag_stopped() {
            let (t0, t1) = (axis.t(r.left()), axis.t(r.right()));
            let row_of = |y: f32| ((y - rows.top() + scroll) / (row_h + ROW_GAP)).floor() as i64;
            let (first, last) = (row_of(r.top()).max(0) as usize, row_of(r.bottom()).max(0) as usize);
            let hits: Vec<(TrackId, ItemId)> = app
                .project
                .tracks
                .iter()
                .enumerate()
                .filter(|(i, _)| (first..=last).contains(i))
                .flat_map(|(_, t)| {
                    t.items.iter().filter(|it| it.end() > t0 && it.position < t1).map(move |it| (t.id, it.id))
                })
                .collect();
            if !ui.ctx().input(|i| i.modifiers.command) {
                app.multi.clear();
            }
            app.multi.extend(hits.iter().map(|(_, id)| *id));
            if let Some(&(track, item)) = hits.first() {
                app.sel = Selection { track: Some(track), item: Some(item) };
            }
            ui.data_mut(|d| d.remove::<Pos2>(anchor_id));
        }
    }

    let mut clicked_track: Option<(TrackId, Option<f64>)> = None;
    for (i, &level) in levels.iter().enumerate() {
        let row = row_rect(i);
        if row.bottom() < rows.top() || row.top() > rows.bottom() {
            continue;
        }
        let track = app.project.tracks[i].clone();
        let selected = app.sel.track == Some(track.id);
        let audible = routing.heard(i);
        let color = track_color(track.color);

        // Header.
        let head = Rect::from_min_size(row.min, vec2(HEADER_W, row_h));
        let head_resp = ui.interact(head.intersect(rows), Id::unique(("head", track.id)), Sense::click());
        clip.rect_filled(head, CornerRadius::same(12), if selected { RAISED_HI } else { RAISED });
        let indent = level as f32 * 10.0;
        let dot = pos2(head.left() + 17.0 + indent, head.top() + 19.0f32.min(row_h / 2.0));
        if track.folder_depth > 0 {
            clip.rect_filled(Rect::from_center_size(dot, vec2(12.0, 10.0)), CornerRadius::same(3), color);
        } else {
            clip.circle_filled(dot, 5.0, color);
        }
        let dot_hit = Rect::from_center_size(dot, vec2(16.0, 16.0)).intersect(rows);
        if dot_hit.height() > 0.0
            && ui
                .interact(dot_hit, Id::unique(("track-color", track.id)), Sense::click())
                .on_hover_text("Track colour")
                .clicked()
        {
            app.view.color_picker = Some((track.id, dot, true));
        }
        let db = format_db(track.volume);
        let db_w = widgets::text_width(ui, &db, theme::mono(11.0));
        let has_groups = track.groups.groups() != 0;
        let name_max = HEADER_W - 44.0 - indent - db_w - 12.0 - if has_groups { 30.0 } else { 0.0 };
        let name = widgets::truncate(ui, &track.name, &theme::semibold(13.0), name_max);
        clip.text(pos2(dot.x + 13.0, dot.y), Align2::LEFT_CENTER, name, theme::semibold(13.0), TEXT);
        clip.text(pos2(head.right() - 12.0, dot.y), Align2::RIGHT_CENTER, db, theme::mono(11.0), TEXT_DIM);
        if has_groups {
            let badge = Rect::from_min_size(pos2(head.right() - 12.0 - db_w - 30.0, dot.y - 8.0), vec2(26.0, 16.0));
            if rows.contains_rect(badge) {
                crate::ui::groups::badge(app, ui, badge, &track);
            }
        }

        let by = head.bottom() - 22.0;
        let btn = |k: f32| Rect::from_min_size(pos2(head.left() + 12.0 + k * 32.0, by - 2.0), vec2(28.0, 24.0));
        let full = row_h >= FULL_ROW_H;
        if full && rows.contains_rect(btn(0.0)) {
            let (mbg, mfg) = if track.muted { (theme::YELLOW, BG) } else { (RAISED_HI, TEXT_MID) };
            if text_pill(ui, btn(0.0), "M", theme::semibold(11.0), mbg, mfg).on_hover_text("Mute").clicked() {
                crate::ui::groups::toggle(app, track.id, GroupedControl::Mute);
            }
            crate::ui::groups::solo_button(app, ui, btn(1.0), &track);

            let arm_bg = if track.armed { theme::ARM_BG } else { RAISED_HI };
            let arm = ui.allocate_rect(btn(2.0), Sense::click()).on_hover_text("Arm for recording");
            ui.painter().rect_filled(
                btn(2.0),
                CornerRadius::same(255),
                if arm.hovered() { widgets::lighten(arm_bg, 0.07) } else { arm_bg },
            );
            ui.painter().circle_filled(btn(2.0).center(), 4.0, if track.armed { theme::PINK } else { theme::ARM_OFF });
            if arm.clicked() {
                crate::ui::groups::toggle(app, track.id, GroupedControl::RecArm);
            }
            arm.context_menu(|ui| crate::ui::recording::track_menu(app, ui, track.id));
        }
        let fx_label = format!("{} fx", track.fx.len());
        let fx_w = widgets::text_width(ui, &fx_label, theme::mono(10.0));
        if full {
            clip.text(
                pos2(head.right() - 12.0, by + 10.0),
                Align2::RIGHT_CENTER,
                &fx_label,
                theme::mono(10.0),
                TEXT_DIM,
            );
        }
        let meter = if full {
            Rect::from_min_max(
                pos2(head.left() + 12.0 + 3.0 * 32.0 + 8.0, by + 8.0),
                pos2(head.right() - 20.0 - fx_w, by + 12.0),
            )
        } else {
            Rect::from_min_max(
                pos2(head.left() + 12.0, head.bottom() - 6.0),
                pos2(head.right() - 12.0, head.bottom() - 3.0),
            )
        };
        clip.rect_filled(meter, CornerRadius::same(255), BG);
        let (l, r) = app.levels.get(&track.id).copied().unwrap_or_default();
        let level = meter_fraction(l.max(r));
        if level > 0.0 && audible {
            let fill = Rect::from_min_size(meter.min, vec2(meter.width() * level, meter.height()));
            clip.rect_filled(fill, CornerRadius::same(255), color);
        }
        if head_resp.clicked() {
            app.sel = Selection { track: Some(track.id), item: None };
        }
        if head_resp.double_clicked() {
            crate::ui::toolbars::mouse_action(app, ui, "MM_CTX_TCP_DBLCLK");
        }

        // Lane.
        let lane = Rect::from_min_max(pos2(axis.left, row.top()), row.max);
        clip.rect_filled(lane, CornerRadius::same(12), if selected { LANE_SELECTED } else { LANE });
        let lane_painter = clip.with_clip_rect(lane.intersect(rows));
        for &t in &subs {
            lane_painter.vline(axis.x(t), lane.y_range(), Stroke::new(1.0, with_alpha(RAISED, 110)));
        }
        for &(_, t) in &grid {
            let x = axis.x(t);
            lane_painter.vline(x, lane.y_range(), Stroke::new(1.0, RAISED));
        }
        let lane_resp = ui.interact(lane.intersect(rows), Id::unique(("lane", track.id)), Sense::click());
        if lane_resp.clicked() {
            let t = lane_resp.interact_pointer_pos().map(|p| axis.t(p.x));
            clicked_track = Some((track.id, t));
        }
        if lane_resp.double_clicked() {
            crate::ui::toolbars::mouse_action(app, ui, "MM_CTX_TRACK_DBLCLK");
        }

        let dim = !audible || (any_solo && !track.solo && !audible);
        for item in &track.items {
            let silent = !track.lane_plays(item.lane());
            draw_item(app, ui, &lane_painter, lane, axis, track.id, item.id, color, dim || silent);
        }
        crate::ui::comp::track_lanes(app, ui, &lane_painter, lane, &|t| axis.x(t), &|x| axis.t(x), track.id);
        if let Some(rec) = app.recording.as_ref().filter(|r| r.tracks.contains(&track.id)) {
            let now = app.engine.transport().position_seconds();
            // After the transport wrapped, the current pass runs from the loop start.
            let from = match rec.pass.looped {
                Some(r) if now < rec.pass.start => r.start,
                _ => rec.pass.start,
            };
            let r = Rect::from_min_max(pos2(axis.x(from), lane.top() + 4.0), pos2(axis.x(now), lane.bottom() - 4.0));
            lane_painter.rect_filled(r, CornerRadius::same(10), with_alpha(theme::PINK, 0x40));
            lane_painter.rect_stroke(r, CornerRadius::same(10), Stroke::new(1.5, theme::PINK), StrokeKind::Inside);
            lane_painter.text(
                r.left_top() + vec2(8.0, 10.0),
                Align2::LEFT_CENTER,
                "Recording",
                theme::semibold(10.0),
                TEXT,
            );
        }
        envelopes(app, ui, &lane_painter, lane, axis, track.id);
    }

    for &(track, _, range) in &app.razor_envelopes {
        let Some(i) = app.project.track_index(track) else { continue };
        let row = row_rect(i);
        let r = Rect::from_min_max(pos2(axis.x(range.start), row.top()), pos2(axis.x(range.end), row.bottom()));
        clip.rect_filled(r, CornerRadius::same(4), with_alpha(theme::YELLOW, 0x28));
        clip.rect_stroke(r, CornerRadius::same(4), Stroke::new(1.0, theme::YELLOW), StrokeKind::Inside);
    }
    for &(track, range) in &app.razor {
        let Some(i) = app.project.track_index(track) else { continue };
        let row = row_rect(i);
        let r = Rect::from_min_max(pos2(axis.x(range.start), row.top()), pos2(axis.x(range.end), row.bottom()));
        clip.rect_filled(r, CornerRadius::same(4), with_alpha(theme::PINK, 0x30));
        clip.rect_stroke(r, CornerRadius::same(4), Stroke::new(1.0, theme::PINK), StrokeKind::Inside);
    }

    if let Some(r) = marquee_rect {
        clip.rect_filled(r, CornerRadius::same(4), with_alpha(ACCENT, 0x22));
        clip.rect_stroke(r, CornerRadius::same(4), Stroke::new(1.0, ACCENT), StrokeKind::Inside);
    }

    if let Some((track, t)) = clicked_track {
        app.sel = Selection { track: Some(track), item: None };
        app.multi.clear();
        app.razor.clear();
        app.razor_envelopes.clear();
        if let Some(t) = t {
            let bypass = ui.ctx().input(|i| i.modifiers.shift);
            let at = app.snap(t.max(0.0), bypass);
            app.set_cursor(at);
        }
        crate::ui::toolbars::mouse_action(app, ui, "MM_CTX_TRACK_CLK");
    }

    // Empty area below the last track: double-click adds a track, dropped files land here too.
    let used = n as f32 * (app.view.row_h + ROW_GAP) - scroll;
    let empty = Rect::from_min_max(pos2(rows.left(), (rows.top() + used).max(rows.top())), rows.max);
    if empty.height() > 4.0 {
        if n == 0 {
            ui.painter().text(
                empty.center(),
                Align2::CENTER_CENTER,
                "Drop audio files here, or press Ctrl+K and open the demo song",
                theme::regular(13.0),
                TEXT_DIM,
            );
        }
        let resp = ui.interact(empty, Id::unique("empty-rows"), Sense::click());
        if resp.double_clicked() {
            app.add_track();
        }
    }

    handle_drop(app, ui, rows, axis, &row_rect);
}

fn handle_drop(app: &mut ScytheApp, ui: &Ui, rows: Rect, axis: TimeAxis, row_rect: &dyn Fn(usize) -> Rect) {
    let dropped: Vec<std::path::PathBuf> = ui.ctx().input(|i| {
        i.raw.dropped_files.iter().map(|f| f.path().to_owned()).filter(|p| !p.as_os_str().is_empty()).collect()
    });
    let hovering = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
    if hovering {
        ui.painter().rect_stroke(rows, CornerRadius::same(12), Stroke::new(2.0, ACCENT), StrokeKind::Inside);
    }
    if dropped.is_empty() {
        return;
    }
    let pos = ui.ctx().input(|i| i.pointer.hover_pos()).unwrap_or(rows.center());
    if app.nam.panel_rect.is_some_and(|r| r.contains(pos)) {
        return;
    }
    let track = (0..app.project.tracks.len()).find(|&i| row_rect(i).contains(pos)).map(|i| app.project.tracks[i].id);
    let (models, media): (Vec<_>, Vec<_>) = dropped.into_iter().partition(|p| crate::nam::is_model_file(p));
    if !models.is_empty() {
        match track.or(app.sel.track) {
            Some(t) => app.drop_nam_files(t, &models),
            None => app.notify("Drop NAM models on a track"),
        }
    }
    if media.is_empty() {
        return;
    }
    let at = if pos.x >= axis.left { app.snap(axis.t(pos.x).max(0.0), false) } else { app.project.cursor };
    app.import_files(media, track, at);
}

/// Perceptual meter position: -60 dB..0 dB mapped linearly in dB.
pub fn meter_fraction(peak: f32) -> f32 {
    if peak <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * peak.log10();
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

#[allow(clippy::too_many_arguments)]
fn draw_item(
    app: &mut ScytheApp,
    ui: &mut Ui,
    painter: &egui::Painter,
    lane: Rect,
    axis: TimeAxis,
    track: TrackId,
    id: ItemId,
    color: Color32,
    dim: bool,
) {
    let Some((_, item)) = app.project.find_item(id) else { return };
    let item = item.clone();
    let x0 = axis.x(item.position);
    let x1 = axis.x(item.end());
    if x1 < lane.left() || x0 > lane.right() {
        return;
    }
    let (top, bottom) = match item.ypos {
        Some((t, h)) if h > 0.0 && h < 1.0 => {
            let y = |f: f64| lane.top() + 2.0 + f as f32 * (lane.height() - 4.0);
            (y(t) + 1.0, y(t + h) - 1.0)
        }
        _ => (lane.top() + 4.0, lane.bottom() - 4.0),
    };
    let outer = Rect::from_min_max(pos2(x0, top), pos2(x1 - 3.0, bottom.max(top + 3.0)));
    let compact = outer.height() < 40.0;
    if outer.width() < 1.0 {
        return;
    }
    let selected = app.is_selected(id);
    let fade = if dim || item.muted { 0.35 } else { 1.0 };
    let bg = with_alpha(color, if selected { 0x3d } else { 0x26 }).gamma_multiply(fade);
    let radius = CornerRadius::same((outer.height() / 2.0).min(10.0) as u8);
    painter.rect_filled(outer, radius, bg);
    if selected {
        painter.rect_stroke(outer, radius, Stroke::new(2.0, ACCENT), StrokeKind::Inside);
    }
    let item_painter = painter.with_clip_rect(outer.intersect(painter.clip_rect()));
    if !compact && item.takes.len() > 1 && outer.width() > 60.0 {
        item_painter.text(
            outer.right_top() + vec2(-8.0, 10.0),
            Align2::RIGHT_CENTER,
            format!("{}/{}", item.active_take + 1, item.takes.len()),
            theme::mono(9.0),
            TEXT_DIM.gamma_multiply(fade),
        );
    }
    if !compact {
        let label = widgets::truncate(ui, item.name(), &theme::semibold(10.0), outer.width() - 16.0);
        item_painter.text(
            outer.left_top() + vec2(8.0, 10.0),
            Align2::LEFT_CENTER,
            label,
            theme::semibold(10.0),
            TEXT.gamma_multiply(fade),
        );
    }

    let wave = if compact {
        outer.shrink2(vec2(3.0, 1.0))
    } else {
        Rect::from_min_max(
            pos2(outer.left() + 6.0, outer.bottom() - 44.0),
            pos2(outer.right() - 6.0, outer.bottom() - 6.0),
        )
    };
    if let Some(take) = item.take() {
        match app.media(take) {
            Some(MediaState::Ready(media)) => {
                let sr = media.audio.sample_rate as f64;
                let media_len = media.audio.frames() as f64 / sr;
                let mut mesh = Mesh::default();
                let fill = color.gamma_multiply(0.9 * fade);
                let visible = wave.intersect(painter.clip_rect());
                let mid = wave.center().y;
                let half = wave.height() / 2.0;
                let mut x = visible.left().floor();
                while x < visible.right() {
                    let t0 = axis.t(x) - item.position;
                    let t1 = axis.t(x + 1.0) - item.position;
                    let (Some(s0), Some(s1)) = (
                        take.source_time(t0, item.looped, media_len),
                        take.source_time(t1.min(item.length - 1e-9), item.looped, media_len),
                    ) else {
                        x += 1.0;
                        continue;
                    };
                    let (f0, f1) = (s0.min(s1) * sr, s0.max(s1) * sr);
                    let (lo, hi) = media.peaks.range(f0, f1.max(f0 + 1.0));
                    let g = (item.volume * take.volume) as f32;
                    let top = mid - (hi * g).clamp(-1.0, 1.0) * half;
                    let bottom = mid - (lo * g).clamp(-1.0, 1.0) * half;
                    mesh.add_colored_rect(Rect::from_min_max(pos2(x, top), pos2(x + 1.0, bottom.max(top + 1.0))), fill);
                    x += 1.0;
                }
                item_painter.add(Shape::mesh(mesh));
            }
            Some(MediaState::Failed(_)) => {
                item_painter.text(
                    wave.center(),
                    Align2::CENTER_CENTER,
                    "media offline",
                    theme::regular(11.0),
                    theme::PINK,
                );
            }
            None if matches!(&take.source, scythe_core::Source::Raw(c) if c.args().first().is_some_and(|k| k == "CLICK")) =>
            {
                let tl = app.project.tempo.timeline();
                let (t0, t1) = (axis.t(wave.left()).max(item.position), axis.t(wave.right()).min(item.end()));
                let color = color.gamma_multiply(fade);
                tl.for_each_beat(t0, t1, |t, down| {
                    let x = axis.x(t);
                    let h = if down { wave.height() } else { wave.height() * 0.5 };
                    item_painter.vline(x, (wave.bottom() - h)..=wave.bottom(), Stroke::new(1.0, color));
                });
            }
            None if app.view.inline_midi.contains(&item.id) => {}
            None if let Some(notes) = app.midi_notes(&item) => {
                draw_midi(&item_painter, wave.expand2(vec2(0.0, 10.0)), axis, &notes, color.gamma_multiply(fade));
            }
            None if take.source.file().is_none() => {
                item_painter.text(
                    wave.center(),
                    Align2::CENTER_CENTER,
                    "source not supported yet",
                    theme::regular(11.0),
                    TEXT_DIM,
                );
            }
            None => {
                item_painter.text(wave.center(), Align2::CENTER_CENTER, "loading…", theme::regular(11.0), TEXT_DIM);
            }
        }
    }

    let hit = outer.intersect(painter.clip_rect());
    if hit.width() <= 0.0 || hit.height() <= 0.0 || razor_dragging(ui) || crate::ui::comp::swiping(ui) {
        return;
    }
    let resp = ui.interact(hit, Id::unique(("item", id)), Sense::click_and_drag());
    let ctrl = ui.ctx().input(|i| i.modifiers.command);
    if resp.clicked() && ctrl {
        // Ctrl+click adds the item to the selection, or takes it out again.
        if app.is_selected(id) {
            app.multi.remove(&id);
            if app.sel.item == Some(id) {
                app.sel.item = app.multi.iter().next().copied();
            }
        } else {
            if let Some(primary) = app.sel.item {
                app.multi.insert(primary);
            }
            app.multi.insert(id);
            app.sel = Selection { track: Some(track), item: Some(id) };
        }
    } else if resp.clicked() || (resp.drag_started() && !app.is_selected(id)) {
        app.multi.clear();
        app.sel = Selection { track: Some(track), item: Some(id) };
        // Clicking a grouped item selects its whole group, as in REAPER.
        app.multi.extend(app.project.with_groups(&[id]).into_iter().filter(|i| *i != id));
    } else if resp.drag_started() {
        app.sel = Selection { track: Some(track), item: Some(id) };
    }
    if resp.drag_started()
        && let Some(p) = crate::ui::widgets::drag_origin(ui, &resp)
    {
        ui.data_mut(|d| d.insert_temp(Id::unique("item-grab"), axis.t(p.x) - item.position));
    }
    let several = !app.multi.is_empty() && app.is_selected(id);
    if resp.dragged()
        && several
        && let Some(p) = resp.interact_pointer_pos()
    {
        // Moving one of several selected items moves them all in time, each on its own track.
        let grab: f64 = ui.data(|d| d.get_temp(Id::unique("item-grab"))).unwrap_or(0.0);
        let bypass = ui.ctx().input(|i| i.modifiers.shift);
        let new_pos = app.snap((axis.t(p.x) - grab).max(0.0), bypass);
        let items = app.selected_items();
        let earliest = items
            .iter()
            .filter_map(|i| app.project.find_item(*i))
            .map(|(_, it)| it.position)
            .fold(f64::INFINITY, f64::min);
        let delta = (new_pos - item.position).max(-earliest);
        app.gesture("Move items", |proj| {
            proj.move_items(&items, delta);
        });
    }
    if resp.dragged()
        && !several
        && let Some(p) = resp.interact_pointer_pos()
    {
        let grab: f64 = ui.data(|d| d.get_temp(Id::unique("item-grab"))).unwrap_or(0.0);
        let bypass = ui.ctx().input(|i| i.modifiers.shift);
        let new_pos = app.snap((axis.t(p.x) - grab).max(0.0), bypass);
        let dest = track_under(app, ui, p);
        let locked = app.project.position_locked(&item);
        app.gesture("Move item", |proj| {
            if let Some(dest) = dest.filter(|_| !locked) {
                proj.move_item_to_track(id, dest);
            }
            let from = proj.find_item(id).map_or(new_pos, |(_, it)| it.position);
            proj.move_items(&[id], new_pos - from);
        });
        if let Some(dest) = dest {
            app.sel.track = Some(dest);
        }
    }
    if resp.drag_stopped() {
        app.end_gesture();
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    let assigned = resp.double_clicked() && crate::ui::toolbars::mouse_action(app, ui, "MM_CTX_ITEM_DBLCLK");
    if resp.clicked() {
        crate::ui::toolbars::mouse_action(app, ui, "MM_CTX_ITEM_CLK");
    }
    if !assigned && resp.double_clicked() && ui.input(|i| i.modifiers.alt) {
        crate::ui::midi_editor::toggle_inline(app, id);
    } else if !assigned
        && resp.double_clicked()
        && item.take().is_some_and(
            |t| matches!(&t.source, scythe_core::Source::Raw(c) if c.args().first().is_some_and(|k| k == "MIDI")),
        )
    {
        app.view.midi_editor = Some(id);
    }
    if !assigned && resp.double_clicked() {
        app.open_subproject(id);
    }
    let (x, t) = (|t: f64| axis.x(t), |x: f32| axis.t(x));
    crate::ui::editing::item_badges(&item_painter, outer, &item);
    crate::ui::editing::draw_preview(app, &item_painter, outer, item.id, &x);
    crate::ui::stretch_markers::show(app, ui, &item_painter, outer, (&x, &t), track, &item);
    item_handles(app, ui, &item_painter, outer, axis, track, &item);
    if app.view.inline_midi.contains(&item.id) {
        crate::ui::midi_editor::inline_editor(app, ui, wave.intersect(outer), item.id, &x, &t);
    }
}

#[derive(Clone, Copy)]
enum ItemHandle {
    TrimStart,
    TrimEnd,
    FadeIn,
    FadeOut,
}

/// Fade curves plus the edge and fade handles of an item. Registered after the item body, so
/// they win over the move gesture where they overlap.
fn item_handles(
    app: &mut ScytheApp,
    ui: &mut Ui,
    painter: &egui::Painter,
    outer: Rect,
    axis: TimeAxis,
    track: TrackId,
    item: &scythe_core::Item,
) {
    let id = item.id;
    let fade_in_px = (item.fade_in.effective_length() * axis.pps) as f32;
    let fade_out_px = (item.fade_out.effective_length() * axis.pps) as f32;
    let curve_color = with_alpha(TEXT, 140);
    for (fade, px, out) in [(item.fade_in, fade_in_px, false), (item.fade_out, fade_out_px, true)] {
        if px < 3.0 {
            continue;
        }
        let steps = (px / 3.0).ceil().max(2.0) as usize;
        let pts: Vec<Pos2> = (0..=steps)
            .map(|s| {
                let f = s as f32 / steps as f32;
                let x = if out { outer.right() - px + f * px } else { outer.left() + f * px };
                let progress = if out { 1.0 - f } else { f };
                let g = if out { fade.out_gain(progress as f64) } else { fade.gain(progress as f64) } as f32;
                pos2(x, outer.bottom() - g * outer.height())
            })
            .collect();
        let mut shade = Mesh::default();
        for w in pts.windows(2) {
            let r = Rect::from_min_max(pos2(w[0].x, outer.top()), pos2(w[1].x, w[0].y.min(w[1].y)));
            shade.add_colored_rect(r, with_alpha(BG, 90));
        }
        painter.add(Shape::mesh(shade));
        painter.add(Shape::line(pts, Stroke::new(1.0, curve_color)));
    }
    if outer.width() < 24.0 {
        return;
    }

    let edge = 6.0;
    let full = outer.height() >= 40.0;
    let below_label = if full { 16.0 } else { 0.0 };
    let handles = [
        (
            ItemHandle::TrimStart,
            Rect::from_min_max(outer.left_top() + vec2(0.0, below_label), pos2(outer.left() + edge, outer.bottom())),
        ),
        (
            ItemHandle::TrimEnd,
            Rect::from_min_max(pos2(outer.right() - edge, outer.top() + below_label), outer.right_bottom()),
        ),
        (
            ItemHandle::FadeIn,
            Rect::from_center_size(pos2(outer.left() + fade_in_px, outer.top() + 4.0), vec2(12.0, 12.0)),
        ),
        (
            ItemHandle::FadeOut,
            Rect::from_center_size(pos2(outer.right() - fade_out_px, outer.top() + 4.0), vec2(12.0, 12.0)),
        ),
    ];
    let (edges_locked, fades_locked) = (app.project.edges_locked(item), app.project.fades_locked(item));
    for (i, (kind, rect)) in handles.into_iter().enumerate() {
        if !full && matches!(kind, ItemHandle::FadeIn | ItemHandle::FadeOut) {
            continue;
        }
        let fade = matches!(kind, ItemHandle::FadeIn | ItemHandle::FadeOut);
        if (fade && fades_locked) || (!fade && edges_locked) {
            continue;
        }
        let hit = rect.intersect(painter.clip_rect());
        if hit.width() <= 0.0 || hit.height() <= 0.0 {
            continue;
        }
        let resp = ui.interact(hit, Id::unique(("item-handle", id, i)), Sense::drag());
        let hot = resp.hovered() || resp.dragged();
        if hot {
            let icon = match kind {
                ItemHandle::TrimStart | ItemHandle::TrimEnd => egui::CursorIcon::ResizeHorizontal,
                _ => egui::CursorIcon::ResizeColumn,
            };
            ui.ctx().set_cursor_icon(icon);
        }
        if matches!(kind, ItemHandle::FadeIn | ItemHandle::FadeOut) {
            painter.circle_filled(
                rect.center() + vec2(0.0, 4.0),
                if hot { 4.5 } else { 3.0 },
                if hot { ACCENT } else { curve_color },
            );
        }
        if resp.drag_started() {
            app.sel = Selection { track: Some(track), item: Some(id) };
        }
        if resp.dragged()
            && let Some(p) = resp.interact_pointer_pos()
        {
            let bypass = ui.ctx().input(|i| i.modifiers.shift);
            let t = axis.t(p.x).max(0.0);
            let snapped = app.snap(t, bypass);
            let label = match kind {
                ItemHandle::TrimStart | ItemHandle::TrimEnd => "Trim item",
                _ => "Change fade",
            };
            app.gesture(label, |proj| {
                let Some(it) = proj.tracks.iter_mut().find_map(|t| t.item_mut(id)) else { return };
                match kind {
                    ItemHandle::TrimStart => it.trim_start(snapped),
                    ItemHandle::TrimEnd => it.trim_end(snapped),
                    ItemHandle::FadeIn => {
                        it.fade_in.length = (t - it.position).clamp(0.0, it.length - it.fade_out.length.min(it.length));
                        it.fade_in.auto_length = 0.0;
                    }
                    ItemHandle::FadeOut => {
                        it.fade_out.length = (it.end() - t).clamp(0.0, it.length - it.fade_in.length.min(it.length));
                        it.fade_out.auto_length = 0.0;
                    }
                }
            });
        }
        if resp.drag_stopped() {
            app.end_gesture();
        }
    }
}

fn track_under(app: &ScytheApp, ui: &Ui, p: Pos2) -> Option<TrackId> {
    // Lanes register an interact rect per track; reuse their ids to find the one under `p`.
    app.project.tracks.iter().find_map(|t| {
        let r = ui.ctx().read_response(Id::unique(("lane", t.id)))?.rect;
        (p.y >= r.top() && p.y <= r.bottom()).then_some(t.id)
    })
}

pub(super) fn envelope_color(kind: EnvelopeKind) -> Color32 {
    match kind {
        EnvelopeKind::Volume | EnvelopeKind::VolumePreFx => theme::GREEN,
        EnvelopeKind::Trim => theme::YELLOW,
        EnvelopeKind::Pan | EnvelopeKind::DualPanLeft | EnvelopeKind::DualPanRight => theme::ORANGE,
        EnvelopeKind::Width => theme::YELLOW,
        EnvelopeKind::Mute => theme::PINK,
        EnvelopeKind::Pitch => theme::TEXT_MID,
    }
}

/// Vertical position of an envelope value within the lane, 0 at the bottom and 1 at the top.
pub(super) fn envelope_y_fraction(kind: EnvelopeKind, value: f64) -> f32 {
    match kind {
        EnvelopeKind::Volume | EnvelopeKind::VolumePreFx | EnvelopeKind::Trim => {
            crate::ui::mixer::fader_position(value)
        }
        EnvelopeKind::Pan | EnvelopeKind::Width | EnvelopeKind::DualPanLeft | EnvelopeKind::DualPanRight => {
            ((value + 1.0) * 0.5) as f32
        }
        EnvelopeKind::Mute => value as f32,
        EnvelopeKind::Pitch => ((value + 24.0) / 48.0) as f32,
    }
}

pub(super) fn envelope_value(kind: EnvelopeKind, fraction: f32) -> f64 {
    let f = fraction.clamp(0.0, 1.0);
    match kind {
        EnvelopeKind::Volume | EnvelopeKind::VolumePreFx | EnvelopeKind::Trim => crate::ui::mixer::fader_gain(f),
        EnvelopeKind::Pan | EnvelopeKind::Width | EnvelopeKind::DualPanLeft | EnvelopeKind::DualPanRight => {
            (f as f64 * 2.0 - 1.0).clamp(-1.0, 1.0)
        }
        EnvelopeKind::Mute => (f >= 0.5) as u8 as f64,
        EnvelopeKind::Pitch => f as f64 * 48.0 - 24.0,
    }
}

pub(super) fn format_envelope_value(kind: EnvelopeKind, value: f64) -> String {
    match kind {
        EnvelopeKind::Volume | EnvelopeKind::VolumePreFx | EnvelopeKind::Trim => format!("{} dB", format_db(value)),
        EnvelopeKind::Pan | EnvelopeKind::DualPanLeft | EnvelopeKind::DualPanRight => {
            scythe_core::units::format_pan(-value)
        }
        EnvelopeKind::Width => format!("{:.0}%", value * 100.0),
        EnvelopeKind::Mute => if value >= 0.5 { "on" } else { "muted" }.to_owned(),
        EnvelopeKind::Pitch => format!("{value:+.2} st"),
    }
}

/// Draws the visible envelopes of a track over its lane and handles point editing: drag a point
/// to move it, right-click to delete it, double-click near the curve to add one.
fn envelopes(app: &mut ScytheApp, ui: &mut Ui, painter: &egui::Painter, lane: Rect, axis: TimeAxis, track_id: TrackId) {
    let Some(track) = app.project.track(track_id) else { return };
    let envs: Vec<_> = track.envelopes.iter().filter(|e| e.visible && !e.points.is_empty()).cloned().collect();
    if envs.is_empty() {
        return;
    }
    let area = lane.shrink2(vec2(0.0, 8.0));
    let y_of = |kind, v| area.bottom() - envelope_y_fraction(kind, v) * area.height();
    let pointer = ui.ctx().pointer_hover_pos();
    for env in &envs {
        let color = envelope_color(env.kind);
        let alpha = if env.active { 255 } else { 110 };
        let mut line = Vec::new();
        let mut x = lane.left().max(painter.clip_rect().left());
        let right = lane.right().min(painter.clip_rect().right());
        while x <= right {
            if let Some(v) = env.value_at(axis.t(x)) {
                line.push(pos2(x, y_of(env.kind, v)));
            }
            x += 2.0;
        }
        painter.add(Shape::line(line, Stroke::new(1.5, with_alpha(color, alpha))));

        for (i, p) in env.points.iter().enumerate() {
            let c = pos2(axis.x(p.time), y_of(env.kind, p.value));
            if !lane.contains(pos2(c.x, lane.center().y)) {
                continue;
            }
            let handle = Rect::from_center_size(c, vec2(12.0, 12.0));
            let id = Id::unique(("env-point", track_id, env.kind.chunk_name(), i));
            let resp = ui.interact(handle, id, Sense::click_and_drag());
            let hot = resp.hovered() || resp.dragged();
            painter.circle_filled(c, if hot { 5.0 } else { 3.5 }, with_alpha(color, alpha));
            if hot {
                painter.text(
                    c + vec2(8.0, -10.0),
                    Align2::LEFT_BOTTOM,
                    format_envelope_value(env.kind, p.value),
                    theme::mono(10.0),
                    TEXT,
                );
            }
            if resp.dragged()
                && let Some(pos) = resp.interact_pointer_pos()
            {
                let bypass = ui.ctx().input(|inp| inp.modifiers.shift);
                let prev = i.checked_sub(1).map(|j| env.points[j].time);
                let next = env.points.get(i + 1).map(|q| q.time);
                let t = app.snap(axis.t(pos.x).max(0.0), bypass).clamp(prev.unwrap_or(0.0), next.unwrap_or(f64::MAX));
                let v = envelope_value(env.kind, (area.bottom() - pos.y) / area.height());
                let kind = env.kind;
                app.gesture("Move envelope point", |p| {
                    let pt = &mut p.track_mut(track_id).unwrap().envelope_mut(kind).points[i];
                    pt.time = t;
                    pt.value = v;
                });
            }
            if resp.drag_stopped() {
                app.end_gesture();
            }
            if resp.secondary_clicked() {
                let kind = env.kind;
                app.edit("Delete envelope point", |p| {
                    p.track_mut(track_id).unwrap().envelope_mut(kind).points.remove(i);
                });
                return;
            }
        }
    }

    let double = ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary));
    if double
        && let Some(pos) = pointer
        && area.expand(6.0).contains(pos)
    {
        let t = axis.t(pos.x).max(0.0);
        let near = envs.iter().find(|e| e.value_at(t).is_some_and(|v| (y_of(e.kind, v) - pos.y).abs() < 8.0));
        if let Some(env) = near {
            let kind = env.kind;
            let bypass = ui.ctx().input(|i| i.modifiers.shift);
            let t = app.snap(t, bypass);
            let v = env.value_at(t).unwrap_or(kind.neutral());
            app.edit("Add envelope point", |p| {
                let e = p.track_mut(track_id).unwrap().envelope_mut(kind);
                e.points.push(scythe_core::EnvPoint::new(t, v));
                e.sort();
            });
        }
    }
}

/// Notes as a compact piano roll scaled to the item's pitch range.
fn draw_midi(
    painter: &egui::Painter,
    area: Rect,
    axis: TimeAxis,
    notes: &[scythe_core::midi::TimedNote],
    color: Color32,
) {
    let Some(lo) = notes.iter().map(|n| n.key).min() else { return };
    let hi = notes.iter().map(|n| n.key).max().unwrap_or(lo);
    let rows = (hi - lo) as f32 + 1.0;
    let h = (area.height() / rows).clamp(1.5, 6.0);
    let span = area.height() - h;
    let visible = painter.clip_rect();
    let mut mesh = Mesh::default();
    for n in notes {
        let (x0, x1) = (axis.x(n.start), axis.x(n.end).max(axis.x(n.start) + 2.0));
        if x1 < visible.left() || x0 > visible.right() {
            continue;
        }
        let y = area.bottom() - h - if rows > 1.0 { (n.key - lo) as f32 / (rows - 1.0) * span } else { span / 2.0 };
        let alpha = 0.45 + 0.55 * n.velocity as f32 / 127.0;
        mesh.add_colored_rect(Rect::from_min_size(pos2(x0, y), vec2(x1 - x0, h)), color.gamma_multiply(alpha));
    }
    painter.add(Shape::mesh(mesh));
}

/// Swatches offered for track colours: the redesign palette and a few muted variants.
const SWATCHES: [u32; 12] = [
    0x66d9ef, 0xa6e22e, 0xe6db74, 0xfd971f, 0xf92672, 0xae81ff, 0x4f9fb0, 0x7fa83a, 0xb8a955, 0xc4762a, 0xb8325f,
    0x8565c4,
];

/// Floating palette for the track whose colour dot was clicked.
pub fn color_picker(app: &mut ScytheApp, ui: &mut Ui) {
    let Some((track_id, anchor, just_opened)) = app.view.color_picker else { return };
    let area =
        egui::Area::new(Id::unique("color-picker")).order(egui::Order::Foreground).fixed_pos(anchor + vec2(10.0, 8.0));
    let resp = area.show(ui.ctx(), |ui| {
        let size = vec2(6.0 * 26.0 + 16.0, 3.0 * 26.0 + 16.0);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        ui.painter().rect_filled(rect, CornerRadius::same(12), RAISED_HI);
        for (k, hex) in SWATCHES.iter().map(Some).chain([None]).enumerate() {
            let cell = Rect::from_min_size(
                rect.min + vec2(8.0 + (k % 6) as f32 * 26.0, 8.0 + (k / 6) as f32 * 26.0),
                vec2(22.0, 22.0),
            );
            let r = ui.interact(cell, Id::unique(("swatch", k)), Sense::click());
            let c = hex.map(|h| scythe_core::Color::from_hex(*h));
            match c {
                Some(c) => {
                    ui.painter().circle_filled(
                        cell.center(),
                        if r.hovered() { 10.0 } else { 8.0 },
                        Color32::from_rgb(c.r, c.g, c.b),
                    );
                }
                None => {
                    ui.painter().circle_stroke(cell.center(), 8.0, Stroke::new(1.5, TEXT_DIM));
                    ui.painter().line_segment(
                        [cell.left_bottom() + vec2(5.0, -5.0), cell.right_top() + vec2(-5.0, 5.0)],
                        Stroke::new(1.5, TEXT_DIM),
                    );
                }
            }
            if r.clicked() {
                app.edit("Track colour", |p| {
                    if let Some(t) = p.track_mut(track_id) {
                        t.color = c;
                    }
                });
                app.view.color_picker = None;
            }
        }
    });
    if just_opened {
        app.view.color_picker = app.view.color_picker.map(|(t, p, _)| (t, p, false));
    } else if resp.response.clicked_elsewhere() {
        app.view.color_picker = None;
    }
}
