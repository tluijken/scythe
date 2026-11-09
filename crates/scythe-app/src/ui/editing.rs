//! The editing tools of the arrange view: the ripple, grid, auto-crossfade, locking and
//! grouping switches above the track headers, REAPER's editing actions on the selection
//! (`scythe_core::edit::actions`), and the crossfade editor, dynamic split, nudge/set, grid
//! and lock settings windows.

use std::collections::HashMap;

use egui::{Align2, CornerRadius, Id, Rect, Sense, Stroke, Ui, pos2, vec2};
use scythe_core::edit::actions::{self as edit_actions, EditContext};
use scythe_core::edit::nudge::{Nudge, NudgeUnit, NudgeWhat};
use scythe_core::edit::ops::RazorArea;
use scythe_core::edit::transients::TransientSettings;
use scythe_core::edit::{GridKind, LOCK_ENABLED, LOCK_MODES, RippleMode};
use scythe_core::{Fade, ItemId, Project};
use scythe_session::api::editing::item_transients;

use crate::app::{ScytheApp, Selection};
use crate::theme::{self, ACCENT, RAISED, RAISED_HI, TEXT, TEXT_DIM, TEXT_MID};
use crate::ui::widgets;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditWindow {
    Crossfade,
    DynamicSplit,
    Nudge,
    Grid,
    Lock,
}

/// The editing windows' state and the snap targets besides the grid.
#[derive(Debug)]
pub struct EditState {
    pub window: Option<EditWindow>,
    /// The last nudge/set settings, which the "by last nudge dialog settings" actions use.
    pub nudge: Nudge,
    pub transients: TransientSettings,
    /// Shortest piece dynamic split leaves, in seconds.
    pub min_slice: f64,
    /// Transients dynamic split would cut at, per selected item, for drawing.
    pub preview: Vec<(ItemId, Vec<f64>)>,
    /// Snapping also catches item edges and markers within a few pixels.
    pub snap_items: bool,
    pub snap_markers: bool,
}

impl Default for EditState {
    fn default() -> Self {
        Self {
            window: None,
            nudge: Nudge::default(),
            transients: TransientSettings::default(),
            min_slice: 0.01,
            preview: Vec::new(),
            snap_items: true,
            snap_markers: true,
        }
    }
}

/// The window an editing action opens, if it is one of those.
fn window_of(id: u32) -> Option<EditWindow> {
    Some(match id {
        41827 => EditWindow::Crossfade,
        40760 => EditWindow::DynamicSplit,
        41226 => EditWindow::Nudge,
        40071 => EditWindow::Grid,
        40277 => EditWindow::Lock,
        _ => return None,
    })
}

/// The editing actions the window runs, by REAPER command id.
pub fn window_ids() -> Vec<u32> {
    let mut ids: Vec<u32> = edit_actions::SESSION_IDS.iter().chain(edit_actions::SELECTION_IDS).copied().collect();
    ids.extend([41827, 40760, 41226, 40071, 40277]);
    ids.retain(|id| *id != 1157);
    ids
}

/// The razor areas as the editing actions take them.
pub fn razor_areas(app: &ScytheApp) -> Vec<RazorArea> {
    let items = app.razor.iter().map(|&(track, range)| RazorArea { track, range, envelope: None });
    let envs = app.razor_envelopes.iter().map(|&(track, kind, range)| RazorArea { track, range, envelope: Some(kind) });
    items.chain(envs).collect()
}

fn set_razor(app: &mut ScytheApp, areas: Vec<RazorArea>) {
    app.razor = areas.iter().filter(|a| a.envelope.is_none()).map(|a| (a.track, a.range)).collect();
    app.razor_envelopes = areas.iter().filter_map(|a| a.envelope.map(|k| (a.track, k, a.range))).collect();
}

/// Selects `items`, the first as the primary selection.
pub fn select(app: &mut ScytheApp, items: &[ItemId]) {
    let first = items.first().copied();
    let track = first.and_then(|i| app.project.find_item(i)).map(|(t, _)| t);
    app.sel = Selection { track: track.or(app.sel.track), item: first };
    app.multi = items.iter().skip(1).copied().collect();
}

/// Runs editing action `id` on the selection.
pub fn run(app: &mut ScytheApp, id: u32) {
    if let Some(w) = window_of(id) {
        app.view.editing.window = Some(w);
        if w == EditWindow::DynamicSplit {
            preview(app);
        }
        return;
    }
    if edit_actions::SESSION_IDS.contains(&id) {
        if let Err(e) = scythe_session::actions::run(&mut app.session, &id.to_string()) {
            app.notify(e.to_string());
        }
        return;
    }
    let items = app.selected_items();
    let settings = app.view.editing.transients;
    let transients: HashMap<ItemId, Vec<f64>> = if matches!(id, 40375 | 40376) {
        items.iter().map(|&i| (i, item_transients(&app.session, i, settings))).collect()
    } else {
        HashMap::new()
    };
    let lookup = |_: &Project, i: ItemId| transients.get(&i).cloned().unwrap_or_default();
    let mut ctx = EditContext::new(items.clone(), app.project.cursor, &lookup);
    ctx.pixel = 1.0 / app.view.px_per_sec.max(1e-3);
    ctx.razor = razor_areas(app);
    ctx.nudge = app.view.editing.nudge;
    let cursor = ctx.cursor;
    if !edit_actions::run_view(&app.project, &mut ctx, id) {
        let label = app.session.action_name(&id.to_string()).unwrap_or_default();
        app.edit(&label, |p| {
            edit_actions::run(p, &mut ctx, id);
        });
    }
    let (new_items, new_cursor, razor) = (ctx.items, ctx.cursor, ctx.razor);
    if new_items != items {
        select(app, &new_items);
    }
    if (new_cursor - cursor).abs() > 1e-12 {
        app.set_cursor(new_cursor);
    }
    set_razor(app, razor);
}

fn preview(app: &mut ScytheApp) {
    let settings = app.view.editing.transients;
    app.view.editing.preview =
        app.selected_items().into_iter().map(|i| (i, item_transients(&app.session, i, settings))).collect();
}

/// The switches above the track headers: ripple mode, grid and snap, auto-crossfade, locking
/// and item grouping.
pub fn switches(app: &mut ScytheApp, ui: &mut Ui, rect: Rect) {
    let e = app.project.editing;
    let h = 20.0;
    let y = rect.center().y - h / 2.0;
    let mut x = rect.left() + 6.0;
    let font = theme::semibold(11.0);
    let mut chip = |ui: &mut Ui, text: &str, on: bool, tip: &str, salt: &str| {
        let w = widgets::text_width(ui, text, font.clone()) + 16.0;
        let r = Rect::from_min_size(pos2(x, y), vec2(w, h));
        x += w + 4.0;
        let (bg, fg) = if on { (RAISED_HI, ACCENT) } else { (RAISED, TEXT_DIM) };
        let resp = ui.interact(r, Id::unique(("edit-switch", salt)), Sense::click()).on_hover_text(tip);
        let bg = if resp.hovered() { widgets::lighten(bg, 0.08) } else { bg };
        ui.painter().rect_filled(r, CornerRadius::same(255), bg);
        ui.painter().text(r.center(), Align2::CENTER_CENTER, text, font.clone(), fg);
        resp
    };
    let ripple = match e.ripple {
        RippleMode::Off => "Ripple",
        RippleMode::PerTrack => "Ripple: track",
        RippleMode::AllTracks => "Ripple: all",
    };
    if chip(ui, ripple, e.ripple != RippleMode::Off, "Cycle ripple editing: off, per-track, all tracks", "ripple")
        .clicked()
    {
        run(app, 1155);
    }
    let grid = e.grid.label();
    let resp = chip(ui, &grid, e.grid.snap, "Snap to grid (click), grid settings (right-click)", "grid");
    if resp.clicked() {
        run(app, 1157);
    }
    if resp.secondary_clicked() {
        app.view.editing.window = Some(EditWindow::Grid);
    }
    if chip(ui, "X-fade", e.auto_crossfade, "Auto-crossfade items that come to overlap", "xfade").clicked() {
        run(app, 40041);
    }
    let resp = chip(ui, "Lock", e.lock & LOCK_ENABLED != 0, "Locking (click), lock settings (right-click)", "lock");
    if resp.clicked() {
        run(app, 1135);
    }
    if resp.secondary_clicked() {
        app.view.editing.window = Some(EditWindow::Lock);
    }
    if chip(ui, "Groups", e.grouping, "Item grouping: grouped items move, split and select together", "group").clicked()
    {
        run(app, 1156);
    }
}

/// Snaps `t` to the grid and, when enabled, to item edges and markers within 8 pixels.
pub fn snap(app: &ScytheApp, t: f64, bypass: bool) -> f64 {
    let p = &app.project;
    if bypass || !p.editing.grid.snap {
        return t;
    }
    let mut best = p.editing.grid.nearest_line(&p.tempo, t);
    let reach = 8.0 / app.view.px_per_sec.max(1e-3);
    let mut consider = |c: f64| {
        if (c - t).abs() <= reach && (c - t).abs() < (best - t).abs() {
            best = c;
        }
    };
    if app.view.editing.snap_markers {
        for m in &p.markers {
            consider(m.position);
            if let Some(end) = m.region_end {
                consider(end);
            }
        }
    }
    if app.view.editing.snap_items {
        let dragged = app.selected_items();
        for it in p.tracks.iter().flat_map(|t| &t.items).filter(|it| !dragged.contains(&it.id)) {
            consider(it.position);
            consider(it.end());
        }
    }
    best
}

/// Lock and group badges drawn on an item.
pub fn item_badges(painter: &egui::Painter, rect: Rect, item: &scythe_core::Item) {
    let mut x = rect.right() - 6.0;
    let y = rect.top() + 9.0;
    if item.locked {
        let body = Rect::from_center_size(pos2(x - 6.0, y + 2.0), vec2(9.0, 7.0));
        painter.rect_filled(body, CornerRadius::same(2), TEXT);
        let shackle = Rect::from_center_size(pos2(x - 6.0, y - 2.0), vec2(6.0, 6.0));
        painter.rect_stroke(shackle, CornerRadius::same(3), Stroke::new(1.5, TEXT), egui::StrokeKind::Inside);
        x -= 16.0;
    }
    if item.group > 0 {
        let text = format!("G{}", item.group);
        painter.text(pos2(x, y), Align2::RIGHT_CENTER, text, theme::mono(10.0), TEXT_MID);
    }
}

/// The editing windows.
pub fn show(app: &mut ScytheApp, ui: &mut Ui) {
    let Some(w) = app.view.editing.window else { return };
    let ctx = ui.ctx().clone();
    let mut open = true;
    let frame = egui::Frame::window(ui.style()).fill(theme::PANEL).corner_radius(CornerRadius::same(18));
    let title = match w {
        EditWindow::Crossfade => "Crossfade editor",
        EditWindow::DynamicSplit => "Dynamic split",
        EditWindow::Nudge => "Nudge/set items",
        EditWindow::Grid => "Snap/grid settings",
        EditWindow::Lock => "Lock settings",
    };
    egui::Window::new(title).id(Id::unique(("edit-window", title))).open(&mut open).frame(frame).resizable(false).show(
        &ctx,
        |ui| match w {
            EditWindow::Crossfade => crossfade_editor(app, ui),
            EditWindow::DynamicSplit => dynamic_split(app, ui),
            EditWindow::Nudge => nudge_dialog(app, ui),
            EditWindow::Grid => grid_settings(app, ui),
            EditWindow::Lock => lock_settings(app, ui),
        },
    );
    if !open {
        app.view.editing.window = None;
        app.view.editing.preview.clear();
    }
}

const FADE_TYPES: [&str; 7] = [
    "1 Linear",
    "2 Slight convex",
    "3 Slight concave",
    "4 Sharp convex",
    "5 Sharp concave",
    "6 Slight S-curve",
    "7 Sharp S-curve",
];
const SHAPES: [&str; 8] = [
    "Linear",
    "Fast start",
    "Fast end",
    "Fast start, steep",
    "Fast end, steep",
    "Slow start/end",
    "Slow start/end, steep",
    "Equal power",
];

/// The fade curve of `fade` in `rect`, as a fade-in or fade-out.
fn plot(ui: &Ui, rect: Rect, fade: &Fade, out: bool, color: egui::Color32) {
    let pts: Vec<egui::Pos2> = (0..=64)
        .map(|i| {
            let x = i as f64 / 64.0;
            let g = if out { fade.out_gain(1.0 - x) } else { fade.gain(x) };
            pos2(rect.left() + x as f32 * rect.width(), rect.bottom() - g as f32 * rect.height())
        })
        .collect();
    ui.painter().add(egui::Shape::line(pts, Stroke::new(2.0, color)));
}

/// Shape, curvature and length of one fade; true when changed.
fn fade_controls(ui: &mut Ui, fade: &mut Fade, out: bool, salt: &str) -> bool {
    let mut changed = false;
    let mut shape = fade.api_shape() as usize;
    egui::ComboBox::from_id_salt(("fade-shape", salt)).selected_text(SHAPES[shape.min(7)]).width(170.0).show_ui(
        ui,
        |ui| {
            for (i, name) in SHAPES.iter().enumerate() {
                changed |= ui.selectable_value(&mut shape, i, *name).changed();
            }
        },
    );
    if changed {
        fade.set_api_shape(shape as u8);
    }
    changed |= ui.add(egui::Slider::new(&mut fade.curve, -1.0..=1.0).text("curve")).changed();
    let mut kind = fade.kind(out);
    egui::ComboBox::from_id_salt(("fade-type", salt))
        .selected_text(kind.map_or("Custom", |k| FADE_TYPES[k]))
        .width(170.0)
        .show_ui(ui, |ui| {
            for (i, name) in FADE_TYPES.iter().enumerate() {
                if ui.selectable_value(&mut kind, Some(i), *name).changed() {
                    fade.set_type(i, out, false);
                    changed = true;
                }
            }
        });
    changed
}

fn crossfade_editor(app: &mut ScytheApp, ui: &mut Ui) {
    let items = app.selected_items();
    let Some(&first) = items.first() else {
        ui.label("Select an item, or two crossfading items.");
        return;
    };
    // A crossfade between two selected items when there is one, else the first item's fades.
    let pair = app.project.tracks.iter().find_map(|t| {
        let mut sel: Vec<_> = t.items.iter().filter(|it| items.contains(&it.id)).collect();
        sel.sort_by(|a, b| a.position.total_cmp(&b.position));
        sel.windows(2).find(|w| w[1].position < w[0].end()).map(|w| (w[0].clone(), w[1].clone()))
    });
    let (left, right) = match pair {
        Some((a, b)) => (Some(a), Some(b)),
        None => (app.project.find_item(first).map(|(_, it)| it.clone()), None),
    };
    let Some(mut a) = left else { return };
    let size = vec2(320.0, 140.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(10), RAISED);
    let inner = rect.shrink(10.0);
    match &right {
        Some(b) => {
            plot(ui, inner, &a.fade_out, true, theme::PINK);
            plot(ui, inner, &b.fade_in, false, ACCENT);
            let len = a.fade_out.effective_length();
            ui.painter().text(
                inner.center_top() + vec2(0.0, 6.0),
                Align2::CENTER_TOP,
                format!("crossfade {:.3} s", len),
                theme::mono(11.0),
                TEXT_DIM,
            );
        }
        None => {
            let half = Rect::from_min_size(inner.min, vec2(inner.width() / 2.0 - 4.0, inner.height()));
            plot(ui, half, &a.fade_in, false, ACCENT);
            plot(ui, half.translate(vec2(inner.width() / 2.0 + 4.0, 0.0)), &a.fade_out, true, theme::PINK);
        }
    }
    let mut b = right.clone();
    let mut changed = false;
    ui.columns(2, |cols| {
        let (l, r) = cols.split_at_mut(1);
        if let Some(b) = b.as_mut() {
            l[0].label("Fade-out of the earlier item");
            changed |= fade_controls(&mut l[0], &mut a.fade_out, true, "xo");
            r[0].label("Fade-in of the later item");
            changed |= fade_controls(&mut r[0], &mut b.fade_in, false, "xi");
        } else {
            l[0].label("Fade-in");
            changed |= fade_controls(&mut l[0], &mut a.fade_in, false, "fi");
            changed |= l[0].add(egui::Slider::new(&mut a.fade_in.length, 0.0..=a.length).text("s")).changed();
            r[0].label("Fade-out");
            changed |= fade_controls(&mut r[0], &mut a.fade_out, true, "fo");
            changed |= r[0].add(egui::Slider::new(&mut a.fade_out.length, 0.0..=a.length).text("s")).changed();
        }
    });
    if b.is_some() && ui.button("Same shape both sides").clicked() {
        if let (Some(k), Some(b)) = (a.fade_out.kind(true), b.as_mut()) {
            b.fade_in.set_type(k, false, a.fade_out.power);
        }
        changed = true;
    }
    if changed && !app.project.fades_locked(&a) {
        app.gesture("Edit crossfade", |p| {
            for new in [Some(&a), b.as_ref()].into_iter().flatten() {
                if let Some(it) = p.item_mut_anywhere(new.id) {
                    it.fade_in = new.fade_in;
                    it.fade_out = new.fade_out;
                }
            }
        });
        app.end_gesture();
    }
    ui.horizontal(|ui| {
        if ui.button("Crossfade overlapping").clicked() {
            run(app, 41059);
        }
        if ui.button("Crossfade time selection").clicked() {
            run(app, 40916);
        }
    });
}

fn dynamic_split(app: &mut ScytheApp, ui: &mut Ui) {
    let mut st = app.view.editing.transients;
    let mut changed = ui.add(egui::Slider::new(&mut st.sensitivity, 0.0..=1.0).text("sensitivity")).changed();
    changed |= ui.add(egui::Slider::new(&mut st.threshold_db, -60.0..=0.0).text("threshold dB")).changed();
    let mut min = app.view.editing.min_slice * 1000.0;
    if ui.add(egui::Slider::new(&mut min, 0.0..=500.0).text("minimum slice ms")).changed() {
        app.view.editing.min_slice = min / 1000.0;
    }
    app.view.editing.transients = st;
    if changed || ui.button("Find transients").clicked() {
        preview(app);
    }
    let count: usize = app.view.editing.preview.iter().map(|(_, t)| t.len()).sum();
    ui.label(format!("{count} transients in {} items", app.view.editing.preview.len()));
    ui.horizontal(|ui| {
        if ui.button("Split at transients").clicked() {
            let (min, cuts) = (app.view.editing.min_slice, app.view.editing.preview.clone());
            let mut pieces = Vec::new();
            app.edit("Dynamic split items", |p| {
                for (item, times) in &cuts {
                    pieces.extend(edit_actions::split_at_times(p, *item, times, min));
                }
            });
            select(app, &pieces);
            app.view.editing.preview.clear();
            app.view.editing.window = None;
        }
        if ui.button("Add stretch markers").clicked() {
            let cuts = app.view.editing.preview.clone();
            app.edit("Add stretch markers at transients", |p| {
                for (item, times) in &cuts {
                    if let Some(it) = p.item_mut_anywhere(*item) {
                        let (pos, len, active) = (it.position, it.length, it.active_take);
                        if let Some(take) = it.takes.get_mut(active) {
                            for t in times.iter().map(|t| t - pos).filter(|t| *t > 0.0 && *t < len) {
                                take.add_stretch_marker(t);
                            }
                        }
                    }
                }
            });
        }
    });
}

/// Transient positions dynamic split would cut at, drawn over the item.
pub fn draw_preview(app: &ScytheApp, painter: &egui::Painter, rect: Rect, item: ItemId, x: &dyn Fn(f64) -> f32) {
    if app.view.editing.window != Some(EditWindow::DynamicSplit) {
        return;
    }
    let Some((_, times)) = app.view.editing.preview.iter().find(|(i, _)| *i == item) else { return };
    for &t in times {
        let px = x(t);
        if px >= rect.left() && px <= rect.right() {
            painter.line_segment([pos2(px, rect.top()), pos2(px, rect.bottom())], Stroke::new(1.0, theme::PINK));
        }
    }
}

fn nudge_dialog(app: &mut ScytheApp, ui: &mut Ui) {
    let mut n = app.view.editing.nudge;
    ui.horizontal(|ui| {
        ui.radio_value(&mut n.set, false, "Nudge");
        ui.radio_value(&mut n.set, true, "Set");
    });
    egui::ComboBox::from_label("what")
        .selected_text(NudgeWhat::ALL.iter().find(|w| w.0 == n.what).map_or("", |w| w.1))
        .show_ui(ui, |ui| {
            for (w, name) in NudgeWhat::ALL {
                ui.selectable_value(&mut n.what, w, name);
            }
        });
    ui.add(egui::DragValue::new(&mut n.value).speed(0.1));
    egui::ComboBox::from_label("unit").selected_text(n.unit.label()).show_ui(ui, |ui| {
        for u in 0..NudgeUnit::COUNT {
            ui.selectable_value(&mut n.unit, NudgeUnit(u), NudgeUnit(u).label());
        }
    });
    if n.what == NudgeWhat::Duplicate {
        ui.add(egui::DragValue::new(&mut n.copies).range(1..=100).prefix("copies "));
    }
    ui.checkbox(&mut n.snap, "Snap to grid");
    app.view.editing.nudge = n;
    ui.horizontal(|ui| {
        if n.set {
            if ui.button("Apply").clicked() {
                run(app, 41249);
            }
        } else {
            if ui.button("Nudge left").clicked() {
                run(app, 41250);
            }
            if ui.button("Nudge right").clicked() {
                run(app, 41249);
            }
        }
    });
}

fn grid_settings(app: &mut ScytheApp, ui: &mut Ui) {
    let mut g = app.project.editing.grid;
    const DIVISIONS: [f64; 14] = [
        1.0 / 128.0,
        1.0 / 64.0,
        1.0 / 48.0,
        1.0 / 32.0,
        1.0 / 24.0,
        1.0 / 16.0,
        1.0 / 12.0,
        1.0 / 8.0,
        1.0 / 6.0,
        1.0 / 4.0,
        1.0 / 3.0,
        1.0 / 2.0,
        1.0,
        4.0,
    ];
    ui.checkbox(&mut g.snap, "Snap");
    ui.checkbox(&mut g.lines, "Show grid lines");
    egui::ComboBox::from_label("grid").selected_text(g.label()).show_ui(ui, |ui| {
        for d in DIVISIONS {
            let label = scythe_core::edit::Grid { qn: d * 4.0, kind: GridKind::Normal, ..g }.label();
            if ui.selectable_label((g.qn - d * 4.0).abs() < 1e-9, label).clicked() {
                g.qn = d * 4.0;
            }
        }
    });
    ui.horizontal(|ui| {
        ui.radio_value(&mut g.kind, GridKind::Normal, "Straight");
        ui.radio_value(&mut g.kind, GridKind::Swing, "Swing");
        ui.radio_value(&mut g.kind, GridKind::Measure, "Measures");
    });
    if g.kind == GridKind::Swing {
        ui.add(egui::Slider::new(&mut g.swing, -1.0..=1.0).text("swing"));
    }
    ui.separator();
    ui.label("Also snap to");
    ui.checkbox(&mut app.view.editing.snap_items, "Item edges");
    ui.checkbox(&mut app.view.editing.snap_markers, "Markers and regions");
    if g != app.project.editing.grid {
        app.gesture("Snap/grid settings", |p| p.editing.grid = g);
        app.end_gesture();
    }
}

fn lock_settings(app: &mut ScytheApp, ui: &mut Ui) {
    let mut lock = app.project.editing.lock;
    let mut on = lock & LOCK_ENABLED != 0;
    ui.checkbox(&mut on, "Locking enabled");
    lock = if on { lock | LOCK_ENABLED } else { lock & !LOCK_ENABLED };
    ui.separator();
    for (bit, name, _) in LOCK_MODES {
        let mut set = lock & bit != 0;
        if ui.checkbox(&mut set, name).changed() {
            lock ^= bit;
        }
    }
    if lock != app.project.editing.lock {
        app.edit("Lock settings", |p| p.editing.lock = lock);
    }
    ui.separator();
    ui.horizontal(|ui| {
        if ui.button("Lock selected items").clicked() {
            run(app, 40688);
        }
        if ui.button("Unlock").clicked() {
            run(app, 40689);
        }
    });
}
