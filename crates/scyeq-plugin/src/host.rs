//! ScyEQ-Pro's editor (`scyeq_ui`) on the plugin's shared half: changes go to the host as
//! parameter gestures through the instance's `GuiContext`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};

use plugin_format::{GuiContext, Shared, egui};
use scyeq_dsp::probe::FxProbe;
use scyeq_dsp::{Meters, ScytheEq, params};
use scyeq_ui::{CompareSource, EditorState, EqHost, Instance, InstanceList, ReferenceAudio, ReferenceError, ViewState};

#[cfg(unix)]
use crate::owner;
use crate::plugin::{PickNotice, ScyEqShared};
use crate::registry;

/// One open editor window.
pub(crate) struct Editor {
    shared: Arc<ScyEqShared>,
    cx: GuiContext,
    state: EditorState,
    themed: bool,
    /// A gesture begun in the instance list, on this instance or another: the context it went
    /// through and the parameter.
    listed_gesture: Option<(GuiContext, u32)>,
}

impl Editor {
    pub(crate) fn new(shared: Arc<ScyEqShared>, cx: GuiContext) -> Self {
        Self { shared, cx, state: EditorState::default(), themed: false, listed_gesture: None }
    }

    pub(crate) fn frame(&mut self, root: &mut egui::Ui) {
        if !std::mem::replace(&mut self.themed, true) {
            scyeq_ui::theme::install(root.ctx());
        }
        let pointer = root.ctx().input(|i| i.pointer.hover_pos());
        self.state.controls.hovered = pointer.is_some_and(|p| root.max_rect().contains(p));
        let view = self.shared.view();
        let mut host = PluginHost { shared: &self.shared, cx: &self.cx, values: self.shared.values(), view };
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
}

impl EqHost for PluginHost<'_> {
    fn values(&self) -> &[f64] {
        &self.values
    }

    fn begin(&mut self, id: u32) {
        self.cx.begin(id);
    }

    fn set(&mut self, id: u32, value: f64) {
        set(self.shared, self.cx, id, value);
        if let Some(at) = self.values.get_mut(id as usize) {
            *at = self.shared.param(id);
        }
    }

    fn end(&mut self, id: u32) {
        self.cx.end(id);
    }

    /// Made on a copy of the values, then sent as one gesture over every changed parameter, so
    /// a host records the edit as one step and gets a dirty mark for it. Solo and audition
    /// change on the instance alone.
    fn edit(&mut self, _label: &str, f: &mut dyn FnMut(&mut ScytheEq)) {
        let mut eq = ScytheEq::with_values(self.values.clone(), self.shared.sample_rate());
        f(&mut eq);
        let mut changed = Vec::new();
        for id in params::ids() {
            let (old, new) = (self.values[id as usize], eq.value(id));
            if old == new {
                continue;
            }
            if params::transient(id) {
                self.shared.set_param(id, new);
                self.values[id as usize] = self.shared.param(id);
            } else {
                changed.push((id, new));
            }
        }
        if changed.is_empty() {
            return;
        }
        for &(id, _) in &changed {
            self.cx.begin(id);
        }
        for &(id, new) in &changed {
            self.cx.set(id, new);
            self.values[id as usize] = self.shared.param(id);
        }
        for &(id, _) in &changed {
            self.cx.end(id);
        }
        self.cx.mark_dirty();
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

    /// The dialog never runs inside the editor's frame. On Linux the host waits for the editor
    /// thread when it closes the window; on Windows the dialog's modal loop would paint the
    /// editor again inside this frame.
    fn pick_reference(&mut self) -> Option<PathBuf> {
        ask(self.shared, keep_loaded(), file_dialog);
        None
    }

    fn picked_reference(&mut self) -> Option<PathBuf> {
        answer(self.shared)
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

    fn notice(&self) -> Option<String> {
        self.shared.notice()
    }

    fn match_closed(&mut self) {
        self.shared.set_pick_notice(None);
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

    fn begin_instance(&mut self, id: egui::Id, param: u32) {
        let Some(cx) = self.cx_of(id) else { return };
        if let Some((unfinished, unfinished_param)) = self.gesture.take() {
            unfinished.end(unfinished_param);
        }
        cx.begin(param);
        *self.gesture = Some((cx, param));
    }

    fn set_instance(&mut self, id: egui::Id, param: u32, value: f64) {
        if let (Some(shared), Some(cx)) = (self.find(id), self.cx_of(id)) {
            set(shared, &cx, param, value);
        }
    }

    fn end_instance(&mut self, id: egui::Id, param: u32) {
        let Some(cx) = self.cx_of(id) else { return };
        cx.end(param);
        *self.gesture = None;
    }
}

/// Sets `id` on `shared` through its context, so the host records it; solo and audition, which
/// the host does not know, on the instance alone.
fn set(shared: &ScyEqShared, cx: &GuiContext, id: u32, value: f64) {
    if params::transient(id) {
        shared.set_param(id, value);
    } else {
        cx.set(id, value);
    }
}

/// Made in the frame, so it belongs to the window the user just clicked in, and shown on the
/// pick's thread.
#[cfg(all(unix, not(target_os = "macos")))]
fn file_dialog() -> impl FnOnce() -> Picked + Send + 'static {
    let mut dialog = rfd::FileDialog::new().add_filter("Audio", scythe_decode::REFERENCE_EXTENSIONS);
    if let Some(owner) = owner::active() {
        dialog = dialog.set_parent(&owner);
    }
    move || match scythe_chooser::show(|| dialog.pick_file()) {
        scythe_chooser::Shown::Picked(path) => Picked::File(path),
        scythe_chooser::Shown::Cancelled => Picked::Cancelled,
        scythe_chooser::Shown::NoDialog => Picked::NoDialog,
    }
}

/// Without an owner: the owner would be on the editor's thread and the dialog on the pick's,
/// and a modal dialog across threads disables the host's window from the outside.
#[cfg(windows)]
fn file_dialog() -> impl FnOnce() -> Picked + Send + 'static {
    || {
        let dialog = rfd::FileDialog::new().add_filter("Audio", scythe_decode::REFERENCE_EXTENSIONS);
        dialog.pick_file().map_or(Picked::Cancelled, Picked::File)
    }
}

/// AppKit's panel must start on the main thread, where the frame runs: begun here as a sheet on
/// the window the user just clicked in, it returns at once, and its answer is waited for on the
/// pick's thread.
#[cfg(target_os = "macos")]
fn file_dialog() -> impl FnOnce() -> Picked + Send + 'static {
    let mut dialog = rfd::AsyncFileDialog::new().add_filter("Audio", scythe_decode::REFERENCE_EXTENSIONS);
    if let Some(owner) = owner::active() {
        dialog = dialog.set_parent(&owner);
    }
    let picked = dialog.pick_file();
    move || block_on(picked).map_or(Picked::Cancelled, |file| Picked::File(file.path().to_path_buf()))
}

/// How a file dialog ended.
#[derive(Debug, PartialEq)]
pub(crate) enum Picked {
    File(PathBuf),
    Cancelled,
    /// None showed: on Linux, neither a file chooser portal nor zenity was found, and rfd gave up
    /// at once.
    NoDialog,
}

/// Opens the dialog `open` makes for `shared` once the library is `kept` loaded, and tells the
/// user why when it does not open.
fn ask<D>(shared: &ScyEqShared, kept: bool, open: impl FnOnce() -> D)
where
    D: FnOnce() -> Picked + Send + 'static,
{
    let notice = if !kept {
        Some(PickNotice::Refused)
    } else if pick(shared.picking(), open) {
        None
    } else {
        Some(PickNotice::AlreadyOpen)
    };
    shared.set_pick_notice(notice);
}

/// The file picked for `shared`, once its dialog closed with one.
fn answer(shared: &ScyEqShared) -> Option<PathBuf> {
    let picked = take_picked(&mut shared.picking().lock().unwrap_or_else(|e| e.into_inner()))?;
    shared.set_pick_notice((picked == Picked::NoDialog).then_some(PickNotice::NoDialog));
    match picked {
        Picked::File(path) => Some(path),
        Picked::Cancelled | Picked::NoDialog => None,
    }
}

/// Opens the dialog `open` makes, unless the instance's last one is still open: an editor
/// closed and opened again meanwhile takes that one's answer rather than open a second.
/// False when one was still open.
fn pick<D>(picking: &Mutex<Option<Receiver<Picked>>>, open: impl FnOnce() -> D) -> bool
where
    D: FnOnce() -> Picked + Send + 'static,
{
    let answer = {
        let mut picking = picking.lock().unwrap_or_else(|e| e.into_inner());
        if picking.is_some() {
            return false;
        }
        let (answer, receiver) = mpsc::channel();
        *picking = Some(receiver);
        answer
    };
    // Should `open` panic, the dropped sender ends the pick.
    let dialog = open();
    std::thread::spawn(move || {
        let _ = answer.send(dialog());
    });
    true
}

#[cfg(any(target_os = "macos", test))]
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    struct Unpark(std::thread::Thread);
    impl std::task::Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = std::task::Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = std::task::Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        if let std::task::Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        std::thread::park();
    }
}

/// How the dialog ended, once it closed; `None` while it is open.
fn take_picked(picking: &mut Option<Receiver<Picked>>) -> Option<Picked> {
    let picked = match picking.as_ref()?.try_recv() {
        Err(TryRecvError::Empty) => return None,
        Ok(picked) => picked,
        Err(TryRecvError::Disconnected) => Picked::Cancelled,
    };
    *picking = None;
    Some(picked)
}

/// A reference file for EQ Match, decoded as Scythe decodes it.
fn load_reference(path: &Path) -> Result<ReferenceAudio, ReferenceError> {
    let audio = scythe_decode::decode_reference(path).map_err(|e| match e {
        scythe_decode::Error::Open(e) => ReferenceError::Open(e.kind()),
        scythe_decode::Error::Decode(_) => ReferenceError::NotAudio,
    })?;
    Ok(ReferenceAudio { channels: audio.channels, sample_rate: audio.sample_rate })
}

/// Keeps this library loaded until the process ends. The file dialog and the reference decode
/// run on threads that cannot be stopped or waited for (the dialog stays open as long as the
/// user leaves it), so a host that unloads the plugin meanwhile must not unmap their code.
/// Called before either starts, which they do only once it is kept: false until then.
fn keep_loaded() -> bool {
    static PINNED: AtomicBool = AtomicBool::new(false);
    if !PINNED.load(Ordering::Acquire) && unsafe { pin(keep_loaded as *const ()) } {
        PINNED.store(true, Ordering::Release);
    }
    PINNED.load(Ordering::Acquire)
}

/// Keeps the library that holds `code` loaded for good; false when it could not.
#[cfg(unix)]
pub(crate) unsafe fn pin(code: *const ()) -> bool {
    unsafe {
        let mut info: libc::Dl_info = std::mem::zeroed();
        if libc::dladdr(code.cast(), &mut info) == 0 || info.dli_fname.is_null() {
            return false;
        }
        // Never closed: with RTLD_NODELETE the library outlives every dlclose.
        !libc::dlopen(info.dli_fname, libc::RTLD_NOW | libc::RTLD_NODELETE).is_null()
    }
}

#[cfg(windows)]
pub(crate) unsafe fn pin(code: *const ()) -> bool {
    use windows_sys::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_PIN, GetModuleHandleExW,
    };
    let mut module = std::ptr::null_mut();
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
            code.cast(),
            &mut module,
        ) != 0
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
    fn a_reference_keeps_its_first_two_channels() {
        let path = std::env::temp_dir().join(format!("scyeq-plugin-quad-{}.wav", std::process::id()));
        std::fs::write(&path, wav(48_000, 4, &[16384, -16384, 8192, 4096])).unwrap();
        let audio = load_reference(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(audio.unwrap().channels, vec![vec![0.5], vec![-0.5]]);
    }

    #[test]
    fn an_ogg_opus_reference_decodes() {
        use opus_pure::{Application, MAX_PACKET_BYTES, OggOpusWriter, OpusEncoder, OpusHead};
        let path = std::env::temp_dir().join(format!("scyeq-plugin-reference-{}.ogg", std::process::id()));
        let tone: Vec<f32> = (0..9600).map(|i| 0.5 * (i as f32 * 0.05).sin()).collect();
        let mut encoder = OpusEncoder::new(48_000, 1, Application::Audio).unwrap();
        let head = OpusHead::for_encoder(&encoder, 48_000);
        let mut writer = OggOpusWriter::new(std::fs::File::create(&path).unwrap(), head).unwrap();
        let mut packet = vec![0u8; MAX_PACKET_BYTES];
        for block in tone.as_chunks::<960>().0 {
            let n = encoder.encode(block, 960, &mut packet).unwrap();
            writer.write_packet(&packet[..n]).unwrap();
        }
        writer.finish().unwrap();
        let audio = load_reference(&path);
        std::fs::remove_file(&path).unwrap();
        let audio = audio.unwrap();
        assert_eq!((audio.sample_rate, audio.channels.len()), (48_000, 1));
        assert!(audio.channels[0].iter().any(|s| s.abs() > 0.25));
    }

    #[test]
    fn a_file_that_is_no_audio_is_an_error() {
        let path = std::env::temp_dir().join(format!("scyeq-plugin-not-audio-{}.wav", std::process::id()));
        std::fs::write(&path, b"not audio").unwrap();
        let audio = load_reference(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(audio, Err(ReferenceError::NotAudio));
        assert_eq!(
            load_reference(Path::new("/nonexistent/reference.wav")),
            Err(ReferenceError::Open(std::io::ErrorKind::NotFound))
        );
    }

    #[test]
    fn a_pick_is_taken_once_the_dialog_closed() {
        let (tx, rx) = mpsc::channel();
        let mut picking = Some(rx);
        assert_eq!(take_picked(&mut picking), None, "still open");
        assert!(picking.is_some());
        tx.send(Picked::File(PathBuf::from("/music/reference.wav"))).unwrap();
        assert_eq!(take_picked(&mut picking), Some(Picked::File(PathBuf::from("/music/reference.wav"))));
        assert!(picking.is_none(), "taken once");
        assert_eq!(take_picked(&mut picking), None);
    }

    #[test]
    fn a_dialog_closed_without_a_file_or_gone_ends_the_pick() {
        let (tx, rx) = mpsc::channel();
        let mut picking = Some(rx);
        tx.send(Picked::Cancelled).unwrap();
        assert_eq!(take_picked(&mut picking), Some(Picked::Cancelled));
        assert!(picking.is_none());
        let (tx, rx) = mpsc::channel::<Picked>();
        let mut picking = Some(rx);
        drop(tx);
        assert_eq!(take_picked(&mut picking), Some(Picked::Cancelled));
        assert!(picking.is_none());
    }

    /// The dialog outlives the editor that opened it: the instance's next editor waits for its
    /// answer instead of opening a second dialog.
    #[test]
    fn an_editor_opened_again_takes_the_open_dialogs_answer() {
        let (_plugin, shared) = <crate::ScyEqPlugin as plugin_format::Plugin>::new();
        let (release, wait) = mpsc::channel::<()>();
        pick(shared.picking(), || {
            move || {
                wait.recv().unwrap();
                Picked::File(PathBuf::from("/music/reference.wav"))
            }
        });
        let mut second = false;
        pick(shared.picking(), || {
            second = true;
            || Picked::Cancelled
        });
        assert!(!second, "a second dialog opened");
        release.send(()).unwrap();
        let picked = loop {
            if let Some(picked) = take_picked(&mut shared.picking().lock().unwrap()) {
                break picked;
            }
            std::thread::yield_now();
        };
        assert_eq!(picked, Picked::File(PathBuf::from("/music/reference.wav")));
    }

    #[test]
    fn a_pick_runs_on_a_thread_of_its_own() {
        let (release, wait) = mpsc::channel::<()>();
        let caller = std::thread::current().id();
        let slot = Mutex::new(None);
        pick(&slot, || {
            assert!(slot.try_lock().is_ok(), "the dialog is made with the pick locked");
            move || {
                assert_ne!(std::thread::current().id(), caller);
                wait.recv().unwrap();
                Picked::File(PathBuf::from("/music/reference.wav"))
            }
        });
        let mut picking = slot.into_inner().unwrap();
        assert_eq!(take_picked(&mut picking), None, "the dialog is still open");
        release.send(()).unwrap();
        let picked = loop {
            match take_picked(&mut picking) {
                Some(picked) => break picked,
                None => {
                    assert!(picking.is_some(), "the pick ended without an answer");
                    std::thread::yield_now();
                }
            }
        };
        assert_eq!(picked, Picked::File(PathBuf::from("/music/reference.wav")));
    }

    fn shared() -> Arc<ScyEqShared> {
        <crate::ScyEqPlugin as plugin_format::Plugin>::new().1
    }

    /// Answers until the pick ends, as the editor asks every frame.
    fn answer_of(shared: &ScyEqShared) -> Option<PathBuf> {
        loop {
            let answer = answer(shared);
            if answer.is_some() || shared.picking().lock().unwrap().is_none() {
                break answer;
            }
            std::thread::yield_now();
        }
    }

    #[test]
    fn a_second_pick_while_the_dialog_is_open_says_so() {
        let shared = shared();
        let (release, wait) = mpsc::channel::<()>();
        ask(&shared, true, || {
            move || {
                wait.recv().unwrap();
                Picked::Cancelled
            }
        });
        assert_eq!(shared.notice(), None);
        ask(&shared, true, || || Picked::Cancelled);
        let notice = shared.notice().expect("a notice");
        assert!(notice.contains("already open"), "{notice}");
        assert!(notice.contains("behind"), "{notice}");
        release.send(()).unwrap();
        assert_eq!(answer_of(&shared), None);
        assert_eq!(shared.notice(), None, "the dialog closed");
    }

    #[test]
    fn a_dialog_that_cannot_show_says_what_is_missing() {
        let shared = shared();
        ask(&shared, true, || || Picked::NoDialog);
        assert_eq!(answer_of(&shared), None);
        let notice = shared.notice().expect("a notice");
        assert!(notice.contains("file chooser portal") && notice.contains("zenity"), "{notice}");
        ask(&shared, true, || || Picked::File(PathBuf::from("/music/reference.wav")));
        assert_eq!(answer_of(&shared), Some(PathBuf::from("/music/reference.wav")));
        assert_eq!(shared.notice(), None, "a dialog showed");
    }

    #[test]
    fn a_pick_the_library_cannot_stay_loaded_for_opens_no_dialog() {
        let shared = shared();
        let mut opened = false;
        ask(&shared, false, || {
            opened = true;
            || Picked::Cancelled
        });
        assert!(!opened);
        assert!(shared.notice().is_some_and(|n| n.contains("could not stay loaded")));
    }

    #[test]
    fn a_future_woken_from_another_thread_is_waited_for() {
        let (tx, rx) = mpsc::channel();
        let mut sent = false;
        let future = std::future::poll_fn(move |cx| {
            if let Ok(value) = rx.try_recv() {
                return std::task::Poll::Ready(value);
            }
            if !std::mem::replace(&mut sent, true) {
                let (tx, waker) = (tx.clone(), cx.waker().clone());
                std::thread::spawn(move || {
                    tx.send(7).unwrap();
                    waker.wake();
                });
            }
            std::task::Poll::Pending
        });
        assert_eq!(block_on(future), 7);
    }
}
