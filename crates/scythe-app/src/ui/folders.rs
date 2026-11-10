//! Track selection as REAPER makes it (click, Ctrl+click toggles, Shift+click selects a range),
//! selection grouping of the track switches, track drag and drop in the track headers and the
//! mixer, and the folder buttons.

use egui::{Color32, CornerRadius, Id, Pos2, Rect, Shape, Stroke, Ui, pos2, vec2};
use scythe_core::folders::Drop;
use scythe_core::{Track, TrackId};

use crate::app::{ScytheApp, Selection};
use crate::theme::{self, ACCENT, TEXT_DIM, TEXT_MID};

/// The tracks selected besides `sel.track`.
#[derive(Debug, Clone, Default)]
pub struct TrackSel {
    /// The selected track the list was made with: when something else selects another track
    /// (an item click, a surface), the list no longer counts.
    for_track: Option<TrackId>,
    tracks: Vec<TrackId>,
    /// Where Shift+click ranges start.
    anchor: Option<TrackId>,
}

impl ScytheApp {
    /// Every selected track, in track order.
    pub fn selected_tracks(&self) -> Vec<TrackId> {
        let Some(primary) = self.sel.track else { return Vec::new() };
        let list = if self.track_sel.for_track == Some(primary) { &self.track_sel.tracks[..] } else { &[] };
        self.project.tracks.iter().map(|t| t.id).filter(|id| *id == primary || list.contains(id)).collect()
    }

    pub fn is_track_selected(&self, id: TrackId) -> bool {
        self.sel.track == Some(id)
            || (self.sel.track.is_some()
                && self.track_sel.for_track == self.sel.track
                && self.track_sel.tracks.contains(&id))
    }

    /// Selects `tracks`; `primary` (or the first) becomes the selected track.
    pub fn set_track_selection(&mut self, tracks: Vec<TrackId>, primary: Option<TrackId>) {
        let primary = primary.filter(|p| tracks.contains(p)).or(tracks.first().copied());
        self.sel = Selection { track: primary, item: None };
        self.track_sel = TrackSel { for_track: primary, anchor: primary, tracks };
    }

    /// A click on a track header or mixer strip: selects the track alone, Ctrl toggles it,
    /// Shift selects the tracks from the last clicked one to it.
    pub fn click_track(&mut self, id: TrackId, mods: egui::Modifiers) {
        if mods.shift {
            let anchor =
                self.track_sel.anchor.filter(|_| self.track_sel.for_track == self.sel.track).or(self.sel.track);
            let index = |t: TrackId| self.project.track_index(t);
            let (Some(a), Some(b)) = (anchor.and_then(index), index(id)) else {
                self.set_track_selection(vec![id], Some(id));
                return;
            };
            let range: Vec<TrackId> = self.project.tracks[a.min(b)..=a.max(b)].iter().map(|t| t.id).collect();
            self.set_track_selection(range, Some(id));
            self.track_sel.anchor = anchor;
        } else if mods.command {
            let mut tracks = self.selected_tracks();
            match tracks.iter().position(|t| *t == id) {
                Some(k) => {
                    tracks.remove(k);
                    let primary = tracks.last().copied();
                    self.set_track_selection(tracks, primary);
                }
                None => {
                    tracks.push(id);
                    self.set_track_selection(tracks, Some(id));
                }
            }
        } else {
            self.set_track_selection(vec![id], Some(id));
        }
    }

    /// The other selected tracks a switch or fader on `id` carries over to, as REAPER does
    /// with a selection: none when `id` is not among them.
    pub fn selection_peers(&self, id: TrackId) -> Vec<usize> {
        if !self.is_track_selected(id) {
            return Vec::new();
        }
        self.selected_tracks().into_iter().filter(|t| *t != id).filter_map(|t| self.project.track_index(t)).collect()
    }
}

/// A track drag in progress: the tracks it carries and where they would land.
#[derive(Debug, Clone)]
pub struct TrackDrag {
    pub tracks: Vec<TrackId>,
    /// The track indices that move (folders with their children).
    pub moving: Vec<usize>,
    pub drop: Option<Drop>,
    /// Which view started it, so the arrange view and the mixer leave each other's drags alone.
    pub mixer: bool,
}

fn drag_id() -> Id {
    Id::unique("scythe-track-drag")
}

pub fn drag(ui: &Ui) -> Option<TrackDrag> {
    ui.data(|d| d.get_temp(drag_id()))
}

pub fn set_drag(ui: &Ui, drag: Option<TrackDrag>) {
    ui.data_mut(|d| match drag {
        Some(v) => _ = d.insert_temp(drag_id(), v),
        None => d.remove::<TrackDrag>(drag_id()),
    });
}

/// Starts dragging track `id`: an unselected track is selected alone first, a selected one
/// takes the whole selection along.
pub fn start_drag(app: &mut ScytheApp, ui: &Ui, id: TrackId, mixer: bool) {
    if !app.is_track_selected(id) {
        app.set_track_selection(vec![id], Some(id));
    }
    let tracks = app.selected_tracks();
    let moving = app.project.with_children(&tracks);
    set_drag(ui, Some(TrackDrag { tracks, moving, drop: None, mixer }));
}

/// Drops the dragged tracks where the indicator shows, as one undo step.
pub fn finish_drag(app: &mut ScytheApp, ui: &Ui) {
    let Some(d) = drag(ui) else { return };
    set_drag(ui, None);
    if let Some(drop) = d.drop {
        app.edit("Change track order", |p| {
            p.move_tracks(&d.tracks, drop.before, drop.level);
        });
    }
}

/// The drop indicator: a line at the drop's nesting level, with a dot where it starts.
pub fn paint_drop_line(painter: &egui::Painter, from: Pos2, to: Pos2) {
    painter.line_segment([from, to], Stroke::new(3.0, ACCENT));
    painter.circle_filled(from, 4.5, ACCENT);
}

/// The folder state of a track the way REAPER's track panel button cycles it.
fn folder_state(t: &Track, level: usize) -> (&'static str, Color32) {
    match t.folder_depth {
        d if d > 0 => ("Folder parent: click to make it the last track in its folder, or a normal track", ACCENT),
        d if d < 0 && level > 0 => ("Last track in its folder: click for a normal track", TEXT_MID),
        _ => ("Normal track: click to make it a folder holding the tracks below", TEXT_DIM),
    }
}

/// REAPER's folder button: normal, folder parent, last in folder ("Track: Cycle track folder
/// state").
pub fn folder_button(app: &mut ScytheApp, ui: &mut Ui, rect: Rect, id: TrackId, level: usize) {
    let Some(t) = app.project.track(id) else { return };
    let (tip, color) = folder_state(t, level);
    let depth = t.folder_depth;
    let resp = ui.interact(rect, Id::unique(("folder-state", id)), egui::Sense::click()).on_hover_text(tip);
    let bg = if resp.hovered() { theme::RAISED_HI } else { Color32::TRANSPARENT };
    ui.painter().rect_filled(rect, CornerRadius::same(255), bg);
    let r = Rect::from_center_size(rect.center(), vec2(14.0, 11.0));
    let tab = [r.left_top(), pos2(r.left() + 5.0, r.top()), pos2(r.left() + 6.5, r.top() + 2.0)];
    let outline = vec![tab[0], tab[1], tab[2], pos2(r.right(), r.top() + 2.0), r.right_bottom(), r.left_bottom()];
    if depth > 0 {
        ui.painter().add(Shape::convex_polygon(outline, color, Stroke::NONE));
    } else {
        ui.painter().add(Shape::closed_line(outline, Stroke::new(1.3, color)));
    }
    if depth < 0 && level > 0 {
        let y = r.bottom() + 2.5;
        ui.painter().line_segment([pos2(r.left(), y), pos2(r.right(), y)], Stroke::new(1.5, color));
    }
    if resp.clicked() {
        app.edit("Toggle track folder", |p| p.cycle_folder_state(id));
    }
}

/// The collapse switch of a folder: "Track: Cycle folder collapsed state" (normal, small,
/// collapsed), not an undo step as in REAPER.
pub fn collapse_button(app: &mut ScytheApp, ui: &mut Ui, rect: Rect, id: TrackId, compact: u8) {
    let tip = match compact {
        0 => "Folder open: click to show its tracks small",
        1 => "Folder tracks small: click to collapse the folder",
        _ => "Folder collapsed: click to open it",
    };
    let resp = ui.interact(rect, Id::unique(("folder-collapse", id)), egui::Sense::click()).on_hover_text(tip);
    let color = if resp.hovered() { theme::TEXT } else { TEXT_MID };
    chevron(ui.painter(), rect.center(), compact, color);
    if resp.clicked() {
        app.change_view(|p| p.cycle_folder_compact(id));
    }
}

/// A triangle pointing down (open), down and flattened (small) or right (collapsed).
pub fn chevron(painter: &egui::Painter, c: Pos2, compact: u8, color: Color32) {
    let pts = match compact {
        0 => vec![c + vec2(-4.5, -2.5), c + vec2(4.5, -2.5), c + vec2(0.0, 3.5)],
        1 => vec![c + vec2(-4.5, -1.5), c + vec2(4.5, -1.5), c + vec2(0.0, 2.0)],
        _ => vec![c + vec2(-2.5, -4.5), c + vec2(3.5, 0.0), c + vec2(-2.5, 4.5)],
    };
    painter.add(Shape::convex_polygon(pts, color, Stroke::NONE));
}
