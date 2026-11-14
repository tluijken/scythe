//! The plugin's editor without a window: drawn with `egui_kittest`, on instances made through
//! the library's own CLAP entry, its changes read back as the CLAP events a host receives.

use std::ffi::{CStr, c_char, c_void};
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};

use clap_sys::events::{
    CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_PARAM_GESTURE_BEGIN, CLAP_EVENT_PARAM_GESTURE_END, CLAP_EVENT_PARAM_VALUE,
    clap_event_header, clap_event_param_gesture, clap_event_param_value, clap_input_events, clap_output_events,
};
use clap_sys::ext::params::{CLAP_EXT_PARAMS, clap_host_params, clap_param_rescan_flags, clap_plugin_params};
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_host_state, clap_plugin_state};
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::host::clap_host;
use clap_sys::plugin::clap_plugin;
use clap_sys::stream::clap_istream;
use clap_sys::version::CLAP_VERSION;
use egui::{Pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use scyeq_dsp::params::{self, band};
use scyeq_plugin::ScyEqPlugin;
use scyeq_plugin::plugin_format::{self, Editor, Shared, egui};

static DIRTY_MARKS: AtomicU32 = AtomicU32::new(0);

/// The dirty marks are counted for every instance, so tests take turns.
static SERIAL: Mutex<()> = Mutex::new(());

fn turn() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

unsafe extern "C" fn nothing(_: *const clap_host) {}
unsafe extern "C" fn rescan(_: *const clap_host, _: clap_param_rescan_flags) {}
unsafe extern "C" fn clear(_: *const clap_host, _: u32, _: u32) {}
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

/// What the plugin sent, as the host's output event list received it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Sent {
    Begin(u32),
    Value(u32, f64),
    End(u32),
}

/// An initialised instance, made through the library's CLAP entry; destroyed when dropped.
struct Instance(*const clap_plugin);

impl Instance {
    fn new() -> Self {
        unsafe {
            let entry = &scyeq_plugin::clap_entry;
            assert!((entry.init.unwrap())(c"".as_ptr()));
            let factory = (entry.get_factory.unwrap())(CLAP_PLUGIN_FACTORY_ID.as_ptr()) as *const clap_plugin_factory;
            let desc = ((*factory).get_plugin_descriptor.unwrap())(factory, 0);
            let plugin = ((*factory).create_plugin.unwrap())(factory, &HOST, (*desc).id);
            assert!(!plugin.is_null());
            assert!(((*plugin).init.unwrap())(plugin));
            Self(plugin)
        }
    }

    /// With band 1 a bell at 1 kHz, as `values` of a fresh EQ say otherwise.
    fn with_a_band() -> Self {
        let instance = Self::new();
        let mut values = params::defaults();
        values[params::band_param(0, band::USED) as usize] = 1.0;
        values[params::band_param(0, band::FREQ) as usize] = 1000.0;
        values[params::band_param(0, band::GAIN) as usize] = 0.0;
        instance.load(&params::encode(&values, &[], params::VERSION));
        instance
    }

    fn extension<T>(&self, id: &CStr) -> &T {
        unsafe {
            let ext = ((*self.0).get_extension.unwrap())(self.0, id.as_ptr()) as *const T;
            assert!(!ext.is_null(), "{id:?} missing");
            &*ext
        }
    }

    fn load(&self, bytes: &[u8]) {
        let state: &clap_plugin_state = self.extension(CLAP_EXT_STATE);
        let mut source = (bytes, 0usize);
        let stream = clap_istream { ctx: &mut source as *mut (&[u8], usize) as *mut c_void, read: Some(read) };
        assert!(unsafe { (state.load.unwrap())(self.0, &stream) });
    }

    fn value(&self, id: u32) -> f64 {
        let params: &clap_plugin_params = self.extension(CLAP_EXT_PARAMS);
        let mut value = f64::NAN;
        assert!(unsafe { (params.get_value.unwrap())(self.0, id, &mut value) });
        value
    }

    /// What the editor sent since the last flush.
    fn flush(&self) -> Vec<Sent> {
        let params: &clap_plugin_params = self.extension(CLAP_EXT_PARAMS);
        let mut sent = Vec::new();
        let input = clap_input_events { ctx: ptr::null_mut(), size: Some(no_events), get: Some(no_event) };
        let output = clap_output_events { ctx: &mut sent as *mut Vec<Sent> as *mut c_void, try_push: Some(record) };
        unsafe { (params.flush.unwrap())(self.0, &input, &output) };
        sent
    }

    /// The host's main-thread callback, where the plugin marks the project dirty.
    fn on_main_thread(&self) {
        unsafe { ((*self.0).on_main_thread.unwrap())(self.0) };
    }

    fn harness(&self) -> Harness<'static, Editor> {
        let editor = unsafe { plugin_format::clap::editor::<ScyEqPlugin>(self.0) }.expect("the instance's editor");
        Harness::builder()
            .with_size(vec2(1180.0, 680.0))
            .with_step_dt(1.0 / 30.0)
            .build_ui_state(|ui, editor: &mut Editor| editor(ui), editor)
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe { ((*self.0).destroy.unwrap())(self.0) };
    }
}

unsafe extern "C" fn read(stream: *const clap_istream, buffer: *mut c_void, size: u64) -> i64 {
    unsafe {
        let (bytes, at) = &mut *((*stream).ctx as *mut (&[u8], usize));
        let n = (size as usize).min(bytes.len() - *at);
        ptr::copy_nonoverlapping(bytes[*at..].as_ptr(), buffer as *mut u8, n);
        *at += n;
        n as i64
    }
}

unsafe extern "C" fn no_events(_: *const clap_input_events) -> u32 {
    0
}

unsafe extern "C" fn no_event(_: *const clap_input_events, _: u32) -> *const clap_event_header {
    ptr::null()
}

unsafe extern "C" fn record(list: *const clap_output_events, header: *const clap_event_header) -> bool {
    unsafe {
        let sent = &mut *((*list).ctx as *mut Vec<Sent>);
        assert_eq!((*header).space_id, CLAP_CORE_EVENT_SPACE_ID);
        let gesture = || (*(header as *const clap_event_param_gesture)).param_id;
        match (*header).type_ {
            CLAP_EVENT_PARAM_GESTURE_BEGIN => sent.push(Sent::Begin(gesture())),
            CLAP_EVENT_PARAM_GESTURE_END => sent.push(Sent::End(gesture())),
            CLAP_EVENT_PARAM_VALUE => {
                let event = &*(header as *const clap_event_param_value);
                sent.push(Sent::Value(event.param_id, event.value));
            }
            other => panic!("unexpected event type {other}"),
        }
        true
    }
}

fn button(h: &mut Harness<Editor>, pos: Pos2, pressed: bool) {
    h.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
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
    for offset in [band::USED, band::FREQ, band::GAIN] {
        let id = p(offset);
        let value = instance.value(id);
        let at = position(&sent, |s| *s == Sent::Value(id, value)).unwrap_or_else(|| panic!("{offset}: {sent:?}"));
        assert!(at > 0 && sent[at - 1] == Sent::Begin(id) && sent[at + 1] == Sent::End(id), "{sent:?}");
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
