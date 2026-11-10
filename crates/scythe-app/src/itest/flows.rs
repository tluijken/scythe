//! The main flows, clicked through as a user does.

use egui::{Key, Modifiers, vec2};

use super::*;

const CMD: Modifiers = Modifiers::COMMAND;

fn cmd_shift() -> Modifiers {
    Modifiers { shift: true, ..Modifiers::COMMAND }
}

fn fx_state(t: &TestApp, track: &str, index: usize) -> String {
    format!("{:?}", t.track(track).fx[index].raw)
}

#[test]
fn open_project_shows_its_tracks_and_items() {
    let t = TestApp::live("open_project");
    for label in ["Track Drums", "Track Bass", "Track Keys", "Item drum loop", "Item bass line", "Item keys"] {
        assert!(t.has(label), "{label} missing: {:#?}", t.labels());
    }
    assert_eq!(t.project().tracks.len(), 3);
}

#[test]
fn click_selects_an_item_and_its_track() {
    let mut t = TestApp::live("select_item");
    t.click("Item bass line");
    let id = t.item("bass line").id;
    let track = t.track("Bass").id;
    assert_eq!(t.app().sel.item, Some(id));
    assert_eq!(t.app().sel.track, Some(track));
    t.click("Track Keys");
    let keys = t.track("Keys").id;
    assert_eq!(t.app().sel.track, Some(keys));
}

#[test]
fn dragging_an_item_moves_it_and_undo_redo_follow() {
    let mut t = TestApp::live("move_item");
    let r = t.rect("Item drum loop");
    t.drag(r.center(), r.center() + vec2(120.0, 0.0));
    let moved = t.item("drum loop").position;
    assert!(moved > 0.5, "the item stayed at {moved}");
    t.key_mod(CMD, Key::Z);
    assert_eq!(t.item("drum loop").position, 0.0, "undo puts it back");
    t.key_mod(cmd_shift(), Key::Z);
    assert_eq!(t.item("drum loop").position, moved, "redo moves it again");
}

#[test]
fn dragging_an_item_down_moves_it_to_the_next_track() {
    let mut t = TestApp::live("move_item_track");
    let r = t.rect("Item drum loop");
    let lane = t.rect("Lane Bass");
    t.drag(r.center(), egui::pos2(r.center().x, lane.center().y));
    assert!(t.track("Bass").items.iter().any(|i| i.name() == "drum loop"), "{:?}", t.track("Bass").items);
    assert!(t.track("Drums").items.is_empty());
}

#[test]
fn s_splits_the_selected_item_at_the_cursor() {
    let mut t = TestApp::live("split_item");
    t.click("Item drum loop");
    t.app().set_cursor(2.0);
    t.frames(2);
    t.key(Key::S);
    let items: Vec<(f64, f64)> = t.track("Drums").items.iter().map(|i| (i.position, i.length)).collect();
    assert_eq!(items.len(), 2, "{items:?}");
    assert!((items[0].1 - 2.0).abs() < 1e-9 && (items[1].0 - 2.0).abs() < 1e-9, "{items:?}");
    t.key_mod(CMD, Key::Z);
    assert_eq!(t.track("Drums").items.len(), 1);
}

#[test]
fn add_track_button_and_shortcut_add_tracks() {
    let mut t = TestApp::live("add_track");
    t.click("Add track");
    assert_eq!(t.project().tracks.len(), 4);
    t.key_mod(CMD, Key::T);
    assert_eq!(t.project().tracks.len(), 5);
    t.key_mod(CMD, Key::Z);
    assert_eq!(t.project().tracks.len(), 4);
}

#[test]
fn fx_browser_adds_reaeq_and_its_editor_drags_a_band() {
    let mut t = TestApp::live("fx_reaeq");
    t.click("Track Drums");
    t.click("Add plugin");
    t.click("Equalizer");
    assert_eq!(t.track("Drums").fx.len(), 1);
    assert!(t.track("Drums").fx[0].name.contains("ReaEQ"), "{}", t.track("Drums").fx[0].name);
    let before = fx_state(&t, "Drums", 0);
    t.click_role(Role::Button, "Edit");
    let drums = t.track("Drums").id;
    assert!(crate::ui::fx_editor::is_open(t.app(), Some(drums), 0), "the editor opened");
    let band = t.rect("Band 2");
    t.drag(band.center(), band.center() + vec2(0.0, -40.0));
    assert_ne!(before, fx_state(&t, "Drums", 0), "dragging the band changed the plugin's state");
    t.key_mod(CMD, Key::Z);
    assert_eq!(fx_state(&t, "Drums", 0), before, "undo restores the band");
}

#[test]
fn guitar_rig_opens_the_nam_panel() {
    let mut t = TestApp::live("rig_panel");
    t.click("Track Bass");
    t.click("Add plugin");
    t.click("Guitar rig");
    assert_eq!(t.track("Bass").fx.len(), 1);
    assert!(t.app().nam.panel.is_some(), "a new rig opens in the panel");
    t.click_role(Role::Button, "Edit");
    assert!(t.app().nam.panel.is_none(), "Edit toggles the panel");
    t.click_role(Role::Button, "Edit");
    assert!(t.app().nam.panel.is_some());
    t.key(Key::Escape);
    assert!(t.app().nam.panel.is_none(), "Escape closes the panel");
}

#[test]
fn setlist_page_builds_a_setlist_plays_and_steps() {
    let mut t = TestApp::live("setlist_page");
    t.click("Setlist (L)");
    assert!(t.app().view.setlist);
    assert!(t.has("Song Opener") && t.has("Song Ballad"), "{:#?}", t.labels());
    t.click("New setlist");
    for song in ["Song Opener", "Song Ballad"] {
        t.h.get_by_label(song).hover();
        t.frames(2);
        t.click("Add to the setlist");
    }
    assert!(t.has("Entry Opener") && t.has("Entry Ballad"), "{:#?}", t.labels());
    t.click_role(Role::Button, "Next song (N)");
    assert_eq!(t.app().live.player.entry, 1);
    t.click_role(Role::Button, "Play song (Enter)");
    t.until("playback", 5.0, |t| t.app().is_playing());
    let pos = t.app().position();
    assert!((6.0..10.0).contains(&pos), "plays Ballad from its marker, at {pos}");
    t.click_role(Role::Button, "Stop (Esc)");
    assert!(!t.app().is_playing());
}

#[test]
fn midi_editor_draws_a_note() {
    let mut t = TestApp::live("midi_draw");
    let r = t.rect("Item keys");
    t.double_click_at(r.center());
    assert!(t.app().view.midi_editor.is_some(), "double-click opens the MIDI editor");
    let notes = |t: &mut TestApp| {
        let item = t.item("keys").clone();
        t.app().midi_notes(&item).map_or(0, |n| n.len())
    };
    let before = notes(&mut t);
    let roll = t.rect("Piano roll");
    t.click_at(roll.center() + vec2(40.0, 0.0));
    assert_eq!(notes(&mut t), before + 1, "a click in the roll draws a note");
    t.key_mod(CMD, Key::Z);
    assert_eq!(notes(&mut t), before);
}

#[test]
fn ctrl_drag_copies_a_note_and_alt_drag_sets_its_velocity() {
    let mut t = TestApp::live("midi_copy");
    let r = t.rect("Item keys");
    t.double_click_at(r.center());
    let notes = |t: &mut TestApp| {
        let item = t.item("keys").clone();
        t.app().midi_notes(&item).unwrap_or_default()
    };
    let before = notes(&mut t).len();
    let at = t.rect("Piano roll").center() + vec2(40.0, 0.0);
    t.click_at(at);
    assert_eq!(notes(&mut t).len(), before + 1);
    t.drag_mod(at, at + vec2(90.0, 0.0), CMD);
    assert_eq!(notes(&mut t).len(), before + 2, "Ctrl+drag copies the note, as REAPER's piano roll does");
    let velocities = |t: &mut TestApp| notes(t).iter().map(|n| n.velocity).collect::<Vec<_>>();
    let was = velocities(&mut t);
    t.drag_mod(at, at - vec2(0.0, 20.0), Modifiers::ALT);
    assert_ne!(velocities(&mut t), was, "Alt+drag edits the velocity");
    assert_eq!(notes(&mut t).len(), before + 2, "and moves nothing");
}

#[test]
fn folder_button_and_header_drag_make_folders() {
    let mut t = TestApp::live("folders");
    let folder_tip = "Normal track: click to make it a folder holding the tracks below";
    t.h.get_all_by_label(folder_tip).next().unwrap().click();
    t.frames(3);
    assert_eq!(t.track("Drums").folder_depth, 1, "Drums became a folder");
    let keys = t.rect("Track Keys");
    let drums = t.rect("Track Drums");
    t.drag(keys.center(), egui::pos2(keys.center().x, drums.top() + 2.0));
    let names: Vec<&str> = t.project().tracks.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["Keys", "Drums", "Bass"], "dragging the header moved the track");
}

#[test]
fn action_palette_searches_and_runs() {
    let mut t = TestApp::live("palette");
    t.key_mod(CMD, Key::K);
    assert!(t.app().palette.is_some());
    t.type_text("insert new track");
    t.key(Key::Enter);
    assert!(t.app().palette.is_none(), "running an action closes the palette");
    assert_eq!(t.project().tracks.len(), 4);
}

#[test]
fn transport_buttons_play_pause_and_go_to_start() {
    let mut t = TestApp::live("transport");
    t.click("Play (Space)");
    t.until("playback", 5.0, |t| t.app().is_playing());
    t.until("time to pass", 5.0, |t| t.app().position() > 0.1);
    t.click("Pause (Space)");
    assert!(!t.app().is_playing());
    t.click("Go to start (Home)");
    assert_eq!(t.app().position(), 0.0);
    t.click("Toggle repeat (R)");
    assert!(t.project().loop_enabled);
}

#[test]
fn escape_closes_the_topmost_window_first() {
    let mut t = TestApp::live("escape");
    t.key_mod(CMD, Key::K);
    assert!(t.app().palette.is_some());
    t.key(Key::Escape);
    assert!(t.app().palette.is_none(), "Escape closes the palette");
    t.click("Track Drums");
    t.click("Add plugin");
    t.click("Equalizer");
    t.click_role(Role::Button, "Edit");
    let drums = t.track("Drums").id;
    assert!(crate::ui::fx_editor::is_open(t.app(), Some(drums), 0));
    // Played from elsewhere (a footswitch, the web UI): a click on Play would focus the main
    // window, where Escape stops.
    t.app().toggle_play();
    t.until("playback", 5.0, |t| t.app().is_playing());
    t.key(Key::Escape);
    assert!(!crate::ui::fx_editor::is_open(t.app(), Some(drums), 0), "Escape closes the focused editor");
    assert!(t.app().is_playing(), "and leaves the transport alone");
    t.key(Key::Escape);
    assert!(!t.app().is_playing(), "with no window open Escape stops");
}

#[test]
fn project_tabs_open_switch_and_close() {
    let mut t = TestApp::live("tabs");
    t.key_mod(Modifiers { alt: true, ..Modifiers::COMMAND }, Key::N);
    assert_eq!(t.app().tab_count(), 2);
    assert!(t.has("New project tab"), "the tab strip shows: {:#?}", t.labels());
    assert!(t.project().tracks.is_empty(), "the new tab is empty");
    t.click("Project tab live");
    assert_eq!(t.project().tracks.len(), 3, "back on the live project");
    t.h.get_all_by_label("Close tab").last().unwrap().click();
    t.frames(3);
    assert_eq!(t.app().tab_count(), 1);
}

#[test]
fn saved_edits_survive_reopening() {
    let mut t = TestApp::live("save_reopen");
    let r = t.rect("Item drum loop");
    t.drag(r.center(), r.center() + vec2(120.0, 0.0));
    let moved = t.item("drum loop").position;
    t.click("Track Drums");
    t.click("Add plugin");
    t.click("Equalizer");
    t.click("Add track");
    t.key_mod(CMD, Key::S);
    let path = t.project().path.clone().unwrap();
    let mut again = TestApp::open("save_reopen_again", &path);
    again.wait_media();
    assert_eq!(again.project().tracks.len(), 4);
    assert_eq!(again.item("drum loop").position, moved);
    assert!(again.track("Drums").fx[0].name.contains("ReaEQ"));
    assert!(again.has("Item drum loop"));
}

#[test]
fn monitor_wizard_creates_a_mix_for_a_musician() {
    let mut t = TestApp::live("monitor_wizard");
    t.click("Monitor mixes (in-ear)");
    assert!(t.app().view.monitor_page);
    t.click("Create the first monitor mix");
    assert!(t.app().view.monitor_wizard.is_some());
    let field =
        t.h.get_all_by_role(Role::TextInput).find(|n| n.value().as_deref() == Some("Musician 1")).unwrap().rect();
    t.click_at(field.center());
    t.key_mod(CMD, Key::A);
    t.type_text("Drummer");
    t.click_role(Role::CheckBox, "Keys");
    t.click_role(Role::Button, "Create mix");
    assert!(t.app().view.monitor_wizard.is_none(), "the wizard closes");
    let mixes = t.project().monitor_mixes();
    assert_eq!(mixes.len(), 1, "{mixes:?}");
    assert_eq!(mixes[0].musician, "Drummer");
    let sources = t.project().monitor_sources(mixes[0].track);
    assert_eq!(sources.len(), 2, "Drums and Bass feed it, Keys was unticked: {sources:?}");
    t.key_mod(CMD, Key::Z);
    assert!(t.project().monitor_mixes().is_empty());
}

#[test]
fn toolbar_buttons_and_the_action_list_run_actions() {
    let mut t = TestApp::live("toolbar_actions");
    t.app().view.main_toolbar = true;
    t.frames(3);
    t.click_role(Role::Button, "Repeat");
    assert!(t.project().loop_enabled, "the toolbar's Repeat toggles repeat");
    t.click_role(Role::Button, "Actions");
    assert!(t.app().view.actions.is_some(), "the toolbar opens the action list");
    t.h.get_by(|n| n.data().placeholder() == Some("name or command id")).focus();
    t.frames(2);
    t.type_text("40001");
    let name = t.app().session.action_name("40001").expect("a name for 40001");
    t.click(&name);
    t.click_role(Role::Button, "Run");
    assert_eq!(t.project().tracks.len(), 4, "running Insert new track added one");
    t.key(Key::Escape);
    assert!(t.app().view.actions.is_none(), "Escape closes the focused action list");
}

#[test]
fn escape_closes_each_window_and_leaves_playback_alone() {
    use crate::palette::Action;
    type Open = fn(&mut ScytheApp) -> bool;
    let windows: [(&str, Action, Open); 11] = [
        ("track groups", Action::TrackGrouping, |a| a.view.groups.is_some()),
        ("region manager", Action::RegionManager, |a| a.view.regions.is_some()),
        ("media explorer", Action::MediaExplorer, |a| a.view.explorer.is_some()),
        ("project bay", Action::ProjectBay, |a| a.view.bay.is_some()),
        ("theme colours", Action::ThemeColors, |a| a.view.theme_editor),
        ("virtual keyboard", Action::ShowVirtualKeyboard, |a| a.view.keyboard_open),
        ("MIDI devices", Action::MidiDevices, |a| a.view.midi_devices_open),
        ("audio device", Action::AudioDeviceSettings, |a| a.view.audio_device.is_some()),
        ("action list", Action::ShowActionList, |a| a.view.actions.is_some()),
        ("toolbar editor", Action::CustomizeToolbars, |a| a.view.toolbar_editor.is_some()),
        ("mouse modifiers", Action::MouseModifiers, |a| a.view.mouse_editor),
    ];
    let mut failed = Vec::new();
    for (name, action, open) in windows {
        let mut t = TestApp::live(&format!("escape_windows/{name}"));
        t.app().toggle_play();
        t.until("playback", 5.0, |t| t.app().is_playing());
        action.run(t.app());
        t.frames(3);
        if !open(t.app()) {
            failed.push(format!("{name}: did not open"));
            continue;
        }
        t.key(Key::Escape);
        if open(t.app()) {
            failed.push(format!("{name}: Escape left it open"));
        }
        if !t.app().is_playing() {
            failed.push(format!("{name}: Escape stopped the transport"));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

#[test]
fn every_input_widget_of_the_main_views_has_a_name() {
    let mut t = TestApp::live("accessibility");
    t.click("Track Drums");
    t.click("Add plugin");
    t.click("Equalizer");
    t.click_role(Role::Button, "Edit");
    t.h.check_accessibility();
    for page in ["Setlist (L)", "Monitor mixes (in-ear)", "Envelope lanes"] {
        t.click(page);
        t.h.check_accessibility();
    }
}

#[test]
fn the_audio_device_window_opens_from_the_status_pill_and_the_action() {
    use crate::palette::Action;
    let mut t = TestApp::live("audio_device");
    t.click("Audio device settings");
    assert!(t.app().view.audio_device.is_some(), "the status pill opens the audio device window");
    for label in ["Audio system", "Input channels", "Input device", "Output channels", "Output device", "Sample rate"] {
        assert!(t.has(label), "{label} has no name: {:?}", t.labels());
    }
    for label in ["Blocksize", "Apply", "Refresh", "Input channel name aliasing", "Edit output names"] {
        assert!(t.has(label), "{label} has no name: {:?}", t.labels());
    }
    t.h.check_accessibility();
    t.key(Key::Escape);
    assert!(t.app().view.audio_device.is_none(), "Escape closes it");

    Action::AudioDeviceSettings.run(t.app());
    t.frames(3);
    assert!(t.app().view.audio_device.is_some(), "the action opens it");
    t.key(Key::Escape);
    t.key_mod(Modifiers::COMMAND, Key::P);
    assert!(t.app().view.audio_device.is_some(), "Ctrl+P (Options: Preferences...) opens it");

    let tracks = t.project().tracks.len();
    let w = t.app().view.audio_device.as_mut().unwrap();
    w.draft.system = "Dummy".into();
    (w.draft.sample_rate, w.draft.block_size) = (44_100, 256);
    t.frames(2);
    t.click_role(Role::Button, "Apply");
    t.frames(3);
    let app = t.app();
    assert_eq!(app.view.audio_device.as_ref().and_then(|w| w.error.clone()), None);
    assert_eq!((app.engine.sample_rate(), app.engine.info().name.as_str()), (44_100, "Dummy Audio"));
    assert_eq!(app.engine.health().block_size, Some(256));
    assert_eq!(t.project().tracks.len(), tracks, "the project stays open");
    assert!(t.has("Playing: Dummy Audio · 44100 Hz · 256 frames · 0 in / 2 out"), "{:?}", t.labels());
}
