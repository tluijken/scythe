//! The plugin's editor without a window: drawn with `egui_kittest`, on instances made through
//! the library's own CLAP entry, its changes read back as the CLAP events a host receives.

use std::ffi::{CStr, c_char, c_void};
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};

use clap_sys::ext::params::{CLAP_EXT_PARAMS, clap_host_params};
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_host_state};
use clap_sys::host::clap_host;
use clap_sys::version::CLAP_VERSION;
use egui::vec2;
use egui_kittest::kittest::Queryable;
use in_process::{Plugin, Sent, button, clear, nothing, rescan};
use scyeq_dsp::params::{self, band};
use scyeq_plugin::plugin_format::{Shared, egui};

mod in_process;

static DIRTY_MARKS: AtomicU32 = AtomicU32::new(0);

/// The dirty marks are counted for every instance, so tests take turns.
static SERIAL: Mutex<()> = Mutex::new(());

fn turn() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

unsafe extern "C" fn mark_dirty(_: *const clap_host) {
    DIRTY_MARKS.fetch_add(1, Ordering::SeqCst);
}

static HOST_PARAMS: clap_host_params =
    clap_host_params { rescan: Some(rescan), clear: Some(clear), request_flush: Some(nothing) };
static HOST_STATE: clap_host_state = clap_host_state { mark_dirty: Some(mark_dirty) };

unsafe extern "C" fn host_extension(_: *const clap_host, id: *const c_char) -> *const c_void {
    match unsafe { CStr::from_ptr(id) } {
        id if id == CLAP_EXT_PARAMS => &HOST_PARAMS as *const _ as *const c_void,
        id if id == CLAP_EXT_STATE => &HOST_STATE as *const _ as *const c_void,
        _ => ptr::null(),
    }
}

static HOST: clap_host = clap_host {
    clap_version: CLAP_VERSION,
    host_data: ptr::null_mut(),
    name: c"scyeq-plugin editor test host".as_ptr(),
    vendor: c"".as_ptr(),
    url: c"".as_ptr(),
    version: c"0".as_ptr(),
    get_extension: Some(host_extension),
    request_restart: Some(nothing),
    request_process: Some(nothing),
    request_callback: Some(nothing),
};

/// An instance of the editor tests' host, with band 1 a bell at 1 kHz.
struct Instance(Plugin);

impl Instance {
    fn with_a_band() -> Self {
        Self(Plugin::new(&HOST))
    }

    /// The host's main-thread callback, where the plugin marks the project dirty.
    fn on_main_thread(&self) {
        unsafe { ((*self.0.0).on_main_thread.unwrap())(self.0.0) };
    }
}

impl std::ops::Deref for Instance {
    type Target = Plugin;

    fn deref(&self) -> &Plugin {
        &self.0
    }
}

fn position(sent: &[Sent], event: impl Fn(&Sent) -> bool) -> Option<usize> {
    sent.iter().position(event)
}

#[test]
fn editor_drag_reaches_the_host() {
    let _turn = turn();
    let instance = Instance::with_a_band();
    let freq = params::band_param(0, band::FREQ);
    let marks = DIRTY_MARKS.load(Ordering::SeqCst);
    let mut h = instance.harness();
    h.step();
    h.step();
    let from = h.get_by_label("Band 1").rect().center();
    let to = from + vec2(60.0, -30.0);
    h.event(egui::Event::PointerMoved(from));
    h.step();
    button(&mut h, from, true);
    for i in 1..=8 {
        h.event(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 8.0)));
        h.step();
    }
    button(&mut h, to, false);
    h.step();

    let sent = instance.flush();
    assert_eq!(sent.iter().filter(|s| **s == Sent::Begin(freq)).count(), 1, "{sent:?}");
    assert_eq!(sent.iter().filter(|s| **s == Sent::End(freq)).count(), 1, "{sent:?}");
    let begin = position(&sent, |s| *s == Sent::Begin(freq));
    let first_value = position(&sent, |s| matches!(s, Sent::Value(id, _) if *id == freq));
    let end = position(&sent, |s| *s == Sent::End(freq));
    assert!(begin < first_value && first_value < end, "{sent:?}");
    let Some(&Sent::Value(_, last)) = sent.iter().rev().find(|s| matches!(s, Sent::Value(id, _) if *id == freq)) else {
        unreachable!()
    };
    assert!(last > 1100.0, "the band moved up in frequency: {last}");
    assert_eq!(instance.value(freq), last, "the host reads what it was sent");
    instance.on_main_thread();
    assert_eq!(DIRTY_MARKS.load(Ordering::SeqCst), marks, "parameter changes need no dirty mark");
}

#[test]
fn add_band_marks_dirty_and_sends_params() {
    let _turn = turn();
    let instance = Instance::with_a_band();
    let listed: Vec<u32> = scyeq_plugin::params::infos().iter().map(|p| p.id).collect();
    let before: Vec<f64> = listed.iter().map(|&id| instance.value(id)).collect();
    let marks = DIRTY_MARKS.load(Ordering::SeqCst);
    let mut h = instance.harness();
    h.step();
    h.step();
    let graph = h.get_by_label("ScyEQ-Pro graph").rect();
    let at = graph.center() + vec2(-200.0, -40.0);
    h.event(egui::Event::PointerMoved(at));
    h.step();
    for _ in 0..2 {
        button(&mut h, at, true);
        button(&mut h, at, false);
    }
    h.step();

    let sent = instance.flush();
    let p = |offset| params::band_param(1, offset);
    assert_eq!(instance.value(p(band::USED)), 1.0, "band 2 is in use");
    let kind = |s: &Sent| match s {
        Sent::Begin(_) => 0,
        Sent::Value(..) => 1,
        Sent::End(_) => 2,
    };
    assert!(sent.is_sorted_by_key(kind), "one gesture: every begin, then every value, then every end: {sent:?}");
    for offset in [band::USED, band::FREQ, band::GAIN] {
        let id = p(offset);
        let value = instance.value(id);
        assert!(sent.contains(&Sent::Value(id, value)), "{offset}: {sent:?}");
        assert_eq!(sent.iter().filter(|s| **s == Sent::Begin(id)).count(), 1, "{sent:?}");
        assert_eq!(sent.iter().filter(|s| **s == Sent::End(id)).count(), 1, "{sent:?}");
    }
    for (&id, old) in listed.iter().zip(before) {
        let now = instance.value(id);
        let sent_value = sent.contains(&Sent::Value(id, now));
        assert_eq!(sent_value, now != old, "{}: {old} -> {now}, {sent:?}", params::name(id));
    }
    assert_eq!(instance.value(p(band::Q)), params::default(p(band::Q)), "as a new band is made");
    instance.on_main_thread();
    assert_eq!(DIRTY_MARKS.load(Ordering::SeqCst), marks + 1);
}

#[test]
fn two_editors_at_once() {
    let _turn = turn();
    let (one, two) = (Instance::with_a_band(), Instance::with_a_band());
    for _ in 0..20 {
        let (mut a, mut b) = (one.harness(), two.harness());
        for _ in 0..3 {
            a.step();
            b.step();
        }
        let at = a.get_by_label("Band 1").rect().center();
        a.event(egui::Event::PointerMoved(at));
        a.step();
        button(&mut a, at, true);
        a.event(egui::Event::PointerMoved(at + vec2(20.0, 0.0)));
        a.step();
        drop(a);
        b.step();
        drop(b);
    }
    let sent = one.flush();
    let begun = sent.iter().filter(|s| matches!(s, Sent::Begin(_))).count();
    let ended = sent.iter().filter(|s| matches!(s, Sent::End(_))).count();
    assert!(begun > 0 && begun == ended, "closing mid-drag ends the gesture: {sent:?}");
    assert!(two.flush().is_empty(), "the other instance was only looked at");
}

#[test]
fn solo_stays_on_the_instance() {
    let _turn = turn();
    let instance = Instance::with_a_band();
    let solo = params::band_param(0, band::SOLO);
    let marks = DIRTY_MARKS.load(Ordering::SeqCst);
    let mut h = instance.harness();
    h.step();
    h.step();
    let at = h.get_by_label("Band 1").rect().center();
    h.event(egui::Event::PointerMoved(at));
    h.step();
    button(&mut h, at, true);
    button(&mut h, at, false);
    h.get_by_label("Solo").click();
    h.step();

    let soloed: Vec<_> =
        scyeq_plugin::registry::others(0).into_iter().filter(|shared| shared.param(solo) == 1.0).collect();
    assert_eq!(soloed.len(), 1, "the band is soloed on its instance");
    assert!(instance.flush().is_empty(), "the host hears nothing of it");
    instance.on_main_thread();
    assert_eq!(DIRTY_MARKS.load(Ordering::SeqCst), marks, "and the project does not change");
    assert_eq!(params::decode(&soloed[0].save()).unwrap().values[solo as usize], 0.0, "nor is it saved");
}

#[test]
fn arrow_keys_move_the_selected_band() {
    let _turn = turn();
    let instance = Instance::with_a_band();
    let freq = params::band_param(0, band::FREQ);
    let mut h = instance.harness();
    h.step();
    h.step();
    let at = h.get_by_label("Band 1").rect().center();
    h.event(egui::Event::PointerMoved(at));
    h.step();
    button(&mut h, at, true);
    button(&mut h, at, false);
    h.key_press(egui::Key::ArrowRight);
    h.step();
    let sent = instance.flush();
    let Some(&Sent::Value(_, moved)) = sent.iter().find(|s| matches!(s, Sent::Value(id, _) if *id == freq)) else {
        panic!("no frequency change: {sent:?}")
    };
    assert!((moved - 1000.0 * 2f64.powf(1.0 / 12.0)).abs() < 1e-6, "{moved}");
}
