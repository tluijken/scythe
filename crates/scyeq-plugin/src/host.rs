//! ScyEQ-Pro's editor (`scyeq_ui`) on the plugin's shared half: changes go to the host as
//! parameter gestures through the instance's `GuiContext`.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Once};

use plugin_format::{GuiContext, Shared, egui};
use scyeq_dsp::probe::FxProbe;
use scyeq_dsp::{Meters, ScytheEq, params};
use scyeq_ui::{CompareSource, EditorState, EqHost, Instance, InstanceList, ReferenceAudio, ViewState};

use crate::plugin::ScyEqShared;
use crate::registry;

/// One open editor window.
pub(crate) struct Editor {
    shared: Arc<ScyEqShared>,
    cx: GuiContext,
    state: EditorState,
    themed: bool,
    /// A reference file being picked on a thread of its own.
    picking: Option<Receiver<Option<PathBuf>>>,
    /// A gesture begun in the instance list, on this instance or another: the context it went
    /// through and the parameter.
    listed_gesture: Option<(GuiContext, u32)>,
}

impl Editor {
    pub(crate) fn new(shared: Arc<ScyEqShared>, cx: GuiContext) -> Self {
        Self { shared, cx, state: EditorState::default(), themed: false, picking: None, listed_gesture: None }
    }

    pub(crate) fn frame(&mut self, root: &mut egui::Ui) {
        if !std::mem::replace(&mut self.themed, true) {
            scyeq_ui::theme::install(root.ctx());
        }
        let view = self.shared.view();
        let mut host = PluginHost {
            shared: &self.shared,
            cx: &self.cx,
            values: self.shared.values(),
            view,
            picking: &mut self.picking,
        };
        egui::CentralPanel::default().show(root, |ui| scyeq_ui::show(ui, &mut host, &mut self.state));
        if host.view.instances {
            let ctx = root.ctx().clone();
            let mut list = Instances::new(&self.shared, &self.cx, &mut self.listed_gesture);
            scyeq_ui::instances_window(&ctx, &mut list, &mut host.view.instances);
        }
        let listing = host.view.instances;
        self.shared.set_view(host.view);
        if !listing {
            self.end_listed_gesture();
        }
    }

    fn end_listed_gesture(&mut self) {
        if let Some((cx, id)) = self.listed_gesture.take() {
            cx.end(id);
        }
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        let cx = &self.cx;
        self.state.end_gesture(|id| cx.end(id));
        self.end_listed_gesture();
    }
}

/// The instance behind an editor, for one frame.
struct PluginHost<'a> {
    shared: &'a ScyEqShared,
    cx: &'a GuiContext,
    values: Vec<f64>,
    view: ViewState,
    picking: &'a mut Option<Receiver<Option<PathBuf>>>,
}

impl EqHost for PluginHost<'_> {
    fn values(&self) -> &[f64] {
        &self.values
    }

    fn begin(&mut self, id: u32) {
        self.cx.begin(id);
    }

    fn set(&mut self, id: u32, value: f64) {
        self.cx.set(id, value);
        if let Some(at) = self.values.get_mut(id as usize) {
            *at = self.shared.param(id);
        }
    }

    fn end(&mut self, id: u32) {
        self.cx.end(id);
    }

    /// Made on a copy of the values, then sent as a gesture per changed parameter: a host
    /// records each, and gets a dirty mark for the edit as a whole.
    fn edit(&mut self, _label: &str, f: &mut dyn FnMut(&mut ScytheEq)) {
        let mut eq = ScytheEq::with_values(self.values.clone(), self.shared.sample_rate());
        f(&mut eq);
        let mut changed = false;
        for id in params::ids() {
            let (old, new) = (self.values[id as usize], eq.value(id));
            if old == new {
                continue;
            }
            self.cx.begin(id);
            self.cx.set(id, new);
            self.cx.end(id);
            self.values[id as usize] = self.shared.param(id);
            changed = true;
        }
        if changed {
            self.cx.mark_dirty();
        }
    }

    fn meters(&self) -> Option<Arc<Meters>> {
        self.shared.meters()
    }

    fn probe(&self) -> Option<Arc<FxProbe>> {
        Some(self.shared.probe().clone())
    }

    fn latency(&self) -> usize {
        self.shared.latency() as usize
    }

    fn sample_rate(&self) -> f64 {
        self.shared.sample_rate() as f64
    }

    fn instances(&self) -> Vec<Instance> {
        registry::listed_others(self.shared.id())
    }

    /// A plugin sees no tracks of its own, only other instances.
    fn compare_sources(&self) -> Vec<CompareSource> {
        Vec::new()
    }

    fn source_probe(&self, id: egui::Id) -> Option<Arc<FxProbe>> {
        registry::others(self.shared.id()).into_iter().find(|s| registry::id_of(s) == id).map(|s| s.probe().clone())
    }

    /// On Linux the editor has a thread of its own, which a dialog must not hold: the host
    /// waits for that thread when it closes the window. Elsewhere the dialog is modal on the
    /// host's UI thread, as dialogs there are.
    fn pick_reference(&mut self) -> Option<PathBuf> {
        keep_loaded();
        if !cfg!(target_os = "linux") {
            return pick_file();
        }
        if self.picking.is_none() {
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(pick_file());
            });
            *self.picking = Some(rx);
        }
        None
    }

    fn picked_reference(&mut self) -> Option<PathBuf> {
        take_picked(self.picking)
    }

    fn decode_reference(&self) -> Option<scyeq_ui::DecodeFn> {
        Some(load_reference)
    }

    /// Instances come and go with the host's tracks.
    fn forgets_gone_sources(&self) -> bool {
        true
    }

    fn view(&mut self) -> &mut ViewState {
        &mut self.view
    }
}

/// Every instance in the library, this editor's first: their bands change through their own
/// contexts, so each one's host records the change. A plugin cannot open another plugin's
/// window or bypass it, and ScyEQ-Pro has no bypass parameter, so the list offers neither.
struct Instances<'a> {
    all: Vec<Arc<ScyEqShared>>,
    cx: &'a GuiContext,
    gesture: &'a mut Option<(GuiContext, u32)>,
}

impl<'a> Instances<'a> {
    fn new(me: &Arc<ScyEqShared>, cx: &'a GuiContext, gesture: &'a mut Option<(GuiContext, u32)>) -> Self {
        let mut all = vec![me.clone()];
        all.extend(registry::others(me.id()));
        Self { all, cx, gesture }
    }

    fn find(&self, id: egui::Id) -> Option<&Arc<ScyEqShared>> {
        self.all.iter().find(|s| registry::id_of(s) == id)
    }

    /// The context changes to instance `id` go through; `None` while its host is unknown.
    fn cx_of(&self, id: egui::Id) -> Option<GuiContext> {
        let (me, others) = self.all.split_first()?;
        if registry::id_of(me) == id {
            return Some(self.cx.clone());
        }
        others.iter().find(|s| registry::id_of(s) == id)?.cx()
    }
}

impl InstanceList for Instances<'_> {
    fn instance_list(&self) -> Vec<Instance> {
        self.all.iter().map(|s| registry::listed(s)).collect()
    }

    fn instance_values(&self, id: egui::Id) -> Option<Vec<f64>> {
        self.find(id).map(|s| s.values())
    }

    fn instance_enabled(&self, _id: egui::Id) -> bool {
        true
    }

    fn toggle_instance_bypass(&mut self, _id: egui::Id) {}

    fn instance_open(&self, id: egui::Id) -> bool {
        self.all.first().is_some_and(|me| registry::id_of(me) == id)
    }

    fn toggle_instance_open(&mut self, _id: egui::Id) {}

    fn begin_instance(&mut self, id: egui::Id, param: u32) {
        let Some(cx) = self.cx_of(id) else { return };
        if let Some((unfinished, unfinished_param)) = self.gesture.take() {
            unfinished.end(unfinished_param);
        }
        cx.begin(param);
        *self.gesture = Some((cx, param));
    }

    fn set_instance(&mut self, id: egui::Id, param: u32, value: f64) {
        if let Some(cx) = self.cx_of(id) {
            cx.set(param, value);
        }
    }

    fn end_instance(&mut self, id: egui::Id, param: u32) {
        let Some(cx) = self.cx_of(id) else { return };
        cx.end(param);
        *self.gesture = None;
    }

    fn controls_instances(&self) -> bool {
        false
    }
}

fn pick_file() -> Option<PathBuf> {
    rfd::FileDialog::new().add_filter("Audio", scythe_decode::REFERENCE_EXTENSIONS).pick_file()
}

/// The file picked, once the dialog closed; `None` while it is open, and after it closed
/// without one.
fn take_picked(picking: &mut Option<Receiver<Option<PathBuf>>>) -> Option<PathBuf> {
    let picked = match picking.as_ref()?.try_recv() {
        Err(TryRecvError::Empty) => return None,
        Ok(path) => path,
        Err(TryRecvError::Disconnected) => None,
    };
    *picking = None;
    picked
}

/// A reference file for EQ Match, decoded as Scythe decodes it.
fn load_reference(path: &Path) -> Result<ReferenceAudio, String> {
    let audio = scythe_decode::decode(path, false).map_err(|e| e.to_string())?;
    Ok(ReferenceAudio { channels: audio.channels, sample_rate: audio.sample_rate })
}

/// Keeps this library loaded until the process ends. The file dialog and the reference decode
/// run on threads that cannot be stopped or waited for (the dialog stays open as long as the
/// user leaves it), so a host that unloads the plugin meanwhile must not unmap their code.
/// Called before either starts.
fn keep_loaded() {
    static PINNED: Once = Once::new();
    PINNED.call_once(|| unsafe { pin(keep_loaded as *const ()) });
}

#[cfg(unix)]
unsafe fn pin(code: *const ()) {
    unsafe {
        let mut info: libc::Dl_info = std::mem::zeroed();
        if libc::dladdr(code.cast(), &mut info) != 0 && !info.dli_fname.is_null() {
            // Never closed: with RTLD_NODELETE the library outlives every dlclose.
            libc::dlopen(info.dli_fname, libc::RTLD_NOW | libc::RTLD_NODELETE);
        }
    }
}

#[cfg(windows)]
unsafe fn pin(code: *const ()) {
    use windows_sys::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_PIN, GetModuleHandleExW,
    };
    let mut module = std::ptr::null_mut();
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
            code.cast(),
            &mut module,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 16-bit PCM WAV file of `frames` interleaved.
    fn wav(rate: u32, channels: u16, frames: &[i16]) -> Vec<u8> {
        let data = (frames.len() * 2) as u32;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
        out.extend_from_slice(&(channels * 2).to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data.to_le_bytes());
        for s in frames {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    #[test]
    fn a_reference_file_decodes_to_planar_channels() {
        let path = std::env::temp_dir().join(format!("scyeq-plugin-reference-{}.wav", std::process::id()));
        std::fs::write(&path, wav(44_100, 2, &[16384, -16384, 8192, 0])).unwrap();
        let audio = load_reference(&path);
        std::fs::remove_file(&path).unwrap();
        let audio = audio.unwrap();
        assert_eq!(audio.sample_rate, 44_100);
        assert_eq!(audio.channels, vec![vec![0.5, 0.25], vec![-0.5, 0.0]]);
    }

    #[test]
    fn a_file_that_is_no_audio_is_an_error() {
        let path = std::env::temp_dir().join(format!("scyeq-plugin-not-audio-{}.wav", std::process::id()));
        std::fs::write(&path, b"not audio").unwrap();
        let audio = load_reference(&path);
        std::fs::remove_file(&path).unwrap();
        assert!(audio.is_err());
        assert!(load_reference(Path::new("/nonexistent/reference.wav")).is_err());
    }

    #[test]
    fn a_pick_is_taken_once_the_dialog_closed() {
        let (tx, rx) = mpsc::channel();
        let mut picking = Some(rx);
        assert_eq!(take_picked(&mut picking), None, "still open");
        assert!(picking.is_some());
        tx.send(Some(PathBuf::from("/music/reference.wav"))).unwrap();
        assert_eq!(take_picked(&mut picking), Some(PathBuf::from("/music/reference.wav")));
        assert!(picking.is_none(), "taken once");
        assert_eq!(take_picked(&mut picking), None);
    }

    #[test]
    fn a_dialog_closed_without_a_file_or_gone_ends_the_pick() {
        let (tx, rx) = mpsc::channel();
        let mut picking = Some(rx);
        tx.send(None).unwrap();
        assert_eq!(take_picked(&mut picking), None);
        assert!(picking.is_none());
        let (tx, rx) = mpsc::channel::<Option<PathBuf>>();
        let mut picking = Some(rx);
        drop(tx);
        assert_eq!(take_picked(&mut picking), None);
        assert!(picking.is_none());
    }

    #[test]
    fn keeping_the_library_loaded_can_be_asked_again() {
        keep_loaded();
        keep_loaded();
    }
}
