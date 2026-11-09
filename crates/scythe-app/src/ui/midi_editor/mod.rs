//! The docked MIDI editor for the active take of one item, with other MIDI items shown behind
//! it. Its settings (mode, grid, swing, key snap, channel filter, controller lanes) live in the
//! take as REAPER keeps them (`CFGEDIT`, `KEYSNAP`, `EVTFILTER`, `VELLANE`), so they travel
//! with the project.
//!
//! Modes: piano roll, named notes (a drum map: the track's note names, one row each) and the
//! event list. Below the roll sit the controller lanes.

mod inline;
mod lanes;
mod list;
mod roll;

use egui::{Align2, CornerRadius, Rect, Ui, pos2, vec2};
use scythe_core::ItemId;
use scythe_core::midi::edit::{self, EditNote, Grid, Humanize, Quantize, QuantizeWhat};
use scythe_core::midi::groove::{self, Groove};
use scythe_core::midi::names::NoteNames;
use scythe_core::midi::scale::{self, KeySnap};
use scythe_core::midi::view::{EditorMode, EditorView, LaneKind};
use scythe_core::midi::{MidiSource, ShapeKind, actions};

pub use inline::{inline_editor, toggle_inline};

use crate::app::ScytheApp;
use crate::theme::{self, PANEL, RAISED, RAISED_HI, TEXT, TEXT_DIM};
use crate::ui::widgets::text_pill;

/// Grid choices: label and quarter notes.
const GRIDS: [(&str, f64); 7] = [
    ("1/4", 1.0),
    ("1/8", 0.5),
    ("1/16", 0.25),
    ("1/32", 0.125),
    ("1/64", 0.0625),
    ("1/8T", 1.0 / 3.0),
    ("1/16T", 1.0 / 6.0),
];

const ROOTS: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tool {
    /// Click adds, drag moves and paints.
    #[default]
    Draw,
    /// Drag draws a controller line or curve (two events joined by a CC shape).
    Line,
}

#[derive(Debug, Clone)]
pub struct EditorState {
    /// Highest visible key.
    pub top_key: Option<u8>,
    /// Pixels per quarter note; `None` fits the item.
    pub zoom: Option<f32>,
    /// Quarter notes from the source start at the left edge.
    pub scroll_qn: f64,
    pub tool: Tool,
    /// The shape the line tool joins its two events with.
    pub shape: ShapeKind,
    /// Key snap root (0 is C) and scale (an index into the scales).
    pub key_root: u8,
    pub scale: usize,
    /// Step input: notes played go in at `step_at` (ticks), which then moves on by the grid.
    pub step: bool,
    pub step_at: u64,
    /// Quantize dialog: strength and window in percent, what moves.
    pub q_strength: f64,
    pub q_what: QuantizeWhat,
    pub q_window: (f64, f64),
    /// Humanize dialog: timing (percent of the grid) and velocity (percent).
    pub h_timing: f64,
    pub h_velocity: f64,
    pub groove_strength: f64,
    /// Other MIDI items shown behind the active one.
    pub others: Vec<ItemId>,
    /// The pointer is over the editor: its keyboard shortcuts (REAPER's MIDI editor section)
    /// come before the main window's.
    pub hovered: bool,
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            top_key: None,
            zoom: None,
            scroll_qn: 0.0,
            tool: Tool::Draw,
            shape: ShapeKind::Linear,
            key_root: 0,
            scale: 0,
            step: false,
            step_at: 0,
            q_strength: 100.0,
            q_what: QuantizeWhat::Position,
            q_window: (0.0, 100.0),
            h_timing: 10.0,
            h_velocity: 10.0,
            groove_strength: 100.0,
            others: Vec::new(),
            hovered: false,
        }
    }
}

impl EditorState {
    pub fn key_snap(&self) -> KeySnap {
        let scales = scale::builtin();
        KeySnap { root: self.key_root % 12, scale: scales[self.scale.min(scales.len() - 1)].clone() }
    }
}

/// What the editor works on this frame: the active take's source, settings and notes.
pub(crate) struct Take {
    pub id: ItemId,
    pub name: String,
    pub src: MidiSource,
    pub view: EditorView,
    pub notes: Vec<EditNote>,
    pub names: NoteNames,
    /// Where the source starts, in project quarter notes.
    pub origin_qn: f64,
}

pub(crate) fn load(app: &ScytheApp, id: ItemId) -> Option<Take> {
    let chunk = app.project.midi_chunk(id)?;
    let src = MidiSource::parse(chunk)?;
    let (track, item) = app.project.find_item(id)?;
    let names = app.project.track(track).map(|t| t.note_names()).unwrap_or_default();
    Some(Take {
        id,
        name: item.name().to_owned(),
        view: EditorView::read(chunk),
        notes: src.edit_notes(),
        src,
        names,
        origin_qn: origin_qn(app, id),
    })
}

/// Where an item's source starts, in project quarter notes.
pub(crate) fn origin_qn(app: &ScytheApp, id: ItemId) -> f64 {
    let Some((_, item)) = app.project.find_item(id) else { return 0.0 };
    let tl = app.project.tempo.timeline();
    let offset = item.take().map_or(0.0, |t| t.source_offset * tl.bpm_at(item.position) / 60.0);
    tl.qn_at(item.position) - offset
}

/// Replaces the take's notes, as one undo step (or a step of the running gesture).
pub(crate) fn apply_notes(app: &mut ScytheApp, id: ItemId, notes: &[EditNote], label: &str, gesture: bool) {
    let notes = notes.to_vec();
    let f = move |p: &mut scythe_core::Project| {
        p.edit_midi(id, |s| s.with_edit_notes(&notes));
    };
    if gesture { app.gesture(label, f) } else { app.edit(label, f) }
}

pub(crate) fn apply_source(app: &mut ScytheApp, id: ItemId, src: MidiSource, label: &str, gesture: bool) {
    let f = move |p: &mut scythe_core::Project| {
        p.edit_midi(id, |_| src.clone());
    };
    if gesture { app.gesture(label, f) } else { app.edit(label, f) }
}

pub(crate) fn apply_view(app: &mut ScytheApp, id: ItemId, view: &EditorView) {
    let view = view.clone();
    app.edit("MIDI editor settings", |p| {
        p.edit_midi_chunk(id, |c| view.write(c));
    });
}

pub(crate) fn is_midi(app: &ScytheApp, id: ItemId) -> bool {
    app.project.midi_chunk(id).is_some()
}

pub fn show(app: &mut ScytheApp, ui: &mut Ui, rect: Rect, id: ItemId) {
    let painter = ui.painter().clone();
    painter.rect_filled(rect, CornerRadius::same(18), PANEL);
    let Some(mut take) = load(app, id) else {
        app.view.midi_editor = None;
        return;
    };
    let mut state = app.view.midi_state.clone();
    state.others.retain(|o| *o != id && is_midi(app, *o));
    for o in app.selected_items() {
        if o != id && is_midi(app, o) && !state.others.contains(&o) {
            state.others.push(o);
        }
    }

    // Escape closes an open menu first; only with none open does it close the editor.
    let menu_open = egui::Popup::is_any_open(ui.ctx()) || ui.ctx().egui_wants_keyboard_input();
    let row1 = Rect::from_min_size(rect.min + vec2(16.0, 8.0), vec2(rect.width() - 32.0, 30.0));
    let row2 = row1.translate(vec2(0.0, 36.0));
    if header(app, ui, row1, &mut take, &mut state) {
        app.view.midi_editor = None;
        return;
    }
    let row2 = toolbar(app, ui, row2, &mut take, &mut state);
    step_input(app, &mut take, &mut state);

    let body = Rect::from_min_max(
        pos2(rect.left() + 12.0, row2.bottom() + 8.0),
        pos2(rect.right() - 12.0, rect.bottom() - 12.0),
    );
    match take.view.mode {
        EditorMode::EventList => list::show(app, ui, body, &take),
        _ => {
            let lane_h = 64.0;
            let lanes_h = (take.view.lanes.len() as f32 * (lane_h + 4.0)).min(body.height() * 0.5);
            let roll_rect = Rect::from_min_max(body.min, pos2(body.right(), body.bottom() - lanes_h - 4.0));
            let axis = roll::show(app, ui, roll_rect, &take, &mut state);
            let lanes_rect = Rect::from_min_max(pos2(body.left(), roll_rect.bottom() + 4.0), body.max);
            lanes::show(app, ui, lanes_rect, &take, &mut state, &axis);
        }
    }

    state.hovered = ui.rect_contains_pointer(rect);
    if state.hovered {
        shortcuts(app, ui, &take, &state);
    }
    if !menu_open && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.view.midi_editor = None;
    }
    app.session.set_step_input(state.step && app.view.midi_editor.is_some());
    app.view.midi_state = state;
}

/// Title, mode, grid and swing, close. Returns whether the editor closed.
fn header(app: &mut ScytheApp, ui: &mut Ui, row: Rect, take: &mut Take, _state: &mut EditorState) -> bool {
    let painter = ui.painter().clone();
    let title = if take.name.is_empty() { "MIDI".to_owned() } else { format!("MIDI · {}", take.name) };
    painter.text(row.left_center(), Align2::LEFT_CENTER, title, theme::semibold(15.0), TEXT);
    let mut x = row.right() - 32.0;
    let close = Rect::from_min_size(pos2(x, row.top()), vec2(32.0, 30.0));
    if text_pill(ui, close, "×", theme::semibold(15.0), RAISED, TEXT_DIM).on_hover_text("Close (Esc)").clicked() {
        return true;
    }
    let mut view = take.view.clone();
    // Swing: a toggle and its strength.
    x -= 116.0;
    let sw = Rect::from_min_size(pos2(x, row.top()), vec2(110.0, 30.0));
    let label = match view.swing {
        Some(s) => format!("Swing {:.0}%", s * 100.0),
        None => "Swing off".into(),
    };
    let resp =
        text_pill(ui, sw, &label, theme::regular(12.0), if view.swing.is_some() { RAISED_HI } else { RAISED }, TEXT)
            .on_hover_text("Swing grid: click toggles, scroll sets the strength");
    if resp.clicked() {
        view.swing = if view.swing.is_some() { None } else { Some(0.5) };
    }
    if resp.hovered()
        && let Some(s) = view.swing
    {
        let dy = ui.input(|i| i.smooth_scroll_delta.y);
        if dy.abs() > 0.5 {
            view.swing = Some((s + dy.signum() as f64 * 0.05).clamp(-1.0, 1.0));
        }
    }
    for (label, qn) in GRIDS.iter().rev() {
        x -= 50.0;
        let r = Rect::from_min_size(pos2(x, row.top()), vec2(46.0, 30.0));
        let on = (view.grid_qn - qn).abs() < 1e-9;
        if text_pill(
            ui,
            r,
            label,
            theme::mono(11.0),
            if on { RAISED_HI } else { RAISED },
            if on { TEXT } else { TEXT_DIM },
        )
        .clicked()
        {
            view.grid_qn = *qn;
        }
    }
    x -= 12.0;
    for (label, mode, tip) in [
        ("List", EditorMode::EventList, "Event list"),
        ("Drums", EditorMode::NamedNotes, "Named notes (drum map): the track's note names"),
        ("Roll", EditorMode::PianoRoll, "Piano roll"),
    ] {
        x -= 58.0;
        let r = Rect::from_min_size(pos2(x, row.top()), vec2(54.0, 30.0));
        let on = view.mode == mode;
        if text_pill(
            ui,
            r,
            label,
            theme::regular(12.0),
            if on { RAISED_HI } else { RAISED },
            if on { TEXT } else { TEXT_DIM },
        )
        .on_hover_text(tip)
        .clicked()
        {
            view.mode = mode;
        }
    }
    if view != take.view {
        apply_view(app, take.id, &view);
        take.view = view;
    }
    false
}

fn pill(ui: &mut Ui, x: &mut f32, row: Rect, w: f32, label: &str, on: bool) -> egui::Response {
    let r = Rect::from_min_size(pos2(*x, row.top()), vec2(w, 28.0));
    *x += w + 6.0;
    text_pill(ui, r, label, theme::regular(12.0), if on { RAISED_HI } else { RAISED }, if on { TEXT } else { TEXT_DIM })
}

/// A toolbar pill that starts a new row when it would run past the right edge.
fn wrapping_pill(ui: &mut Ui, x: &mut f32, row: &mut Rect, w: f32, label: &str, on: bool) -> egui::Response {
    if *x + w > row.right() && *x > row.left() {
        *x = row.left();
        *row = row.translate(vec2(0.0, 34.0));
    }
    pill(ui, x, *row, w, label, on)
}

/// The editing commands, key snap, channels, step input, tools and files, wrapped over as many
/// rows as the width needs. Returns the last row.
fn toolbar(app: &mut ScytheApp, ui: &mut Ui, mut row: Rect, take: &mut Take, state: &mut EditorState) -> Rect {
    let mut x = row.left();
    let ppq = take.src.ticks_per_qn;
    let grid = Grid { qn: take.view.grid_qn, swing: take.view.swing.unwrap_or(0.0) };
    let mut notes = take.notes.clone();
    let mut label = None;

    let q = wrapping_pill(ui, &mut x, &mut row, 78.0, "Quantize", false)
        .on_hover_text("Quantize the selected notes (all when none): click uses the settings, right-click sets them");
    if q.clicked() {
        let what = state.q_what;
        let window = (state.q_window.0 / 100.0, state.q_window.1 / 100.0);
        edit::quantize(&mut notes, ppq, &Quantize { grid, strength: state.q_strength / 100.0, what, window });
        label = Some("Quantize notes");
    }
    egui::Popup::context_menu(&q).show(|ui| {
        ui.add(egui::Slider::new(&mut state.q_strength, 0.0..=100.0).text("strength %"));
        ui.add(egui::Slider::new(&mut state.q_window.0, 0.0..=100.0).text("only from % of grid"));
        ui.add(egui::Slider::new(&mut state.q_window.1, 0.0..=100.0).text("to % of grid"));
        for (w, name) in [
            (QuantizeWhat::Position, "Position (keep lengths)"),
            (QuantizeWhat::PositionAndEnd, "Position and note end"),
            (QuantizeWhat::End, "Note end only"),
            (QuantizeWhat::Length, "Note length"),
        ] {
            ui.radio_value(&mut state.q_what, w, name);
        }
    });
    let h = wrapping_pill(ui, &mut x, &mut row, 84.0, "Humanize", false)
        .on_hover_text("Random timing and velocity changes: click applies, right-click sets the amounts");
    if h.clicked() {
        let seed = ui.input(|i| i.time.to_bits());
        let hz =
            Humanize { timing: state.h_timing / 100.0, grid_qn: grid.qn, velocity: state.h_velocity / 100.0, seed };
        edit::humanize(&mut notes, ppq, &hz);
        label = Some("Humanize notes");
    }
    egui::Popup::context_menu(&h).show(|ui| {
        ui.add(egui::Slider::new(&mut state.h_timing, 0.0..=100.0).text("timing % of grid"));
        ui.add(egui::Slider::new(&mut state.h_velocity, 0.0..=100.0).text("velocity %"));
    });
    let snap = take.view.key_snap.then(|| state.key_snap());
    for (text, semis, tip) in [
        ("−1", -1, "Down a semitone (a scale step with key snap)"),
        ("+1", 1, "Up a semitone (a scale step with key snap)"),
        ("−12", -12, "Down an octave"),
        ("+12", 12, "Up an octave"),
    ] {
        if wrapping_pill(ui, &mut x, &mut row, 40.0, text, false).on_hover_text(tip).clicked() {
            edit::transpose(&mut notes, semis, if semis.abs() == 1 { snap.as_ref() } else { None });
            label = Some("Transpose notes");
        }
    }
    if wrapping_pill(ui, &mut x, &mut row, 64.0, "Legato", false)
        .on_hover_text("End each note where the next begins")
        .clicked()
    {
        edit::legato(&mut notes);
        label = Some("Legato");
    }
    if wrapping_pill(ui, &mut x, &mut row, 50.0, "Split", false).on_hover_text("Split notes on the grid").clicked() {
        edit::split_on_grid(&mut notes, ppq, grid);
        label = Some("Split notes");
    }
    if wrapping_pill(ui, &mut x, &mut row, 46.0, "Join", false).on_hover_text("Join notes of the same key").clicked() {
        edit::join(&mut notes);
        label = Some("Join notes");
    }
    if wrapping_pill(ui, &mut x, &mut row, 50.0, "Mute", false)
        .on_hover_text("Toggle mute of the selected notes")
        .clicked()
    {
        edit::mute(&mut notes, None);
        label = Some("Mute notes");
    }
    x += 8.0;

    // Key snap: toggle, root, scale.
    let mut view = take.view.clone();
    if wrapping_pill(ui, &mut x, &mut row, 74.0, "Key snap", view.key_snap)
        .on_hover_text("Snap new and moved notes to the scale; transpose by scale steps")
        .clicked()
    {
        view.key_snap = !view.key_snap;
    }
    let scales = scale::builtin();
    let root = wrapping_pill(ui, &mut x, &mut row, 34.0, ROOTS[state.key_root as usize % 12], view.key_snap);
    egui::Popup::menu(&root).show(|ui| {
        for (i, r) in ROOTS.iter().enumerate() {
            if ui.selectable_label(state.key_root as usize == i, *r).clicked() {
                state.key_root = i as u8;
            }
        }
    });
    let sc = wrapping_pill(ui, &mut x, &mut row, 110.0, &scales[state.scale.min(scales.len() - 1)].name, view.key_snap);
    egui::Popup::menu(&sc).show(|ui| {
        for (i, s) in scales.iter().enumerate() {
            if ui.selectable_label(state.scale == i, &s.name).clicked() {
                state.scale = i;
            }
        }
        if ui.button("Force notes into key").clicked() {
            edit::force_into_key(&mut notes, &state.key_snap());
            label = Some("Force notes into key");
        }
    });

    // Channels: new events go to one, the filter shows some.
    let ch_label = if view.channel_filter == 0 {
        format!("Ch {} · all", view.channel + 1)
    } else {
        format!("Ch {} · filter", view.channel + 1)
    };
    let ch = wrapping_pill(ui, &mut x, &mut row, 96.0, &ch_label, view.channel_filter != 0)
        .on_hover_text("Channel for new notes and which channels show");
    egui::Popup::menu(&ch).show(|ui| {
        ui.label("New events on");
        ui.horizontal_wrapped(|ui| {
            for c in 0..16u8 {
                if ui.selectable_label(view.channel == c, format!("{}", c + 1)).clicked() {
                    view.channel = c;
                }
            }
        });
        ui.separator();
        ui.label("Show");
        if ui.selectable_label(view.channel_filter == 0, "All channels").clicked() {
            view.channel_filter = 0;
        }
        ui.horizontal_wrapped(|ui| {
            for c in 0..16u16 {
                let on = view.channel_filter & (1 << c) != 0;
                if ui.selectable_label(on, format!("{}", c + 1)).clicked() {
                    view.channel_filter ^= 1 << c;
                }
            }
        });
        if ui.button("Set selected notes to the channel").clicked() {
            edit::set_channel(&mut notes, view.channel);
            label = Some("Set note channel");
        }
    });
    x += 8.0;
    if wrapping_pill(ui, &mut x, &mut row, 52.0, "Step", state.step)
        .on_hover_text("Step input: notes played on a MIDI input or the virtual keyboard go in at the step cursor")
        .clicked()
    {
        state.step = !state.step;
        state.step_at = take.notes.iter().map(|n| n.end).max().unwrap_or(0);
    }
    if wrapping_pill(ui, &mut x, &mut row, 48.0, "Draw", state.tool == Tool::Draw)
        .on_hover_text("Draw notes and controller values")
        .clicked()
    {
        state.tool = Tool::Draw;
    }
    let line = wrapping_pill(
        ui,
        &mut x,
        &mut row,
        64.0,
        &format!("Line {}", shape_glyph(state.shape)),
        state.tool == Tool::Line,
    )
    .on_hover_text("Drag in a lane for a line or curve; right-click picks the shape");
    if line.clicked() {
        state.tool = Tool::Line;
    }
    egui::Popup::context_menu(&line).show(|ui| {
        for k in ShapeKind::ALL {
            ui.radio_value(&mut state.shape, k, shape_name(k));
        }
    });
    let files =
        wrapping_pill(ui, &mut x, &mut row, 60.0, "Files", false).on_hover_text("Note names and groove templates");
    egui::Popup::menu(&files).show(|ui| {
        if ui.button("Load note names…").clicked()
            && let Some(path) = rfd::FileDialog::new().add_filter("Note names", &["txt"]).pick_file()
        {
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    let names = NoteNames::parse_file(&text);
                    let track = app.project.find_item(take.id).map(|(t, _)| t);
                    app.edit("Load note names", |p| {
                        if let Some(t) = track.and_then(|t| p.track_mut(t)) {
                            t.set_note_names(&names);
                        }
                    });
                }
                Err(e) => app.notify(format!("Could not read {}: {e}", path.display())),
            }
        }
        if ui.button("Save note names…").clicked()
            && let Some(path) = rfd::FileDialog::new().add_filter("Note names", &["txt"]).save_file()
            && let Err(e) = std::fs::write(&path, take.names.to_file())
        {
            app.notify(format!("Could not write {}: {e}", path.display()));
        }
        ui.separator();
        ui.add(egui::Slider::new(&mut state.groove_strength, 0.0..=100.0).text("groove strength %"));
        if ui.button("Apply groove (.rgt)…").clicked()
            && let Some(path) = rfd::FileDialog::new().add_filter("Groove", &["rgt"]).pick_file()
        {
            match std::fs::read_to_string(&path).ok().and_then(|t| Groove::parse(&t)) {
                Some(g) => {
                    groove::apply_to_notes(&mut notes, ppq, &g, take.origin_qn, state.groove_strength / 100.0, 0.0);
                    label = Some("Apply groove");
                }
                None => app.notify(format!("{} is not a groove template", path.display())),
            }
        }
        if ui.button("Save notes as groove…").clicked()
            && let Some(path) = rfd::FileDialog::new().add_filter("Groove", &["rgt"]).save_file()
        {
            let hits: Vec<(f64, u8)> =
                take.notes.iter().map(|n| (take.origin_qn + n.start as f64 / ppq as f64, n.velocity)).collect();
            if let Some(g) = Groove::from_hits(&hits, 4.0)
                && let Err(e) = std::fs::write(&path, g.write())
            {
                app.notify(format!("Could not write {}: {e}", path.display()));
            }
        }
    });
    if view != take.view {
        apply_view(app, take.id, &view);
        take.view = view;
    }
    if let Some(label) = label {
        apply_notes(app, take.id, &notes, label, false);
        take.notes = notes;
    }
    row
}

pub(crate) fn shape_name(k: ShapeKind) -> &'static str {
    match k {
        ShapeKind::Square => "Square",
        ShapeKind::Linear => "Linear",
        ShapeKind::SlowStartEnd => "Slow start/end",
        ShapeKind::FastStart => "Fast start",
        ShapeKind::FastEnd => "Fast end",
        ShapeKind::Bezier => "Bezier",
    }
}

fn shape_glyph(k: ShapeKind) -> &'static str {
    match k {
        ShapeKind::Square => "⊓",
        ShapeKind::Linear => "/",
        ShapeKind::SlowStartEnd => "∫",
        ShapeKind::FastStart => "⌒",
        ShapeKind::FastEnd => "⌣",
        ShapeKind::Bezier => "~",
    }
}

/// Runs the keys bound in REAPER's MIDI editor section (`reaper-kb.ini` section 32060) as the
/// MIDI editor actions with those ids.
fn shortcuts(app: &mut ScytheApp, ui: &Ui, take: &Take, state: &EditorState) {
    let pressed: Vec<(egui::Key, egui::Modifiers)> = ui.input(|i| {
        i.events
            .iter()
            .filter_map(|e| match e {
                egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. } => Some((*key, *modifiers)),
                _ => None,
            })
            .collect()
    });
    for (key, mods) in pressed {
        let Some(chord) = crate::keys::chord(key, mods) else { continue };
        let Some(binding) = app.session.key_command(chord, scythe_core::keymap::SECTION_MIDI_EDITOR) else { continue };
        let Ok(id) = binding.command.parse::<u32>() else { continue };
        let ctx = actions::Context { key: Some(state.key_snap()) };
        if let Some((src, view)) = actions::run(id, &take.src, &take.view, &ctx) {
            let label = actions::name(id).unwrap_or_else(|| "MIDI editor action".into());
            app.edit(&label, |p| {
                p.edit_midi_chunk(take.id, |c| {
                    *c = src.write_chunk(c);
                    view.write(c);
                });
            });
        }
    }
}

/// Puts the notes played since the last frame in at the step cursor.
fn step_input(app: &mut ScytheApp, take: &mut Take, state: &mut EditorState) {
    if !state.step {
        return;
    }
    let played = app.session.take_step_notes();
    if played.is_empty() {
        return;
    }
    let len = (take.view.grid_qn * take.src.ticks_per_qn as f64).round().max(1.0) as u64;
    let mut notes = take.notes.clone();
    for m in played {
        let key = if take.view.key_snap { state.key_snap().nearest(m[1]) } else { m[1] };
        notes.push(EditNote::new(state.step_at, state.step_at + len, take.view.channel, key, m[2]));
    }
    // A chord played together stays at one step.
    state.step_at += len;
    notes.sort_by_key(|n| (n.start, n.key, n.channel));
    apply_notes(app, take.id, &notes, "Step input", false);
    take.notes = notes;
}

pub(crate) fn lane_menu(ui: &mut Ui, current: LaneKind) -> Option<LaneKind> {
    let mut picked = None;
    let mut item = |ui: &mut Ui, k: LaneKind| {
        if ui.selectable_label(current == k, k.label()).clicked() {
            picked = Some(k);
        }
    };
    for k in [
        LaneKind::Velocity,
        LaneKind::OffVelocity,
        LaneKind::PitchBend,
        LaneKind::Program,
        LaneKind::ChannelPressure,
        LaneKind::PolyAftertouch,
        LaneKind::Sysex,
        LaneKind::Text,
    ] {
        item(ui, k);
    }
    ui.menu_button("Controllers 0-63", |ui| {
        for n in 0..64 {
            item(ui, LaneKind::Cc(n));
        }
    });
    ui.menu_button("Controllers 64-127", |ui| {
        for n in 64..128 {
            item(ui, LaneKind::Cc(n));
        }
    });
    ui.menu_button("14-bit controllers", |ui| {
        for n in 0..32 {
            item(ui, LaneKind::Cc14(n));
        }
    });
    picked
}
