use egui::{Align2, CornerRadius, Key, KeyboardShortcut, Modifiers, Rect, Sense, vec2};

use crate::app::ScytheApp;
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    PlayPause,
    Stop,
    GoToStart,
    ToggleRepeat,
    ToggleMetronome,
    Record,
    ToggleAutoPunch,
    ToggleArm,
    AddTrack,
    SaveFxChain,
    LoadFxChain,
    SaveTrackTemplate,
    InsertTrackTemplate,
    ImportMedia,
    DeleteSelection,
    SplitItem,
    SplitAtSelection,
    RemoveSelectionContents,
    RippleCutSelection,
    InsertSpace,
    CopyItem,
    CutItem,
    PasteItem,
    DuplicateItem,
    ReverseItem,
    GlueItems,
    ApplyTrackFx,
    NextTake,
    OpenMidiEditor,
    ExportMidi,
    ToggleItemMute,
    PreviousMarker,
    NextMarker,
    DeleteMarker,
    InsertMarker,
    InsertTempoMarker,
    RemoveTempoMarker,
    ToggleMuteTrack,
    ToggleSoloTrack,
    PreviousTrack,
    NextTrack,
    MoveTrackUp,
    MoveTrackDown,
    IndentTrack,
    OutdentTrack,
    Undo,
    Redo,
    NewProject,
    OpenProject,
    SaveProject,
    SaveProjectAs,
    RenderToWav,
    RenderSelection,
    RenderStems,
    RenderProjectSettings,
    LoadDemo,
    ToggleMixer,
    ToggleSnap,
    CycleGrid,
    ZoomIn,
    ZoomOut,
    ZoomToProject,
    TallerTracks,
    ShorterTracks,
    ShowPalette,
    RunScript,
    RunLastScript,
    StopScript,
    MasterMoreChannels,
    MasterFewerChannels,
}

const CMD: Modifiers = Modifiers::COMMAND;
const CMD_SHIFT: Modifiers = Modifiers { shift: true, ..Modifiers::COMMAND };
const CMD_ALT: Modifiers = Modifiers { alt: true, ..Modifiers::COMMAND };

impl Action {
    pub const ALL: &[Action] = &[
        Action::PlayPause,
        Action::Stop,
        Action::GoToStart,
        Action::ToggleRepeat,
        Action::ToggleMetronome,
        Action::Record,
        Action::ToggleAutoPunch,
        Action::ToggleArm,
        Action::AddTrack,
        Action::SaveFxChain,
        Action::LoadFxChain,
        Action::SaveTrackTemplate,
        Action::InsertTrackTemplate,
        Action::ImportMedia,
        Action::DeleteSelection,
        Action::SplitItem,
        Action::SplitAtSelection,
        Action::RemoveSelectionContents,
        Action::RippleCutSelection,
        Action::InsertSpace,
        Action::CopyItem,
        Action::CutItem,
        Action::PasteItem,
        Action::DuplicateItem,
        Action::ReverseItem,
        Action::GlueItems,
        Action::ApplyTrackFx,
        Action::NextTake,
        Action::OpenMidiEditor,
        Action::ExportMidi,
        Action::ToggleItemMute,
        Action::PreviousMarker,
        Action::NextMarker,
        Action::DeleteMarker,
        Action::InsertMarker,
        Action::InsertTempoMarker,
        Action::RemoveTempoMarker,
        Action::ToggleMuteTrack,
        Action::ToggleSoloTrack,
        Action::PreviousTrack,
        Action::NextTrack,
        Action::MoveTrackUp,
        Action::MoveTrackDown,
        Action::IndentTrack,
        Action::OutdentTrack,
        Action::Undo,
        Action::Redo,
        Action::NewProject,
        Action::OpenProject,
        Action::SaveProject,
        Action::SaveProjectAs,
        Action::RenderToWav,
        Action::RenderSelection,
        Action::RenderStems,
        Action::RenderProjectSettings,
        Action::LoadDemo,
        Action::ToggleMixer,
        Action::ToggleSnap,
        Action::CycleGrid,
        Action::ZoomIn,
        Action::ZoomOut,
        Action::ZoomToProject,
        Action::TallerTracks,
        Action::ShorterTracks,
        Action::ShowPalette,
        Action::RunScript,
        Action::RunLastScript,
        Action::StopScript,
        Action::MasterMoreChannels,
        Action::MasterFewerChannels,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Action::PlayPause => "Transport: Play/pause",
            Action::Stop => "Transport: Stop",
            Action::GoToStart => "Transport: Go to start of project",
            Action::ToggleRepeat => "Transport: Toggle repeat",
            Action::ToggleMetronome => "Options: Toggle metronome",
            Action::Record => "Transport: Record",
            Action::ToggleAutoPunch => "Record mode: Toggle time selection auto-punch",
            Action::ToggleArm => "Track: Toggle record arm for selected track",
            Action::AddTrack => "Track: Insert new track",
            Action::SaveFxChain => "FX: Save track FX chain",
            Action::LoadFxChain => "FX: Add FX chain to track",
            Action::SaveTrackTemplate => "Track: Save track as track template",
            Action::InsertTrackTemplate => "Track: Insert track from template",
            Action::ImportMedia => "Insert media files…",
            Action::DeleteSelection => "Edit: Delete selected item or track",
            Action::SplitItem => "Item: Split at cursor",
            Action::SplitAtSelection => "Item: Split items at time selection",
            Action::RemoveSelectionContents => "Time selection: Remove contents",
            Action::RippleCutSelection => "Time selection: Remove contents and move later items",
            Action::InsertSpace => "Time selection: Insert empty space",
            Action::CopyItem => "Edit: Copy item",
            Action::CutItem => "Edit: Cut item",
            Action::PasteItem => "Edit: Paste at cursor",
            Action::DuplicateItem => "Item: Duplicate",
            Action::ReverseItem => "Item: Reverse",
            Action::GlueItems => "Item: Glue items",
            Action::ApplyTrackFx => "Item: Apply track FX to items as new take",
            Action::NextTake => "Take: Switch items to next take",
            Action::OpenMidiEditor => "Item: Open in MIDI editor",
            Action::ExportMidi => "Item: Export MIDI item to file",
            Action::ToggleItemMute => "Item: Toggle mute",
            Action::PreviousMarker => "Markers: Go to previous marker or region",
            Action::NextMarker => "Markers: Go to next marker or region",
            Action::DeleteMarker => "Markers: Delete marker nearest the cursor",
            Action::InsertMarker => "Markers: Insert marker at cursor",
            Action::InsertTempoMarker => "Tempo: Insert tempo marker at cursor",
            Action::RemoveTempoMarker => "Tempo: Remove tempo marker nearest the cursor",
            Action::ToggleMuteTrack => "Track: Toggle mute for selected track",
            Action::ToggleSoloTrack => "Track: Toggle solo for selected track",
            Action::PreviousTrack => "Track: Select previous track",
            Action::NextTrack => "Track: Select next track",
            Action::MoveTrackUp => "Track: Move selected track up",
            Action::MoveTrackDown => "Track: Move selected track down",
            Action::IndentTrack => "Track: Move into the folder above",
            Action::OutdentTrack => "Track: Move out of its folder",
            Action::Undo => "Edit: Undo",
            Action::Redo => "Edit: Redo",
            Action::NewProject => "File: New project",
            Action::OpenProject => "File: Open project…",
            Action::SaveProject => "File: Save project",
            Action::SaveProjectAs => "File: Save project as…",
            Action::RenderToWav => "File: Render project to WAV…",
            Action::RenderSelection => "File: Render time selection to WAV…",
            Action::RenderStems => "File: Render stems (one WAV per track)…",
            Action::RenderProjectSettings => "File: Render with the project's render settings",
            Action::LoadDemo => "File: Open demo song",
            Action::ToggleMixer => "View: Toggle mixer",
            Action::ToggleSnap => "Options: Toggle snap to grid",
            Action::CycleGrid => "Options: Next grid size",
            Action::ZoomIn => "View: Zoom in horizontally",
            Action::ZoomOut => "View: Zoom out horizontally",
            Action::ZoomToProject => "View: Zoom to project",
            Action::TallerTracks => "View: Increase track height",
            Action::ShorterTracks => "View: Decrease track height",
            Action::ShowPalette => "Actions: Search actions",
            Action::RunScript => "Script: Run ReaScript (Lua, EEL2 or Python)",
            Action::RunLastScript => "Script: Run last script again",
            Action::StopScript => "Script: Stop the running script",
            Action::MasterMoreChannels => "Master: More channels",
            Action::MasterFewerChannels => "Master: Fewer channels",
        }
    }

    pub fn shortcut(self) -> Option<KeyboardShortcut> {
        let k = |m, key| Some(KeyboardShortcut::new(m, key));
        match self {
            Action::PlayPause => k(Modifiers::NONE, Key::Space),
            Action::Stop => k(Modifiers::NONE, Key::Escape),
            Action::GoToStart => k(Modifiers::NONE, Key::Home),
            Action::ToggleRepeat => k(Modifiers::NONE, Key::R),
            Action::ToggleMetronome => k(Modifiers::NONE, Key::C),
            Action::Record => k(CMD, Key::R),
            Action::ToggleAutoPunch => None,
            Action::ToggleArm => k(Modifiers::ALT, Key::R),
            Action::AddTrack => k(CMD, Key::T),
            Action::SaveFxChain | Action::LoadFxChain | Action::SaveTrackTemplate | Action::InsertTrackTemplate => None,
            Action::ImportMedia => k(Modifiers::NONE, Key::Insert),
            Action::DeleteSelection => k(Modifiers::NONE, Key::Delete),
            Action::SplitItem => k(Modifiers::NONE, Key::S),
            Action::SplitAtSelection => k(Modifiers::SHIFT, Key::S),
            Action::RemoveSelectionContents => None,
            Action::RippleCutSelection => k(CMD_ALT, Key::X),
            Action::InsertSpace => None,
            Action::CopyItem => k(CMD, Key::C),
            Action::CutItem => k(CMD, Key::X),
            Action::PasteItem => k(CMD, Key::V),
            Action::DuplicateItem => k(CMD, Key::D),
            Action::ReverseItem => None,
            Action::GlueItems => k(CMD, Key::G),
            Action::ApplyTrackFx => None,
            Action::NextTake => k(Modifiers::NONE, Key::T),
            Action::OpenMidiEditor => k(Modifiers::NONE, Key::E),
            Action::ExportMidi => None,
            Action::ToggleItemMute => k(Modifiers::NONE, Key::U),
            Action::PreviousMarker => k(Modifiers::NONE, Key::OpenBracket),
            Action::NextMarker => k(Modifiers::NONE, Key::CloseBracket),
            Action::DeleteMarker => None,
            Action::InsertMarker => k(Modifiers::NONE, Key::M),
            Action::InsertTempoMarker => k(Modifiers::SHIFT, Key::C),
            Action::RemoveTempoMarker => None,
            Action::ToggleMuteTrack => k(Modifiers::ALT, Key::M),
            Action::ToggleSoloTrack => k(Modifiers::ALT, Key::S),
            Action::PreviousTrack => k(Modifiers::NONE, Key::ArrowUp),
            Action::NextTrack => k(Modifiers::NONE, Key::ArrowDown),
            Action::MoveTrackUp => k(CMD, Key::ArrowUp),
            Action::MoveTrackDown => k(CMD, Key::ArrowDown),
            Action::IndentTrack => k(CMD, Key::ArrowRight),
            Action::OutdentTrack => k(CMD, Key::ArrowLeft),
            Action::Undo => k(CMD, Key::Z),
            Action::Redo => k(CMD_SHIFT, Key::Z),
            Action::NewProject => k(CMD, Key::N),
            Action::OpenProject => k(CMD, Key::O),
            Action::SaveProject => k(CMD, Key::S),
            Action::SaveProjectAs => k(CMD_SHIFT, Key::S),
            Action::RenderToWav => k(CMD_ALT, Key::R),
            Action::RenderSelection => None,
            Action::RenderStems => None,
            Action::RenderProjectSettings => None,
            Action::LoadDemo => None,
            Action::ToggleMixer => k(CMD, Key::M),
            Action::ToggleSnap => k(Modifiers::ALT, Key::G),
            Action::CycleGrid => k(Modifiers::SHIFT, Key::G),
            Action::ZoomIn => k(Modifiers::NONE, Key::Equals),
            Action::ZoomOut => k(Modifiers::NONE, Key::Minus),
            Action::ZoomToProject => k(CMD, Key::Num0),
            Action::TallerTracks => k(Modifiers::NONE, Key::PageUp),
            Action::ShorterTracks => k(Modifiers::NONE, Key::PageDown),
            Action::ShowPalette => k(CMD, Key::K),
            Action::RunScript | Action::RunLastScript | Action::StopScript => None,
            Action::MasterMoreChannels | Action::MasterFewerChannels => None,
        }
    }

    pub fn run(self, app: &mut ScytheApp) {
        match self {
            Action::PlayPause => app.toggle_play(),
            Action::Stop => app.stop(),
            Action::GoToStart => app.go_to_start(),
            Action::ToggleRepeat => app.toggle_loop(),
            Action::ToggleMetronome => app.toggle_metronome(),
            Action::Record => app.toggle_record(),
            Action::ToggleAutoPunch => {
                app.auto_punch ^= true;
                app.notify(if app.auto_punch {
                    "Record mode: time selection auto-punch"
                } else {
                    "Record mode: normal"
                });
            }
            Action::ToggleArm => {
                if let Some(t) = app.sel.track {
                    app.edit("Toggle record arm", |p| {
                        if let Some(t) = p.track_mut(t) {
                            t.armed = !t.armed;
                        }
                    });
                }
            }
            Action::AddTrack => app.add_track(),
            Action::SaveFxChain => app.save_fx_chain(),
            Action::LoadFxChain => app.load_fx_chain(),
            Action::SaveTrackTemplate => app.save_track_template(),
            Action::InsertTrackTemplate => app.insert_track_template(),
            Action::ImportMedia => app.import_dialog(),
            Action::DeleteSelection => app.delete_selection(),
            Action::SplitItem => app.split_selected_item(),
            Action::SplitAtSelection
            | Action::RemoveSelectionContents
            | Action::RippleCutSelection
            | Action::InsertSpace => {
                let Some(range) = app.project.time_selection.filter(|r| !r.is_empty()) else {
                    app.notify("Make a time selection first (drag on the ruler)");
                    return;
                };
                let label = self.label().split(": ").nth(1).unwrap_or("Edit").to_owned();
                app.edit(&label, |p| match self {
                    Action::SplitAtSelection => {
                        p.split_items_at(range.start, None);
                        p.split_items_at(range.end, None);
                    }
                    Action::RemoveSelectionContents => p.remove_range(range, None, false),
                    Action::RippleCutSelection => p.remove_range(range, None, true),
                    _ => p.insert_space(range),
                });
            }
            Action::CopyItem => app.copy_item(false),
            Action::CutItem => app.copy_item(true),
            Action::PasteItem => app.paste_item(),
            Action::DuplicateItem => app.duplicate_item(),
            Action::ReverseItem => app.reverse_item(),
            Action::GlueItems => app.glue_selected(),
            Action::ApplyTrackFx => app.apply_track_fx(),
            Action::NextTake => {
                if let Some(id) = app.sel.item {
                    app.edit("Next take", |p| {
                        if let Some(it) = p.tracks.iter_mut().find_map(|t| t.item_mut(id))
                            && !it.takes.is_empty()
                        {
                            it.active_take = (it.active_take + 1) % it.takes.len();
                        }
                    });
                }
            }
            Action::ExportMidi => app.export_midi(),
            Action::OpenMidiEditor => {
                let midi = app.sel.item.and_then(|id| app.project.find_item(id)).is_some_and(|(_, it)| {
                    it.take().is_some_and(|t| matches!(&t.source, scythe_core::Source::Raw(c) if c.args().first().is_some_and(|k| k == "MIDI")))
                });
                if midi {
                    app.view.midi_editor = app.sel.item;
                } else {
                    app.notify("Select a MIDI item first");
                }
            }
            Action::ToggleItemMute => {
                let items = app.selected_items();
                app.edit("Toggle item mute", |p| {
                    for it in p.tracks.iter_mut().flat_map(|t| t.items.iter_mut()).filter(|it| items.contains(&it.id)) {
                        it.muted = !it.muted;
                    }
                });
            }
            Action::PreviousMarker | Action::NextMarker => {
                let at = app.position();
                let target = if self == Action::NextMarker {
                    app.project
                        .markers
                        .iter()
                        .map(|m| m.position)
                        .filter(|&p| p > at + 1e-6)
                        .fold(f64::INFINITY, f64::min)
                } else {
                    app.project
                        .markers
                        .iter()
                        .map(|m| m.position)
                        .filter(|&p| p < at - 1e-6)
                        .fold(f64::NEG_INFINITY, f64::max)
                };
                if target.is_finite() {
                    app.set_cursor(target);
                }
            }
            Action::DeleteMarker => {
                let at = app.position();
                let nearest = app
                    .project
                    .markers
                    .iter()
                    .enumerate()
                    .min_by(|a, b| (a.1.position - at).abs().total_cmp(&(b.1.position - at).abs()))
                    .map(|(i, _)| i);
                if let Some(i) = nearest {
                    app.edit("Delete marker", |p| {
                        p.markers.remove(i);
                    });
                }
            }
            Action::InsertMarker => {
                let at = app.position();
                app.edit("Insert marker", |p| p.add_marker(at, ""));
            }
            Action::InsertTempoMarker => {
                let at = app.position();
                app.edit("Insert tempo marker", |p| p.tempo.insert_point(at));
            }
            Action::RemoveTempoMarker => {
                let at = app.position();
                let nearest = app
                    .project
                    .tempo
                    .points
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| p.time > 0.0)
                    .min_by(|a, b| (a.1.time - at).abs().total_cmp(&(b.1.time - at).abs()))
                    .map(|(i, _)| i);
                if let Some(i) = nearest {
                    app.edit("Remove tempo marker", |p| {
                        p.tempo.points.remove(i);
                    });
                }
            }
            Action::ToggleMuteTrack => {
                if let Some(t) = app.sel.track {
                    app.edit("Toggle mute", |p| {
                        if let Some(t) = p.track_mut(t) {
                            t.muted = !t.muted;
                        }
                    });
                }
            }
            Action::ToggleSoloTrack => {
                if let Some(t) = app.sel.track {
                    app.edit("Toggle solo", |p| {
                        if let Some(t) = p.track_mut(t) {
                            t.solo = !t.solo;
                        }
                    });
                }
            }
            Action::PreviousTrack | Action::NextTrack => {
                let n = app.project.tracks.len();
                if n > 0 {
                    let cur = app.selected_track_index();
                    let next = match (self, cur) {
                        (Action::PreviousTrack, Some(i)) => i.saturating_sub(1),
                        (_, Some(i)) => (i + 1).min(n - 1),
                        (_, None) => 0,
                    };
                    app.sel.track = Some(app.project.tracks[next].id);
                    app.sel.item = None;
                }
            }
            Action::MoveTrackUp | Action::MoveTrackDown | Action::IndentTrack | Action::OutdentTrack => {
                if let Some(t) = app.sel.track {
                    let label = self.label().trim_start_matches("Track: ").to_owned();
                    app.edit(&label, |p| {
                        match self {
                            Action::MoveTrackUp => p.move_track(t, true),
                            Action::MoveTrackDown => p.move_track(t, false),
                            Action::IndentTrack => p.indent_track(t, true),
                            _ => p.indent_track(t, false),
                        };
                    });
                }
            }
            Action::Undo => app.undo(),
            Action::Redo => app.redo(),
            Action::NewProject => app.new_project(),
            Action::OpenProject => app.open_dialog(),
            Action::SaveProject => app.save(),
            Action::SaveProjectAs => app.save_as(),
            Action::RenderToWav => app.render_dialog(),
            Action::RenderSelection => app.render(crate::app::RenderKind::Selection),
            Action::RenderStems => app.render(crate::app::RenderKind::Stems),
            Action::RenderProjectSettings => app.render(crate::app::RenderKind::ProjectSettings),
            Action::LoadDemo => app.load_demo(),
            Action::ToggleMixer => app.view.show_mixer = !app.view.show_mixer,
            Action::ToggleSnap => {
                app.view.snap = !app.view.snap;
                let state = if app.view.snap { "on" } else { "off" };
                app.notify(format!("Snap to grid {state}"));
            }
            Action::CycleGrid => {
                app.view.grid = (app.view.grid + 1) % crate::app::GRID_STEPS.len();
                app.notify(format!("Grid {}", crate::app::GRID_STEPS[app.view.grid].0));
            }
            Action::ZoomIn => app.view.px_per_sec = (app.view.px_per_sec * 1.5).min(2000.0),
            Action::ZoomOut => app.view.px_per_sec = (app.view.px_per_sec / 1.5).max(2.0),
            Action::ZoomToProject => {
                let end = app.project.length().max(app.project.tempo.bar_seconds() * 8.0);
                app.view.fit = Some(scythe_core::TimeRange::new(0.0, end));
            }
            Action::TallerTracks => app.view.row_h = (app.view.row_h * 1.25).min(240.0),
            Action::ShorterTracks => app.view.row_h = (app.view.row_h / 1.25).max(30.0),
            Action::ShowPalette => app.palette = Some(PaletteState::default()),
            Action::MasterMoreChannels | Action::MasterFewerChannels => {
                const CHANNELS: [u32; 7] = [2, 4, 6, 8, 12, 16, 32];
                let now = app.project.master.channels;
                let at = CHANNELS.iter().position(|&c| c >= now).unwrap_or(0);
                let next = if self == Action::MasterMoreChannels {
                    CHANNELS[(at + 1).min(CHANNELS.len() - 1)]
                } else {
                    CHANNELS[at.saturating_sub(1)]
                };
                app.edit("Set master channels", |p| p.master.channels = next);
                app.notify(format!("Master: {next} channels"));
            }
            Action::RunScript => app.run_script_dialog(),
            Action::RunLastScript => match app.last_script.clone() {
                Some(path) => app.run_script(&path),
                None => app.notify("No script run yet"),
            },
            Action::StopScript => {
                if app.script_running() {
                    app.stop_script();
                } else {
                    app.notify("No script is running");
                }
            }
        }
    }
}

pub fn shortcut_text(ctx: &egui::Context, action: Action) -> String {
    action.shortcut().map(|s| ctx.format_shortcut(&s)).unwrap_or_default()
}

/// Dispatches keyboard shortcuts unless a text field has focus.
pub fn handle_shortcuts(app: &mut ScytheApp, ctx: &egui::Context) {
    if ctx.egui_wants_keyboard_input() || app.palette.is_some() {
        return;
    }
    // The windowing layer turns the clipboard shortcuts into events instead of key presses.
    let clipboard = ctx.input(|i| {
        i.events.iter().find_map(|e| match e {
            egui::Event::Copy => Some(Action::CopyItem),
            egui::Event::Cut => Some(Action::CutItem),
            egui::Event::Paste(_) => Some(Action::PasteItem),
            _ => None,
        })
    });
    if let Some(action) = clipboard {
        action.run(app);
    }
    // Longer modifier combinations first so Ctrl+Shift+S does not also trigger Ctrl+S.
    let mut actions: Vec<Action> = Action::ALL.to_vec();
    actions.sort_by_key(|a| {
        std::cmp::Reverse(a.shortcut().map_or(0, |s| {
            s.modifiers.shift as u8 + s.modifiers.alt as u8 + s.modifiers.command as u8 + s.modifiers.ctrl as u8
        }))
    });
    for action in actions {
        if matches!(action, Action::CopyItem | Action::CutItem | Action::PasteItem) {
            continue;
        }
        if let Some(sc) = action.shortcut()
            && ctx.input_mut(|i| i.consume_shortcut(&sc))
        {
            action.run(app);
        }
    }
}

#[derive(Debug, Default)]
pub struct PaletteState {
    pub query: String,
    pub selected: usize,
}

fn matches(label: &str, query: &str) -> bool {
    let label = label.to_lowercase();
    query.to_lowercase().split_whitespace().all(|w| label.contains(w))
}

/// The ⌘K action search overlay.
pub fn show(app: &mut ScytheApp, ui: &mut egui::Ui) {
    let Some(state) = app.palette.as_mut() else { return };
    let ctx = ui.ctx().clone();
    let screen = ui.max_rect();
    let results: Vec<Action> = Action::ALL.iter().copied().filter(|a| matches(a.label(), &state.query)).collect();
    state.selected = state.selected.min(results.len().saturating_sub(1));

    let (up, down, enter, escape) = ctx.input_mut(|i| {
        (
            i.consume_key(Modifiers::NONE, Key::ArrowUp),
            i.consume_key(Modifiers::NONE, Key::ArrowDown),
            i.consume_key(Modifiers::NONE, Key::Enter),
            i.consume_key(Modifiers::NONE, Key::Escape),
        )
    });
    if up {
        state.selected = state.selected.saturating_sub(1);
    }
    if down && state.selected + 1 < results.len() {
        state.selected += 1;
    }

    let painter = ui.painter().clone();
    painter.rect_filled(screen, CornerRadius::ZERO, egui::Color32::from_black_alpha(120));
    let backdrop = ui.allocate_rect(screen, Sense::click());

    let width = 560f32.min(screen.width() - 32.0);
    let row_h = 40.0;
    let visible = results.len().min(9);
    let height = 64.0 + visible as f32 * row_h + 12.0;
    let rect =
        Rect::from_min_size(egui::pos2(screen.center().x - width / 2.0, screen.top() + 120.0), vec2(width, height));
    painter.rect_filled(rect, CornerRadius::same(18), theme::PANEL);

    let input_rect = Rect::from_min_size(rect.min + vec2(12.0, 12.0), vec2(width - 24.0, 44.0));
    painter.rect_filled(input_rect, CornerRadius::same(255), theme::BG);
    let edit = egui::TextEdit::singleline(&mut state.query)
        .hint_text("Search actions")
        .frame(egui::Frame::NONE)
        .font(theme::regular(15.0))
        .desired_width(width - 64.0);
    let field = ui.put(Rect::from_min_size(input_rect.min + vec2(40.0, 12.0), vec2(width - 72.0, 22.0)), edit);
    field.request_focus();
    crate::ui::widgets::paint_icon(
        ui,
        Rect::from_center_size(input_rect.left_center() + vec2(22.0, 0.0), vec2(16.0, 16.0)),
        crate::ui::widgets::Icon::Search,
        theme::TEXT_DIM,
        16.0,
    );

    let mut run = None;
    let first = state.selected.saturating_sub(visible.saturating_sub(1));
    for (row, &action) in results.iter().enumerate().skip(first).take(visible) {
        let r = Rect::from_min_size(
            egui::pos2(rect.left() + 8.0, input_rect.bottom() + 8.0 + (row - first) as f32 * row_h),
            vec2(width - 16.0, row_h - 4.0),
        );
        let resp = ui.allocate_rect(r, Sense::click());
        if row == state.selected || resp.hovered() {
            painter.rect_filled(r, CornerRadius::same(12), theme::RAISED);
        }
        painter.text(
            r.left_center() + vec2(14.0, 0.0),
            Align2::LEFT_CENTER,
            action.label(),
            theme::regular(13.0),
            theme::TEXT,
        );
        let sc = shortcut_text(&ctx, action);
        if !sc.is_empty() {
            painter.text(
                r.right_center() - vec2(14.0, 0.0),
                Align2::RIGHT_CENTER,
                sc,
                theme::mono(11.0),
                theme::TEXT_DIM,
            );
        }
        if resp.clicked() {
            run = Some(action);
        }
    }
    if results.is_empty() {
        painter.text(
            egui::pos2(rect.center().x, input_rect.bottom() + 28.0),
            Align2::CENTER_CENTER,
            "No matching actions",
            theme::regular(13.0),
            theme::TEXT_DIM,
        );
    }
    if enter {
        run = results.get(state.selected).copied();
    }
    if escape || (backdrop.clicked() && !backdrop.interact_pointer_pos().is_some_and(|p| rect.contains(p))) {
        app.palette = None;
    }
    if let Some(action) = run {
        app.palette = None;
        action.run(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_matches_all_words() {
        assert!(matches("Transport: Toggle repeat", "rep tog"));
        assert!(!matches("Transport: Toggle repeat", "mixer"));
    }

    #[test]
    fn every_shortcut_is_unique() {
        let mut seen = std::collections::HashSet::new();
        for a in Action::ALL {
            if let Some(s) = a.shortcut() {
                assert!(seen.insert(format!("{s:?}")), "duplicate shortcut for {a:?}");
            }
        }
    }
}
