//! UI integration tests: the whole window driven through egui's test harness, by the labels
//! and roles AccessKit sees, on a silent engine. Built with `--features itest` only (see
//! docs/testing.md), so `cargo test` stays fast.

mod agents;
mod corpus;
mod flows;
mod nam;
mod scythe_eq;
mod snapshot;
mod tone3000_mock;

use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{By, NodeT, Queryable};
use scythe_core::{Item, Project, Track};

use crate::app::ScytheApp;

/// The window size the tests draw at: the app's default.
pub const SIZE: egui::Vec2 = egui::vec2(1440.0, 940.0);

/// A folder of the worktree's `target` for everything the tests write, so nothing lands in
/// the user's home: projects, preferences, the NAM library, TONE3000 sessions.
pub fn scratch() -> PathBuf {
    let dir = std::env::var_os("SCYTHE_ITEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/itest"));
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

/// Points the XDG folders into [`scratch`] before anything reads them.
fn isolate() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let root = scratch();
        for (var, dir) in [("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"), ("XDG_CACHE_HOME", "cache")] {
            let dir = root.join(dir);
            std::fs::create_dir_all(&dir).unwrap();
            // SAFETY: std's environment functions take a lock of their own, and nothing in
            // the test binary reads the environment through libc while the tests start.
            unsafe { std::env::set_var(var, dir) };
        }
    });
}

/// A fresh folder for one test.
pub fn test_dir(name: &str) -> PathBuf {
    isolate();
    let dir = scratch().join("tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A second of 220 Hz at 48 kHz, the audio the fixture's items play.
pub fn write_tone(path: &Path) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for i in 0..48_000 * 4 {
        let s = (i as f32 * 220.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.25;
        w.write_sample((s * i16::MAX as f32) as i16).unwrap();
    }
    w.finalize().unwrap();
}

/// Copies the `live.RPP` fixture (three tracks, two songs with stop markers, a MIDI item)
/// with its audio into `dir`.
pub fn live_fixture(dir: &Path) -> PathBuf {
    let path = dir.join("live.RPP");
    std::fs::write(&path, include_str!("../../tests/fixtures/itest/live.RPP")).unwrap();
    write_tone(&dir.join("tone.wav"));
    path
}

/// The app in the test harness.
pub struct TestApp {
    pub h: Harness<'static, ScytheApp>,
    pub dir: PathBuf,
}

impl TestApp {
    /// A window with the `live.RPP` fixture open, as `scythe live.RPP` opens it.
    pub fn live(name: &str) -> Self {
        let dir = test_dir(name);
        let path = live_fixture(&dir);
        let mut app = Self::build(dir, |app| app.open_path(&path));
        app.wait_media();
        app
    }

    /// A window opening `path`, whatever it holds.
    pub fn open(name: &str, path: &Path) -> Self {
        let dir = test_dir(name);
        let path = path.to_owned();
        Self::build(dir, move |app| app.open_path(&path))
    }

    fn build(dir: PathBuf, setup: impl FnOnce(&mut ScytheApp)) -> Self {
        isolate();
        let mut app = ScytheApp::with_devices(egui::Context::default(), &Default::default(), true);
        app.no_input = true;
        app.session.script_runner = Some(crate::script_host::control_runner());
        // Tests run side by side in one process and share its XDG folders: the NAM library and
        // TONE3000 settings one test saves must not reach another.
        app.nam = crate::nam::NamHub::with(
            Default::default(),
            dir.join("nam"),
            crate::nam::tone3000::TokenStore::new(dir.join("tone3000-session.json")),
            dir.join("nam-preview"),
        );
        setup(&mut app);
        let h = Harness::builder()
            .with_size(SIZE)
            .with_step_dt(1.0 / 30.0)
            .with_max_steps(64)
            .with_accessibility_check(false)
            .with_context_setup(crate::theme::install)
            .build_ui_state(
                |ui, app: &mut ScytheApp| {
                    app.session.acquire();
                    app.poll();
                    crate::ui::frame(app, ui);
                },
                app,
            );
        let mut t = Self { h, dir };
        let ctx = t.h.ctx.clone();
        t.app().ctx = ctx;
        t.frames(3);
        t
    }

    pub fn app(&mut self) -> &mut ScytheApp {
        self.h.state_mut()
    }

    pub fn project(&self) -> &Project {
        &self.h.state().project
    }

    /// Runs `n` frames.
    pub fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.h.step();
        }
    }

    /// Runs frames until `done` holds, failing after `secs` of wall time.
    pub fn until(&mut self, what: &str, secs: f64, mut done: impl FnMut(&mut Self) -> bool) {
        let start = Instant::now();
        while !done(self) {
            assert!(start.elapsed().as_secs_f64() < secs, "timed out waiting for {what}");
            self.h.step();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Waits for the project's media to be read.
    pub fn wait_media(&mut self) {
        self.until("media", 30.0, |t| !t.h.state().media_loading());
        self.frames(2);
    }

    /// Clicks the widget AccessKit knows by exactly `label` and draws the result. The widget
    /// may need a few frames to appear: what the last action asked for can come from the engine
    /// or a worker thread, which a loaded machine answers late.
    #[track_caller]
    pub fn click(&mut self, label: &str) {
        self.wait_has(label);
        let wanted = label.to_owned();
        self.settle(label, move |t| t.h.query_by_label(&wanted).map(|n| n.rect()));
        let node = self.h.get_by_label(label);
        let (id, at) = (node.accesskit_node().locate().0, node.rect().center());
        self.press(label, id, at);
    }

    /// Clicks the `role` widget labelled `label` (a button among labels with the same text).
    #[track_caller]
    pub fn click_role(&mut self, role: Role, label: &str) {
        let wanted = label.to_owned();
        self.settle(label, move |t| t.h.query_by_role_and_label(role, &wanted).map(|n| n.rect()));
        let node = self.h.get_by_role_and_label(role, label);
        let (id, at) = (node.accesskit_node().locate().0, node.rect().center());
        self.press(label, id, at);
    }

    /// Clicks the first widget labelled `label`, for labels several widgets share.
    #[track_caller]
    pub fn click_first(&mut self, label: &str) {
        self.click_nth(label, |_| 0);
    }

    #[track_caller]
    pub fn click_last(&mut self, label: &str) {
        self.click_nth(label, |n| n - 1);
    }

    /// Clicks widget `pick(count)` of those labelled `label`.
    #[track_caller]
    fn click_nth(&mut self, label: &str, pick: fn(usize) -> usize) {
        let wanted = label.to_owned();
        self.until(label, 10.0, move |t| t.h.query_all_by_label(&wanted).next().is_some());
        let wanted = label.to_owned();
        let nth = move |t: &Self| {
            let nodes: Vec<_> = t.h.query_all_by_label(&wanted).collect();
            nodes.get(pick(nodes.len())).map(|n| (n.accesskit_node().locate().0, n.rect()))
        };
        self.settle(label, |t| nth(t).map(|(_, r)| r));
        let (id, rect) = nth(self).expect("the widget stays");
        self.press(label, id, rect.center());
    }

    /// A click at `at` on the widget AccessKit knows as `target`, a frame for each event as a
    /// hand gives them. egui pairs the release with the press by widget id; a widget earlier in
    /// the frame that comes and goes shifts the automatic ids after it, and the click then goes
    /// to whichever widget took the id, so that fails here rather than as a missing effect.
    #[track_caller]
    fn press(&mut self, what: &str, target: egui::accesskit::NodeId, at: egui::Pos2) {
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        self.h.event(egui::Event::PointerMoved(at));
        self.h.step();
        self.h.event(button(true));
        self.h.step();
        self.h.event(button(false));
        self.h.step();
        let clicked = self.h.ctx.viewport(|v| v.interact_widgets.clicked);
        if let Some(id) = clicked {
            assert_eq!(
                id.accesskit_id(),
                target,
                "the click on {what:?} went to another widget ({id:?}): an id shifted between frames"
            );
        }
        self.frames(2);
    }

    pub fn has(&self, label: &str) -> bool {
        self.h.query_by_label(label).is_some()
    }

    /// Runs frames until `rect` reads the same two frames running, so a click lands where the
    /// widget is drawn rather than where a shifting layout had it a frame ago.
    fn settle(&mut self, what: &str, mut rect: impl FnMut(&Self) -> Option<egui::Rect>) {
        let mut last = rect(self);
        self.h.step();
        self.until(what, 10.0, move |t| {
            let now = rect(t);
            let still = now.is_some() && now == last;
            last = now;
            still
        });
    }

    /// Runs frames until a widget labelled `label` shows, failing after 10 s.
    #[track_caller]
    pub fn wait_has(&mut self, label: &str) {
        let wanted = label.to_owned();
        self.until(label, 10.0, move |t| t.has(&wanted));
    }

    /// Where the widget labelled `label` is on screen.
    #[track_caller]
    pub fn rect(&self, label: &str) -> egui::Rect {
        self.h.get_by_label(label).rect()
    }

    pub fn key(&mut self, key: egui::Key) {
        self.h.key_press(key);
        self.frames(3);
    }

    pub fn key_mod(&mut self, modifiers: egui::Modifiers, key: egui::Key) {
        self.h.key_press_modifiers(modifiers, key);
        self.frames(3);
    }

    /// Types `text` into the focused text field.
    pub fn type_text(&mut self, text: &str) {
        self.h.event(egui::Event::Text(text.to_owned()));
        self.frames(3);
    }

    /// A press at `from`, a drag through `steps` moves to `to` and a release, as a hand does.
    pub fn drag(&mut self, from: egui::Pos2, to: egui::Pos2) {
        self.drag_mod(from, to, egui::Modifiers::NONE);
    }

    pub fn drag_mod(&mut self, from: egui::Pos2, to: egui::Pos2, modifiers: egui::Modifiers) {
        // Held through the drag: widgets read the held modifiers, which the pointer events do not
        // change.
        self.h.event(egui::Event::ModifiersChanged(modifiers));
        let button =
            |pressed, pos| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers };
        self.h.event(egui::Event::PointerMoved(from));
        self.frames(1);
        self.h.event(button(true, from));
        self.frames(1);
        let steps = 8;
        for i in 1..=steps {
            let p = from + (to - from) * (i as f32 / steps as f32);
            self.h.event(egui::Event::PointerMoved(p));
            self.frames(1);
        }
        self.h.event(button(false, to));
        self.frames(3);
        self.h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        self.frames(1);
    }

    /// A click (press and release without moving) at `pos`.
    pub fn click_at(&mut self, pos: egui::Pos2) {
        self.click_at_mod(pos, egui::Modifiers::NONE);
    }

    pub fn click_at_mod(&mut self, pos: egui::Pos2, modifiers: egui::Modifiers) {
        self.h.event(egui::Event::PointerMoved(pos));
        self.frames(1);
        for pressed in [true, false] {
            self.h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers });
            self.frames(1);
        }
        self.frames(2);
    }

    pub fn double_click_at(&mut self, pos: egui::Pos2) {
        self.h.event(egui::Event::PointerMoved(pos));
        self.frames(1);
        for _ in 0..2 {
            for pressed in [true, false] {
                self.h.event(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
                self.h.step();
            }
        }
        self.frames(3);
    }

    pub fn track(&self, name: &str) -> &Track {
        self.project().tracks.iter().find(|t| t.name == name).unwrap_or_else(|| panic!("no track {name:?}"))
    }

    pub fn item(&self, name: &str) -> &Item {
        self.project()
            .tracks
            .iter()
            .flat_map(|t| &t.items)
            .find(|i| i.name() == name)
            .unwrap_or_else(|| panic!("no item {name:?}"))
    }

    /// The labels of every widget AccessKit sees, for a failing test's message.
    pub fn labels(&self) -> Vec<String> {
        self.h.query_all(By::new()).filter_map(|n| n.accesskit_node().label()).collect()
    }
}
