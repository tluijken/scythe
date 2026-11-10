//! Scythe's floating windows: one card frame, a place to open (centred on the main window,
//! or beside the control that opened it), kept on screen, remembered across sessions, closed
//! with Escape while focused, and opened on top.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use egui::{Context, CornerRadius, Id, InnerResponse, Key, LayerId, Modifiers, Order, Pos2, Rect, Ui, Vec2, vec2};

use crate::theme;

/// Room kept between a placed window and the edge of the main window.
const MARGIN: f32 = 16.0;
/// How far a window opened beside the pointer stands off from it.
const ANCHOR_GAP: f32 = 12.0;
/// How far each further window of the same kind is moved, so they do not stack exactly.
const CASCADE: f32 = 28.0;
const PANEL_MIN: Vec2 = vec2(320.0, 200.0);
/// A guess at a dialog's height before its first frame has measured it.
const DIALOG_GUESS_H: f32 = 240.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowKind {
    /// A resizable tool window; remembers its position and size.
    Panel,
    /// A small settings or properties window that sizes to its contents; remembers its position.
    Dialog,
    /// A toolbar or graphics window; remembers its position, its size comes from its contents.
    Floating,
}

/// Starts a window of `kind`. `key` names it for the remembered placement and, unless
/// [`ScytheWindow::id`] gives another, is its identity too.
pub fn scythe_window(key: impl Into<String>, title: impl Into<String>, kind: WindowKind) -> ScytheWindow {
    let key = key.into();
    ScytheWindow {
        id: Id::unique(("scythe-window", &key)),
        key,
        title: title.into(),
        kind,
        size: None,
        width: None,
        min: None,
        anchored: false,
        escape: true,
        resizable: None,
    }
}

#[must_use]
pub struct ScytheWindow {
    key: String,
    id: Id,
    title: String,
    kind: WindowKind,
    size: Option<Vec2>,
    width: Option<f32>,
    min: Option<Vec2>,
    anchored: bool,
    escape: bool,
    resizable: Option<bool>,
}

impl ScytheWindow {
    /// For windows open more than once at a time (one per effect, per toolbar): they share
    /// the key's remembered placement and cascade from it.
    pub fn id(mut self, id: Id) -> Self {
        self.id = id;
        self
    }

    /// The outer size it first opens at.
    pub fn default_size(mut self, size: Vec2) -> Self {
        self.size = Some(size);
        self
    }

    pub fn default_width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    pub fn min_size(mut self, min: Vec2) -> Self {
        self.min = Some(min);
        self
    }

    /// Opens beside the pointer, where the control that opened it was, each time it opens.
    pub fn anchored(mut self) -> Self {
        self.anchored = true;
        self
    }

    /// Whether Escape closes it while focused; off while it reads keys itself.
    pub fn escape_closes(mut self, on: bool) -> Self {
        self.escape = on;
        self
    }

    pub fn resizable(mut self, on: bool) -> Self {
        self.resizable = Some(on);
        self
    }

    pub fn show<R>(
        self,
        ctx: &Context,
        open: &mut bool,
        add_contents: impl FnOnce(&mut Ui) -> R,
    ) -> Option<InnerResponse<Option<R>>> {
        let id = self.id;
        let layer = LayerId::new(Order::Middle, id);
        let closing = registry(ctx, |r| r.close.take_if(|c| *c == id).is_some());
        if closing {
            *open = false;
        }
        if !*open {
            return None;
        }
        let screen = ctx.content_rect();
        let bounds = screen.shrink(MARGIN);
        let opening = !ctx.memory(|m| m.areas().visible_last_frame(&layer));
        let area = egui::AreaState::load(ctx, id);
        let measured = area.as_ref().and_then(|a| a.size);
        let resizable = self.resizable.unwrap_or(self.kind == WindowKind::Panel);
        let remembers_size = self.kind == WindowKind::Panel && resizable && !self.anchored;
        let guess = self.size.unwrap_or_else(|| vec2(self.width.unwrap_or(420.0), DIALOG_GUESS_H));

        let mut window = egui::Window::new(self.title)
            .id(id)
            .open(open)
            .frame(frame(&ctx.global_style()))
            .collapsible(false)
            .resizable(resizable)
            .constrain(true);
        if self.kind == WindowKind::Panel {
            window = window.min_size(self.min.unwrap_or(PANEL_MIN).min(bounds.size()));
        } else if let Some(min) = self.min {
            window = window.min_size(min);
        }
        if let Some(w) = self.width {
            window = window.default_width(w.min(bounds.width()));
        }

        if opening {
            ctx.move_to_top(layer);
            let pointer = ctx.input(|i| i.pointer.interact_pos().or(i.pointer.latest_pos()));
            let siblings = registry(ctx, |r| {
                r.focus(id);
                r.shown.iter().filter(|s| s.key == self.key && s.id != id).count()
            });
            let cascade = Vec2::splat(CASCADE * siblings as f32);
            let remembered = registry(ctx, |r| r.placements.get(&self.key).copied());
            match (self.anchored, area.is_some(), remembered) {
                (true, _, _) => {
                    let place = match pointer.filter(|p| screen.contains(*p)) {
                        Some(p) => Placement::Beside(p),
                        None => Placement::Centred,
                    };
                    let rect = place.rect(measured.unwrap_or(guess), bounds);
                    window = place_now(window, rect.min);
                    if let Some(size) = self.size {
                        window = window.default_size(size.min(bounds.size()));
                    }
                    if measured.is_none() {
                        registry(ctx, |r| r.pending.insert(id, place));
                    }
                }
                (false, true, _) => {}
                (false, false, Some(rect)) => {
                    let size = if remembers_size { rect.size() } else { self.size.unwrap_or(rect.size()) };
                    let rect = clamp_rect(Rect::from_min_size(rect.min + cascade, size), bounds);
                    window = window.default_pos(rect.min);
                    if remembers_size || self.size.is_some() {
                        window = window.default_size(rect.size());
                    }
                }
                (false, false, None) => {
                    let place = Placement::Centred;
                    let rect = place.rect(guess, bounds).translate(cascade);
                    let rect = clamp_rect(rect, bounds);
                    window = window.default_pos(rect.min);
                    if self.size.is_some() {
                        window = window.default_size(rect.size());
                    }
                    registry(ctx, |r| r.pending.insert(id, Placement::Cascaded(cascade)));
                }
            }
        } else if let Some(size) = measured
            && let Some(place) = registry(ctx, |r| r.pending.remove(&id))
        {
            window = place_now(window, place.rect(size, bounds).min);
        }

        let shown = window.show(ctx, add_contents);
        let rect = shown.as_ref().map(|s| s.response.rect);
        let user_moved = ctx.input(|i| i.pointer.any_down() || i.pointer.any_released());
        registry(ctx, |r| {
            r.shown_now.push(Shown { id, key: self.key.clone(), escape: self.escape });
            let Some(rect) = rect else { return };
            let before = r.last_rect.insert(id, rect);
            let changed = before.is_some_and(|b| b.min != rect.min || (remembers_size && b.size() != rect.size()));
            if opening || self.anchored || !user_moved || !changed || r.pending.contains_key(&id) {
                return;
            }
            let previous = r.placements.get(&self.key).copied();
            let size =
                if remembers_size { rect.size() } else { previous.map_or(rect.size(), |p| p.size()) };
            let next = Rect::from_min_size(rect.min.round(), size.round());
            if previous != Some(next) {
                r.placements.insert(self.key.clone(), next);
                r.dirty = true;
            }
        });
        shown
    }
}

/// Moves `window` to `pos` this frame. In title-bar drag mode egui puts back the position it
/// had before the frame began, so a position set from outside needs the other drag mode for
/// the frame it is set in.
fn place_now(window: egui::Window<'_>, pos: Pos2) -> egui::Window<'_> {
    window.current_pos(pos).drag_area(egui::WindowDrag::Anywhere)
}

/// The card every window is drawn on.
pub fn frame(style: &egui::Style) -> egui::Frame {
    egui::Frame::window(style).fill(theme::PANEL).corner_radius(CornerRadius::same(18)).shadow(egui::Shadow {
        offset: [0, 12],
        blur: 28,
        spread: 0,
        color: egui::Color32::from_black_alpha(110),
    })
}

/// Where a window goes once its size is known.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Placement {
    Centred,
    /// Centred, then moved along by this much for the windows of its kind already open.
    Cascaded(Vec2),
    /// Beside this point: below and to the right of it, or flipped to whichever side fits.
    Beside(Pos2),
}

impl Placement {
    fn rect(self, size: Vec2, bounds: Rect) -> Rect {
        match self {
            Placement::Centred => clamp_rect(Rect::from_center_size(bounds.center(), size), bounds),
            Placement::Cascaded(by) => clamp_rect(Rect::from_center_size(bounds.center(), size).translate(by), bounds),
            Placement::Beside(p) => beside(p, size, bounds),
        }
    }
}

/// `rect` moved, and shrunk if it must be, to lie within `bounds`.
pub fn clamp_rect(rect: Rect, bounds: Rect) -> Rect {
    let size = rect.size().min(bounds.size());
    let x = rect.min.x.min(bounds.right() - size.x).max(bounds.left());
    let y = rect.min.y.min(bounds.bottom() - size.y).max(bounds.top());
    Rect::from_min_size(egui::pos2(x, y), size)
}

/// A window of `size` beside `point`: right of and below it where it fits, flipped left or
/// up where it does not, and within `bounds` either way.
pub fn beside(point: Pos2, size: Vec2, bounds: Rect) -> Rect {
    let right = point.x + ANCHOR_GAP;
    let x = if right + size.x <= bounds.right() { right } else { point.x - ANCHOR_GAP - size.x };
    let below = point.y + ANCHOR_GAP;
    let y = if below + size.y <= bounds.bottom() { below } else { point.y - ANCHOR_GAP - size.y };
    clamp_rect(Rect::from_min_size(egui::pos2(x, y), size), bounds)
}

#[derive(Debug, Clone)]
struct Shown {
    id: Id,
    key: String,
    escape: bool,
}

#[derive(Debug, Clone, Default)]
struct Registry {
    /// Remembered outer rectangles, by window key.
    placements: BTreeMap<String, Rect>,
    file: Option<PathBuf>,
    dirty: bool,
    /// The windows shown last frame, and those shown so far this frame.
    shown: Vec<Shown>,
    shown_now: Vec<Shown>,
    /// Focused windows, the focused one last: closing it gives focus back to the one before.
    focus: Vec<Id>,
    /// The window Escape closes this frame.
    close: Option<Id>,
    pending: HashMap<Id, Placement>,
    /// Each window's rectangle when last drawn, to tell a move or resize from a click.
    last_rect: HashMap<Id, Rect>,
}

impl Registry {
    fn focus(&mut self, id: Id) {
        self.focus.retain(|f| *f != id);
        self.focus.push(id);
    }
}

fn registry<R>(ctx: &Context, f: impl FnOnce(&mut Registry) -> R) -> R {
    ctx.data_mut(|d| f(d.get_temp_mut_or_default::<Registry>(Id::unique("scythe-windows"))))
}

/// Reads the remembered placements from `file`, where they are written back as they change.
pub fn load(ctx: &Context, file: PathBuf) {
    let placements = std::fs::read_to_string(&file).map(|t| parse(&t)).unwrap_or_default();
    registry(ctx, |r| {
        r.placements = placements;
        r.file = Some(file);
    });
}

/// Where the placements are kept: beside Scythe's preferences.
pub fn placements_file() -> PathBuf {
    scythe_session::preferences_path().with_file_name("windows.ini")
}

/// Before the shortcuts run: a press on a window focuses it, a press outside the windows
/// gives focus back to the main window.
pub fn begin_frame(ctx: &Context) {
    let press = ctx.input(|i| i.pointer.any_pressed().then(|| i.pointer.press_origin()).flatten());
    let layer = press.and_then(|p| ctx.layer_id_at(p));
    registry(ctx, |r| {
        r.close = None;
        match layer {
            Some(l) if l.order == Order::Middle && r.shown.iter().any(|s| s.id == l.id) => r.focus(l.id),
            Some(l) if l.order != Order::Background => {}
            _ if press.is_some() => r.focus.clear(),
            _ => {}
        }
    });
}

/// Escape closes the focused window instead of reaching the shortcuts. Answers whether it did.
pub fn take_escape(ctx: &Context) -> bool {
    if !ctx.input(|i| i.key_pressed(Key::Escape) && i.modifiers.is_none()) {
        return false;
    }
    let target = registry(ctx, |r| {
        let top = *r.focus.last()?;
        r.shown.iter().find(|s| s.id == top).filter(|s| s.escape).map(|s| s.id)
    });
    let Some(id) = target else { return false };
    ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
    registry(ctx, |r| {
        r.close = Some(id);
        r.focus.retain(|f| *f != id);
    });
    true
}

/// After the windows have drawn: forgets those that closed and writes the placements once a
/// move or resize has ended.
pub fn end_frame(ctx: &Context) {
    let dragging = ctx.input(|i| i.pointer.any_down());
    let save = registry(ctx, |r| {
        r.shown = std::mem::take(&mut r.shown_now);
        let shown = &r.shown;
        r.focus.retain(|f| shown.iter().any(|s| s.id == *f));
        r.pending.retain(|id, _| shown.iter().any(|s| s.id == *id));
        r.last_rect.retain(|id, _| shown.iter().any(|s| s.id == *id));
        if !r.dirty || dragging {
            return None;
        }
        r.dirty = false;
        Some((r.file.clone()?, format(&r.placements)))
    });
    if let Some((file, text)) = save {
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = write_atomic(&file, &text) {
            eprintln!("Cannot write {}: {e}", file.display());
        }
    }
}

fn write_atomic(file: &Path, text: &str) -> std::io::Result<()> {
    let tmp = file.with_extension("ini.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, file)
}

/// One line per window: `key = x y width height`, in points.
fn format(placements: &BTreeMap<String, Rect>) -> String {
    placements.iter().map(|(k, r)| format!("{k} = {} {} {} {}\n", r.min.x, r.min.y, r.width(), r.height())).collect()
}

fn parse(text: &str) -> BTreeMap<String, Rect> {
    text.lines()
        .filter_map(|line| {
            let (key, values) = line.split_once('=')?;
            let v: Vec<f32> = values.split_whitespace().filter_map(|n| n.parse().ok()).collect();
            let [x, y, w, h] = v[..] else { return None };
            (w > 0.0 && h > 0.0 && [x, y, w, h].iter().all(|n| n.is_finite()))
                .then(|| (key.trim().to_owned(), Rect::from_min_size(egui::pos2(x, y), vec2(w, h))))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    fn screen() -> Rect {
        Rect::from_min_size(Pos2::ZERO, vec2(1280.0, 720.0))
    }

    #[test]
    fn clamping_keeps_a_window_inside() {
        let b = screen().shrink(MARGIN);
        let r = clamp_rect(Rect::from_min_size(pos2(1200.0, -40.0), vec2(300.0, 200.0)), b);
        assert_eq!(r, Rect::from_min_size(pos2(1264.0 - 300.0, 16.0), vec2(300.0, 200.0)));
        let big = clamp_rect(Rect::from_min_size(pos2(100.0, 100.0), vec2(1600.0, 900.0)), b);
        assert_eq!(big, b);
        let inside = Rect::from_min_size(pos2(200.0, 150.0), vec2(300.0, 200.0));
        assert_eq!(clamp_rect(inside, b), inside);
    }

    #[test]
    fn anchoring_flips_to_the_side_that_fits() {
        let b = screen().shrink(MARGIN);
        let size = vec2(300.0, 200.0);
        assert_eq!(beside(pos2(100.0, 100.0), size, b).min, pos2(112.0, 112.0));
        assert_eq!(beside(pos2(1200.0, 100.0), size, b).min, pos2(1200.0 - 12.0 - 300.0, 112.0));
        assert_eq!(beside(pos2(100.0, 650.0), size, b).min, pos2(112.0, 650.0 - 12.0 - 200.0));
        // Too tall to go above or below a point in the middle: kept on screen.
        let tall = beside(pos2(600.0, 360.0), vec2(300.0, 600.0), b);
        assert!(b.contains_rect(tall));
    }

    #[test]
    fn centring_uses_the_main_window() {
        let b = screen().shrink(MARGIN);
        let r = Placement::Centred.rect(vec2(400.0, 300.0), b);
        assert_eq!(r.center(), pos2(640.0, 360.0));
        let r = Placement::Cascaded(vec2(28.0, 28.0)).rect(vec2(400.0, 300.0), b);
        assert_eq!(r.center(), pos2(668.0, 388.0));
    }

    #[test]
    fn placements_round_trip_through_text() {
        let mut p = BTreeMap::new();
        p.insert("nam-panel".to_owned(), Rect::from_min_size(pos2(120.0, 80.5), vec2(880.0, 760.0)));
        p.insert("toolbar-3".to_owned(), Rect::from_min_size(pos2(-4.0, 10.0), vec2(200.0, 48.0)));
        assert_eq!(parse(&format(&p)), p);
        let junk = parse("nonsense\nx = 1 2 3\ny = 1 2 0 4\nz = 1 2 nan 4\nok = 1 2 3 4\n");
        assert_eq!(junk.keys().collect::<Vec<_>>(), ["ok"]);
    }

    /// Runs one frame of a main window with `show` drawing the windows; answers what it drew.
    fn frame(ctx: &Context, events: Vec<egui::Event>, show: &mut dyn FnMut(&Context) -> Option<Rect>) -> Option<Rect> {
        let input = egui::RawInput { screen_rect: Some(screen()), events, ..Default::default() };
        let mut out = None;
        let mut output = ctx.run_ui(input, |ui| {
            let ctx = ui.ctx().clone();
            begin_frame(&ctx);
            take_escape(&ctx);
            out = show(&ctx);
            end_frame(&ctx);
        });
        output.textures_delta.clear();
        out
    }

    fn key(key: Key) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }
    }

    fn button(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
    }

    fn panel(ctx: &Context, open: &mut bool) -> Option<Rect> {
        scythe_window("test-panel", "Test", WindowKind::Panel)
            .default_size(vec2(400.0, 300.0))
            .show(ctx, open, |ui| ui.allocate_space(ui.available_size()))
            .map(|s| s.response.rect)
    }

    #[test]
    fn a_new_window_opens_centred_and_escape_closes_it() {
        let ctx = Context::default();
        let mut open = true;
        let mut rect = None;
        for _ in 0..3 {
            rect = frame(&ctx, vec![], &mut |c| panel(c, &mut open));
        }
        let r = rect.expect("shown");
        assert!((r.center() - pos2(640.0, 360.0)).length() < 2.0, "{r:?}");
        frame(&ctx, vec![key(Key::Escape)], &mut |c| panel(c, &mut open));
        assert!(!open);
    }

    #[test]
    fn a_dialog_is_centred_once_its_size_is_known() {
        let ctx = Context::default();
        let mut open = true;
        let mut rect = None;
        for _ in 0..4 {
            rect = frame(&ctx, vec![], &mut |c| {
                scythe_window("test-dialog", "Dialog", WindowKind::Dialog)
                    .show(c, &mut open, |ui| ui.allocate_space(vec2(150.0, 90.0)))
                    .map(|s| s.response.rect)
            });
        }
        let r = rect.expect("shown");
        assert!((r.center() - pos2(640.0, 360.0)).length() < 2.0, "{r:?}");
    }

    #[test]
    fn escape_is_left_alone_once_the_main_window_has_focus() {
        let ctx = Context::default();
        let mut open = true;
        for _ in 0..2 {
            frame(&ctx, vec![], &mut |c| panel(c, &mut open));
        }
        let outside = pos2(5.0, 5.0);
        frame(&ctx, vec![egui::Event::PointerMoved(outside), button(outside, true)], &mut |c| panel(c, &mut open));
        frame(&ctx, vec![button(outside, false)], &mut |c| panel(c, &mut open));
        frame(&ctx, vec![key(Key::Escape)], &mut |c| panel(c, &mut open));
        assert!(open);
    }

    #[test]
    fn a_window_dragged_away_reopens_there_in_the_next_session() {
        let dir = std::env::temp_dir().join(format!("scythe-windows-{}", std::process::id()));
        let file = dir.join("windows.ini");
        let ctx = Context::default();
        load(&ctx, file.clone());
        let mut open = true;
        let mut rect = None;
        for _ in 0..3 {
            rect = frame(&ctx, vec![], &mut |c| panel(c, &mut open));
        }
        let start = rect.expect("shown");
        let grab = start.min + vec2(60.0, 12.0);
        let by = vec2(-300.0, -150.0);
        frame(&ctx, vec![egui::Event::PointerMoved(grab), button(grab, true)], &mut |c| panel(c, &mut open));
        for step in 1..=4 {
            let p = grab + by * (step as f32 / 4.0);
            frame(&ctx, vec![egui::Event::PointerMoved(p)], &mut |c| panel(c, &mut open));
        }
        let moved = frame(&ctx, vec![button(grab + by, false)], &mut |c| panel(c, &mut open)).expect("shown");
        frame(&ctx, vec![], &mut |c| panel(c, &mut open));
        assert!((moved.min - (start.min + by)).length() < 6.0, "{moved:?} from {start:?}");
        let saved = parse(&std::fs::read_to_string(&file).expect("saved"))["test-panel"];
        assert_eq!(saved.min, moved.min.round());

        let ctx = Context::default();
        load(&ctx, file.clone());
        let mut back = None;
        for _ in 0..3 {
            back = frame(&ctx, vec![], &mut |c| panel(c, &mut open));
        }
        let back = back.expect("shown");
        assert!((back.min - saved.min).length() < 1.0, "{back:?} vs {saved:?}");
        assert!((back.size() - saved.size()).length() < 1.0, "{back:?} vs {saved:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_remembered_window_is_kept_on_a_smaller_screen() {
        let ctx = Context::default();
        registry(&ctx, |r| {
            r.placements.insert("test-panel".into(), Rect::from_min_size(pos2(1500.0, 900.0), vec2(1900.0, 400.0)));
        });
        let mut open = true;
        let mut rect = None;
        for _ in 0..3 {
            rect = frame(&ctx, vec![], &mut |c| panel(c, &mut open));
        }
        let r = rect.expect("shown");
        assert!(screen().contains_rect(r), "{r:?}");
    }
}
