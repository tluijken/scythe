//! Plugin editors and their windows, kept on the plugin thread
//! ([`scythe_engine::plugin_thread`]) where the plugins live: they open, run their timers and
//! close there, at that thread's pace and not the window's frames. The window only keeps the
//! ids of the editors it opened and learns from [`take_closed`] which ones went away.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use scythe_engine::editor::Editor;
use scythe_engine::fx::FxInstance;
use scythe_engine::plugin_thread;

use crate::plugin_window::PluginWindow;

/// An editor in its window. Fields drop in this order: the editor, its window, then the
/// plugin, which outlives both.
struct Hosted {
    id: u64,
    editor: Box<dyn Editor>,
    window: PluginWindow,
    instance: Arc<FxInstance>,
    /// The scale the editor was last told.
    scale: f64,
}

thread_local! {
    static OPEN: RefCell<Vec<Hosted>> = const { RefCell::new(Vec::new()) };
    static TICKING: Cell<bool> = const { Cell::new(false) };
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
/// Editors that closed by themselves: their window was closed, the plugin closed its editor
/// or the plugin left the project.
static CLOSED: Mutex<Vec<u64>> = Mutex::new(Vec::new());

pub enum OpenError {
    NoEditor,
    Failed(String),
}

/// Opens the editor of `instance` in a window titled `title`; answers its id.
pub fn open(instance: Arc<FxInstance>, title: String) -> Result<u64, OpenError> {
    plugin_thread::run(move || {
        let mut editor = instance.with(|e| e.editor()).flatten().ok_or(OpenError::NoEditor)?;
        let window = PluginWindow::new(&title, 640, 480)
            .map_err(|e| OpenError::Failed(format!("Cannot open a plugin window: {e}")))?;
        let scale = window.scale();
        editor.set_scale(scale);
        let (w, h) = editor.open(window.handle()).map_err(|e| OpenError::Failed(format!("{title}: {e}")))?;
        window.resize(w, h);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        OPEN.with(|o| o.borrow_mut().push(Hosted { id, editor, window, instance, scale }));
        if !TICKING.with(|t| t.replace(true)) {
            plugin_thread::add_ticker(tick);
        }
        Ok(id)
    })
}

/// Closes editor `id`, when it is still open.
pub fn close(id: u64) {
    plugin_thread::run(move || {
        let hosted = OPEN.with(|o| {
            let mut o = o.borrow_mut();
            o.iter().position(|h| h.id == id).map(|at| o.remove(at))
        });
        if let Some(mut hosted) = hosted {
            hosted.editor.close();
        }
    });
}

/// The editors that closed by themselves since the last call.
pub fn take_closed() -> Vec<u64> {
    std::mem::take(&mut *CLOSED.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Runs the editors' timers and descriptors, tells them when their window moved to a screen
/// of another scale, follows their size requests and closes those whose window or plugin went
/// away. Stops ticking once none is open.
fn tick() -> bool {
    let gone: Vec<Hosted> = OPEN.with(|o| {
        let mut o = o.borrow_mut();
        for hosted in o.iter_mut() {
            let scale = hosted.window.scale();
            if scale != hosted.scale {
                hosted.scale = scale;
                hosted.editor.set_scale(scale);
            }
            if let Some((width, height)) = hosted.editor.idle() {
                hosted.window.resize(width, height);
            }
        }
        let (gone, open): (Vec<Hosted>, Vec<Hosted>) = o
            .drain(..)
            .partition(|h| h.window.close_requested() || h.editor.closed() || Arc::strong_count(&h.instance) <= 1);
        *o = open;
        gone
    });
    if !gone.is_empty() {
        CLOSED.lock().unwrap_or_else(|e| e.into_inner()).extend(gone.iter().map(|h| h.id));
    }
    for mut h in gone {
        h.editor.close();
    }
    let any = OPEN.with(|o| !o.borrow().is_empty());
    if !any {
        TICKING.with(|t| t.set(false));
    }
    any
}
