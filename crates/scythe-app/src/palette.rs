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
    FreezeTrack,
    UnfreezeTrack,
    NextTake,
    OpenMidiEditor,
    ExportMidi,
    ToggleItemMute,
    AddStretchMarker,
    RemoveStretchMarkers,
    PreviousMarker,
    NextMarker,
    DeleteMarker,
    InsertMarker,
    InsertTempoMarker,
    RemoveTempoMarker,
    ToggleMuteTrack,
    ToggleSoloTrack,
    TogglePolarity,
    ToggleSoloDefeat,
    TrackGrouping,
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
    QueueRender,
    RegionManager,
    PerformanceMeter,
    MediaExplorer,
    ProjectBay,
    CleanProjectDirectory,
    ThemeColors,
    RegionRenderMatrix,
    NewTab,
    OpenInNewTab,
    CloseTab,
    NextTab,
    PreviousTab,
    RunRenderQueue,
    LoadDemo,
    ToggleMixer,
    ToggleEnvelopeLanes,
    ToggleBypassAutomation,
    ToggleSnap,
    CycleGrid,
    ZoomIn,
    ZoomOut,
    ZoomToProject,
    TallerTracks,
    ShorterTracks,
    OpenInlineEditors,
    CloseInlineEditors,
    ShowPalette,
    RunScript,
    RunLastScript,
    StopScript,
    MasterMoreChannels,
    MasterFewerChannels,
    NamPanel,
    NamPrevious,
    NamNext,
    NamSnapshotA,
    NamSnapshotB,
    NamSnapshotC,
    NamSnapshotD,
    ShowMonitorMixes,
    ShowTempoEnvelope,
    NewMonitorMix,
    CreateCueBus,
    ToggleSetlist,
    SetlistPlay,
    SetlistNext,
    SetlistPrevious,
    ToggleMarkerActions,
    ShowVirtualKeyboard,
    MidiDevices,
    ToggleTimecodeSync,
    /// REAPER's "Options: Preferences..." (Ctrl+P), which opens on the audio device page.
    Preferences,
    AudioDeviceSettings,
    ShowActionList,
    CustomizeToolbars,
    MouseModifiers,
    ToggleMainToolbar,
    /// Floating toolbar 1 to 32.
    Toolbar(u8),
    /// Window set 1 to 10.
    LoadScreenset(u8),
    SaveScreenset(u8),
    /// One of REAPER's editing actions (`scythe_core::edit::actions`), by command id.
    Edit(u32),
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
        Action::FreezeTrack,
        Action::UnfreezeTrack,
        Action::NextTake,
        Action::OpenMidiEditor,
        Action::ExportMidi,
        Action::ToggleItemMute,
        Action::AddStretchMarker,
        Action::RemoveStretchMarkers,
        Action::PreviousMarker,
        Action::NextMarker,
        Action::DeleteMarker,
        Action::InsertMarker,
        Action::InsertTempoMarker,
        Action::RemoveTempoMarker,
        Action::ToggleMuteTrack,
        Action::ToggleSoloTrack,
        Action::TogglePolarity,
        Action::ToggleSoloDefeat,
        Action::TrackGrouping,
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
        Action::QueueRender,
        Action::RegionManager,
        Action::PerformanceMeter,
        Action::MediaExplorer,
        Action::ProjectBay,
        Action::CleanProjectDirectory,
        Action::ThemeColors,
        Action::RegionRenderMatrix,
        Action::NewTab,
        Action::OpenInNewTab,
        Action::CloseTab,
        Action::NextTab,
        Action::PreviousTab,
        Action::RunRenderQueue,
        Action::LoadDemo,
        Action::ToggleMixer,
        Action::ToggleEnvelopeLanes,
        Action::ToggleBypassAutomation,
        Action::ToggleSnap,
        Action::CycleGrid,
        Action::ZoomIn,
        Action::ZoomOut,
        Action::ZoomToProject,
        Action::TallerTracks,
        Action::ShorterTracks,
        Action::OpenInlineEditors,
        Action::CloseInlineEditors,
        Action::ShowPalette,
        Action::RunScript,
        Action::RunLastScript,
        Action::StopScript,
        Action::MasterMoreChannels,
        Action::MasterFewerChannels,
        Action::NamPanel,
        Action::NamPrevious,
        Action::NamNext,
        Action::NamSnapshotA,
        Action::NamSnapshotB,
        Action::NamSnapshotC,
        Action::NamSnapshotD,
        Action::ShowMonitorMixes,
        Action::ShowTempoEnvelope,
        Action::NewMonitorMix,
        Action::CreateCueBus,
        Action::ToggleSetlist,
        Action::SetlistPlay,
        Action::SetlistNext,
        Action::SetlistPrevious,
        Action::ToggleMarkerActions,
        Action::ShowVirtualKeyboard,
        Action::MidiDevices,
        Action::ToggleTimecodeSync,
        Action::Preferences,
        Action::AudioDeviceSettings,
    ];

    /// Every action, the toolbars and screensets included.
    pub fn all() -> Vec<Action> {
        let mut out = Self::ALL.to_vec();
        out.extend([
            Action::ShowActionList,
            Action::CustomizeToolbars,
            Action::MouseModifiers,
            Action::ToggleMainToolbar,
        ]);
        out.extend((1..=scythe_core::menus::FLOATING_TOOLBARS as u8).map(Action::Toolbar));
        out.extend((1..=scythe_session::actions::SCREENSETS as u8).map(Action::LoadScreenset));
        out.extend((1..=scythe_session::actions::SCREENSETS as u8).map(Action::SaveScreenset));
        out.extend(crate::ui::editing::window_ids().into_iter().map(Action::Edit));
        out
    }

    /// The action's name in the action list.
    pub fn label(self) -> String {
        let command = self.command();
        scythe_session::actions::builtin()
            .iter()
            .find(|a| a.command == command)
            .map(|a| a.name.clone())
            .unwrap_or(command)
    }

    /// The command that runs it: REAPER's command id where REAPER has the action, else a
    /// Scythe named command.
    pub fn command(self) -> String {
        let id = |n: u32| n.to_string();
        let named = |n: &str| n.to_owned();
        match self {
            Action::PlayPause => id(40073),
            Action::Stop => id(1016),
            Action::GoToStart => id(40042),
            Action::ToggleRepeat => id(1068),
            Action::ToggleMetronome => id(40364),
            Action::Record => id(1013),
            Action::ToggleAutoPunch => named("_SCYTHE_TOGGLE_AUTO_PUNCH"),
            Action::ToggleArm => id(9),
            Action::AddTrack => id(40001),
            Action::SaveFxChain => named("_SCYTHE_SAVE_FX_CHAIN"),
            Action::LoadFxChain => named("_SCYTHE_LOAD_FX_CHAIN"),
            Action::SaveTrackTemplate => id(40392),
            Action::InsertTrackTemplate => named("_SCYTHE_INSERT_TRACK_TEMPLATE"),
            Action::ImportMedia => id(40018),
            Action::DeleteSelection => id(40697),
            Action::SplitItem => id(40012),
            Action::SplitAtSelection => id(40061),
            Action::RemoveSelectionContents => id(40312),
            Action::RippleCutSelection => id(40201),
            Action::InsertSpace => id(40200),
            Action::CopyItem => id(40698),
            Action::CutItem => id(40699),
            Action::PasteItem => id(42398),
            Action::DuplicateItem => id(41295),
            Action::ReverseItem => id(41051),
            Action::GlueItems => id(40362),
            Action::ApplyTrackFx => id(40209),
            Action::FreezeTrack => id(41223),
            Action::UnfreezeTrack => id(41644),
            Action::NextTake => id(40125),
            Action::OpenMidiEditor => id(40153),
            Action::ExportMidi => named("_SCYTHE_EXPORT_MIDI_ITEM"),
            Action::ToggleItemMute => id(40175),
            Action::AddStretchMarker => id(41842),
            Action::RemoveStretchMarkers => id(41844),
            Action::PreviousMarker => id(40172),
            Action::NextMarker => id(40173),
            Action::DeleteMarker => id(40613),
            Action::InsertMarker => id(40157),
            Action::InsertTempoMarker => id(42330),
            Action::RemoveTempoMarker => named("_SCYTHE_REMOVE_TEMPO_MARKER"),
            Action::ToggleMuteTrack => id(6),
            Action::ToggleSoloTrack => id(7),
            Action::TogglePolarity => id(40282),
            Action::ToggleSoloDefeat => id(41199),
            Action::TrackGrouping => id(40768),
            Action::PreviousTrack => id(40286),
            Action::NextTrack => id(40285),
            Action::MoveTrackUp => id(43647),
            Action::MoveTrackDown => id(43648),
            Action::IndentTrack => named("_SCYTHE_INDENT_TRACK"),
            Action::OutdentTrack => named("_SCYTHE_OUTDENT_TRACK"),
            Action::Undo => id(40029),
            Action::Redo => id(40030),
            Action::NewProject => id(40023),
            Action::OpenProject => id(40025),
            Action::SaveProject => id(40026),
            Action::SaveProjectAs => id(40022),
            Action::RenderToWav => id(40015),
            Action::RenderSelection => named("_SCYTHE_RENDER_TIME_SELECTION"),
            Action::RenderStems => named("_SCYTHE_RENDER_STEMS"),
            Action::RenderProjectSettings => id(41824),
            Action::QueueRender => id(41823),
            Action::RegionManager => id(40326),
            Action::PerformanceMeter => id(40240),
            Action::MediaExplorer => id(50124),
            Action::ProjectBay => id(41157),
            Action::CleanProjectDirectory => id(40098),
            Action::ThemeColors => id(42392),
            Action::RegionRenderMatrix => id(41888),
            Action::NewTab => id(40859),
            Action::OpenInNewTab => named("_SCYTHE_OPEN_IN_NEW_TAB"),
            Action::CloseTab => id(40860),
            Action::NextTab => id(40861),
            Action::PreviousTab => id(40862),
            Action::RunRenderQueue => id(41207),
            Action::LoadDemo => named("_SCYTHE_LOAD_DEMO"),
            Action::ToggleMixer => id(40078),
            Action::ToggleEnvelopeLanes => named("_SCYTHE_TOGGLE_ENVELOPE_LANES"),
            Action::ToggleBypassAutomation => id(40908),
            Action::ToggleSnap => id(1157),
            Action::CycleGrid => named("_SCYTHE_NEXT_GRID"),
            Action::ZoomIn => id(1012),
            Action::ZoomOut => id(1011),
            Action::ZoomToProject => id(40295),
            Action::TallerTracks => id(41325),
            Action::ShorterTracks => id(41326),
            Action::OpenInlineEditors => id(40847),
            Action::CloseInlineEditors => id(41887),
            Action::ShowPalette => named("_SCYTHE_SEARCH_ACTIONS"),
            Action::RunScript => id(41060),
            Action::RunLastScript => id(41061),
            Action::StopScript => id(41898),
            Action::MasterMoreChannels => named("_SCYTHE_MASTER_MORE_CHANNELS"),
            Action::MasterFewerChannels => named("_SCYTHE_MASTER_FEWER_CHANNELS"),
            Action::NamPanel => named("_SCYTHE_NAM_PANEL"),
            Action::NamPrevious => named("_SCYTHE_NAM_PREVIOUS"),
            Action::NamNext => named("_SCYTHE_NAM_NEXT"),
            Action::NamSnapshotA => named("_SCYTHE_NAM_SNAPSHOT_A"),
            Action::NamSnapshotB => named("_SCYTHE_NAM_SNAPSHOT_B"),
            Action::NamSnapshotC => named("_SCYTHE_NAM_SNAPSHOT_C"),
            Action::NamSnapshotD => named("_SCYTHE_NAM_SNAPSHOT_D"),
            Action::ShowMonitorMixes => named("_SCYTHE_TOGGLE_MONITOR_MIXES"),
            Action::ShowTempoEnvelope => id(41046),
            Action::NewMonitorMix => named("_SCYTHE_NEW_MONITOR_MIX"),
            Action::CreateCueBus => named("_SCYTHE_CREATE_CUE_BUS"),
            Action::ToggleSetlist => named("_SCYTHE_TOGGLE_SETLIST"),
            Action::SetlistPlay => named("_SCYTHE_SETLIST_PLAY"),
            Action::SetlistNext => named("_SCYTHE_SETLIST_NEXT"),
            Action::SetlistPrevious => named("_SCYTHE_SETLIST_PREVIOUS"),
            Action::ToggleMarkerActions => named("_SCYTHE_TOGGLE_MARKER_ACTIONS"),
            Action::ShowVirtualKeyboard => id(40377),
            Action::MidiDevices => id(40619),
            Action::ToggleTimecodeSync => id(40620),
            Action::Preferences => id(40016),
            Action::AudioDeviceSettings => named("_SCYTHE_AUDIO_DEVICE_SETTINGS"),
            Action::ShowActionList => id(40605),
            Action::CustomizeToolbars => id(40905),
            Action::MouseModifiers => id(41356),
            Action::ToggleMainToolbar => id(scythe_core::menus::OPEN_MAIN_TOOLBAR),
            Action::Toolbar(n) => id(scythe_core::menus::open_toolbar_command(n as usize).unwrap_or_default()),
            Action::LoadScreenset(n) => id(scythe_session::actions::LOAD_SCREENSET + n as u32 - 1),
            Action::SaveScreenset(n) => id(scythe_session::actions::SAVE_SCREENSET + n as u32 - 1),
            Action::Edit(n) => id(n),
        }
    }

    /// The action a command runs, where the window has it: REAPER's alternatives for the same
    /// action (40757 splits like 40012) included.
    pub fn from_command(command: &str) -> Option<Self> {
        let c = scythe_session::actions::normalize(command);
        let alias = match c.as_str() {
            "40757" => Some(Action::SplitItem),
            "41588" | "42432" => Some(Action::GlueItems),
            "41929" => Some(Action::NewTab),
            _ => None,
        };
        static BY_COMMAND: std::sync::LazyLock<std::collections::HashMap<String, Action>> =
            std::sync::LazyLock::new(|| {
                Action::all().into_iter().map(|a| (a.command().to_ascii_uppercase(), a)).collect()
            });
        alias.or_else(|| BY_COMMAND.get(&c.to_ascii_uppercase()).copied())
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
            Action::ApplyTrackFx | Action::FreezeTrack | Action::UnfreezeTrack => None,
            Action::NextTake => k(Modifiers::NONE, Key::T),
            Action::OpenMidiEditor => k(Modifiers::NONE, Key::E),
            Action::ExportMidi => None,
            Action::ToggleItemMute => k(Modifiers::NONE, Key::U),
            Action::AddStretchMarker => k(Modifiers::NONE, Key::W),
            Action::RemoveStretchMarkers => None,
            Action::PreviousMarker => k(Modifiers::NONE, Key::OpenBracket),
            Action::NextMarker => k(Modifiers::NONE, Key::CloseBracket),
            Action::DeleteMarker => None,
            Action::InsertMarker => k(Modifiers::NONE, Key::M),
            Action::InsertTempoMarker => k(Modifiers::SHIFT, Key::C),
            Action::RemoveTempoMarker => None,
            Action::ToggleMuteTrack => k(Modifiers::ALT, Key::M),
            Action::ToggleSoloTrack => k(Modifiers::ALT, Key::S),
            Action::TogglePolarity | Action::ToggleSoloDefeat | Action::TrackGrouping => None,
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
            Action::RenderStems | Action::OpenInlineEditors | Action::CloseInlineEditors => None,
            Action::RenderProjectSettings | Action::QueueRender | Action::RunRenderQueue => None,
            Action::NewTab => k(CMD_ALT, Key::N),
            Action::CloseTab => k(CMD, Key::F4),
            Action::NextTab => k(Modifiers::CTRL, Key::Tab),
            Action::PreviousTab => k(Modifiers { shift: true, ..Modifiers::CTRL }, Key::Tab),
            Action::OpenInNewTab | Action::RegionManager | Action::RegionRenderMatrix | Action::PerformanceMeter => {
                None
            }
            Action::MediaExplorer | Action::ProjectBay | Action::CleanProjectDirectory | Action::ThemeColors => None,
            Action::LoadDemo => None,
            Action::ToggleMixer => k(CMD, Key::M),
            Action::ToggleEnvelopeLanes => None,
            Action::ToggleBypassAutomation => None,
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
            Action::NamPanel => k(Modifiers::ALT, Key::N),
            Action::NamPrevious => k(Modifiers::ALT, Key::ArrowLeft),
            Action::NamNext => k(Modifiers::ALT, Key::ArrowRight),
            Action::NamSnapshotA => k(Modifiers::ALT, Key::Num1),
            Action::NamSnapshotB => k(Modifiers::ALT, Key::Num2),
            Action::NamSnapshotC => k(Modifiers::ALT, Key::Num3),
            Action::NamSnapshotD => k(Modifiers::ALT, Key::Num4),
            Action::ShowMonitorMixes | Action::ShowTempoEnvelope | Action::NewMonitorMix | Action::CreateCueBus => None,
            Action::ToggleSetlist => k(Modifiers::NONE, Key::L),
            Action::SetlistPlay => k(Modifiers::NONE, Key::Enter),
            Action::SetlistNext => k(Modifiers::NONE, Key::N),
            Action::SetlistPrevious => k(Modifiers::NONE, Key::P),
            Action::ToggleMarkerActions => None,
            Action::ShowVirtualKeyboard => k(Modifiers::ALT, Key::B),
            Action::MidiDevices | Action::ToggleTimecodeSync => None,
            Action::Preferences => k(CMD, Key::P),
            Action::AudioDeviceSettings => None,
            Action::ShowActionList
            | Action::CustomizeToolbars
            | Action::MouseModifiers
            | Action::ToggleMainToolbar
            | Action::Toolbar(_)
            | Action::LoadScreenset(_)
            | Action::SaveScreenset(_)
            | Action::Edit(_) => None,
        }
    }

    /// Whether the shortcut works on the page shown: the setlist's bare N, P and Enter only on
    /// its own page, so they do not start a song from a dialog or the arrange view.
    pub fn shortcut_active(self, on_setlist: bool) -> bool {
        on_setlist || !matches!(self, Action::SetlistPlay | Action::SetlistNext | Action::SetlistPrevious)
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
                use scythe_core::recording::ProjectRecordMode;
                let next = match ProjectRecordMode::of(&app.project) {
                    ProjectRecordMode::TimeSelectionPunch => ProjectRecordMode::Normal,
                    _ => ProjectRecordMode::TimeSelectionPunch,
                };
                app.session.set_record_mode(next);
            }
            Action::ToggleArm => {
                crate::ui::groups::toggle_selected(app, scythe_core::groups::GroupedControl::RecArm);
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
            Action::FreezeTrack => {
                if let Some(t) = app.sel.track {
                    app.busy = Some("Freezing…".into());
                    let result = app.freeze_track(t);
                    app.busy = None;
                    if let Err(e) = result {
                        app.notify(format!("Cannot freeze: {e}"));
                    }
                }
            }
            Action::UnfreezeTrack => {
                if let Some(t) = app.sel.track
                    && !app.unfreeze_track(t)
                {
                    app.notify("The track is not frozen");
                }
            }
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
            Action::AddStretchMarker => {
                let at = app.position();
                let items = app.selected_items();
                app.edit("Add stretch marker", |p| {
                    for it in p.tracks.iter_mut().flat_map(|t| t.items.iter_mut()).filter(|it| items.contains(&it.id)) {
                        let rel = at - it.position;
                        if rel <= 0.0 || rel >= it.length {
                            continue;
                        }
                        let i = it.active_take;
                        if let Some(take) = it.takes.get_mut(i) {
                            take.add_stretch_marker(rel);
                        }
                    }
                });
            }
            Action::RemoveStretchMarkers => {
                let items = app.selected_items();
                app.edit("Remove stretch markers", |p| {
                    for it in p.tracks.iter_mut().flat_map(|t| t.items.iter_mut()).filter(|it| items.contains(&it.id)) {
                        it.takes.iter_mut().for_each(|t| t.stretch_markers.clear());
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
            Action::ToggleMuteTrack | Action::ToggleSoloTrack | Action::TogglePolarity => {
                use scythe_core::groups::GroupedControl;
                let control = match self {
                    Action::ToggleMuteTrack => GroupedControl::Mute,
                    Action::ToggleSoloTrack => GroupedControl::Solo,
                    _ => GroupedControl::Polarity,
                };
                crate::ui::groups::toggle_selected(app, control);
            }
            Action::ToggleSoloDefeat => {
                if let Some(t) = app.sel.track {
                    app.edit("Toggle solo defeat", |p| {
                        if let Some(t) = p.track_mut(t) {
                            t.solo_defeat = !t.solo_defeat;
                        }
                    });
                }
            }
            Action::TrackGrouping => app.view.groups = Some(app.view.groups.unwrap_or(0)),
            Action::PreviousTrack | Action::NextTrack => {
                let shown = app.project.row_shows();
                let visible = |i: &usize| shown[*i] != scythe_core::folders::RowShow::Hidden;
                let next = match app.selected_track_index() {
                    Some(i) if self == Action::PreviousTrack => (0..i).rev().find(visible).or(Some(i)),
                    Some(i) => (i + 1..shown.len()).find(visible).or(Some(i)),
                    None => (0..shown.len()).find(visible),
                };
                if let Some(k) = next {
                    let id = app.project.tracks[k].id;
                    app.set_track_selection(vec![id], Some(id));
                }
            }
            Action::MoveTrackUp | Action::MoveTrackDown | Action::IndentTrack | Action::OutdentTrack => {
                let tracks = app.selected_tracks();
                if !tracks.is_empty() {
                    let label = self.label().trim_start_matches("Track: ").to_owned();
                    app.edit(&label, |p| match self {
                        Action::MoveTrackUp => _ = p.step_tracks(&tracks, true),
                        Action::MoveTrackDown => _ = p.step_tracks(&tracks, false),
                        _ => {
                            for &t in &tracks {
                                p.indent_track(t, self == Action::IndentTrack);
                            }
                        }
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
            Action::QueueRender => app.queue_render(),
            Action::RegionManager => app.view.regions = Some(crate::ui::regions::RegionsTab::List),
            Action::PerformanceMeter => app.view.performance = true,
            Action::MediaExplorer => {
                app.view.explorer = match app.view.explorer.take() {
                    Some(_) => {
                        app.session.preview_stop();
                        None
                    }
                    None => Some(crate::ui::explorer::ExplorerWindow::open()),
                }
            }
            Action::ProjectBay => app.view.bay = Some(crate::ui::bay::BayWindow::new(crate::ui::bay::BayTab::Media)),
            Action::CleanProjectDirectory => {
                app.view.bay = Some(crate::ui::bay::BayWindow::new(crate::ui::bay::BayTab::Clean))
            }
            Action::ThemeColors => app.view.theme_editor = true,
            Action::RegionRenderMatrix => app.view.regions = Some(crate::ui::regions::RegionsTab::Matrix),
            Action::NewTab => app.new_tab(),
            Action::OpenInNewTab => app.open_tab_dialog(),
            Action::CloseTab => app.close_tab(app.active_tab()),
            Action::NextTab => app.cycle_tab(1),
            Action::PreviousTab => app.cycle_tab(-1),
            Action::RunRenderQueue => app.run_render_queue(),
            Action::LoadDemo => app.load_demo(),
            Action::ToggleMixer => app.view.show_mixer = !app.view.show_mixer,
            Action::ToggleEnvelopeLanes => app.view.envelope_lanes = !app.view.envelope_lanes,
            Action::ToggleBypassAutomation => {
                let on = app.project.global_automation == Some(scythe_core::model::GlobalAutomation::Bypass);
                app.session.edit("Bypass all automation", |p| {
                    p.global_automation = (!on).then_some(scythe_core::model::GlobalAutomation::Bypass);
                });
            }
            Action::ShowTempoEnvelope => app.view.tempo_envelope = !app.view.tempo_envelope,
            Action::ShowMonitorMixes => {
                app.view.monitor_page = !app.view.monitor_page;
                app.view.setlist = false;
            }
            Action::NewMonitorMix => {
                app.view.monitor_wizard = Some(crate::ui::monitor::Wizard::monitor(&app.project));
            }
            Action::CreateCueBus => {
                app.view.cue_dialog = Some(crate::ui::monitor::Wizard::cue_bus(&app.project, app.sel.track));
            }
            Action::ToggleSnap => {
                crate::ui::editing::run(app, 1157);
                let state = if app.project.editing.grid.snap { "on" } else { "off" };
                app.notify(format!("Snap to grid {state}"));
            }
            Action::CycleGrid => {
                let steps = crate::app::GRID_STEPS;
                let now = steps.iter().position(|d| (d * 4.0 - app.project.editing.grid.qn).abs() < 1e-9);
                let next = steps[now.map_or(0, |i| (i + 1) % steps.len())];
                app.edit("Set grid", |p| {
                    p.editing.grid.qn = next * 4.0;
                    p.editing.grid.kind = scythe_core::edit::GridKind::Normal;
                });
                let label = app.project.editing.grid.label();
                app.session.notify(format!("Grid {label}"));
            }
            Action::ZoomIn => app.view.px_per_sec = (app.view.px_per_sec * 1.5).min(2000.0),
            Action::ZoomOut => app.view.px_per_sec = (app.view.px_per_sec / 1.5).max(2.0),
            Action::ZoomToProject => {
                let end = app.project.length().max(app.project.tempo.bar_seconds() * 8.0);
                app.view.fit = Some(scythe_core::TimeRange::new(0.0, end));
            }
            Action::TallerTracks => app.view.row_h = (app.view.row_h * 1.25).min(240.0),
            Action::ShorterTracks => app.view.row_h = (app.view.row_h / 1.25).max(30.0),
            Action::OpenInlineEditors => {
                for id in app.selected_items() {
                    if !app.view.inline_midi.contains(&id) {
                        crate::ui::midi_editor::toggle_inline(app, id);
                    }
                }
            }
            Action::CloseInlineEditors => {
                let selected = app.selected_items();
                app.view.inline_midi.retain(|id| !selected.contains(id));
            }
            Action::ShowPalette => app.palette = Some(PaletteState::default()),
            Action::NamPanel => match app.nam_focus() {
                Some(t) => {
                    let index = app.nam_index(&t).unwrap_or(0);
                    app.toggle_nam_panel(t.track, index);
                }
                None => match app.sel.track {
                    Some(track) => app.add_nam_slot(track),
                    None => app.notify("Select a track first"),
                },
            },
            Action::NamPrevious => app.nam_step(-1),
            Action::NamNext => app.nam_step(1),
            Action::NamSnapshotA => app.nam_recall_focused(0),
            Action::NamSnapshotB => app.nam_recall_focused(1),
            Action::NamSnapshotC => app.nam_recall_focused(2),
            Action::NamSnapshotD => app.nam_recall_focused(3),
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
            Action::ToggleSetlist => {
                app.view.setlist = !app.view.setlist;
                app.view.monitor_page = false;
            }
            Action::SetlistPlay => app.setlist_play(),
            Action::SetlistNext => app.setlist_step(true),
            Action::SetlistPrevious => app.setlist_step(false),
            Action::ShowVirtualKeyboard => app.view.keyboard_open = !app.view.keyboard_open,
            Action::MidiDevices => app.view.midi_devices_open = !app.view.midi_devices_open,
            Action::Preferences | Action::AudioDeviceSettings => crate::ui::audio_device::toggle(app),
            Action::ToggleTimecodeSync => {
                let on = !app.project.timecode_sync.enabled();
                app.edit("Toggle external timecode synchronization", |p| p.timecode_sync.set_enabled(on));
                app.notify(if on { "Chasing external timecode" } else { "External timecode synchronization off" });
            }
            Action::ToggleMarkerActions => {
                let on = !app.engine.marker_actions();
                app.engine.set_marker_actions(on);
                app.notify(if on { "Marker actions on" } else { "Marker actions off" });
            }
            Action::RunScript => app.run_script_dialog(),
            Action::ShowActionList => app.view.actions = Some(crate::ui::actions::ActionsWindow::default()),
            Action::CustomizeToolbars => app.view.toolbar_editor = Some(crate::ui::toolbars::ToolbarEditor::default()),
            Action::MouseModifiers => app.view.mouse_editor = !app.view.mouse_editor,
            Action::ToggleMainToolbar => app.view.main_toolbar = !app.view.main_toolbar,
            Action::Toolbar(n) => {
                if !app.view.toolbars.remove(&n) {
                    app.view.toolbars.insert(n);
                }
            }
            Action::LoadScreenset(n) => app.load_screenset(n),
            Action::SaveScreenset(n) => app.save_screenset(n),
            Action::Edit(n) => crate::ui::editing::run(app, n),
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

/// The keys that run `action`, as REAPER writes them.
pub fn shortcut_text(app: &ScytheApp, action: Action) -> String {
    crate::keys::keys_text(&app.session, &action.command())
}

/// Dispatches keyboard shortcuts unless a text field, an open menu or a graphics window takes
/// the keys: the shortcut file's bindings first, then Scythe's defaults.
pub fn handle_shortcuts(app: &mut ScytheApp, ctx: &egui::Context) {
    let menu_open = egui::Popup::is_any_open(ctx) || ctx.any_popup_open();
    if ctx.egui_wants_keyboard_input() || menu_open || app.palette.is_some() || crate::ui::gfx_view::takes_keys(app) {
        return;
    }
    crate::ui::nam::preview_keys(app, ctx);
    crate::ui::window::take_escape(ctx);
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
    let editor_keys = crate::ui::fx_editor::takes_keys(app);
    let presses: Vec<(usize, Option<String>)> = ctx.input(|i| {
        i.events
            .iter()
            .enumerate()
            .filter_map(|(n, e)| match e {
                egui::Event::Key { key, pressed: true, modifiers, .. }
                    if !(editor_keys && crate::ui::fx_editor::is_editor_key(*key)) =>
                {
                    let chord = crate::keys::chord(*key, *modifiers)?;
                    Some((n, crate::keys::command_for(app, chord)))
                }
                _ => None,
            })
            .collect()
    });
    let mut taken = Vec::new();
    let mut commands = Vec::new();
    for (n, command) in presses {
        let Some(command) = command else { continue };
        if matches!(Action::from_command(&command), Some(Action::CopyItem | Action::CutItem | Action::PasteItem)) {
            continue;
        }
        taken.push(n);
        commands.push(command);
    }
    if !taken.is_empty() {
        ctx.input_mut(|i| {
            let mut n = 0;
            i.events.retain(|_| {
                n += 1;
                !taken.contains(&(n - 1))
            });
        });
    }
    for c in commands {
        app.run_command(&c);
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

/// The ⌘K action search overlay: every action of the main section, custom actions and
/// scripts included. It sits above the floating windows.
pub fn show(app: &mut ScytheApp, ui: &mut egui::Ui) {
    if app.palette.is_none() {
        return;
    }
    let screen = ui.max_rect();
    egui::Area::new(egui::Id::unique("action-palette")).order(egui::Order::Foreground).fixed_pos(screen.min).show(
        ui.ctx(),
        |ui| {
            ui.set_min_size(screen.size());
            overlay(app, ui, screen);
        },
    );
}

fn overlay(app: &mut ScytheApp, ui: &mut egui::Ui, screen: Rect) {
    let Some(query) = app.palette.as_ref().map(|s| s.query.clone()) else { return };
    let results: Vec<(String, String, String)> = app
        .session
        .action_list()
        .into_iter()
        .filter(|a| a.section == scythe_core::keymap::SECTION_MAIN && matches(&a.name, &query))
        .map(|a| {
            let keys = crate::keys::keys_text(&app.session, &a.command);
            (a.command, a.name, keys)
        })
        .collect();
    let Some(state) = app.palette.as_mut() else { return };
    let ctx = ui.ctx().clone();
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
    for (row, (command, name, keys)) in results.iter().enumerate().skip(first).take(visible) {
        let r = Rect::from_min_size(
            egui::pos2(rect.left() + 8.0, input_rect.bottom() + 8.0 + (row - first) as f32 * row_h),
            vec2(width - 16.0, row_h - 4.0),
        );
        let resp = ui.allocate_rect(r, Sense::click());
        if row == state.selected || resp.hovered() {
            painter.rect_filled(r, CornerRadius::same(12), theme::RAISED);
        }
        let keys_w =
            if keys.is_empty() { 0.0 } else { crate::ui::widgets::text_width(ui, keys, theme::mono(11.0)) + 20.0 };
        let label = crate::ui::widgets::truncate(ui, name, &theme::regular(13.0), r.width() - 28.0 - keys_w);
        painter.text(r.left_center() + vec2(14.0, 0.0), Align2::LEFT_CENTER, label, theme::regular(13.0), theme::TEXT);
        if !keys.is_empty() {
            painter.text(
                r.right_center() - vec2(14.0, 0.0),
                Align2::RIGHT_CENTER,
                keys,
                theme::mono(11.0),
                theme::TEXT_DIM,
            );
        }
        if resp.clicked() {
            run = Some(command.clone());
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
        run = results.get(state.selected).map(|r| r.0.clone());
    }
    if escape || (backdrop.clicked() && !backdrop.interact_pointer_pos().is_some_and(|p| rect.contains(p))) {
        app.palette = None;
    }
    if let Some(command) = run {
        app.palette = None;
        app.run_command(&command);
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
    fn setlist_keys_only_work_on_the_setlist_page() {
        for a in [Action::SetlistPlay, Action::SetlistNext, Action::SetlistPrevious] {
            assert!(!a.shortcut_active(false), "{a:?}");
            assert!(a.shortcut_active(true), "{a:?}");
        }
        assert!(Action::ToggleSetlist.shortcut_active(false));
        assert!(Action::PlayPause.shortcut_active(false));
    }

    #[test]
    fn every_action_is_in_the_registry_and_back() {
        for a in Action::all() {
            let c = a.command();
            assert!(scythe_session::actions::builtin().iter().any(|b| b.command == c), "{a:?} ({c}) not registered");
            assert_eq!(Action::from_command(&c), Some(a), "{c}");
        }
        for b in scythe_session::actions::builtin() {
            if b.runs == scythe_session::actions::Runs::Window && b.section == scythe_core::keymap::SECTION_MAIN {
                assert!(Action::from_command(&b.command).is_some(), "{} ({}) has no window action", b.name, b.command);
            }
        }
    }

    #[test]
    fn every_shortcut_is_unique() {
        let mut seen = std::collections::HashSet::new();
        for a in Action::all() {
            if let Some(s) = a.shortcut() {
                assert!(seen.insert(format!("{s:?}")), "duplicate shortcut for {a:?}");
            }
        }
    }
}
